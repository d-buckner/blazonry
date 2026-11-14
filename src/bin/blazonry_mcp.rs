// SPDX-License-Identifier: AGPL-3.0-only
//! `blazonry-mcp` — the authoring API as an MCP server.
//!
//! A client spawns this over stdio. It needs two things: where blazonry is, and an
//! admin token. Both come from the environment, which is how a client config passes
//! them:
//!
//! ```text
//! BLAZONRY_URL=http://127.0.0.1:2301
//! BLAZONRY_ADMIN_TOKEN=<32+ characters, the same one the server was started with>
//! ```
//!
//! Or on the command line: `blazonry-mcp --url http://host:2301 --token ...`.
//!
//! Every tool is a POST to the admin API, so this binary holds no state and can be
//! restarted at any point in an authoring session.

use blazonry::mcp::{self, Cfg};
use serde_json::Value;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "usage: blazonry-mcp [--url URL] [--token TOKEN]\n\
             \n\
             Environment: BLAZONRY_URL, BLAZONRY_ADMIN_TOKEN.\n\
             Speaks newline-delimited JSON-RPC 2.0 on stdin and stdout."
        );
        return Ok(());
    }
    let cfg = Cfg::from_args(&args)?;
    // A build is cargo compiling a fresh crate, which is minutes on a Pi. The MCP
    // client's own timeout is the one that matters to a user, so this one is long
    // and the server's per-op limit is what actually bounds the work.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1200))
        .build()?;

    tracing::info!(url = %cfg.url, "blazonry-mcp: serving tools over stdio");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        // One writer task, so nothing contends and stdout needs no lock.
        let mut out = tokio::io::stdout();
        use tokio::io::AsyncWriteExt as _;
        while let Some(body) = rx.recv().await {
            let line = format!("{body}\n");
            if let Err(e) = out.write_all(line.as_bytes()).await {
                tracing::error!("blazonry-mcp: stdout closed: {e}");
                break;
            }
            let _ = out.flush().await;
        }
    });

    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::BufReader::new(stdin).lines();
    use tokio::io::AsyncBufReadExt as _;
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            // The client closed its side, which is how stdio sessions end.
            Ok(None) => break,
            Err(e) => {
                tracing::error!("blazonry-mcp: stdin error: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // One task per request, so a build that takes minutes does not stop the
        // client's ping from being answered.
        let cfg = cfg.clone();
        let client = client.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Some(body) = mcp::handle(&line, &cfg, &client).await {
                let _ = tx.send(body);
            }
        });
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}
