// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_host` — the extension host process (ADR-0002).
//!
//! The server never loads a plugin itself. This binary does, and it answers over
//! a unix socket, so a plugin that segfaults takes this process down while the
//! panels keep the last good image.
//!
//! ```text
//! ext_host serve --extensions /data/extensions --socket /data/run/exthost.sock
//! ext_host list  --extensions /data/extensions
//! ext_host build /data/extensions/clock
//! ext_host probe /data/extensions/clock
//! ```
//!
//! `serve` reads one JSON request per line and answers with one JSON response
//! per line:
//!
//! ```text
//! {"op":"ping"}    -> {"ok":true,"arch":"x86_64","loaded":1,"refused":0}
//! {"op":"kinds"}   -> {"kinds":[{"manifest":…,"template":"…"}],"refused":[…]}
//! {"op":"reload"}  -> the same body as kinds
//! {"op":"vars","req":{…}} -> {"vars":…} | {"error":"…"}
//! ```
//!
//! `build` needs a Rust toolchain, so it only works in the builder image.
//! `probe` runs one built extension against the request in its folder's
//! `request.json` and prints the vars. That is the check step of the authoring
//! loop, and it runs in a throwaway process: a bad extension dies here, not in
//! the server.

use blazonry::ext_host::{self, Loaded};
use blazonry::ext_link::HostRequest;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).cloned()
}

/// Everything that is not a `--flag` or the value that follows one.
fn positionals(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            skip = true;
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// What the host knows after a scan: what loaded, and what refused.
#[derive(Default)]
struct State {
    loaded: Vec<Arc<Loaded>>,
    refused: Vec<(String, String)>,
}

impl State {
    fn report(&self) -> Value {
        json!({
            "kinds": self.loaded.iter().map(|l| json!({
                "manifest": l.manifest, "template": l.template,
            })).collect::<Vec<_>>(),
            "loaded": self.loaded.len(),
            "refused": self.refused.iter().map(|(folder, why)| json!({
                "folder": folder, "reason": why,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The live set. A reload replaces the whole state, so a rebuild in flight keeps
/// the generation it started with.
type Live = Arc<RwLock<State>>;

fn rescan(dir: &Path) -> State {
    let scan = ext_host::scan(dir);
    for (folder, why) in &scan.refused {
        tracing::warn!("ext_host: refused {folder}: {why}");
    }
    State {
        loaded: scan.loaded,
        refused: scan.refused,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "serve" => serve(&args).await,
        "list" => {
            println!("{}", report(ext_host::scan(&extensions_dir(&args))));
            Ok(())
        }
        "build" => {
            let folder = folder_arg(&args)?;
            let so = ext_host::build(&folder, ext_api(&args).as_deref())?;
            println!(
                "{}",
                json!({"ok": true, "artifact": so.display().to_string()})
            );
            Ok(())
        }
        "probe" => probe(&folder_arg(&args)?),
        _ => {
            eprintln!(
                "usage: ext_host serve|list|build|probe\n\
                 see the source header for the request shapes"
            );
            anyhow::bail!("unknown command '{cmd}'");
        }
    }
}

fn report(scan: ext_host::Scan) -> Value {
    json!({
        "kinds": scan.loaded.iter().map(|l| json!({
            "manifest": l.manifest, "template": l.template,
        })).collect::<Vec<_>>(),
        "loaded": scan.loaded.len(),
        "refused": scan.refused.iter().map(|(folder, why)| json!({
            "folder": folder, "reason": why,
        })).collect::<Vec<_>>(),
    })
}

fn extensions_dir(args: &[String]) -> PathBuf {
    PathBuf::from(arg_after(args, "--extensions").unwrap_or_else(|| "/data/extensions".into()))
}

/// Where `blazonry-ext-api` lives. The builder image sets this; a folder that
/// depends on it cannot build without it.
fn ext_api(args: &[String]) -> Option<PathBuf> {
    arg_after(args, "--ext-api")
        .or_else(|| std::env::var("BLAZONRY_EXT_API").ok())
        .map(PathBuf::from)
}

fn folder_arg(args: &[String]) -> anyhow::Result<PathBuf> {
    positionals(args)
        .get(1)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .ok_or_else(|| anyhow::anyhow!("give the path of one extension folder"))
}

async fn serve(args: &[String]) -> anyhow::Result<()> {
    let dir = extensions_dir(args);
    let socket = PathBuf::from(
        arg_after(args, "--socket").unwrap_or_else(|| "/data/run/exthost.sock".into()),
    );
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket file left by a killed host would make bind fail forever.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
    }

    let live: Live = Arc::new(RwLock::new(rescan(&dir)));
    tracing::info!(
        "ext_host: listening on {}, {} loaded, {} refused",
        socket.display(),
        live.read().expect("live lock").loaded.len(),
        live.read().expect("live lock").refused.len()
    );

    let reload = live.clone();
    let reload_dir = dir.clone();
    tokio::spawn(async move {
        let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        else {
            return;
        };
        while sig.recv().await.is_some() {
            let state = rescan(&reload_dir);
            tracing::info!(
                "ext_host: reloaded, {} loaded, {} refused",
                state.loaded.len(),
                state.refused.len()
            );
            *reload.write().expect("live lock") = State {
                loaded: state.loaded,
                refused: state.refused,
            };
        }
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let live = live.clone();
        let dir = dir.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(stream, live, dir).await {
                tracing::warn!("ext_host: connection ended: {e}");
            }
        });
    }
}

async fn serve_conn(
    stream: tokio::net::UnixStream,
    live: Live,
    dir: PathBuf,
) -> anyhow::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<HostRequest>(&line) {
            Ok(req) => dispatch(req, &live, &dir).await,
            Err(e) => json!({"error": format!("unreadable request: {e}")}),
        };
        w.write_all(answer.to_string().as_bytes()).await?;
        w.write_all(b"\n").await?;
    }
    Ok(())
}

async fn dispatch(req: HostRequest, live: &Live, dir: &Path) -> Value {
    match req {
        HostRequest::Ping => json!({
            "ok": true,
            "arch": blazonry_ext_api::ARCH,
            "loaded": live.read().expect("live lock").loaded.len(),
        }),
        HostRequest::Kinds => live.read().expect("live lock").report(),
        HostRequest::Reload => {
            let state = rescan(dir);
            let out = state.report();
            *live.write().expect("live lock") = State {
                loaded: state.loaded,
                refused: state.refused,
            };
            out
        }
        HostRequest::Vars { req } => {
            let kind = req.kind.clone();
            let found = live
                .read()
                .expect("live lock")
                .loaded
                .iter()
                .find(|l| l.kind == kind)
                .cloned();
            let Some(loaded) = found else {
                return json!({"error": format!("no loaded extension named '{kind}'")});
            };
            // The call is a synchronous FFI call, so it goes off the runtime.
            match tokio::task::spawn_blocking(move || loaded.vars(&req)).await {
                Ok(Ok(vars)) => json!({"vars": vars}),
                Ok(Err(e)) => json!({"error": e}),
                Err(e) => json!({"error": format!("extension task failed: {e}")}),
            }
        }
    }
}

/// Run one built extension against its own fixture request.
fn probe(folder: &Path) -> anyhow::Result<()> {
    let loaded = ext_host::load(folder).map_err(|e| anyhow::anyhow!("{e}"))?;
    let fixture = folder.join("request.json");
    let req: blazonry::extensions::VarsRequest = if fixture.is_file() {
        serde_json::from_str(&std::fs::read_to_string(&fixture)?)?
    } else {
        blazonry::extensions::VarsRequest {
            kind: loaded.kind.clone(),
            settings: json!({}),
            local_now: chrono::Utc::now().to_rfc3339(),
            provider_ids: vec![],
            snapshots: vec![],
        }
    };
    match loaded.vars(&req) {
        Ok(vars) => {
            println!("{}", json!({"ok": true, "kind": loaded.kind, "vars": vars}));
            Ok(())
        }
        Err(e) => {
            println!("{}", json!({"ok": false, "kind": loaded.kind, "error": e}));
            anyhow::bail!(e)
        }
    }
}
