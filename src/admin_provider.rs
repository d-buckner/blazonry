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
    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("calendar");
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
    let id = providers::create(&st.pool, kind, name, &config, refresh_secs)
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

/// Probe a Home Assistant instance for its dashboards and tabs. Lovelace
/// config is WebSocket-only, so this is how the admin UI offers real tabs
/// instead of asking for a tab id typed by hand.
pub async fn ha_tabs(Json(b): Json<Value>) -> Res {
    let want = |key: &str| -> Option<String> {
        b.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let Some(url) = want("url") else {
        return Err(bad("url required"));
    };
    let Some(token) = want("token") else {
        return Err(bad("token required"));
    };
    let dashboard = want("dashboard").unwrap_or_else(|| "lovelace".to_string());
    let out = crate::extensions::dashboard::home_assistant::browse(&url, &token, &dashboard)
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
    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("Calendar");
    let settings = b.get("settings").cloned().unwrap_or(json!({}));
    match extensions::create(&st.pool, &st.backends, &st.cache, kind, name, settings).await {
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
