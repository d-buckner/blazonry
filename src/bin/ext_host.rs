// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_host` — the extension host process (ADR-0002).
//!
//! The server never loads a plugin itself. This binary does, and it answers over
//! a unix socket, so a plugin that segfaults takes this process down while the
//! panels keep the last good image.
//!
//! ```text
//! ext_host serve --extensions /data/extensions --socket /data/run/exthost.sock
//! ext_host list   --extensions /data/extensions
//! ext_host build  /data/extensions/clock
//! ext_host check  /data/extensions/clock
//! ext_host apply  /data/extensions/clock
//! ext_host rollback /data/extensions/clock
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
//! A build lands in the folder's staging directory and changes nothing that is
//! live. `check` runs the staged artifact in a throwaway process, so a bad
//! extension dies there and the host keeps serving what it already loaded.
//! `apply` promotes it, and `rollback` puts back what apply displaced.
//!
//! `build` needs a Rust toolchain, so it only works in the builder image.

use blazonry::ext_host::{self, Loaded};
use blazonry::ext_link::{self, HostRequest};
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
                json!({"ok": true, "staged": so.display().to_string()})
            );
            Ok(())
        }
        "check" => match ext_host::check(&folder_arg(&args)?) {
            Ok(v) => {
                println!("{v}");
                Ok(())
            }
            Err(e) => {
                println!("{}", json!({"ok": false, "error": e.to_string()}));
                Err(e)
            }
        },
        "apply" => {
            let out = ext_host::apply(&folder_arg(&args)?)?;
            println!("{out}");
            Ok(())
        }
        "rollback" => {
            let out = ext_host::rollback(&folder_arg(&args)?)?;
            println!("{out}");
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: ext_host serve|list|build|check|apply|rollback\n\
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

    let api = ext_api(args);
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
        let api = api.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(stream, live, dir, api).await {
                tracing::warn!("ext_host: connection ended: {e}");
            }
        });
    }
}

async fn serve_conn(
    stream: tokio::net::UnixStream,
    live: Live,
    dir: PathBuf,
    api: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let answer = match ext_link::read_request(&line) {
            Ok(req) => dispatch(req, &live, &dir, api.as_deref()).await,
            Err(e) => json!({"error": e}),
        };
        w.write_all(answer.to_string().as_bytes()).await?;
        w.write_all(b"\n").await?;
    }
    Ok(())
}

async fn dispatch(req: HostRequest, live: &Live, dir: &Path, api: Option<&Path>) -> Value {
    match req {
        HostRequest::Ping => json!({
            "ok": true,
            "protocol": blazonry::ext_link::PROTOCOL_VERSION,
            "arch": blazonry_ext_api::ARCH,
            "loaded": live.read().expect("live lock").loaded.len(),
        }),
        HostRequest::Kinds => {
            let mut report = live.read().expect("live lock").report();
            report["protocol"] = json!(ext_link::PROTOCOL_VERSION);
            report
        }
        HostRequest::Reload => {
            let state = rescan(dir);
            let out = state.report();
            *live.write().expect("live lock") = State {
                loaded: state.loaded,
                refused: state.refused,
            };
            out
        }
        HostRequest::Write { kind, files } => match ext_host::folder_in(dir, &kind, true) {
            Ok(folder) => ext_host::write_files(&folder, &files),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
        HostRequest::Build { kind } => match ext_host::folder_for(dir, &kind) {
            Err(e) => json!({"ok": false, "error": e.to_string()}),
            Ok(folder) => {
                let api = api.map(Path::to_path_buf);
                match tokio::task::spawn_blocking(move || ext_host::build(&folder, api.as_deref()))
                    .await
                {
                    Ok(Ok(so)) => json!({"ok": true, "staged": so.display().to_string()}),
                    Ok(Err(e)) => json!({"ok": false, "error": e.to_string()}),
                    Err(e) => json!({"ok": false, "error": format!("build task failed: {e}")}),
                }
            }
        },
        // A child process, so a staged artifact that segfaults takes the child and
        // not every extension this host already loaded.
        HostRequest::Check { kind } => match ext_host::folder_for(dir, &kind) {
            Ok(folder) => child_check(&folder).await,
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
        HostRequest::Apply { kind } => match ext_host::folder_for(dir, &kind) {
            Ok(folder) => swapped(ext_host::apply(&folder), live, dir),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
        HostRequest::Rollback { kind } => match ext_host::folder_for(dir, &kind) {
            Ok(folder) => swapped(ext_host::rollback(&folder), live, dir),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
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

/// Promote or restore, then load what is on disk now. The report carries the
/// refusal of any folder, including the one just applied, so a caller sees a
/// generation that will not load instead of assuming the swap worked.
fn swapped(out: anyhow::Result<Value>, live: &Live, dir: &Path) -> Value {
    let mut out = match out {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": e.to_string()}),
    };
    let state = rescan(dir);
    let report = state.report();
    *live.write().expect("live lock") = state;
    if let Some(map) = out.as_object_mut() {
        map.insert("ok".to_string(), Value::Bool(true));
        map.insert(
            "loaded".to_string(),
            report.get("loaded").cloned().unwrap_or(Value::Null),
        );
        map.insert(
            "refused".to_string(),
            report.get("refused").cloned().unwrap_or(Value::Null),
        );
    }
    out
}

/// Run `ext_host check` in a fresh process and return its JSON line.
async fn child_check(folder: &Path) -> Value {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return json!({"ok": false, "error": format!("cannot find this binary: {e}")}),
    };
    let folder = folder.to_path_buf();
    let ran = tokio::task::spawn_blocking(move || {
        std::process::Command::new(exe)
            .arg("check")
            .arg(folder)
            .output()
    })
    .await;
    let out = match ran {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return json!({"ok": false, "error": format!("could not run the check: {e}")});
        }
        Err(e) => return json!({"ok": false, "error": format!("check task failed: {e}")}),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&stdout) {
        return v;
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !stderr.is_empty() {
        return json!({"ok": false, "error": stderr});
    }
    json!({"ok": false, "error": format!("the check exited {}", out.status)})
}
