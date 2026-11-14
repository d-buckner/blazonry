// SPDX-License-Identifier: AGPL-3.0-only
//! Integration tests for the MCP server. These drive the real `blazonry_mcp`
//! binary over stdio against a stub of the admin API, so the framing, the
//! tool-to-route mapping, and the image block are seen as a client sees them.
//! The unit tests in `src/mcp.rs` cover the same mapping without a process or a
//! socket between the two halves.

use base64::Engine as _;
use serde_json::{Value, json};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

/// The admin token has to clear the 32-character floor the server enforces.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A stub of the admin API on an ephemeral port. It answers one request per
/// connection and closes, and records `METHOD path authorization body` for each.
async fn stub() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            // One request per connection, so a connection that never sends a
            // request must not stop the next one from being served.
            if tokio::time::timeout(Duration::from_secs(5), answer(sock, log))
                .await
                .is_err()
            {
                continue;
            }
        }
    });
    (format!("http://{addr}"), seen)
}

async fn answer(sock: TcpStream, log: Arc<Mutex<Vec<String>>>) {
    let (read, mut write) = sock.into_split();
    let mut read = BufReader::new(read);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line.trim().is_empty() {
            break;
        }
        head.push_str(&line);
    }
    let len = head
        .lines()
        .find_map(|l| header(l, "content-length"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut raw = vec![0u8; len];
    if read.read_exact(&mut raw).await.is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&raw).to_string();
    let line0 = head.lines().next().unwrap_or("").to_string();
    let method = line0.split_whitespace().next().unwrap_or("").to_string();
    let path = line0.split_whitespace().nth(1).unwrap_or("").to_string();
    let auth = head
        .lines()
        .find_map(|l| header(l, "authorization"))
        .unwrap_or_default();
    log.lock()
        .unwrap()
        .push(format!("{method} {path} {auth} {body}"));

    let (status, kind, out) = match (method.as_str(), path.as_str()) {
        ("GET", "/admin/kinds") => (
            200,
            "application/json",
            r#"{"extensions":[{"kind":"quote"}],"providers":[]}"#.to_string(),
        ),
        (_, "/admin/extensions/read") => (
            200,
            "application/json",
            r#"{"ok":true,"files":{"src/lib.rs":"fn main() {}"}}"#.to_string(),
        ),
        (_, "/admin/extensions/check") => {
            let png = base64::engine::general_purpose::STANDARD.encode([0x89, b'P', b'N', b'G']);
            (
                200,
                "application/json",
                format!(r#"{{"ok":true,"vars":{{"text":"hi"}},"png_base64":"{png}"}}"#),
            )
        }
        // A refused apply answers 409 with the reason as the whole body.
        (_, "/admin/extensions/apply") => (
            409,
            "text/plain",
            "apply refused: check is not green".to_string(),
        ),
        _ => (200, "application/json", r#"{"ok":true}"#.to_string()),
    };
    let reason = if status == 200 { "OK" } else { "Conflict" };
    let wire = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
        out.len()
    );
    let _ = write.write_all(wire.as_bytes()).await;
}

/// One header value, matched without case, because hyper sends them lowercase.
fn header(line: &str, name: &str) -> Option<String> {
    let (key, value) = line.split_once(':')?;
    key.trim()
        .eq_ignore_ascii_case(name)
        .then(|| value.trim().to_string())
}

/// Send the lines to a fresh `blazonry_mcp`, close stdin, and collect what it
/// answered, what it logged, and its exit code. Responses may arrive in any
/// order: the server answers each request on its own task, so a long build
/// cannot block a ping.
async fn drive(url: &str, token: &str, level: &str, lines: &[String]) -> (Vec<Value>, String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_blazonry_mcp"))
        .env("BLAZONRY_URL", url)
        .env("BLAZONRY_ADMIN_TOKEN", token)
        .env("RUST_LOG", level)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", lines.join("\n")).as_bytes())
        .await
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = async {
        let mut msgs = Vec::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            if !line.trim().is_empty() {
                msgs.push(
                    serde_json::from_str::<Value>(&line)
                        .unwrap_or_else(|e| panic!("stdout is not JSON-RPC: {line}: {e}")),
                );
            }
        }
        let mut err = Vec::new();
        BufReader::new(stderr).read_to_end(&mut err).await.unwrap();
        (msgs, String::from_utf8_lossy(&err).to_string())
    };
    let (msgs, err) = tokio::time::timeout(Duration::from_secs(60), read)
        .await
        .expect("blazonry_mcp never closed stdout");
    let code = child.wait().await.unwrap().code().unwrap_or(-1);
    (msgs, err, code)
}

fn req(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn by_id(msgs: &[Value], id: u64) -> Value {
    msgs.iter()
        .find(|m| m["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no answer with id {id} in {msgs:?}"))
}

#[tokio::test]
async fn initialize_echoes_the_newest_revision_and_names_the_server() {
    let (url, _seen) = stub().await;
    let (msgs, _, code) = drive(
        &url,
        TOKEN,
        "warn",
        &[req(
            1,
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
        )],
    )
    .await;
    assert_eq!(code, 0, "a closed stdin ends the session");
    assert_eq!(msgs.len(), 1);
    assert_eq!(by_id(&msgs, 1)["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(by_id(&msgs, 1)["result"]["serverInfo"]["name"], "blazonry");
}

#[tokio::test]
async fn a_notification_gets_no_answer_on_the_wire() {
    let (url, _seen) = stub().await;
    let (msgs, _, _) = drive(
        &url,
        TOKEN,
        "warn",
        &[
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            req(2, "ping", json!({})),
        ],
    )
    .await;
    assert_eq!(msgs.len(), 1, "only the ping is answered: {msgs:?}");
}

#[tokio::test]
async fn tools_list_carries_the_seven_authoring_tools() {
    let (url, _seen) = stub().await;
    let (msgs, _, _) = drive(&url, TOKEN, "warn", &[req(3, "tools/list", json!({}))]).await;
    let tools = by_id(&msgs, 3)["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), 7);
    for t in &tools {
        assert!(!t["title"].as_str().unwrap_or("").is_empty(), "{t}");
        assert!(t["description"].as_str().unwrap().len() > 40, "{t}");
    }
}

#[tokio::test]
async fn ext_kinds_is_a_get_and_carries_the_bearer_token() {
    let (url, seen) = stub().await;
    let (msgs, _, _) = drive(
        &url,
        TOKEN,
        "warn",
        &[req(4, "tools/call", json!({"name": "ext_kinds"}))],
    )
    .await;
    assert_eq!(by_id(&msgs, 4)["result"]["isError"], false);
    assert!(
        by_id(&msgs, 4)["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("quote")
    );
    let calls = seen.lock().unwrap().clone();
    assert_eq!(
        calls[0],
        format!("GET /admin/kinds Bearer {TOKEN} "),
        "kinds is the one GET route"
    );
}

#[tokio::test]
async fn ext_read_names_its_kind_on_the_read_route() {
    let (url, seen) = stub().await;
    let (msgs, _, _) = drive(
        &url,
        TOKEN,
        "warn",
        &[req(
            5,
            "tools/call",
            json!({"name": "ext_read", "arguments": {"kind": "quote"}}),
        )],
    )
    .await;
    assert_eq!(by_id(&msgs, 5)["result"]["isError"], false);
    let calls = seen.lock().unwrap().clone();
    assert_eq!(
        calls[0],
        format!("POST /admin/extensions/read Bearer {TOKEN} {{\"kind\":\"quote\"}}"),
        "the body carries the kind and nothing else"
    );
}

#[tokio::test]
async fn the_check_png_arrives_as_an_image_block() {
    let (url, _seen) = stub().await;
    let (msgs, _, _) = drive(
        &url,
        TOKEN,
        "warn",
        &[req(
            6,
            "tools/call",
            json!({"name": "ext_check", "arguments": {"kind": "quote"}}),
        )],
    )
    .await;
    let result = &by_id(&msgs, 6)["result"];
    assert_eq!(result["isError"], false);
    assert_eq!(result["content"][1]["type"], "image");
    assert_eq!(result["content"][1]["mimeType"], "image/png");
    assert!(
        !result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("iVBOR"),
        "the base64 must not be left in the text block"
    );
}

#[tokio::test]
async fn a_refused_apply_is_a_failed_tool_call_not_a_protocol_error() {
    let (url, _seen) = stub().await;
    let (msgs, _, _) = drive(
        &url,
        TOKEN,
        "warn",
        &[req(
            7,
            "tools/call",
            json!({"name": "ext_apply", "arguments": {"kind": "quote"}}),
        )],
    )
    .await;
    let result = &by_id(&msgs, 7)["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["content"][0]["text"],
        "apply refused: check is not green"
    );
}

#[tokio::test]
async fn an_unknown_method_and_an_unknown_tool_answer_differently() {
    let (url, _seen) = stub().await;
    let (msgs, _, _) = drive(
        &url,
        TOKEN,
        "warn",
        &[
            req(8, "resources/list", json!({})),
            req(
                9,
                "tools/call",
                json!({"name": "ext_exfiltrate", "arguments": {"kind": "quote"}}),
            ),
        ],
    )
    .await;
    assert_eq!(by_id(&msgs, 8)["error"]["code"], -32601);
    // A tool that does not exist is the model's mistake, not the protocol's.
    assert_eq!(by_id(&msgs, 9)["result"]["isError"], true);
}

#[tokio::test]
async fn a_short_token_stops_the_server_before_it_answers() {
    let (url, _seen) = stub().await;
    let (msgs, err, code) = drive(&url, "short", "error", &[req(1, "ping", json!({}))]).await;
    assert!(msgs.is_empty(), "nothing is answered: {msgs:?}");
    assert_ne!(code, 0);
    assert!(err.contains("32 characters"), "{err}");
}

#[tokio::test]
async fn logs_go_to_stderr_and_never_to_stdout() {
    let (url, _seen) = stub().await;
    let (msgs, err, _) = drive(&url, TOKEN, "info", &[req(1, "ping", json!({}))]).await;
    assert_eq!(msgs.len(), 1);
    assert!(err.contains("serving tools over stdio"), "{err}");
}
