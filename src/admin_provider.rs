//! Admin CRUD for providers and extensions (the data-plane registry).
//!
//!   providers   GET|POST /providers, GET|PATCH|DELETE /providers/:id,
//!               POST /providers/:id/refresh
//!   extensions  GET|POST /extensions, GET|PATCH|DELETE /extensions/:id
//!
//! Cardinality validation lives in extension::create_family_calendar:
//! "requires a calendar, picked none" fails here at config time.

use crate::api::SharedState;
use crate::providers;
use axum::extract::{Path, State};
use sqlx::AssertSqlSafe;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;

type Err = (StatusCode, String);
type Res = Result<Json<Value>, Err>;

fn err(e: impl std::fmt::Display) -> Err {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
fn bad(msg: impl Into<String>) -> Err {
    (StatusCode::BAD_REQUEST, msg.into())
}

fn provider_json(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "id": row.get::<i64, _>(0),
        "kind": row.get::<String, _>(1),
        "name": row.get::<String, _>(2),
        "config": serde_json::from_str::<Value>(&row.get::<String, _>(3)).unwrap_or(Value::Null),
        "refresh_secs": row.get::<i64, _>(4),
        "last_ok_at": row.get::<Option<String>, _>(5),
        "last_error": row.get::<Option<String>, _>(6),
    })
}

const PROVIDER_COLS: &str = "id, kind, name, config, refresh_secs, last_ok_at, last_error";

pub async fn list_providers(State(st): State<SharedState>) -> Res {
    let rows = sqlx::query(AssertSqlSafe(format!("SELECT {PROVIDER_COLS} FROM providers ORDER BY id")))
        .fetch_all(&st.pool).await.map_err(err)?;
    Ok(Json(json!(rows.iter().map(provider_json).collect::<Vec<_>>())))
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
    let id = sqlx::query("INSERT INTO providers (kind, name, config, refresh_secs) VALUES (?, ?, ?, ?)")
        .bind(kind).bind(name).bind(config.to_string()).bind(refresh_secs)
        .execute(&st.pool).await.map_err(err)?.last_insert_rowid();
    // First refresh inline so the admin sees success/failure immediately.
    let refresh = providers::refresh_provider(&st.pool, id).await;
    let row = sqlx::query(AssertSqlSafe(format!("SELECT {PROVIDER_COLS} FROM providers WHERE id = ?")))
        .bind(id).fetch_one(&st.pool).await.map_err(err)?;
    let mut out = provider_json(&row);
    out["last_event_count"] = json!(refresh.as_ref().ok().copied().unwrap_or(0));
    if let Err(e) = refresh {
        out["first_refresh_error"] = json!(e.to_string());
    }
    // New provider may satisfy an "all" selector on existing extensions.
    if let Err(e) = crate::extensions::rebuild_extensions(&st.pool, &st.cache).await {
        tracing::warn!(%e, "extension rebuild after provider create failed");
    }
    Ok(Json(out))
}

pub async fn get_provider(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let row = sqlx::query(AssertSqlSafe(format!("SELECT {PROVIDER_COLS} FROM providers WHERE id = ?")))
        .bind(id).fetch_optional(&st.pool).await.map_err(err)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "unknown provider".into()))?;
    let events = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM provider_events WHERE provider_id = ?")
        .bind(id).fetch_one(&st.pool).await.map_err(err)?;
    let mut out = provider_json(&row);
    out["event_count"] = json!(events);
    Ok(Json(out))
}

pub async fn patch_provider(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM providers WHERE id = ?")
        .bind(id).fetch_one(&st.pool).await.map_err(err)?;
    if exists == 0 {
        return Err((StatusCode::NOT_FOUND, "unknown provider".into()));
    }
    if let Some(name) = b.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE providers SET name = ? WHERE id = ?").bind(name).bind(id)
            .execute(&st.pool).await.map_err(err)?;
    }
    if let Some(secs) = b.get("refresh_secs").and_then(|v| v.as_i64()) {
        sqlx::query("UPDATE providers SET refresh_secs = ? WHERE id = ?")
            .bind(secs.max(60)).bind(id).execute(&st.pool).await.map_err(err)?;
    }
    if let Some(config) = b.get("config") {
        let kind: String = sqlx::query_scalar("SELECT kind FROM providers WHERE id = ?")
            .bind(id).fetch_one(&st.pool).await.map_err(err)?;
        providers::validate_config(&kind, config).map_err(|e| bad(e.to_string()))?;
        sqlx::query("UPDATE providers SET config = ? WHERE id = ?")
            .bind(config.to_string()).bind(id).execute(&st.pool).await.map_err(err)?;
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_provider(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    sqlx::query("DELETE FROM provider_events WHERE provider_id = ?").bind(id)
        .execute(&st.pool).await.map_err(err)?;
    sqlx::query("DELETE FROM providers WHERE id = ?").bind(id)
        .execute(&st.pool).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

/// Manual "refresh now" — the escape hatch beside the timer.
pub async fn refresh_provider_now(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    match providers::refresh_provider(&st.pool, id).await {
        Ok(n) => {
            if let Err(e) = crate::extensions::rebuild_extensions(&st.pool, &st.cache).await {
                tracing::warn!(%e, "extension rebuild after refresh failed");
            }
            Ok(Json(json!({ "ok": true, "events": n })))
        }
        Err(e) => Ok(Json(json!({ "ok": false, "error": e.to_string() }))),
    }
}

// ---------- extensions ----------

pub async fn list_extensions(State(st): State<SharedState>) -> Res {
    let rows = sqlx::query("SELECT id, kind, name, settings, screen_id FROM extensions ORDER BY id")
        .fetch_all(&st.pool).await.map_err(err)?;
    Ok(Json(json!(rows.iter().map(|r| json!({
        "id": r.get::<i64, _>(0),
        "kind": r.get::<String, _>(1),
        "name": r.get::<String, _>(2),
        "settings": serde_json::from_str::<Value>(&r.get::<String, _>(3)).unwrap_or(Value::Null),
        "screen_id": r.get::<Option<i64>, _>(4),
    })).collect::<Vec<_>>())))
}

pub async fn create_extension(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let kind = b.get("kind").and_then(|v| v.as_str()).ok_or_else(|| bad("kind required"))?;
    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("Family Calendar");
    let settings = b.get("settings").cloned().unwrap_or(json!({}));
    match crate::extensions::create(&st.pool, &st.cache, kind, name, settings).await {
        Ok((id, screen_id)) => Ok(Json(json!({ "id": id, "screen_id": screen_id }))),
        Err(e) => Err(bad(e.to_string())),
    }
}

pub async fn patch_extension(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM extensions WHERE id = ?")
        .bind(id).fetch_one(&st.pool).await.map_err(err)?;
    if exists == 0 {
        return Err((StatusCode::NOT_FOUND, "unknown extension".into()));
    }
    if let Some(name) = b.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE extensions SET name = ? WHERE id = ?").bind(name).bind(id)
            .execute(&st.pool).await.map_err(err)?;
    }
    if let Some(settings) = b.get("settings") {
        // Re-validate cardinality on settings change (explicit picks must exist).
        if let Some(ids) = settings.get("provider_ids").and_then(|v| v.as_array()) {
            if !ids.is_empty() {
                let placeholders = vec!["?"; ids.len()].join(",");
                let mut q = sqlx::query(AssertSqlSafe(format!(
                    "SELECT COUNT(*) FROM providers WHERE id IN ({placeholders})")));
                for v in ids {
                    let id_v = v.as_i64().ok_or_else(|| bad("provider_ids must be integers"))?;
                    q = q.bind(id_v);
                }
                let n: i64 = q.fetch_one(&st.pool).await.map_err(err)?.get(0);
                if n != ids.len() as i64 {
                    return Err(bad("provider_ids contains unknown or non-calendar providers"));
                }
            }
        }
        sqlx::query("UPDATE extensions SET settings = ? WHERE id = ?")
            .bind(settings.to_string()).bind(id).execute(&st.pool).await.map_err(err)?;
    }
    if let Err(e) = crate::extensions::rebuild_extensions(&st.pool, &st.cache).await {
        tracing::warn!(%e, "extension rebuild after patch failed");
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_extension(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let sid: Option<i64> = sqlx::query_scalar("SELECT screen_id FROM extensions WHERE id = ?")
        .bind(id).fetch_optional(&st.pool).await.map_err(err)?.flatten();
    sqlx::query("DELETE FROM extensions WHERE id = ?").bind(id)
        .execute(&st.pool).await.map_err(err)?;
    if let Some(sid) = sid {
        sqlx::query("DELETE FROM playlist_screens WHERE screen_id = ?").bind(sid)
            .execute(&st.pool).await.map_err(err)?;
        sqlx::query("DELETE FROM screens WHERE id = ?").bind(sid)
            .execute(&st.pool).await.map_err(err)?;
        crate::cache::invalidate(&st.cache, sid).await;
    }
    Ok(Json(json!({ "ok": true })))
}
