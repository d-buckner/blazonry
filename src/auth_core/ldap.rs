// SPDX-License-Identifier: AGPL-3.0-only
//! LDAP protocol: verify a username/password by simple bind.
//!
//! Two DN resolution modes (exactly one is configured; enforced at startup):
//! - bind-DN template: `uid={username},ou=people,...` — no service account.
//! - search mode: optional service-account bind, search for the user, bind as
//!   the found DN.
//!
//! ldap3 is a blocking client; callers must wrap in spawn_blocking.

use super::{env_or, env_set, ContextEnv};

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

impl LdapCfg {
    pub fn from_env() -> anyhow::Result<Self> {
        let url = std::env::var("BLAZONRY_LDAP_URL").context_env("BLAZONRY_LDAP_URL")?;
        let template = env_set("BLAZONRY_LDAP_BIND_DN_TEMPLATE");
        let search_base = env_set("BLAZONRY_LDAP_SEARCH_BASE");
        if template.is_none() && search_base.is_none() {
            anyhow::bail!("ldap auth needs BLAZONRY_LDAP_BIND_DN_TEMPLATE or BLAZONRY_LDAP_SEARCH_BASE");
        }
        Ok(LdapCfg {
            url,
            bind_dn_template: template,
            search_base,
            search_filter: env_or("BLAZONRY_LDAP_SEARCH_FILTER", "(uid={username})"),
            search_bind_dn: env_set("BLAZONRY_LDAP_SEARCH_BIND_DN"),
            search_bind_pw: env_set("BLAZONRY_LDAP_SEARCH_BIND_PW"),
            email_attr: env_or("BLAZONRY_LDAP_EMAIL_ATTR", "mail"),
        })
    }
}

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
        _ => unreachable!("LdapCfg::from_env enforces template-or-base"),
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
