//! Integration tests: spin the server on an ephemeral port, drive it over
//! HTTP exactly like a device + admin client would.

use std::process::Stdio;

async fn spawn_server(tag: &str) -> (tokio::process::Child, String) {
    let dir = std::env::temp_dir().join(format!("mntr-test-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.db").to_str().unwrap().to_string();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mntr"))
        .args(["serve", "--db", &db, "--addr", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // server logs the bound addr; read until we see it
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut addr = String::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        if let Some(i) = line.find("127.0.0.1:") {
            addr = line[i..].trim().trim_end_matches('"').to_string();
            break;
        }
    }
    // strip trailing quote/brace if tracing printed json
    if let Some(q) = addr.find('"') { addr.truncate(q); }
    (child, addr)
}

async fn req(addr: &str, method: &str, path: &str, body: Option<serde_json::Value>, headers: &[(&str, String)]) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let mut r = client.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), format!("http://{addr}{path}"));
    for (k, v) in headers { r = r.header(*k, v); }
    if let Some(b) = body { r = r.json(&b); }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    let json = resp.json().await.unwrap_or(serde_json::json!(null));
    (status, json)
}

#[tokio::test]
async fn full_device_and_admin_flow() {
    let (mut child, addr) = spawn_server("flow").await;
    let cli = reqwest::Client::new();

    // admin gate closed
    let (s, _) = req(&addr, "GET", "/admin/devices", None, &[]).await;
    assert_eq!(s, 401);

    // first user is admin
    let (s, r) = req(&addr, "POST", "/admin/register",
        Some(serde_json::json!({"email":"a@b.c","password":"longenough9"})), &[]).await;
    assert_eq!(s, 200);
    assert_eq!(r["role"], "admin");
    let tok = r["token"].as_str().unwrap().to_string();
    let auth = vec![("authorization", format!("Bearer {tok}"))];

    // screen + playlist via admin
    let template = "{\"name\":\"t\",\"parts\":[{\"type\":\"text\",\"text\":\"{{ h }}\",\"padding\":[10]}]}";
    let (s, r) = req(&addr, "POST", "/admin/screens",
        Some(serde_json::json!({"name":"t","template":template,"vars":{"h":"hello"}})), &auth).await;
    assert_eq!(s, 200);
    let screen_id = r["id"].as_i64().unwrap();
    let (s, _) = req(&addr, "POST", "/admin/playlists",
        Some(serde_json::json!({"name":"p","screen_ids":[screen_id]})), &auth).await;
    assert_eq!(s, 200);

    // device: pair, assign, poll
    let mac = "DE:AD:BE:EF:00:01";
    let (s, setup) = req(&addr, "GET", "/api/setup", None,
        &[("idempotency-key", mac.into())]).await;
    assert_eq!(s, 200);
    let dev_tok = setup["api_key"].as_str().unwrap().to_string();
    let (s, _) = req(&addr, "POST", "/admin/devices/assign",
        Some(serde_json::json!({"mac":mac,"playlist_id":1})), &auth).await;
    assert_eq!(s, 200);

    let (s, d) = req(&addr, "GET", "/api/display", None,
        &[("idempotency-key", mac.into()), ("access-token", dev_tok.clone())]).await;
    assert_eq!(s, 200);
    // payload is lean (no inline image); the BMP is fetched from image_url
    let url = d["image_url"].as_str().unwrap().to_string();
    let bmp = cli.get(&url).send().await.unwrap().bytes().await.unwrap().to_vec();
    assert_eq!(&bmp[0..2], b"BM");
    assert_eq!(u32::from_le_bytes(bmp[18..22].try_into().unwrap()), 800);
    assert_eq!(d["next_refresh_in"].as_i64().unwrap(), 900);

    // wrong device token: hosted-TRMNL contract = status 500 + reset_firmware
    let (s, d) = req(&addr, "GET", "/api/display", None,
        &[("idempotency-key", mac.into()), ("access-token", "nope".into())]).await;
    assert_eq!(d["status"], 500);
    assert_eq!(d["reset_firmware"], true);
    let _ = s;

    // preview endpoint returns PNG
    let resp = cli.get(format!("http://{addr}/admin/screens/{screen_id}/preview"))
        .header("authorization", format!("Bearer {tok}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "image/png");

    child.kill().await.ok();
}

#[tokio::test]
async fn pwa_assets_serve_with_correct_types() {
    let (mut child, addr) = spawn_server("pwa").await;
    let cli = reqwest::Client::new();

    let cases: [(&str, &str); 5] = [
        ("/sw.js", "text/javascript"),
        ("/manifest.webmanifest", "application/manifest+json"),
        ("/icons/icon.svg", "image/svg+xml"),
        ("/icons/icon-192.png", "image/png"),
        ("/icons/icon-512.png", "image/png"),
    ];
    for (path, want_ct) in cases {
        let resp = cli.get(format!("http://{addr}{path}")).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{path}");
        let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
        assert!(ct.starts_with(want_ct), "{path}: {ct} !~ {want_ct}");
    }

    // sw itself must never be browser-cached (its scope must always be fresh)
    let resp = cli.get(format!("http://{addr}/sw.js")).send().await.unwrap();
    let cc = resp.headers()["cache-control"].to_str().unwrap().to_string();
    assert!(cc.contains("no-store"), "sw.js cache-control: {cc}");

    // the page must reference the manifest and register the SW
    let html = cli.get(format!("http://{addr}/")).send().await.unwrap().text().await.unwrap();
    assert!(html.contains("/manifest.webmanifest"));
    assert!(html.contains("/sw.js"));

    child.kill().await.ok();
}

use base64::Engine as _;
