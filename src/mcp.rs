// SPDX-License-Identifier: AGPL-3.0-only
//! The MCP side of the authoring API (ADR-0003).
//!
//! A model authors an extension by editing Rust, building it, and looking at what
//! it painted. That loop is already the admin API; this module maps it onto MCP
//! tools so a client does not need a shell on the device.
//!
//! The transport is newline-delimited JSON-RPC 2.0 over stdio, which is what a
//! client spawns a local server with. Only the four methods a tools-only server
//! needs are implemented: `initialize`, `tools/list`, `tools/call`, and `ping`.
//!
//! Shapes follow the 2025-11-25 specification:
//! <https://modelcontextprotocol.io/specification/2025-11-25/server/tools>
//!
//! This is a legacy-era server on purpose. The 2026-07-28 revision drops the
//! `initialize` handshake, makes `server/discover` required, and puts a
//! `resultType` on every result. A dual-era client probes `server/discover`,
//! gets `-32601`, and falls back to `initialize`, so it works here. A client
//! that speaks only 2026-07-28 does not.

use base64::Engine as _;
use serde_json::{Value, json};
use std::io::Write;

/// Protocol versions this server will answer with, newest first. A client that
/// asks for one of these gets it back; any other string gets the newest.
pub const PROTOCOLS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Where the admin API is, and how to prove who is asking.
#[derive(Clone)]
pub struct Cfg {
    pub url: String,
    pub token: String,
}

/// The token is redacted, because a `{:?}` in a log line or a test failure would
/// print a credential.
impl std::fmt::Debug for Cfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cfg")
            .field("url", &self.url)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl Cfg {
    /// `--url` and `--token` win over `BLAZONRY_URL` and `BLAZONRY_ADMIN_TOKEN`,
    /// because a client config file is easier to read than a command line.
    pub fn from_args(args: &[String]) -> anyhow::Result<Cfg> {
        Self::from_args_with(args, |k| std::env::var(k).ok())
    }

    /// The same, with the environment behind a lookup, so precedence is testable
    /// without mutating process state that other threads read.
    pub fn from_args_with(
        args: &[String],
        env: impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Cfg> {
        let pick = |flag: &str, var: &str, default: &str| -> String {
            args.windows(2)
                .find(|w| w[0] == flag)
                .map(|w| w[1].clone())
                .or_else(|| env(var))
                .unwrap_or_else(|| default.to_string())
        };
        let url = pick("--url", "BLAZONRY_URL", "http://127.0.0.1:2301")
            .trim_end_matches('/')
            .to_string();
        let token = pick("--token", "BLAZONRY_ADMIN_TOKEN", "");
        if token.chars().count() < 32 {
            anyhow::bail!(
                "an admin token of at least 32 characters is required: pass --token or set \
                 BLAZONRY_ADMIN_TOKEN. The server refuses a shorter one at boot."
            );
        }
        Ok(Cfg { url, token })
    }
}

/// One admin call. Returns the parsed answer, or the server's own error text.
///
/// The admin API answers a refused apply with 409 and the reason as the whole
/// body, so a non-2xx is parsed as JSON first and only falls back to text.
/// `body` is sent as JSON when it is an object, and not sent at all when it is
/// `Value::Null`, which is how a GET is expressed.
pub async fn api(
    client: &reqwest::Client,
    cfg: &Cfg,
    path: &str,
    body: Value,
) -> anyhow::Result<Value> {
    let url = format!("{}{}", cfg.url, path);
    let mut req = client.post(&url).bearer_auth(&cfg.token);
    if body.is_null() {
        req = client.get(&url).bearer_auth(&cfg.token);
    } else {
        req = req.json(&body);
    }
    let res = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("cannot reach the blazonry server at {}: {e}", cfg.url))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("{} answered unreadably: {e}", status))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if status.is_success() => Ok(v),
        Ok(v) => Ok(json!({"ok": false, "status": status.as_u16(),
                           "error": v.as_str().map(str::to_string).unwrap_or(text)})),
        Err(_) if status.is_success() => Ok(json!({"ok": true, "text": text})),
        Err(_) => Ok(json!({"ok": false, "status": status.as_u16(), "error": text})),
    }
}

/// The `tools/list` payload. The descriptions are the documentation a model reads
/// before it touches anything, so they carry the workflow, not just the shape.
pub fn tools() -> Value {
    let kind_only = json!({
        "type": "object",
        "properties": {"kind": {"type": "string", "description": "The extension kind, which is also its folder name."}},
        "required": ["kind"],
        "additionalProperties": false,
    });
    json!({"tools": [
        {"name": "ext_kinds", "title": "List screen kinds",
         "description": "List every screen kind: the ones built into blazonry and the ones loaded from the data volume. Call this first to see what already exists.",
         "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}},
        {"name": "ext_read", "title": "Read an extension folder",
         "description": "Read every text file in one extension folder: Cargo.toml, src/lib.rs, manifest.json, template.liquid, and the check fixture. Call this before editing an extension you did not write.",
         "inputSchema": kind_only.clone()},
        {"name": "ext_write", "title": "Write an extension folder",
         "description": "Write files into one extension folder. Paths are inside the folder, so 'src/lib.rs'. This does not build or apply anything; it only puts text on disk. A new kind needs Cargo.toml, src/lib.rs, manifest.json, and template.liquid, and the folder name must equal the crate name.",
         "inputSchema": {
            "type": "object",
            "properties": {
                "kind": {"type": "string", "description": "The extension kind, which is also its folder name."},
                "files": {"type": "object", "description": "Path inside the folder to its full text content.",
                          "additionalProperties": {"type": "string"}},
            },
            "required": ["kind", "files"],
            "additionalProperties": false,
        }},
        {"name": "ext_build", "title": "Build an extension",
         "description": "Compile one extension into its staging directory. Nothing live changes. On a failure the answer carries the compiler's own output, which is the thing to read. A cold build of a new kind can take a minute.",
         "inputSchema": kind_only.clone()},
        {"name": "ext_check", "title": "Check an extension and paint it",
         "description": "Run the staged build against its fixture and paint the result. The answer carries the vars as text and the painted screen as an image, so look at the image before you apply: it is what the panel will show. Nothing live changes.",
         "inputSchema": kind_only.clone()},
        {"name": "ext_apply", "title": "Apply an extension",
         "description": "Promote the staged build so screens start using it, and repaint the screens bound to this kind. Refused unless the newest build and check are both green and still match the staged bytes, so an edit after a check means building again. The previous generation stays available to ext_rollback.",
         "inputSchema": kind_only.clone()},
        {"name": "ext_rollback", "title": "Roll back an extension",
         "description": "Put back the generation the last apply displaced, and repaint. Use this when an applied extension paints something wrong and you cannot fix it quickly.",
         "inputSchema": kind_only},
    ]})
}

/// What the server says about itself, and how to use it.
pub fn server_info() -> Value {
    json!({
        "name": "blazonry",
        "title": "blazonry extensions",
        "version": env!("CARGO_PKG_VERSION"),
        "instructions": concat!(
            "You author screen extensions for blazonry, a server that paints e-paper ",
            "panels. An extension is Rust: a cdylib that links blazonry-ext-api, plus ",
            "manifest.json and template.liquid. The loop is ext_write, ext_build, ",
            "ext_check, then ext_apply. ext_check returns a PNG of the painted screen; ",
            "look at it before you apply, because a layout that reads fine as text can ",
            "overflow the panel. apply refuses until build and check are both green and ",
            "nothing changed since. Rollback exists for a bad apply. Text metrics come ",
            "from host.wrap and host.text_width inside vars, and the renderer never ",
            "wraps, so a long line must be split by the extension.",
        ),
    })
}

/// Turn one tool's answer into a `tools/call` result.
///
/// The check answer carries a base64 PNG. As text that is a wall of characters a
/// model cannot read; as an image block it is the thing the author needs.
fn tool_result(mut answer: Value) -> Value {
    let mut content = Vec::new();
    let png = answer
        .as_object_mut()
        .and_then(|m| m.remove("png_base64"))
        .and_then(|v| v.as_str().map(str::to_string));
    if let Some(b64) = png {
        // The client decodes this, so a corrupt string is worth a real error.
        if base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .is_ok()
        {
            content.push(json!({"type": "image", "data": b64, "mimeType": "image/png"}));
        } else {
            content.push(json!({"type": "text", "text": "the painted PNG did not decode"}));
        }
    }
    let failed = answer.get("ok").and_then(Value::as_bool) == Some(false);
    let text = match answer.get("error").and_then(Value::as_str) {
        Some(e) if !e.is_empty() => e.to_string(),
        Some(_) => match answer.get("status").and_then(Value::as_u64) {
            Some(code) => format!("the server answered {code} with no reason"),
            None => "the server refused with no reason".to_string(),
        },
        None => answer.to_string(),
    };
    content.insert(0, json!({"type": "text", "text": text}));
    json!({"content": content, "isError": failed})
}

/// One line of JSON-RPC in, one line out. `None` is a notification, which gets no
/// answer.
/// One line of JSON-RPC in, one line out. `None` is a notification, which gets no
/// answer. A line with no usable id still gets an answer with `"id": null`, which
/// is what JSON-RPC asks for when the request could not be read.
pub async fn handle(line: &str, cfg: &Cfg, client: &reqwest::Client) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(err(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    // A notification carries no id and must not be answered.
    let id = msg.get("id").cloned().filter(|v| !v.is_null());
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    if method.starts_with("notifications/") {
        return None;
    }
    let Some(id) = id else {
        return Some(err(
            Value::Null,
            -32600,
            "a request that is not a notification needs an id",
        ));
    };
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(err(id, -32600, "jsonrpc must be \"2.0\""));
    }
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let out: Result<Value, Value> = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|a| PROTOCOLS.contains(a))
                .unwrap_or(PROTOCOLS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": server_info(),
                "instructions": server_info()["instructions"],
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools()),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(err(id, -32602, "tools/call needs a name"));
            };
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(tool_result(call_tool(name, &args, cfg, client).await))
        }
        other => Err(err(
            id.clone(),
            -32601,
            &format!("unknown method '{other}'"),
        )),
    };
    // The error arm above already built the whole response.
    match out {
        Ok(res) => Some(json!({"jsonrpc": "2.0", "id": id, "result": res})),
        Err(done) => Some(done),
    }
}

fn err(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Run one tool against the admin API.
async fn call_tool(name: &str, args: &Value, cfg: &Cfg, client: &reqwest::Client) -> Value {
    let kind = || match args.get("kind").and_then(Value::as_str) {
        Some(k) if !k.is_empty() => Ok(k.to_string()),
        _ => Err(json!({"ok": false, "error": "this tool needs a kind"})),
    };
    let body = match name {
        // /admin/kinds is a GET; every other route is a POST under the kind.
        "ext_kinds" => {
            return api(client, cfg, "/admin/kinds", Value::Null)
                .await
                .unwrap_or_else(e2v);
        }
        "ext_read" => match kind() {
            Ok(k) => json!({"kind": k}),
            Err(e) => return e,
        },
        "ext_write" => {
            let k = match kind() {
                Ok(k) => k,
                Err(e) => return e,
            };
            match args.get("files") {
                Some(v) if v.is_object() && v.as_object().is_some_and(|m| !m.is_empty()) => {
                    json!({"kind": k, "files": v})
                }
                _ => {
                    return json!({"ok": false, "error": "ext_write needs files: a path-to-text object"});
                }
            }
        }
        "ext_build" | "ext_check" | "ext_apply" | "ext_rollback" => match kind() {
            Ok(k) => json!({"kind": k}),
            Err(e) => return e,
        },
        other => {
            return json!({"ok": false, "error": format!("no tool named '{other}'")});
        }
    };
    let path = format!(
        "/admin/extensions/{}",
        name.strip_prefix("ext_").unwrap_or(name)
    );
    api(client, cfg, &path, body).await.unwrap_or_else(e2v)
}

fn e2v(e: anyhow::Error) -> Value {
    json!({"ok": false, "error": e.to_string()})
}

/// Write one message the way the stdio transport wants it: one line, flushed.
pub fn write_line(out: &mut impl Write, body: &Value) -> std::io::Result<()> {
    writeln!(out, "{body}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Cfg {
        Cfg {
            url: "http://127.0.0.1:1".to_string(),
            token: "0123456789abcdef0123456789abcdef".to_string(),
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    async fn ask(line: &str) -> Option<Value> {
        handle(line, &cfg(), &client()).await
    }

    fn req(method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string()
    }

    #[tokio::test]
    async fn initialize_echoes_a_version_it_supports() {
        let out = ask(&req("initialize", json!({"protocolVersion": "2024-11-05"})))
            .await
            .unwrap();
        assert_eq!(out["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(out["result"]["serverInfo"]["name"], "blazonry");
        assert!(out["result"]["instructions"].as_str().unwrap().len() > 80);
    }

    /// A client asking for a version nobody heard of gets the newest one, not an
    /// error, because the spec says the server answers with what it supports.
    #[tokio::test]
    async fn an_unknown_protocol_falls_back_to_the_newest() {
        let out = ask(&req("initialize", json!({"protocolVersion": "1999-01-01"})))
            .await
            .unwrap();
        assert_eq!(out["result"]["protocolVersion"], PROTOCOLS[0]);
    }

    #[tokio::test]
    async fn a_notification_gets_no_answer() {
        assert!(
            ask(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_unknown_method_is_a_jsonrpc_error() {
        let out = ask(&req("resources/list", json!({}))).await.unwrap();
        assert_eq!(out["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn a_line_that_is_not_json_is_a_parse_error() {
        let out = ask("not json at all").await.unwrap();
        assert_eq!(out["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn every_tool_declares_its_arguments() {
        let out = ask(&req("tools/list", json!({}))).await.unwrap();
        let tools = out["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 7);
        for t in tools {
            assert!(t["inputSchema"]["type"] == "object", "{t}");
            assert!(!t["description"].as_str().unwrap().is_empty());
            assert!(!t["title"].as_str().unwrap_or("").is_empty(), "{t}");
        }
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ext_check"));
        assert!(names.contains(&"ext_rollback"));
    }

    #[tokio::test]
    async fn a_tool_without_its_kind_fails_before_the_network() {
        let out = ask(&req(
            "tools/call",
            json!({"name": "ext_build", "arguments": {}}),
        ))
        .await
        .unwrap();
        assert_eq!(out["result"]["isError"], true);
        assert!(
            out["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("needs a kind")
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_names_itself() {
        let out = ask(&req(
            "tools/call",
            json!({"name": "ext_exfiltrate", "arguments": {"kind": "x"}}),
        ))
        .await
        .unwrap();
        assert!(
            out["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("no tool named")
        );
    }

    /// The check answer carries a base64 PNG. Left as text it is a wall of
    /// characters; the point of the mapping is that a model sees a picture.
    #[test]
    fn the_check_png_becomes_an_image_block() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0x89, b'P', b'N', b'G']);
        let out = tool_result(json!({"ok": true, "vars": {"a": 1}, "png_base64": b64}));
        assert_eq!(out["isError"], false);
        assert_eq!(out["content"][1]["type"], "image");
        assert_eq!(out["content"][1]["mimeType"], "image/png");
        assert_eq!(out["content"][1]["data"], b64);
        // The text block must not still carry the base64.
        assert!(
            !out["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("png_base64")
        );
    }

    #[test]
    fn a_refusal_is_a_failed_tool_call() {
        let out = tool_result(json!({"ok": false, "error": "apply refused: nothing is staged"}));
        assert_eq!(out["isError"], true);
        assert_eq!(
            out["content"][0]["text"],
            "apply refused: nothing is staged"
        );
    }

    /// A short token is a weak credential and the server refuses it at boot, so the
    /// MCP refuses it at startup rather than failing every call with 401.
    #[test]
    fn a_short_token_is_refused_at_startup() {
        let err = Cfg::from_args(&["--token".to_string(), "short".to_string()]).unwrap_err();
        assert!(err.to_string().contains("32 characters"), "{err}");
    }

    #[test]
    fn args_beat_the_environment_and_the_default() {
        let tok = "0123456789abcdef0123456789abcdef";
        let env = |k: &str| match k {
            "BLAZONRY_URL" => Some("http://from-env:1".to_string()),
            "BLAZONRY_ADMIN_TOKEN" => Some(tok.to_string()),
            _ => None,
        };
        let cfg = Cfg::from_args_with(&[], env).unwrap();
        assert_eq!(cfg.url, "http://from-env:1");
        let cfg = Cfg::from_args_with(
            &[
                "--url".to_string(),
                "http://from-arg:2/".to_string(),
                "--token".to_string(),
                tok.to_string(),
            ],
            env,
        )
        .unwrap();
        assert_eq!(cfg.url, "http://from-arg:2", "a trailing slash is dropped");
        let cfg = Cfg::from_args_with(&["--token".to_string(), tok.to_string()], |_| None).unwrap();
        assert_eq!(cfg.url, "http://127.0.0.1:2301");
        // A debug print of the config must not carry the credential.
        assert!(!format!("{cfg:?}").contains("0123456789"));
    }
}
