// SPDX-License-Identifier: AGPL-3.0-only
//! Auth-mode integration tests: real OpenLDAP (slapd) and a real OIDC
//! provider (node oidc-provider) must be running locally. These are opt-in:
//! they skip cleanly when the test IdPs are absent.
//!
//!   LDAP:  slapd on 127.0.0.1:2389, dc=blazonry,dc=test, uid=alice/a... (see scripts/auth_test_idps.md)
//!   OIDC:  oidc-provider on 127.0.0.1:2390, client blazonry/blazonry-secret, user alice/alicepass
//!
//! The blazonry server under test binds 127.0.0.1:2391 so the OIDC redirect_uri
//! can be registered ahead of time (providers require exact match).

#![allow(dead_code)]

use std::process::Stdio;

fn idp_up(url: &str) -> bool {
    std::net::TcpStream::connect(url).is_ok()
}

async fn spawn_server_env(tag: &str, port: u16, envs: &[(&str, String)]) -> (tokio::process::Child, String) {
    let dir = std::env::temp_dir().join(format!("blazonry-test-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.db").to_str().unwrap().to_string();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_blazonry"));
    cmd.args(["serve", "--db", &db, "--addr", &format!("127.0.0.1:{port}")])
        .env_remove("BLAZONRY_AUTH")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs { cmd.env(k, v); }
    // fixed ports: a leaked server from a panicking test would squat them
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    while let Some(line) = lines.next_line().await.unwrap() {
        if line.contains("listening") { break; }
    }
    (child, format!("127.0.0.1:{port}"))
}

async fn req_no_redirect(client: &reqwest::Client, url: &str, headers: &[(&str, String)]) -> reqwest::Response {
    let mut r = client.get(url);
    for (k, v) in headers { r = r.header(*k, v); }
    r.send().await.unwrap()
}

// ----------------------------------------------------------------- none ----

#[tokio::test]
async fn auth_none_opens_admin_plane() {
    let (mut child, addr) = spawn_server_env("auth-none", 2392, &[("BLAZONRY_AUTH", "none".into())]).await;
    let cli = reqwest::Client::new();
    // no token at all -> admin plane works
    let s = cli.get(format!("http://{addr}/admin/devices")).send().await.unwrap().status();
    assert_eq!(s, 200);
    let me: serde_json::Value = cli.get(format!("http://{addr}/admin/me")).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["role"], "admin");
    // mode endpoint reports none; login/register are refused
    let mode: serde_json::Value = cli.get(format!("http://{addr}/admin/auth/mode")).send().await.unwrap().json().await.unwrap();
    assert_eq!(mode["mode"], "none");
    let s = cli.post(format!("http://{addr}/admin/register")).json(&serde_json::json!({"email":"x@y.z","password":"<REDACTED>"})).send().await.unwrap().status();
    assert_eq!(s, 404);
    let s = cli.post(format!("http://{addr}/admin/login")).json(&serde_json::json!({"email":"x@y.z","password":"x"})).send().await.unwrap().status();
    assert_eq!(s, 400);
    // device plane unaffected
    let s = cli.get(format!("http://{addr}/api/setup")).header("id", "aa:bb:cc:00:00:01").send().await.unwrap().status();
    assert_eq!(s, 200);
    child.kill().await.unwrap();
}

// ----------------------------------------------------------------- ldap ---

const LDAP_BASE: &[(&str, String)] = &[];

fn ldap_envs(mode: &str) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = vec![
        ("BLAZONRY_AUTH", "ldap".into()),
        ("BLAZONRY_LDAP_URL", "ldap://127.0.0.1:2389".into()),
    ];
    match mode {
        "template" => v.push(("BLAZONRY_LDAP_BIND_DN_TEMPLATE", "uid={username},ou=people,dc=blazonry,dc=test".into())),
        "search" => {
            v.push(("BLAZONRY_LDAP_SEARCH_BASE", "ou=people,dc=blazonry,dc=test".into()));
            v.push(("BLAZONRY_LDAP_SEARCH_FILTER", "(uid={username})".into()));
        }
        _ => {}
    }
    v
}

async fn ldap_flow(tag: &'static str, mode: &'static str) {
    let envs = ldap_envs(mode);
    let envs_ref: Vec<(&str, String)> = envs.iter().map(|(k, v)| (*k, v.clone())).collect();
    let _ = LDAP_BASE;
    let port: u16 = if mode == "template" { 2393 } else { 2394 };
    let (mut child, addr) = spawn_server_env(tag, port, &envs_ref).await;
    let cli = reqwest::Client::new();

    // mode endpoint
    let mode_json: serde_json::Value = cli.get(format!("http://{addr}/admin/auth/mode")).send().await.unwrap().json().await.unwrap();
    assert_eq!(mode_json["mode"], "ldap");

    // good credentials -> session token; first user becomes admin
    let r: serde_json::Value = cli.post(format!("http://{addr}/admin/login"))
        .json(&serde_json::json!({"username": "alice", "password": "alicepass"}))
        .send().await.unwrap().json().await.unwrap();
    let tok = r["token"].as_str().expect(&format!("login should succeed, got {r}")).to_string();
    assert_eq!(r["role"], "admin");
    assert_eq!(r["email"], "alice@blazonry.test"); // from mail attr

    // session token works on the admin plane
    let s = cli.get(format!("http://{addr}/admin/devices")).header("authorization", format!("Bearer {tok}")).send().await.unwrap().status();
    assert_eq!(s, 200);

    // wrong password -> 401
    let s = cli.post(format!("http://{addr}/admin/login"))
        .json(&serde_json::json!({"username": "alice", "password": "WRONG"}))
        .send().await.unwrap().status();
    assert_eq!(s, 401);

    // unknown user -> 401
    let s = cli.post(format!("http://{addr}/admin/login"))
        .json(&serde_json::json!({"username": "mallory", "password": "x"}))
        .send().await.unwrap().status();
    assert_eq!(s, 401);

    // second user -> role user (not admin)
    let r: serde_json::Value = cli.post(format!("http://{addr}/admin/login"))
        .json(&serde_json::json!({"username": "bob", "password": "bobpass"}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(r["role"], "user");

    // registration disabled
    let s = cli.post(format!("http://{addr}/admin/register"))
        .json(&serde_json::json!({"email": "e@e.e", "password": "<REDACTED>"}))
        .send().await.unwrap().status();
    assert_eq!(s, 404);

    // email-style login also accepted (localpart extracted)
    let r: serde_json::Value = cli.post(format!("http://{addr}/admin/login"))
        .json(&serde_json::json!({"email": "alice@blazonry.test", "password": "alicepass"}))
        .send().await.unwrap().json().await.unwrap();
    assert!(r["token"].is_string());

    child.kill().await.unwrap();
}

#[tokio::test]
async fn auth_ldap_bind_dn_template() {
    if !idp_up("127.0.0.1:2389") { eprintln!("skipping: slapd not on 127.0.0.1:2389"); return; }
    ldap_flow("auth-ldap-tmpl", "template").await;
}

#[tokio::test]
async fn auth_ldap_search_mode() {
    if !idp_up("127.0.0.1:2389") { eprintln!("skipping: slapd not on 127.0.0.1:2389"); return; }
    ldap_flow("auth-ldap-srch", "search").await;
}

// ----------------------------------------------------------------- oidc ----

fn oidc_envs(port: u16) -> Vec<(&'static str, String)> {
    vec![
        ("BLAZONRY_AUTH", "oidc".into()),
        ("BLAZONRY_OIDC_ISSUER", "http://127.0.0.1:2390".into()),
        ("BLAZONRY_OIDC_CLIENT_ID", "blazonry".into()),
        ("BLAZONRY_OIDC_CLIENT_SECRET", "blazonry-secret".into()),
        ("BLAZONRY_OIDC_REDIRECT_URI", format!("http://127.0.0.1:{port}/auth/oidc/callback")),
        ("BLAZONRY_OIDC_ADMIN_EMAILS", "alice@blazonry.test".into()),
    ]
}

/// Drive the full browser flow with a cookie jar, exactly like a browser:
/// /auth/oidc/login -> provider interaction -> login form -> callback.
async fn oidc_browser_login(cli: &reqwest::Client, addr: &str, user: &str, pass: &str) -> Option<String> {
    // 1. start: blazonry redirects to the provider authorize endpoint
    let resp = req_no_redirect(cli, &format!("http://{addr}/auth/oidc/login"), &[]).await;
    assert!(resp.status().is_redirection(), "login start should redirect, got {}", resp.status());
    let authorize_url = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    // state must be present and PKCE advertised
    assert!(authorize_url.contains("code_challenge_method=S256") && authorize_url.contains("state="));

    // 2. provider redirects to the login interaction (sets its cookies)
    let resp = req_no_redirect(cli, &authorize_url, &[]).await;
    assert!(resp.status().is_redirection(), "authorize should redirect to interaction, got {}", resp.status());
    let interaction = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    let interaction_url = format!("http://127.0.0.1:2390{interaction}");
    let resp = req_no_redirect(cli, &interaction_url, &[]).await;
    assert_eq!(resp.status(), 200); // login form

    // 3. submit credentials
    let resp = cli.post(&format!("{interaction_url}/login"))
        .form(&[("uid", user), ("pw", pass)])
        .send().await.unwrap();
    assert!(resp.status().is_redirection(), "provider should redirect back with a code, got {}", resp.status());
    let mut back = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    // interactionFinished resumes via /auth/<uid>; following it lands on the
    // client redirect_uri with ?code=
    let mut hops = 0;
    while !back.contains("/auth/oidc/callback") && hops < 3
        && (back.contains("127.0.0.1:2390") || back.starts_with("/auth/"))
    {
        hops += 1;
        let url = if back.starts_with('/') { format!("http://127.0.0.1:2390{back}") } else { back.clone() };
        let resp = req_no_redirect(cli, &url, &[]).await;
        assert!(resp.status().is_redirection(), "resume should redirect to callback, got {}", resp.status());
        back = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    }
    assert!(back.starts_with("http://127.0.0.1:2391/auth/oidc/callback?code="), "callback url: {back}");

    // 4. blazonry callback: exchanges code, verifies id_token, issues session
    let resp = req_no_redirect(cli, &back, &[]).await;
    assert!(resp.status().is_redirection(), "callback should redirect to / with a token, got {}", resp.status());
    let loc = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    loc.strip_prefix("/#blazonry_token=").map(|t| t.to_string())
}

#[tokio::test]
async fn auth_oidc_full_browser_flow() {
    if !idp_up("127.0.0.1:2390") { eprintln!("skipping: oidc-provider not on 127.0.0.1:2390"); return; }
    let envs = oidc_envs(2391);
    let envs_ref: Vec<(&str, String)> = envs.iter().map(|(k, v)| (*k, v.clone())).collect();
    let (mut child, addr) = spawn_server_env("auth-oidc", 2391, &envs_ref).await;
    // cookie jar like a browser session
    let cli = reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap();

    // mode endpoint + password login refused
    let mode: serde_json::Value = cli.get(format!("http://{addr}/admin/auth/mode")).send().await.unwrap().json().await.unwrap();
    assert_eq!(mode["mode"], "oidc");
    let s = cli.post(format!("http://{addr}/admin/login")).json(&serde_json::json!({"email":"a@b.c","password":"x"})).send().await.unwrap().status();
    assert_eq!(s, 400);

    // admin email -> admin role
    let tok = oidc_browser_login(&cli, &addr, "alice", "alicepass").await.expect("oidc flow should complete");
    let me: serde_json::Value = cli.get(format!("http://{addr}/admin/me")).header("authorization", format!("Bearer {tok}")).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["email"], "alice@blazonry.test");
    assert_eq!(me["role"], "admin"); // via BLAZONRY_OIDC_ADMIN_EMAILS

    // non-admin email -> user role
    let cli2 = reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let tok2 = oidc_browser_login(&cli2, &addr, "bob", "bobpass").await.expect("second oidc flow");
    let me2: serde_json::Value = cli2.get(format!("http://{addr}/admin/me")).header("authorization", format!("Bearer {tok2}")).send().await.unwrap().json().await.unwrap();
    assert_eq!(me2["email"], "bob@blazonry.test");
    assert_eq!(me2["role"], "user");

    // replayed callback (same code/state) must be rejected
    let bad = req_no_redirect(&cli, &format!("http://{addr}/auth/oidc/callback?code=zzz&state=zzz"), &[]).await;
    assert_eq!(bad.status(), 401);

    // forged state on a fresh login attempt: start a flow, then callback with unknown state
    let resp = req_no_redirect(&cli, &format!("http://{addr}/auth/oidc/login"), &[]).await;
    assert_eq!(resp.status(), 302);
    let bad = req_no_redirect(&cli, &format!("http://{addr}/auth/oidc/callback?code=x&state=forged"), &[]).await;
    assert_eq!(bad.status(), 401);

    child.kill().await.unwrap();
}

#[tokio::test]
async fn auth_oidc_bad_credentials_rejected() {
    if !idp_up("127.0.0.1:2390") { eprintln!("skipping: oidc-provider not on 127.0.0.1:2390"); return; }
    let envs = oidc_envs(2301);
    let envs_ref: Vec<(&str, String)> = envs.iter().map(|(k, v)| (*k, v.clone())).collect();
    let (mut child, addr) = spawn_server_env("auth-oidc-bad", 2301, &envs_ref).await;
    let cli = reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap();

    // wrong password at the provider: form returns 401, no code issued
    let resp = req_no_redirect(&cli, &format!("http://{addr}/auth/oidc/login"), &[]).await;
    let authorize_url = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    let resp = req_no_redirect(&cli, &authorize_url, &[]).await;
    let interaction = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    let interaction_url = format!("http://127.0.0.1:2390{interaction}");
    let _ = req_no_redirect(&cli, &interaction_url, &[]).await;
    let resp = cli.post(&format!("{interaction_url}/login")).form(&[("uid", "alice"), ("pw", "WRONG")]).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    // and no session was created: /admin/me without token still 401
    let s = cli.get(format!("http://{addr}/admin/me")).send().await.unwrap().status();
    assert_eq!(s, 401);
    child.kill().await.unwrap();
}
