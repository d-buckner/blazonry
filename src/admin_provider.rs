//! Admin CRUD for providers and extensions (the data-plane registry).
//!
//!   providers   GET|POST /providers, GET|PATCH|DELETE /providers/:id,
//!               POST /providers/:id/refresh
//!   extensions  GET|POST /extensions, GET|PATCH|DELETE /extensions/:id
//!
//! HTTP only: parse request, call the owning module (`providers` /
//! `extensions`), map errors to status codes. No raw SQL here (issue #9).
//! Cardinality validation lives in extensions::create — "requires a
//! calendar, picked none" fails here at config time.

use crate::api::SharedState;
use crate::extensions;
use crate::providers;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

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

pub async fn create_provider(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = b.get("kind").and_then(|v| v.as_str()).ok_or_else(|| bad("kind required"))?;
    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("calendar");
    let config = b.get("config").cloned().unwrap_or(Value::Object(Default::default()));
    // Validate the config through the provider registry — bad config fails
    // here, not at render time.
    providers::validate_config(kind, &config).map_err(|e| bad(e.to_string()))?;
    let refresh_secs = b.get("refresh_secs").and_then(|v| v.as_i64())
        .unwrap_or(providers::DEFAULT_REFRESH_SECS).max(60);
    let id = providers::create(&st.pool, kind, name, &config, refresh_secs).await.map_err(err)?;
    // First refresh inline so the admin sees success/failure immediately.
    let refresh = providers::refresh_provider(&st.pool, id).await;
    let mut out = providers::get(&st.pool, id).await.map_err(err)?
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
    let mut out = providers::get(&st.pool, id).await.map_err(err)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "unknown provider".into()))?;
    out["item_count"] = json!(providers::snapshot_items(&st.pool, id).await.map_err(err)?);
    Ok(Json(out))
}

pub async fn patch_provider(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    if !providers::exists(&st.pool, id).await.map_err(err)? {
        return Err((StatusCode::NOT_FOUND, "unknown provider".into()));
    }
    let name = b.get("name").and_then(|v| v.as_str());
    let secs = b.get("refresh_secs").and_then(|v| v.as_i64()).map(|s| s.max(60));
    let config = b.get("config");
    if let Some(config) = config {
        let kind = providers::kind_of(&st.pool, id).await.map_err(err)?;
        providers::validate_config(&kind, config).map_err(|e| bad(e.to_string()))?;
    }
    providers::patch(&st.pool, id, name, secs, config).await.map_err(err)?;
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
            if r.changed { shapes.insert(r.shape); }
            if !shapes.is_empty()
                && let Err(e) = extensions::rebuild_extensions_shapes(&st.pool, &st.backends, &st.cache, &shapes).await {
                tracing::warn!(%e, "extension rebuild after refresh failed");
            }
            Ok(Json(json!({ "ok": true, "items": r.items, "changed": r.changed })))
        }
        Err(e) => Ok(Json(json!({ "ok": false, "error": e.to_string() }))),
    }
}

// ---------- extensions ----------

pub async fn list_extensions(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(extensions::list(&st.pool).await.map_err(err)?)))
}

pub async fn create_extension(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = b.get("kind").and_then(|v| v.as_str()).ok_or_else(|| bad("kind required"))?;
    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("Calendar");
    let settings = b.get("settings").cloned().unwrap_or(json!({}));
    match extensions::create(&st.pool, &st.backends, &st.cache, kind, name, settings).await {
        Ok((id, screen_id)) => Ok(Json(json!({ "id": id, "screen_id": screen_id }))),
        Err(e) => Err(bad(e.to_string())),
    }
}

pub async fn patch_extension(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    if !extensions::exists(&st.pool, id).await.map_err(err)? {
        return Err((StatusCode::NOT_FOUND, "unknown extension".into()));
    }
    if let Some(name) = b.get("name").and_then(|v| v.as_str()) {
        extensions::set_name(&st.pool, id, name).await.map_err(err)?;
    }
    if let Some(settings) = b.get("settings") {
        // Re-validate cardinality on settings change (explicit picks must
        // exist AND publish the shape this extension consumes).
        if let Some(ids) = settings.get("provider_ids").and_then(|v| v.as_array()) {
            let ids: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
            if ids.len() != settings["provider_ids"].as_array().map_or(0, |a| a.len()) {
                return Err(bad("provider_ids must be integers"));
            }
            let kind = extensions::kind_of(&st.pool, id).await.map_err(err)?;
            match extensions::consumes_shape(&kind) {
                Some(shape) => {
                    if !providers::ids_with_shape_exist(&st.pool, shape, &ids).await.map_err(err)? {
                        return Err(bad(format!("provider_ids contains unknown or non-{shape} providers")));
                    }
                }
                None => return Err(bad("this extension kind consumes no providers; provider_ids is not applicable")),
            }
        }
        extensions::set_settings(&st.pool, id, settings).await.map_err(err)?;
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
