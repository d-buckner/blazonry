// SPDX-License-Identifier: AGPL-3.0-only
//! Playlist ordering regressions (#22): concurrent appends must never
//! collide on a duplicate position, and the replace endpoint must
//! reassign positions 0..n in order.

use serde_json::json;
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
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut addr = String::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        if let Some(i) = line.find("127.0.0.1:") {
            addr = line[i..].trim().trim_end_matches('"').to_string();
            break;
        }
    }
    if let Some(q) = addr.find('"') { addr.truncate(q); }
    (child, addr)
}

async fn req(addr: &str, method: &str, path: &str, body: Option<serde_json::Value>, token: &str) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let mut r = client.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap(), format!("http://{addr}{path}"))
        .header("authorization", format!("Bearer {token}"));
    if let Some(b) = body { r = r.json(&b); }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    let json = resp.json().await.unwrap_or(serde_json::json!(null));
    (status, json)
}

#[tokio::test]
async fn concurrent_appends_get_unique_positions() {
    let (mut child, addr) = spawn_server("plorder").await;
    let (_, reg) = req(&addr, "POST", "/admin/register",
        Some(json!({ "email": "a@t.test", "password": "passw0rd!x" })), "").await;
    let token = reg["token"].as_str().unwrap().to_string();

    for i in 0..3 {
        let (s, _) = req(&addr, "POST", "/admin/screens",
            Some(json!({ "name": format!("s{i}"), "template": "{\"name\":\"t\",\"parts\":[]}", "vars": "{}" })), &token).await;
        assert_eq!(s, 200);
    }
    let (_, pl) = req(&addr, "POST", "/admin/playlists",
        Some(json!({ "name": "race", "screen_ids": [] })), &token).await;
    let pid = pl["id"].as_i64().unwrap();

    // fire all three appends concurrently — the old read-then-insert raced
    let handles: Vec<_> = (1..=3).map(|sid| {
        let a = addr.clone();
        let t = token.clone();
        tokio::spawn(async move {
            req(&a, "POST", &format!("/admin/playlists/{pid}/items"),
                Some(json!({ "screen_id": sid })), &t).await
        })
    }).collect();
    for h in handles { assert_eq!(h.await.unwrap().0, 200); }

    let (_, list) = req(&addr, "GET", "/admin/playlists", None, &token).await;
    let items = list[0]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "all appends landed");
    let mut pos: Vec<i64> = items.iter().map(|i| i["position"].as_i64().unwrap()).collect();
    pos.sort();
    assert_eq!(pos, vec![0, 1, 2], "positions must be unique and sequential, got {pos:?}");

    // replace reassigns in order (the UI's move up/down path)
    let ids: Vec<i64> = items.iter().map(|i| i["screen_id"].as_i64().unwrap()).collect();
    let mut rotated = ids.clone(); rotated.rotate_left(1);
    let (s, _) = req(&addr, "PATCH", &format!("/admin/playlists/{pid}"),
        Some(json!({ "screen_ids": rotated })), &token).await;
    assert_eq!(s, 200);
    let (_, list) = req(&addr, "GET", "/admin/playlists", None, &token).await;
    let items = list[0]["items"].as_array().unwrap();
    let mut pairs: Vec<(i64, i64)> = items.iter()
        .map(|i| (i["position"].as_i64().unwrap(), i["screen_id"].as_i64().unwrap()))
        .collect();
    pairs.sort();
    let order: Vec<i64> = pairs.into_iter().map(|(_, sid)| sid).collect();
    assert_eq!(order, rotated);

    child.kill().await.ok();
}
