//! OIDC protocol: authorization-code flow with PKCE.
//!
//! Narrow surface for the HTTP layer (src/auth.rs):
//! - `start_login(cfg, pending) -> authorize_url`
//! - `finish_login(cfg, pending, query) -> Result<OidcIdentity, OidcError>`
//!
//! Everything spec-critical lives behind those two calls: discovery, PKCE
//! S256, code exchange (client_secret_basic), JWKS fetch, ID-token signature
//! (RS256) + iss/aud/exp/iat/nonce validation, and the email-trust rule
//! (email only usable when email_verified is true or absent).

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;

use super::{env_or, random_b64url, ContextEnv};

#[derive(Clone, Debug)]
pub struct OidcCfg {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub admin_emails: Vec<String>,
}

impl OidcCfg {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(OidcCfg {
            issuer: std::env::var("MNTR_OIDC_ISSUER").context_env("MNTR_OIDC_ISSUER")?,
            client_id: std::env::var("MNTR_OIDC_CLIENT_ID").context_env("MNTR_OIDC_CLIENT_ID")?,
            client_secret: std::env::var("MNTR_OIDC_CLIENT_SECRET").context_env("MNTR_OIDC_CLIENT_SECRET")?,
            redirect_uri: std::env::var("MNTR_OIDC_REDIRECT_URI").context_env("MNTR_OIDC_REDIRECT_URI")?,
            scopes: env_or("MNTR_OIDC_SCOPES", "openid email profile"),
            admin_emails: std::env::var("MNTR_OIDC_ADMIN_EMAILS").unwrap_or_default()
                .split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect(),
        })
    }

    fn is_admin_email(&self, email: &str) -> bool {
        self.admin_emails.contains(&email.to_ascii_lowercase())
    }
}

/// Errors classified by the HTTP status the handler should return.
#[derive(Debug)]
pub enum OidcError {
    /// provider unreachable / misconfigured -> 502
    Upstream(String),
    /// the login attempt is not trustworthy -> 401
    Auth(String),
    /// malformed callback -> 400
    BadRequest(String),
}

/// A verified external identity from a successful OIDC login.
pub struct OidcIdentity {
    pub email: String,
    pub sub: String,
    pub force_admin: bool,
}

/// In-memory pending authorization requests (state -> nonce + PKCE verifier).
/// Single-process server; entries expire after 10 minutes.
pub struct PendingOidc(pub Mutex<HashMap<String, (String, String, std::time::Instant)>>);

impl PendingOidc {
    pub fn new() -> Self {
        PendingOidc(Mutex::new(HashMap::new()))
    }
    pub fn insert(&self, state: String, nonce: String, verifier: String) {
        let mut m = self.0.lock().unwrap();
        m.retain(|_, (_, _, t)| t.elapsed().as_secs() < 600);
        m.insert(state, (nonce, verifier, std::time::Instant::now()));
    }
    pub fn take(&self, state: &str) -> Option<(String, String)> {
        self.0.lock().unwrap().remove(state).map(|(n, v, _)| (n, v))
    }
}

/// Build the provider authorize URL (state + PKCE + nonce generated and
/// remembered in `pending`). The handler turns this into a redirect.
pub async fn start_login(cfg: &OidcCfg, pending: &PendingOidc) -> Result<String, OidcError> {
    let disc = discover(cfg).await.map_err(|e| OidcError::Upstream(e.to_string()))?;
    let state = random_b64url(24);
    let nonce = random_b64url(24);
    let verifier = random_b64url(32);
    let challenge = pkce_challenge(&verifier);
    pending.insert(state.clone(), nonce.clone(), verifier);
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        disc.authorization_endpoint,
        urlencoding(&cfg.client_id), urlencoding(&cfg.redirect_uri),
        urlencoding(&cfg.scopes), urlencoding(&state), urlencoding(&nonce),
        urlencoding(&challenge),
    );
    Ok(url)
}

/// Complete the flow from the provider's callback query: verify state,
/// exchange the code, verify the ID token, apply the email-trust rule and
/// the admin allowlist. Returns the verified identity; the handler owns
/// user provisioning and session creation.
pub async fn finish_login(
    cfg: &OidcCfg,
    pending: &PendingOidc,
    q: &HashMap<String, String>,
) -> Result<OidcIdentity, OidcError> {
    if let Some(err) = q.get("error") {
        let desc = q.get("error_description").cloned().unwrap_or_default();
        return Err(OidcError::Auth(format!("oidc provider error: {err} {desc}")));
    }
    let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else {
        return Err(OidcError::BadRequest("missing code or state".into()));
    };
    let Some((nonce, verifier)) = pending.take(state) else {
        return Err(OidcError::Auth("unknown or expired state".into()));
    };
    let disc = discover(cfg).await.map_err(|e| OidcError::Upstream(e.to_string()))?;
    let tokens = exchange(cfg, &disc, code, &verifier).await
        .map_err(|e| OidcError::Auth(e.to_string()))?;
    let id_token = tokens.get("id_token").and_then(|v| v.as_str()).unwrap_or("");
    let jwks = fetch_jwks(cfg, &disc).await.map_err(|e| OidcError::Upstream(e.to_string()))?;
    let claims = verify_idtoken(id_token, &jwks, cfg, &nonce)
        .map_err(|e| OidcError::Auth(e.to_string()))?;
    // OIDC core: email may only be trusted when verified (or unverified-but-configured)
    let email = match (&claims.email, claims.email_verified) {
        (Some(e), Some(true)) | (Some(e), None) => e.clone(),
        _ => return Err(OidcError::Auth("id_token has no verified email".into())),
    };
    Ok(OidcIdentity {
        force_admin: cfg.is_admin_email(&email),
        email,
        sub: claims.sub,
    })
}

// ------------------------------------------------------- protocol internals ----

#[derive(Debug, Deserialize)]
struct OidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

async fn discover(cfg: &OidcCfg) -> anyhow::Result<OidcDiscovery> {
    let url = format!("{}/.well-known/openid-configuration", cfg.issuer.trim_end_matches('/'));
    let d: OidcDiscovery = reqwest::Client::new().get(&url).send().await
        .map_err(|e| anyhow::anyhow!("oidc discovery: {e}"))?
        .error_for_status().map_err(|e| anyhow::anyhow!("oidc discovery: {e}"))?
        .json().await.map_err(|e| anyhow::anyhow!("oidc discovery parse: {e}"))?;
    // issuer mismatch is a hard error: tokens from a different issuer must never be accepted
    if d.issuer != cfg.issuer {
        anyhow::bail!("oidc discovery issuer '{}' != configured '{}'", d.issuer, cfg.issuer);
    }
    Ok(d)
}

fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

fn urlencoding(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// Exchange an authorization code for tokens (client_secret_basic + PKCE).
async fn exchange(
    cfg: &OidcCfg,
    disc: &OidcDiscovery,
    code: &str,
    verifier: &str,
) -> anyhow::Result<serde_json::Value> {
    let resp = reqwest::Client::new()
        .post(&disc.token_endpoint)
        .basic_auth(&cfg.client_id, Some(&cfg.client_secret))
        .form(&[
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", cfg.redirect_uri.clone()),
            ("code_verifier", verifier.to_string()),
        ])
        .send().await.map_err(|e| anyhow::anyhow!("token exchange: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("token exchange failed ({status}): {body}");
    }
    Ok(body)
}

#[derive(Debug, Deserialize)]
struct Jwks { keys: Vec<Jwk> }

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

async fn fetch_jwks(cfg: &OidcCfg, disc: &OidcDiscovery) -> anyhow::Result<Jwks> {
    // jwks_uri is absolute per spec; fall back to issuer join for lenient providers
    let url = if disc.jwks_uri.starts_with("http") {
        disc.jwks_uri.clone()
    } else {
        format!("{}{}", cfg.issuer.trim_end_matches('/'), disc.jwks_uri)
    };
    let j: Jwks = reqwest::Client::new().get(&url).send().await
        .map_err(|e| anyhow::anyhow!("jwks fetch: {e}"))?
        .error_for_status().map_err(|e| anyhow::anyhow!("jwks fetch: {e}"))?
        .json().await.map_err(|e| anyhow::anyhow!("jwks parse: {e}"))?;
    Ok(j)
}

/// Claims mntr requires from a verified ID token.
#[derive(Debug, Deserialize)]
struct IdClaims {
    #[allow(dead_code)] // validated by jsonwebtoken against Validation
    iss: String,
    aud: OidcAud,
    #[allow(dead_code)] // validated by jsonwebtoken
    exp: i64,
    #[allow(dead_code)] // validated by jsonwebtoken
    iat: i64,
    sub: String,
    nonce: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
    #[allow(dead_code)] // reserved: display name once the SPA shows it
    name: Option<String>,
}

/// OIDC aud may be a string or an array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OidcAud { One(String), Many(Vec<String>) }

impl OidcAud {
    fn contains(&self, a: &str) -> bool {
        match self {
            OidcAud::One(s) => s == a,
            OidcAud::Many(v) => v.iter().any(|s| s == a),
        }
    }
}

/// Verify an ID token against the provider JWKS and OIDC core rules
/// (signature RS256, iss, aud=client_id, exp/iat, nonce). Returns claims.
fn verify_idtoken(
    id_token: &str,
    jwks: &Jwks,
    cfg: &OidcCfg,
    nonce: &str,
) -> anyhow::Result<IdClaims> {
    let header = jsonwebtoken::decode_header(id_token)
        .map_err(|e| anyhow::anyhow!("id_token header: {e}"))?;
    let alg = match header.alg {
        jsonwebtoken::Algorithm::RS256 => jsonwebtoken::Algorithm::RS256,
        other => anyhow::bail!("unsupported id_token alg {other:?} (RS256 only)"),
    };
    let key = jwks.keys.iter().find(|k| {
        k.kty == "RSA" && header.kid.as_deref().map(|kid| k.kid.as_deref() == Some(kid)).unwrap_or(true)
    }).ok_or_else(|| anyhow::anyhow!("no matching JWK for kid {:?}", header.kid))?;
    let n = key.n.as_ref().ok_or_else(|| anyhow::anyhow!("JWK missing n"))?;
    let e = key.e.as_ref().ok_or_else(|| anyhow::anyhow!("JWK missing e"))?;
    let decoding = jsonwebtoken::DecodingKey::from_rsa_components(n, e)
        .map_err(|e| anyhow::anyhow!("jwk decode: {e}"))?;
    let mut v = jsonwebtoken::Validation::new(alg);
    v.set_audience(&[cfg.client_id.as_str()]);
    v.set_issuer(&[&cfg.issuer]);
    v.leeway = 30;
    v.required_spec_claims = ["exp", "iat", "iss", "aud", "sub"].into_iter().map(String::from).collect();
    let data = jsonwebtoken::decode::<IdClaims>(id_token, &decoding, &v)
        .map_err(|e| anyhow::anyhow!("id_token verify: {e}"))?;
    let claims = data.claims;
    if !claims.aud.contains(&cfg.client_id) {
        anyhow::bail!("id_token aud mismatch");
    }
    if claims.nonce.as_deref() != Some(nonce) {
        anyhow::bail!("id_token nonce mismatch");
    }
    Ok(claims)
}
