//! Pluggable admin auth modes: local (argon2), ldap, oidc, none.
//!
//! Selected at startup via `MNTR_AUTH` (default `local`). The device plane is
//! unaffected by auth mode — device auth is the per-device Access-Token.
//!
//! - `local`: existing argon2 accounts + registration (unchanged).
//! - `ldap`: /admin/login verifies against a directory (simple bind, search
//!   mode or DN template). Registration is disabled; users are provisioned on
//!   first successful bind.
//! - `oidc`: authorization-code + PKCE flow at /auth/oidc/login +
//!   /auth/oidc/callback. Registration is disabled; users are provisioned on
//!   first successful callback. ID tokens are verified against the provider's
//!   JWKS (RS256).
//! - `none`: admin plane is open (LAN-trust deployments). No accounts exist.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Local,
    Ldap,
    Oidc,
    None,
}

#[derive(Clone, Debug)]
pub struct LdapCfg {
    pub url: String,
    /// Either bind_dn_template ("uid={username},ou=people,dc=x,dc=y") or
    /// search_base (+ optional search_filter, {username} placeholder).
    pub bind_dn_template: Option<String>,
    pub search_base: Option<String>,
    pub search_filter: String,
    pub search_bind_dn: Option<String>,
    pub search_bind_pw: Option<String>,
    pub email_attr: String,
}

#[derive(Clone, Debug)]
pub struct OidcCfg {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub admin_emails: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub mode: AuthMode,
    pub ldap: Option<LdapCfg>,
    pub oidc: Option<OidcCfg>,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

impl AuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let mode = match env_or("MNTR_AUTH", "local").to_ascii_lowercase().as_str() {
            "local" => AuthMode::Local,
            "ldap" => AuthMode::Ldap,
            "oidc" => AuthMode::Oidc,
            "none" => AuthMode::None,
            other => anyhow::bail!("MNTR_AUTH must be local|ldap|oidc|none, got '{other}'"),
        };
        let ldap = if mode == AuthMode::Ldap {
            let url = std::env::var("MNTR_LDAP_URL").context_env("MNTR_LDAP_URL")?;
            let template = std::env::var("MNTR_LDAP_BIND_DN_TEMPLATE").ok().filter(|s| !s.is_empty());
            let search_base = std::env::var("MNTR_LDAP_SEARCH_BASE").ok().filter(|s| !s.is_empty());
            if template.is_none() && search_base.is_none() {
                anyhow::bail!("ldap auth needs MNTR_LDAP_BIND_DN_TEMPLATE or MNTR_LDAP_SEARCH_BASE");
            }
            Some(LdapCfg {
                url,
                bind_dn_template: template,
                search_base,
                search_filter: env_or("MNTR_LDAP_SEARCH_FILTER", "(uid={username})"),
                search_bind_dn: std::env::var("MNTR_LDAP_SEARCH_BIND_DN").ok().filter(|s| !s.is_empty()),
                search_bind_pw: std::env::var("MNTR_LDAP_SEARCH_BIND_PW").ok().filter(|s| !s.is_empty()),
                email_attr: env_or("MNTR_LDAP_EMAIL_ATTR", "mail"),
            })
        } else {
            None
        };
        let oidc = if mode == AuthMode::Oidc {
            Some(OidcCfg {
                issuer: std::env::var("MNTR_OIDC_ISSUER").context_env("MNTR_OIDC_ISSUER")?,
                client_id: std::env::var("MNTR_OIDC_CLIENT_ID").context_env("MNTR_OIDC_CLIENT_ID")?,
                client_secret: std::env::var("MNTR_OIDC_CLIENT_SECRET").context_env("MNTR_OIDC_CLIENT_SECRET")?,
                redirect_uri: std::env::var("MNTR_OIDC_REDIRECT_URI").context_env("MNTR_OIDC_REDIRECT_URI")?,
                scopes: env_or("MNTR_OIDC_SCOPES", "openid email profile"),
                admin_emails: std::env::var("MNTR_OIDC_ADMIN_EMAILS").unwrap_or_default()
                    .split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect(),
            })
        } else {
            None
        };
        Ok(AuthConfig { mode, ldap, oidc })
    }

    pub fn mode_str(&self) -> &'static str {
        match self.mode {
            AuthMode::Local => "local",
            AuthMode::Ldap => "ldap",
            AuthMode::Oidc => "oidc",
            AuthMode::None => "none",
        }
    }
}

trait ContextEnv {
    fn context_env(self, key: &str) -> anyhow::Result<String>;
}
impl ContextEnv for Result<String, std::env::VarError> {
    fn context_env(self, key: &str) -> anyhow::Result<String> {
        self.map_err(|_| anyhow::anyhow!("missing required env var {key}"))
    }
}

// ---------------------------------------------------------------- LDAP ----

/// Verify an LDAP username/password by simple bind. Returns the user's email
/// (from `email_attr`, falling back to `{username}@{host}` of the LDAP URL).
/// Blocking; call via spawn_blocking.
pub fn ldap_verify(cfg: &LdapCfg, username: &str, password: &str) -> anyhow::Result<String> {
    use ldap3::{LdapConn, Scope};
    let host = cfg.url.split("//").nth(1).unwrap_or("localhost").trim_matches('/');
    let fallback_email = format!("{username}@{host}");
    let mut ldap = LdapConn::new(&cfg.url)
        .map_err(|e| anyhow::anyhow!("ldap connect: {e}"))?;
    let mut email: Option<String> = None;
    let dn = match (&cfg.bind_dn_template, &cfg.search_base) {
        (Some(t), _) => t.replace("{username}", username),
        (None, Some(base)) => {
            if let (Some(bdn), Some(bpw)) = (&cfg.search_bind_dn, &cfg.search_bind_pw) {
                let r = ldap.simple_bind(bdn, bpw).map_err(|e| anyhow::anyhow!("ldap search bind: {e}"))?;
                if r.rc != 0 { anyhow::bail!("ldap search bind rejected (rc {})", r.rc); }
            }
            let filter = cfg.search_filter.replace("{username}", username);
            let res = ldap.search(base, Scope::Subtree, &filter, vec![cfg.email_attr.as_str()])
                .map_err(|e| anyhow::anyhow!("ldap search: {e}"))?;
            let entry = res.0.into_iter().next().ok_or_else(|| anyhow::anyhow!("ldap: user not found"))?;
            let entry = ldap3::SearchEntry::construct(entry);
            email = entry.attrs.get(&cfg.email_attr).and_then(|v| v.first()).cloned().filter(|s| !s.is_empty());
            entry.dn
        }
        _ => unreachable!("AuthConfig::from_env enforces template-or-base"),
    };
    // simple bind with the user's own credentials is the actual verification.
    // NOTE: Ok() only means the protocol exchange succeeded — the verdict is
    // in rc (49 = invalid credentials). Never trust Ok() alone.
    let r = ldap.simple_bind(&dn, password).map_err(|e| anyhow::anyhow!("ldap bind failed: {e}"))?;
    if r.rc != 0 { anyhow::bail!("ldap bind rejected (rc {})", r.rc); }
    // template mode: read the email attr from the user's own entry (post-bind,
    // scope=base — reading one's own entry is universally permitted)
    if email.is_none() && cfg.bind_dn_template.is_some() {
        if let Ok(res) = ldap.search(&dn, Scope::Base, "(objectClass=*)", vec![cfg.email_attr.as_str()]) {
            if let Some(entry) = res.0.into_iter().next().map(ldap3::SearchEntry::construct) {
                email = entry.attrs.get(&cfg.email_attr).and_then(|v| v.first()).cloned().filter(|s| !s.is_empty());
            }
        }
    }
    Ok(email.unwrap_or(fallback_email))
}

// ---------------------------------------------------------------- OIDC ----

#[derive(Debug, Deserialize)]
pub struct OidcDiscovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

pub async fn oidc_discover(cfg: &OidcCfg) -> anyhow::Result<OidcDiscovery> {
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

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn random_b64url(nbytes: usize) -> String {
    let mut bytes = vec![0u8; nbytes];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").expect("/dev/urandom"), &mut bytes)
        .expect("urandom read");
    b64url(&bytes)
}

impl PendingOidc {
    pub fn random() -> String {
        random_b64url(24)
    }
}

/// PKCE S256: BASE64URL(SHA256(verifier)).
pub fn pkce_challenge(verifier: &str) -> String {
    use sha2::Digest;
    b64url(&sha2::Sha256::digest(verifier.as_bytes()))
}

#[derive(Debug, Deserialize)]
pub struct Jwks { pub keys: Vec<Jwk> }

#[derive(Debug, Deserialize)]
pub struct Jwk {
    pub kid: Option<String>,
    pub kty: String,
    pub n: Option<String>,
    pub e: Option<String>,
}

pub async fn oidc_jwks(cfg: &OidcCfg, disc: &OidcDiscovery) -> anyhow::Result<Jwks> {
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
pub struct IdClaims {
    pub iss: String,
    pub aud: OidcAud,
    pub exp: i64,
    pub iat: i64,
    pub sub: String,
    pub nonce: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub name: Option<String>,
}

/// OIDC aud may be a string or an array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum OidcAud { One(String), Many(Vec<String>) }

impl OidcAud {
    pub fn contains(&self, a: &str) -> bool {
        match self {
            OidcAud::One(s) => s == a,
            OidcAud::Many(v) => v.iter().any(|s| s == a),
        }
    }
}

/// Verify an ID token against the provider JWKS and OIDC core rules
/// (signature RS256, iss, aud=client_id, exp/iat, nonce). Returns claims.
pub fn oidc_verify_idtoken(
    id_token: &str,
    jwks: &Jwks,
    cfg: &OidcCfg,
    nonce: &str,
    _now: i64,
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

/// Exchange an authorization code for tokens (client_secret_basic + PKCE).
pub async fn oidc_exchange(
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
