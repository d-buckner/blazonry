// SPDX-License-Identifier: AGPL-3.0-only
//! Admin CRUD for providers and extensions (the data-plane registry).
//!
//!   providers   GET|POST /providers, GET|PATCH|DELETE /providers/:id,
//!               POST /providers/:id/refresh
//!   extensions  GET|POST /extensions, GET|PATCH|DELETE /extensions/:id
//!   kinds       GET /kinds — the manifests of every kind this build ships
//!
//! HTTP only: parse request, call the owning module (`providers` /
//! `extensions`), map errors to status codes. No raw SQL here (issue #9).
//! Settings and cardinality validation lives in `extensions` — "needs a
//! calendar source, picked none" fails here at config time.

use crate::api::SharedState;
use crate::ext_link::{self, HostRequest};
use crate::extensions;
use crate::providers;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

type Err = (StatusCode, String);
type Res = Result<Json<Value>, Err>;

fn err(e: impl std::fmt::Display) -> Err {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
fn bad(msg: impl Into<String>) -> Err {
    (StatusCode::BAD_REQUEST, msg.into())
}

pub async fn list_providers(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(providers::list(&st.pool).await.map_err(err)?)))
}

/// The plugin catalog: every extension and provider kind this build ships,
/// described by the manifest in its folder. The admin UI builds both "add"
/// forms from this response, so a new extension needs no front-end change.
pub async fn kinds() -> Res {
    let ext: Vec<Value> = extensions::registry()
        .plugins
        .iter()
        .map(|p| serde_json::to_value(&p.manifest).map_err(err))
        .collect::<Result<_, _>>()?;
    Ok(Json(json!({
        "extensions": ext,
        "providers": providers::kind_metadata(),
    })))
}

pub async fn create_provider(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = b
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("kind required"))?;
    // A name is optional; the kind's own manifest label stands in, so no
    // default is hardcoded per kind here.
    let name = b
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            providers::lookup(kind)
                .map(|p| p.manifest().label.clone())
                .unwrap_or_else(|| kind.to_string())
        });
    let config = b
        .get("config")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    // Validate the config through the provider registry — bad config fails
    // here, not at render time.
    providers::validate_config(kind, &config).map_err(|e| bad(e.to_string()))?;
    let refresh_secs = b
        .get("refresh_secs")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| providers::default_refresh_secs(kind))
        .max(60);
    let id = providers::create(&st.pool, kind, &name, &config, refresh_secs)
        .await
        .map_err(err)?;
    // First refresh inline so the admin sees success/failure immediately.
    let refresh = providers::refresh_provider(&st.pool, id).await;
    let mut out = providers::get(&st.pool, id)
        .await
        .map_err(err)?
        .ok_or_else(|| err("provider vanished after create"))?;
    out["last_item_count"] = json!(refresh.as_ref().ok().map(|r| r.items).unwrap_or(0));
    if let Err(e) = refresh {
        out["first_refresh_error"] = json!(e.to_string());
    }
    // New provider may satisfy an "all" selector on existing extensions.
    if let Err(e) = extensions::rebuild_extensions(&st.pool, &st.backends, &st.cache).await {
        tracing::warn!(%e, "extension rebuild after provider create failed");
    }
    Ok(Json(out))
}

pub async fn get_provider(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let mut out = providers::get(&st.pool, id)
        .await
        .map_err(err)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "unknown provider".into()))?;
    out["item_count"] = json!(providers::snapshot_items(&st.pool, id).await.map_err(err)?);
    Ok(Json(out))
}

pub async fn patch_provider(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    if !providers::exists(&st.pool, id).await.map_err(err)? {
        return Err((StatusCode::NOT_FOUND, "unknown provider".into()));
    }
    let name = b.get("name").and_then(|v| v.as_str());
    let secs = b
        .get("refresh_secs")
        .and_then(|v| v.as_i64())
        .map(|s| s.max(60));
    let config = b.get("config");
    if let Some(config) = config {
        let kind = providers::kind_of(&st.pool, id).await.map_err(err)?;
        providers::validate_config(&kind, config).map_err(|e| bad(e.to_string()))?;
    }
    providers::patch(&st.pool, id, name, secs, config)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_provider(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    providers::delete(&st.pool, id).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

/// Manual "refresh now" — the escape hatch beside the timer.
pub async fn refresh_provider_now(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    match providers::refresh_provider(&st.pool, id).await {
        Ok(r) => {
            // Shape-scoped: a manual refresh repaints only screens consuming
            // this provider's shape (and only if content changed).
            let mut shapes: std::collections::HashSet<String> = Default::default();
            if r.changed {
                shapes.insert(r.shape);
            }
            if !shapes.is_empty()
                && let Err(e) = extensions::rebuild_extensions_shapes(
                    &st.pool,
                    &st.backends,
                    &st.cache,
                    &shapes,
                )
                .await
            {
                tracing::warn!(%e, "extension rebuild after refresh failed");
            }
            Ok(Json(
                json!({ "ok": true, "items": r.items, "changed": r.changed }),
            ))
        }
        Err(e) => Ok(Json(json!({ "ok": false, "error": e.to_string() }))),
    }
}

/// Run a provider kind's live browse ("which tabs does this dashboard have?").
/// Which kinds offer one, and which fields the answer fills, is the kind's own
/// business; this handler only routes it. A failure comes back as
/// `{ok: false, error}` so the dialog can show the reason inline.
pub async fn browse_provider(Path(kind): Path<String>, Json(b): Json<Value>) -> Res {
    let out = providers::browse(&kind, b)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
    Ok(Json(out))
}

// ---------- extensions ----------

pub async fn list_extensions(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(extensions::list(&st.pool).await.map_err(err)?)))
}

pub async fn create_extension(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = b
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("kind required"))?;
    // Optional name, same rule as providers: the manifest label stands in.
    let name = b
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            extensions::lookup(kind)
                .map(|p| p.manifest.label.clone())
                .unwrap_or_else(|| kind.to_string())
        });
    let settings = b.get("settings").cloned().unwrap_or(json!({}));
    match extensions::create(&st.pool, &st.backends, &st.cache, kind, &name, settings).await {
        Ok((id, screen_id)) => Ok(Json(json!({ "id": id, "screen_id": screen_id }))),
        Err(e) => Err(bad(e.to_string())),
    }
}

pub async fn patch_extension(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    if !extensions::exists(&st.pool, id).await.map_err(err)? {
        return Err((StatusCode::NOT_FOUND, "unknown extension".into()));
    }
    if let Some(name) = b.get("name").and_then(|v| v.as_str()) {
        extensions::set_name(&st.pool, id, name)
            .await
            .map_err(err)?;
    }
    if let Some(settings) = b.get("settings") {
        // Same check as create: manifest schema, defaults, and a provider pick
        // that must exist and publish the shape this extension consumes.
        let kind = extensions::kind_of(&st.pool, id).await.map_err(err)?;
        let settings = extensions::validate_settings(&st.pool, &kind, settings)
            .await
            .map_err(|e| bad(e.to_string()))?;
        extensions::set_settings(&st.pool, id, &settings)
            .await
            .map_err(err)?;
    }
    if let Err(e) = extensions::rebuild_extensions(&st.pool, &st.backends, &st.cache).await {
        tracing::warn!(%e, "extension rebuild after patch failed");
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_extension(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    if let Some(sid) = extensions::delete(&st.pool, id).await.map_err(err)? {
        crate::cache::invalidate(&st.cache, sid).await;
    }
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Runtime extension authoring (ADR-0003).
//
// All five forward to the extension host. The host owns the data volume and the
// compiler, so the server never names a path on disk and a request carries a kind.
//
// A red build or check answers 200 with `"ok": false`, because the request was
// valid and the finding is the answer. A 400 means this server cannot act on the
// request at all: no kind, or no host to ask.
// ---------------------------------------------------------------------------

fn host(st: &SharedState) -> Result<ext_link::Client, Err> {
    st.ext_host
        .as_ref()
        .map(ext_link::Client::new)
        .ok_or_else(|| bad("no extension host: start this server with --ext-host <socket>"))
}

fn kind_of(b: &Value) -> Result<String, Err> {
    b.get("kind")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad("kind required"))
}

pub async fn ext_write(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = kind_of(&b)?;
    let files: std::collections::BTreeMap<String, String> =
        serde_json::from_value(b.get("files").cloned().unwrap_or(Value::Null))
            .map_err(|e| bad(format!("files must map a path to text: {e}")))?;
    if files.is_empty() {
        return Err(bad("files is empty"));
    }
    Ok(Json(
        host(&st)?
            .call(HostRequest::Write { kind, files })
            .await
            .map_err(err)?,
    ))
}

pub async fn ext_build(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = kind_of(&b)?;
    Ok(Json(
        host(&st)?
            .call(HostRequest::Build { kind })
            .await
            .map_err(err)?,
    ))
}

pub async fn ext_check(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = kind_of(&b)?;
    Ok(Json(
        host(&st)?
            .call(HostRequest::Check { kind })
            .await
            .map_err(err)?,
    ))
}

pub async fn ext_apply(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = kind_of(&b)?;
    let socket = st
        .ext_host
        .as_ref()
        .ok_or_else(|| bad("no extension host: start this server with --ext-host <socket>"))?
        .clone();
    let mut out = ext_link::Client::new(&socket)
        .call(HostRequest::Apply { kind: kind.clone() })
        .await
        .map_err(err)?;
    if out.get("ok").and_then(Value::as_bool) == Some(true) {
        apply_aftermath(&st, &socket, &mut out, &kind).await;
    }
    Ok(Json(out))
}

pub async fn ext_rollback(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = kind_of(&b)?;
    let socket = st
        .ext_host
        .as_ref()
        .ok_or_else(|| bad("no extension host: start this server with --ext-host <socket>"))?
        .clone();
    let mut out = ext_link::Client::new(&socket)
        .call(HostRequest::Rollback { kind: kind.clone() })
        .await
        .map_err(err)?;
    if out.get("ok").and_then(Value::as_bool) == Some(true) {
        apply_aftermath(&st, &socket, &mut out, &kind).await;
    }
    Ok(Json(out))
}

/// A generation swap changes what the server can see and what is on the wall.
/// Re-sync the kinds, because a first apply adds one, and repaint the screens
/// bound to this kind, so the author sees the result now rather than at the next
/// refresh.
async fn apply_aftermath(st: &SharedState, socket: &std::path::Path, out: &mut Value, kind: &str) {
    if let Err(e) = ext_link::sync(socket).await {
        tracing::warn!(%e, "kind sync after a generation swap failed");
    }
    match extensions::rebuild_kind(&st.pool, &st.backends, &st.cache, kind).await {
        Ok(n) => out["rebuilt"] = json!(n),
        Err(e) => out["rebuild_error"] = json!(e.to_string()),
    }
}
