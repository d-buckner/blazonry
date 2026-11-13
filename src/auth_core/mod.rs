// SPDX-License-Identifier: AGPL-3.0-only
//! Pluggable admin auth modes: local (argon2), ldap, oidc, none.
//!
//! Selected at startup via `BLAZONRY_AUTH` (default `local`). The device plane is
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
//!
//! `BLAZONRY_ADMIN_TOKEN` adds a static bearer the admin plane accepts in every
//! mode. It exists for a machine caller that provisions the server over the
//! admin API and has no browser to sign in with; under `oidc` and `ldap` there is
//! no password route it could use.
//!
//! Module layout: `config` concerns live here (AuthConfig::from_env, fail-fast
//! at startup); protocol logic lives in `ldap` and `oidc`. Each protocol
//! module exposes a narrow surface: ldap = one verify call, oidc = start_login
//! + finish_login. HTTP translation stays in src/auth.rs.

mod ldap;
mod oidc;

pub use ldap::{LdapCfg, ldap_verify};
pub use oidc::{OidcCfg, OidcError, OidcIdentity, PendingOidc, finish_login, start_login};

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMode {
    Local,
    Ldap,
    Oidc,
    None,
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub mode: AuthMode,
    pub ldap: Option<LdapCfg>,
    pub oidc: Option<OidcCfg>,
    /// A static bearer the admin plane accepts in every mode, for a machine
    /// caller that cannot drive a browser sign-in. `None` when unset.
    pub admin_token: Option<String>,
}

pub type Shared = Arc<AuthConfig>;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Empty-string env vars count as unset (docker-compose passes empty through).
fn env_set(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

impl AuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let mode = match env_or("BLAZONRY_AUTH", "local")
            .to_ascii_lowercase()
            .as_str()
        {
            "local" => AuthMode::Local,
            "ldap" => AuthMode::Ldap,
            "oidc" => AuthMode::Oidc,
            "none" => AuthMode::None,
            other => anyhow::bail!("BLAZONRY_AUTH must be local|ldap|oidc|none, got '{other}'"),
        };
        let ldap = if mode == AuthMode::Ldap {
            Some(LdapCfg::from_env()?)
        } else {
            None
        };
        let oidc = if mode == AuthMode::Oidc {
            Some(OidcCfg::from_env()?)
        } else {
            None
        };
        let admin_token = env_set("BLAZONRY_ADMIN_TOKEN");
        if admin_token.as_deref().is_some_and(|t| t.len() < 32) {
            anyhow::bail!("BLAZONRY_ADMIN_TOKEN must be at least 32 characters");
        }
        Ok(AuthConfig {
            mode,
            ldap,
            oidc,
            admin_token,
        })
    }

    /// True when `bearer` is the configured admin token. Constant-time so a
    /// caller who can time the response learns nothing about the value.
    pub fn admin_token_matches(&self, bearer: &str) -> bool {
        match &self.admin_token {
            Some(expected) => constant_time_eq(expected.as_bytes(), bearer.as_bytes()),
            None => false,
        }
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

// ------------------------------------------------------------- comparison ----

/// Compare two byte slices without leaking their equality through timing: every
/// byte is examined and the differences are accumulated rather than returned
/// early. Lengths differ, so no early return is available anyway.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().min(b.len()) {
        diff |= (a[i] ^ b[i]) as usize;
    }
    diff == 0
}

// ------------------------------------------------------------ randomness ----

/// n cryptographically random bytes. /dev/urandom is the zero-dependency
/// CSPRNG on Linux; blazonry targets LAN boxes, so no getrandom crate.
pub fn rand_bytes(n: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; n];
    std::io::Read::read_exact(
        &mut std::fs::File::open("/dev/urandom").expect("/dev/urandom"),
        &mut bytes,
    )
    .expect("urandom read");
    bytes
}

pub fn random_hex(nbytes: usize) -> String {
    rand_bytes(nbytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn random_b64url(nbytes: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand_bytes(nbytes))
}

pub(crate) trait ContextEnv {
    fn context_env(self, key: &str) -> anyhow::Result<String>;
}
impl ContextEnv for Result<String, std::env::VarError> {
    fn context_env(self, key: &str) -> anyhow::Result<String> {
        self.map_err(|_| anyhow::anyhow!("missing required env var {key}"))
    }
}
