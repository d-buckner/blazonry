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
        let stream = UnixStream::connect(&self.socket).await.map_err(|e| {
            anyhow::anyhow!(
                "cannot reach the extension host at {}: {e}",
                self.socket.display()
            )
        })?;
        let (r, mut w) = stream.into_split();
        w.write_all(serde_json::to_string(&req)?.as_bytes()).await?;
        w.write_all(b"\n").await?;
        let mut line = String::new();
        // One request, one answer. A host that sends nothing and hangs up is a
        // host that is restarting.
        if BufReader::new(r).read_line(&mut line).await? == 0 {
            anyhow::bail!("the extension host closed the socket without answering");
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
