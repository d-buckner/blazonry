// SPDX-License-Identifier: AGPL-3.0-only
//! Integration tests for the streamable-HTTP MCP transport. These boot the real
//! server and drive `POST /admin/mcp` over the wire, so the auth layer, the
//! loopback call every tool makes back into the admin API, and the JSON framing
//! are seen the way an HTTP client sees them. `tests/mcp_stdio.rs` covers the
//! same dispatch over the other transport.

use serde_json::{Value, json};
use std::process::Stdio;

/// Clears the 32-character floor the server enforces at boot.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

async fn spawn(tag: &str, port: u16) -> tokio::process::Child {
    let dir =
        std::env::temp_dir().join(format!("blazonry-mcp-http-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.db").to_str().unwrap().to_string();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_blazonry"));
    cmd.args(["serve", "--db", &db, "--addr", &format!("127.0.0.1:{port}")])
        .env_remove("BLAZONRY_AUTH")
        .env("BLAZONRY_ADMIN_TOKEN", TOKEN)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    while let Some(line) = lines.next_line().await.unwrap() {
        if line.contains("listening") {
            break;
        }
    }
    child
}

fn req(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

async fn post(port: u16, body: Value, bearer: Option<&str>) -> reqwest::Response {
    let mut r = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/admin/mcp"))
        .body(body.to_string());
    if let Some(t) = bearer {
        r = r.bearer_auth(t);
    }
    r.send().await.unwrap()
}

async fn ask(port: u16, body: Value) -> Value {
    let res = post(port, body, Some(TOKEN)).await;
    assert_eq!(res.status(), 200, "a JSON-RPC answer is always a 200");
    res.json().await.unwrap()
}

#[tokio::test]
async fn initialize_echoes_the_version_the_client_asked_for() {
    let port = 2401;
    let _srv = spawn("init", port).await;
    let out = ask(
        port,
        req("initialize", json!({"protocolVersion": "2025-03-26"})),
    )
    .await;
    assert_eq!(out["id"], 1);
    assert_eq!(out["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(out["result"]["serverInfo"]["name"], "blazonry");
}

#[tokio::test]
async fn tools_list_carries_every_tool_with_a_title() {
    let port = 2402;
    let _srv = spawn("tools", port).await;
    let out = ask(port, req("tools/list", json!({}))).await;
    let tools = out["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 7, "the seven authoring tools");
    for t in tools {
        assert!(
            t["title"].as_str().is_some_and(|s| !s.is_empty()),
            "a tool reached the client with no title: {t}"
        );
    }
}

#[tokio::test]
async fn a_notification_answers_accepted_with_no_body() {
    let port = 2403;
    let _srv = spawn("notify", port).await;
    let res = post(
        port,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(res.status(), 202);
    assert!(res.text().await.unwrap().is_empty());
}

/// The one test that exercises the whole path: the endpoint calls the admin API
/// back over loopback with the caller's bearer, so a tool answer here proves the
/// self-call, the forwarded credential, and the framing all line up.
#[tokio::test]
async fn a_tool_call_runs_against_the_real_admin_api() {
    let port = 2404;
    let _srv = spawn("call", port).await;
    let out = ask(
        port,
        req("tools/call", json!({"name": "ext_kinds", "arguments": {}})),
    )
    .await;
    let res = &out["result"];
    assert_eq!(
        res["isError"], false,
        "ext_kinds needs no extension host: {res}"
    );
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("calendar"),
        "the built-in calendar kind should be listed: {text}"
    );
}

#[tokio::test]
async fn a_request_with_no_bearer_never_reaches_the_dispatch() {
    let port = 2405;
    let _srv = spawn("noauth", port).await;
    let res = post(port, req("tools/list", json!({})), None).await;
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn an_unknown_method_is_a_protocol_error_not_a_failed_tool() {
    let port = 2406;
    let _srv = spawn("unknown", port).await;
    let out = ask(port, req("tools/exfiltrate", json!({}))).await;
    assert_eq!(out["error"]["code"], -32601);
    assert!(out.get("result").is_none());
}

/// Streamable HTTP lets a server offer a server-to-client SSE stream. This one
/// has none, so 405 is the specification's answer rather than a gap.
#[tokio::test]
async fn get_is_refused_because_there_is_no_sse_stream() {
    let port = 2407;
    let _srv = spawn("get", port).await;
    let res = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/admin/mcp"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 405);
}
