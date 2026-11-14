// SPDX-License-Identifier: AGPL-3.0-only
//! The server's side of the extension host (ADR-0002).
//!
//! The server never opens a plugin. It asks a second process over a unix socket,
//! and a host that will not answer leaves the bundled extensions running.
//!
//! The server learns a host extension's manifest and template once, at sync, and
//! calls the host for vars on every rebuild.

use crate::extensions::{self, Extension, ExtensionManifest, Plugin, VarsRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// One request the host serves, one per line of JSON.
///
/// `build`, `check`, `apply`, and `rollback` are the ADR-0003 authoring loop. The
/// server forwards them; the host does the work, because the host is the process
/// with a compiler and the loaded libraries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    /// Liveness, plus what the host has loaded.
    Ping,
    /// Every loaded manifest and template, so the server can register them.
    Kinds,
    /// Rescan the folder. Refusals come back in the same report.
    Reload,
    /// Vars for one rebuild.
    Vars { req: VarsRequest },
    /// Write source, manifest, template, and fixtures into one folder.
    Write {
        /// The extension folder, named by its kind. The host owns the volume, so
        /// the server never sends a path.
        kind: String,
        /// File path inside the folder to text content.
        files: std::collections::BTreeMap<String, String>,
    },
    /// Compile one extension folder into its staging directory.
    Build { kind: String },
    /// Run the staged artifact in a throwaway process and paint it.
    Check { kind: String },
    /// Promote the staged artifact, then reload it.
    Apply { kind: String },
    /// Put the displaced artifact back, then reload it.
    Rollback { kind: String },
}

/// One connection per request. A rebuild is seconds apart, so a kept-alive
/// socket would buy nothing and would have to notice a restarted host.
impl HostRequest {
    /// The op name, for a message a log can be read in.
    fn name(&self) -> &'static str {
        match self {
            HostRequest::Ping => "ping",
            HostRequest::Kinds => "kinds",
            HostRequest::Reload => "reload",
            HostRequest::Vars { .. } => "vars",
            HostRequest::Write { .. } => "write",
            HostRequest::Build { .. } => "build",
            HostRequest::Check { .. } => "check",
            HostRequest::Apply { .. } => "apply",
            HostRequest::Rollback { .. } => "rollback",
        }
    }

    /// How long to wait for this op. One limit for everything is wrong: `vars`
    /// should answer in a blink and `build` runs cargo.
    ///
    /// The limit protects the caller, not the host. A plugin that spins forever
    /// keeps the host's blocking thread forever; this stops the server from
    /// waiting on it, and keeps the rebuild loop alive for every other screen.
    fn timeout(&self) -> std::time::Duration {
        use std::time::Duration;
        let secs = match self {
            HostRequest::Ping => 5,
            // First kinds call loads every library on disk.
            HostRequest::Kinds => 30,
            HostRequest::Reload => 60,
            HostRequest::Vars { .. } => 30,
            HostRequest::Write { .. } => 30,
            // A cold cargo build of a plugin with serde and chrono, on a Pi.
            HostRequest::Build { .. } => 900,
            HostRequest::Check { .. } => 120,
            HostRequest::Apply { .. } => 120,
            HostRequest::Rollback { .. } => 120,
        };
        Duration::from_secs(secs)
    }
}

#[derive(Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Client {
            socket: socket.into(),
        }
    }

    pub async fn call(&self, req: HostRequest) -> anyhow::Result<Value> {
        let op = req.name();
        let limit = req.timeout();
        tokio::time::timeout(limit, self.call_inner(&req))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "the extension host did not answer '{op}' within {}s at {}",
                    limit.as_secs(),
                    self.socket.display()
                )
            })?
    }

    async fn call_inner(&self, req: &HostRequest) -> anyhow::Result<Value> {
        let where_ = format!("the extension host at {}", self.socket.display());
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| anyhow::anyhow!("cannot reach {where_}: {e}"))?;
        let (r, mut w) = stream.into_split();
        let line_out = serde_json::to_string(req)?;
        // A host killed mid-request resets the socket. Without the op and the
        // socket in the message, "Connection reset by peer" tells a reader
        // nothing about which process died.
        w.write_all(line_out.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("cannot send '{}' to {where_}: {e}", req.name()))?;
        w.write_all(b"\n").await?;
        let mut line = String::new();
        let read = BufReader::new(r)
            .read_line(&mut line)
            .await
            .map_err(|e| anyhow::anyhow!("{where_} failed while answering: {e}"))?;
        // One request, one answer. A host that reads the request, sends nothing,
        // and closes cleanly is a host that is restarting.
        if read == 0 {
            anyhow::bail!("{where_} closed the socket without answering");
        }
        Ok(serde_json::from_str(&line)?)
    }
}

/// An extension the server cannot see, only ask.
struct Remote {
    client: Client,
    template: String,
}

impl Extension for Remote {
    fn template(&self) -> String {
        self.template.clone()
    }

    fn vars(
        &self,
        req: VarsRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send>> {
        let client = self.client.clone();
        Box::pin(async move {
            let answer = client.call(HostRequest::Vars { req }).await?;
            match answer.get("vars") {
                Some(vars) => Ok(vars.clone()),
                None => {
                    let why = answer
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("the host sent no vars");
                    anyhow::bail!("{why}")
                }
            }
        })
    }
}

/// Ask the host what it has loaded and put those kinds in the registry. Returns
/// how many joined. A host that is not running syncs zero kinds, which is the
/// same state as a device with no extensions installed.
pub async fn sync(socket: &Path) -> anyhow::Result<usize> {
    let client = Client::new(socket);
    let report = client.call(HostRequest::Kinds).await?;
    let empty = Vec::new();
    let kinds = report
        .get("kinds")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let mut plugins = Vec::with_capacity(kinds.len());
    for entry in kinds {
        let Some(template) = entry.get("template").and_then(Value::as_str) else {
            tracing::warn!("ext_link: a host extension sent no template; skipped");
            continue;
        };
        match ExtensionManifest::parse(&entry["manifest"].to_string(), "the extension host") {
            Ok(manifest) => plugins.push(Arc::new(Plugin {
                manifest,
                extension: Arc::new(Remote {
                    client: client.clone(),
                    template: template.to_string(),
                }),
                // A runtime extension reads providers the core already fetches;
                // it cannot add a provider of its own.
                providers: Vec::new(),
            })),
            Err(e) => tracing::warn!("ext_link: refused a host extension: {e}"),
        }
    }
    for refused in report
        .get("refused")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        tracing::warn!(
            "ext_link: the host refused {}: {}",
            refused["folder"].as_str().unwrap_or("?"),
            refused["reason"].as_str().unwrap_or("?")
        );
    }
    Ok(extensions::merge_host(plugins))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blazonry-link-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("host.sock")
    }

    /// A host that accepts the connection and then says nothing must not hang the
    /// caller forever. This waits the real ping limit, so it is slow on purpose:
    /// the point is the limit the production table actually sets.
    #[tokio::test]
    async fn a_host_that_never_answers_times_out() {
        let sock = temp_socket("silent");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (_stream, _addr) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        });
        let err = Client::new(&sock).call(HostRequest::Ping).await;
        let err = err.unwrap_err().to_string();
        assert!(err.contains("did not answer 'ping' within 5s"), "{err}");
    }

    /// A host that reads the request, answers nothing, and closes cleanly is
    /// restarting. The message has to say so, or it reads like a broken protocol.
    #[tokio::test]
    async fn a_host_that_answers_nothing_says_so() {
        let sock = temp_socket("hangup");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (mut stream, _addr) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 256];
            use tokio::io::AsyncReadExt as _;
            let _ = stream.read(&mut buf).await;
            drop(stream);
        });
        let err = Client::new(&sock).call(HostRequest::Ping).await;
        let err = err.unwrap_err().to_string();
        assert!(err.contains("closed the socket without answering"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_socket_names_the_socket() {
        let err = Client::new("/tmp/blazonry-link-absent/host.sock")
            .call(HostRequest::Kinds)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot reach the extension host at"), "{err}");
    }
}
