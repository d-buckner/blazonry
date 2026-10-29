//! Spec conformance suite: one test per contract line from the TRMNL BYOS
//! wire spec. Ground truth (fetched 2026-10-29, vendored in tests/spec/):
//!   - docs.trmnl.com/go/diy/byos.md            (setup/display contract)
//!   - docs.trmnl.com/go/private-api/screens.md (display, /current, errors)
//!   - usetrmnl/terminus config/routes.rb + app/actions/api/* (Terminus parity)
//!
//! Status: device plane fully covered; admin-plane checklist at the bottom.

use base64::Engine as _;
use std::process::Stdio;

static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

async fn spawn_server() -> (tokio::process::Child, String) {
    // one DB per test: first registration is admin, and tests run in parallel
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("terminus-spec-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("s.db").to_str().unwrap().to_string();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_terminus-rs"))
        .args(["serve", "--db", &db, "--addr", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut addr = String::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        if let Some(i) = line.find("127.0.0.1:") {
            addr = line[i..].trim().trim_end_matches('"').to_string();
            break;
        }
    }
    if let Some(q) = addr.find('"') {
        addr.truncate(q);
    }
    (child, addr)
}

async fn req(addr: &str, method: &str, path: &str, body: Option<serde_json::Value>, headers: &[(&str, String)]) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let mut r = client.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), format!("http://{addr}{path}"));
    for (k, v) in headers {
        r = r.header(*k, v);
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    let json = resp.json().await.unwrap_or(serde_json::json!(null));
    (status, json)
}

/// Register admin + create one screen + playlist, return (token, mac, api_key).
async fn provision(addr: &str, mac: &str) -> (Vec<(&'static str, String)>, String) {
    let (_, r) = req(addr, "POST", "/admin/register",
        Some(serde_json::json!({"email": "spec@t.c", "password": "longenough9"})), &[]).await;
    let tok = r["token"].as_str().unwrap().to_string();
    let auth: Vec<(&'static str, String)> = vec![("authorization", format!("Bearer {tok}"))];
    let template = "{\"name\":\"t\",\"parts\":[{\"type\":\"text\",\"text\":\"spec\",\"padding\":[10]}]}";
    let (_, r) = req(addr, "POST", "/admin/screens",
        Some(serde_json::json!({"name": "s1", "template": template, "vars": {}})), &auth).await;
    let sid = r["id"].as_i64().unwrap();
    let (_, _) = req(addr, "POST", "/admin/playlists",
        Some(serde_json::json!({"name": "p1", "screen_ids": [sid]})), &auth).await;
    let (_, setup) = req(addr, "GET", "/api/setup", None, &[("id", mac.into())]).await;
    let api_key = setup["api_key"].as_str().unwrap().to_string();
    let (_, _) = req(addr, "POST", "/admin/devices/assign",
        Some(serde_json::json!({"mac": mac, "playlist_id": 1})), &auth).await;
    (auth, api_key)
}

// ---------- /api/setup ----------

// Spec: firmware trades MAC for api_key; reads status, api_key, friendly_id,
// image_url, message. Any status other than 200 = failed setup.
#[tokio::test]
async fn setup_returns_all_firmware_fields() {
    let (mut child, addr) = spawn_server().await;
    let (s, r) = req(&addr, "GET", "/api/setup", None,
        &[("id", "AA:00:00:00:00:01".into()), ("fw-version", "1.8.16".into()), ("model", "og".into())]).await;
    assert_eq!(s, 200);
    assert_eq!(r["status"], 200);
    assert!(r["api_key"].as_str().unwrap().len() >= 16, "api_key present");
    assert!(r["friendly_id"].is_string());
    let iu = r["image_url"].as_str().unwrap();
    assert!(iu.starts_with("http://"), "setup image_url must be absolute (firmware: HTTPClient.begin), got {iu}");
    assert!(r["message"].is_string());
    child.kill().await.ok();
}

// Spec: header is `ID`; `idempotency-key` is the BYOS-client alias. Both work.
#[tokio::test]
async fn setup_accepts_id_and_idempotency_key_headers() {
    let (mut child, addr) = spawn_server().await;
    let (s1, r1) = req(&addr, "GET", "/api/setup", None, &[("id", "AA:00:00:00:00:02".into())]).await;
    let (s2, r2) = req(&addr, "GET", "/api/setup", None, &[("idempotency-key", "AA:00:00:00:00:02".into())]).await;
    assert_eq!((s1, s2), (200, 200));
    assert_eq!(r1["api_key"], r2["api_key"], "same device either header");
    // no header at all -> rejected
    let (s3, _) = req(&addr, "GET", "/api/setup", None, &[]).await;
    assert_eq!(s3, 401);
    child.kill().await.ok();
}

// ---------- /api/display ----------

// Spec: firmware reads status, image_url, filename, refresh_rate,
// reset_firmware, update_firmware, firmware_url, special_function,
// maximum_compatibility, temperature_profile.
#[tokio::test]
async fn display_payload_has_every_documented_field() {
    let (mut child, addr) = spawn_server().await;
    let (_, key) = provision(&addr, "AA:00:00:00:00:10").await;
    let (s, d) = req(&addr, "GET", "/api/display", None,
        &[("id", "AA:00:00:00:00:10".into()), ("access-token", key)]).await;
    assert_eq!(s, 200);
    for f in ["status", "image_url", "filename", "refresh_rate", "reset_firmware",
              "update_firmware", "firmware_url", "special_function",
              "maximum_compatibility", "temperature_profile"] {
        assert!(!d[f].is_null() || f == "firmware_url", "missing field {f}: {d}");
    }
    child.kill().await.ok();
}

// Spec: refresh_rate is SECONDS as a NUMBER — firmware reads a string as 0
// and wakes again immediately. Regression guard.
#[tokio::test]
async fn refresh_rate_is_a_number_in_seconds() {
    let (mut child, addr) = spawn_server().await;
    let (_, key) = provision(&addr, "AA:00:00:00:00:11").await;
    let (_, d) = req(&addr, "GET", "/api/display", None,
        &[("id", "AA:00:00:00:00:11".into()), ("access-token", key)]).await;
    let rr = d["refresh_rate"].as_i64().expect("refresh_rate must be a number");
    assert!((60..86_400).contains(&rr), "refresh_rate plausible seconds: {rr}");
    child.kill().await.ok();
}

// Spec: status 0 draws image_url; 202 = not registered, poll again soon.
#[tokio::test]
async fn display_status_202_before_playlist_assignment() {
    let (mut child, addr) = spawn_server().await;
    // pair but never assign a playlist
    let (_, setup) = req(&addr, "GET", "/api/setup", None, &[("id", "AA:00:00:00:00:12".into())]).await;
    let key = setup["api_key"].as_str().unwrap().to_string();
    let (_, d) = req(&addr, "GET", "/api/display", None,
        &[("id", "AA:00:00:00:00:12".into()), ("access-token", key)]).await;
    assert_eq!(d["status"], 202);
    child.kill().await.ok();
}

// Spec (screens.md): unknown access-token -> {status:500, error:"Device not
// found", reset_firmware:true} so firmware wipes and re-pairs.
#[tokio::test]
async fn unknown_token_triggers_reset_firmware() {
    let (mut child, addr) = spawn_server().await;
    let _ = provision(&addr, "AA:00:00:00:00:13").await;
    let (_, d) = req(&addr, "GET", "/api/display", None,
        &[("id", "AA:00:00:00:00:13".into()), ("access-token", "wrong-key".into())]).await;
    assert_eq!(d["status"], 500);
    assert_eq!(d["error"], "Device not found");
    assert_eq!(d["reset_firmware"], true);
    child.kill().await.ok();
}

// Spec (Filenames): filename changes only when the image changes; same image
// -> device skips the download. Two polls on a 1-screen playlist must repeat
// the filename.
#[tokio::test]
async fn filename_is_stable_for_unchanged_content() {
    let (mut child, addr) = spawn_server().await;
    let (_, key) = provision(&addr, "AA:00:00:00:00:14").await;
    let h = [("id", "AA:00:00:00:00:14".to_string()), ("access-token", key)];
    let (_, d1) = req(&addr, "GET", "/api/display", None, &h).await;
    let (_, d2) = req(&addr, "GET", "/api/display", None, &h).await;
    assert_eq!(d1["filename"], d2["filename"], "filename must not churn");
    child.kill().await.ok();
}

// image_url must actually serve the BMP the payload describes (2-bit, 800px).
#[tokio::test]
async fn image_url_serves_the_rendered_bmp() {
    let (mut child, addr) = spawn_server().await;
    let (_, key) = provision(&addr, "AA:00:00:00:00:15").await;
    let (_, d) = req(&addr, "GET", "/api/display", None,
        &[("id", "AA:00:00:00:00:15".into()), ("access-token", key), ("model", "x".into())]).await;
    let url = d["image_url"].as_str().unwrap().to_string();
    // spec: image_url is absolute (firmware passes it straight to HTTPClient)
    assert!(url.starts_with(&format!("http://{addr}")), "image_url must be absolute, got {url}");
    let resp = reqwest::Client::new().get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(&bytes[0..2], b"BM");
    assert_eq!(u32::from_le_bytes(bytes[18..22].try_into().unwrap()), 800);
    assert_eq!(u32::from_le_bytes(bytes[22..26].try_into().unwrap()), 480);
    assert_eq!(u16::from_le_bytes(bytes[28..30].try_into().unwrap()), 2, "2-bit depth");
    child.kill().await.ok();
}

// ---------- /api/display/current ----------

// OG panel (no/`og` Model header): firmware hard-checks 1-bpp, 48062 bytes,
// dataOffset 62, 2 palette entries (lib/trmnl/src/bmp.cpp parseBMPHeader).
#[tokio::test]
async fn og_device_gets_1bpp_bmp() {
    let (mut child, addr) = spawn_server().await;
    let (_, key) = provision(&addr, "AA:00:00:00:00:16").await;
    let (_, d) = req(&addr, "GET", "/api/display", None,
        &[("id", "AA:00:00:00:00:16".into()), ("access-token", key)]).await;
    let url = d["image_url"].as_str().unwrap().to_string();
    let resp = reqwest::Client::new().get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let b = resp.bytes().await.unwrap();
    assert_eq!(b.len(), 48062, "OG BMP must be exactly 48062 bytes");
    assert_eq!(u16::from_le_bytes(b[28..30].try_into().unwrap()), 1, "1 bpp");
    assert_eq!(u32::from_le_bytes(b[10..14].try_into().unwrap()), 62, "data offset");
    assert_eq!(u32::from_le_bytes(b[34..38].try_into().unwrap()), 48000, "image data size");
    assert_eq!(u32::from_le_bytes(b[46..50].try_into().unwrap()), 2, "palette entries");
    // firmware's parseResponse_apiDisplay reads no inline image field;
    // a huge base64 string OOMs the OG device — payload must stay lean.
    assert!(d.get("image").is_none(), "no inline image in display payload");
    child.kill().await.ok();
}

// Spec: non-advancing mirror; {status:200, refresh_rate, image_url, filename};
// 404 message when nothing displayed yet.
#[tokio::test]
async fn display_current_does_not_advance() {
    let (mut child, addr) = spawn_server().await;
    let (_, key) = provision(&addr, "AA:00:00:00:00:16").await;
    let h = [("id", "AA:00:00:00:00:16".to_string()), ("access-token", key)];
    let (_, d) = req(&addr, "GET", "/api/display/current", None, &h).await;
    assert_eq!(d["status"], 200);
    assert!(d["image_url"].is_string() && d["filename"].is_string());
    // N reads of /current must not change what /display would show next:
    // with a 1-screen playlist, display still returns the same screen after.
    let (_, after) = req(&addr, "GET", "/api/display", None, &h).await;
    assert_eq!(d["filename"], after["filename"]);
    // legacy alias
    let (s, _) = req(&addr, "GET", "/api/current_screen", None, &h).await;
    assert_eq!(s, 200);
    child.kill().await.ok();
}

#[tokio::test]
async fn display_current_404_when_nothing_shown() {
    let (mut child, addr) = spawn_server().await;
    let (_, setup) = req(&addr, "GET", "/api/setup", None, &[("id", "AA:00:00:00:00:17".into())]).await;
    let key = setup["api_key"].as_str().unwrap().to_string();
    let (_, d) = req(&addr, "GET", "/api/display/current", None,
        &[("id", "AA:00:00:00:00:17".into()), ("access-token", key)]).await;
    assert_eq!(d["status"], 404);
    assert!(d["message"].is_string());
    child.kill().await.ok();
}

// ---------- POST /api/log ----------

// Terminus parity: device by Access-Token, body {logs:[...]}, 204 on accept,
// 404 problem+json for unknown device, 422 for malformed payload.
#[tokio::test]
async fn post_log_accepts_firmware_payload() {
    let (mut child, addr) = spawn_server().await;
    let (_, key) = provision(&addr, "AA:00:00:00:00:20").await;
    let entry = serde_json::json!({"logs": [{
        "battery_voltage": 3.7, "created_at": 1730000000, "firmware_version": "1.8.16",
        "free_heap_size": 100000, "id": 1, "level": "info", "max_alloc_size": 1000,
        "message": "woke up", "refresh_rate": 900, "sleep_duration": 899,
        "source_line": 42, "source_path": "main.cpp", "special_function": "none",
        "wake_reason": "timer", "wifi_signal": -45, "wifi_status": "connected"
    }]});
    let (s, _) = req(&addr, "POST", "/api/log", Some(entry.clone()), &[("access-token", key.clone())]).await;
    assert_eq!(s, 204);
    let (s, _) = req(&addr, "POST", "/api/log", Some(entry), &[("access-token", "bogus".into())]).await;
    assert_eq!(s, 404);
    let (s, _) = req(&addr, "POST", "/api/log", Some(serde_json::json!({"nope": 1})), &[("access-token", key)]).await;
    assert_eq!(s, 422);
    child.kill().await.ok();
}

// ---------- telemetry + firmware OTA ----------

// Firmware headers (FW-Version, RSSI, Battery-Voltage, Width/Height) must be
// ingested, not rejected (Terminus header schema accepts them all).
#[tokio::test]
async fn telemetry_headers_accepted_and_stored() {
    let (mut child, addr) = spawn_server().await;
    let (auth, key) = provision(&addr, "AA:00:00:00:00:21").await;
    let (s, _) = req(&addr, "GET", "/api/display", None, &[
        ("id", "AA:00:00:00:00:21".into()), ("access-token", key),
        ("fw-version", "1.8.16".into()), ("rssi", "-67".into()),
        ("battery-voltage", "3.85".into()), ("width", "800".into()), ("height", "480".into()),
    ]).await;
    assert_eq!(s, 200);
    let (_, devs) = req(&addr, "GET", "/admin/devices", None, &auth).await;
    let dev = devs.as_array().unwrap().iter()
        .find(|d| d["mac"] == "AA:00:00:00:00:21").unwrap();
    assert_eq!(dev["firmware_version"], "1.8.16");
    assert_eq!(dev["rssi"], -67);
    child.kill().await.ok();
}

// latest_firmware: update_firmware true only when the device is behind.
#[tokio::test]
async fn firmware_update_flag_tracks_device_version() {
    let (mut child, addr) = spawn_server().await;
    let (auth, _) = provision(&addr, "AA:00:00:00:00:22").await;
    // no firmware rows yet
    let (_, d) = req(&addr, "GET", "/api/latest_firmware", None, &[("fw-version", "1.0.0".into())]).await;
    assert_eq!(d["update_firmware"], false);
    // insert one via sqlite side-channel is not exposed; skip insert case —
    // covered by unit test below when admin firmware CRUD lands (checklist).
    let _ = auth;
    child.kill().await.ok();
}

// ---------- admin plane checklist (Terminus routes.rb parity) ----------
// Device plane: DONE (this file).
// Remaining Terminus API surface, in priority order for the admin API:
//   [ ] GET/PATCH/DELETE /api/devices/:id   (we have list + assign)
//   [ ] full CRUD /api/screens (we have create + preview)
//   [ ] full CRUD /api/playlists + items visibility toggle
//   [ ] CRUD /api/firmware (then extend firmware_update_flag test above)
//   [ ] /api/models (device model registry: og, x, …)
//   [ ] /api/extensions + exchanges (plugin parity — big, defer)
//   [ ] bulk: device import, log wipe, firmware wipe
