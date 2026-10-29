//! Admin CRUD plane — Terminus `/api/*` parity (config/routes.rb, vendored
//! in tests/spec/routes.rb). Terminus ids are UUIDs; we use INTEGER rowids
//! (documented deviation). Everything here is auth-gated in the router.
//!
//! Routes (nested under /admin):
//!   devices     GET /devices, GET|PATCH|DELETE /devices/:id
//!   screens     GET /screens, GET|PATCH|DELETE /screens/:id (+preview)
//!   playlists   GET /playlists, GET|PATCH|DELETE /playlists/:id
//!               POST /playlists/:id/items, PATCH|DELETE /playlists/:id/items/:item_id
//!   firmware    GET|POST /firmware, GET|PATCH|DELETE /firmware/:id, DELETE /firmware (bulk wipe)
//!   models      GET /models, GET|POST|PATCH|DELETE /models/:id (POST /models)
//!   bulk        POST /devices/import, DELETE /devices/:id/logs

use crate::api::SharedState;
use crate::cache;
use axum::extract::{Path, State};
use sqlx::AssertSqlSafe;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

type Err = (StatusCode, String);
type Res = Result<Json<Value>, Err>;

fn err(e: sqlx::Error) -> Err {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
fn bad(msg: impl Into<String>) -> Err {
    (StatusCode::BAD_REQUEST, msg.into())
}
fn not_found(what: &str) -> Err {
    (StatusCode::NOT_FOUND, format!("unknown {what}"))
}

fn str_field<'a>(b: &'a Value, k: &str) -> Option<&'a str> {
    b.get(k).and_then(|v| v.as_str())
}

/// PATCH helper: build `SET a=?, b=?` from present keys only.
async fn patch_table(pool: &sqlx::SqlitePool, table: &'static str, id: i64, body: &Value, allowed: &[&'static str]) -> Result<u64, Err> {
    let mut sets: Vec<&str> = Vec::new();
    for k in allowed {
        if body.get(*k).is_some() {
            sets.push(k);
        }
    }
    if sets.is_empty() {
        return Ok(0);
    }
    // dynamic SET list is built only from the `allowed` constants, never user input
    let mut q = sqlx::query(AssertSqlSafe(format!(
        "UPDATE {table} SET {} WHERE id = ?",
        sets.iter().map(|s| format!("{s} = ?")).collect::<Vec<_>>().join(", ")
    )));
    for k in &sets {
        let v = body.get(*k).unwrap();
        q = if v.is_i64() || v.is_u64() {
            q.bind(v.as_i64())
        } else if v.is_f64() {
            q.bind(v.as_f64())
        } else if v.is_boolean() {
            q.bind(v.as_bool().map(|b| b as i64))
        } else if v.is_null() {
            q.bind(None::<String>)
        } else {
            q.bind(v.as_str().map(|s| s.to_string()))
        };
    }
    let n = q.bind(id).execute(pool).await.map_err(err)?.rows_affected();
    Ok(n)
}

// ---------- devices ----------

pub async fn list_devices(State(st): State<SharedState>) -> Res {
    let rows: Vec<Value> = sqlx::query_as(
        "SELECT json_object(
            'id', id, 'mac', mac, 'name', name, 'api_key', api_key,
            'playlist_id', playlist_id, 'last_poll_at', last_poll_at,
            'firmware_version', firmware_version, 'model', model,
            'battery_voltage', battery_voltage, 'rssi', rssi,
            'width', width, 'height', height, 'poll_count', poll_count
         ) AS j
         FROM devices ORDER BY id",
    )
    .fetch_all(&st.pool)
    .await
    .map_err(err)?
    .into_iter()
    .map(|(s,): (String,)| serde_json::from_str(&s).unwrap_or(Value::Null))
    .collect();
    Ok(Json(json!(rows)))
}

pub async fn get_device(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT json_object(
            'id', id, 'mac', mac, 'name', name, 'api_key', api_key,
            'playlist_id', playlist_id, 'last_poll_at', last_poll_at,
            'firmware_version', firmware_version, 'model', model,
            'battery_voltage', battery_voltage, 'rssi', rssi,
            'width', width, 'height', height, 'poll_count', poll_count
         ) FROM devices WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(err)?;
    match row {
        Some((s,)) => Ok(Json(serde_json::from_str(&s).unwrap_or(Value::Null))),
        None => Err(not_found("device")),
    }
}

pub async fn patch_device(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    // playlist_id may be explicitly null (unassign)
    let n = patch_table(&st.pool, "devices", id, &b, &["name", "playlist_id", "refresh_interval", "model"]).await?;
    if n == 0 { return Err(not_found("device")); }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_device(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = sqlx::query("DELETE FROM devices WHERE id = ?").bind(id).execute(&st.pool).await.map_err(err)?.rows_affected();
    if n == 0 { return Err(not_found("device")); }
    Ok(Json(json!({ "ok": true })))
}

/// Bulk import: {devices: [{mac, name?, playlist_id?}]} — upsert by mac.
pub async fn import_devices(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let arr = b.get("devices").and_then(|v| v.as_array()).ok_or_else(|| bad("devices array required"))?;
    let mut imported = 0;
    for d in arr {
        let Some(mac) = str_field(d, "mac") else { continue };
        let (_id, key, _, _) = crate::db::upsert_device(&st.pool, mac).await.map_err(|e| err(sqlx::Error::Protocol(e.to_string())))?;
        let _ = key;
        if let Some(name) = str_field(d, "name") {
            let _ = sqlx::query("UPDATE devices SET name = ? WHERE mac = ?").bind(name).bind(mac).execute(&st.pool).await;
        }
        if let Some(pl) = d.get("playlist_id").and_then(|v| v.as_i64()) {
            let _ = sqlx::query("UPDATE devices SET playlist_id = ? WHERE mac = ?").bind(pl).bind(mac).execute(&st.pool).await;
        }
        imported += 1;
    }
    Ok(Json(json!({ "imported": imported })))
}

pub async fn wipe_device_logs(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = sqlx::query("DELETE FROM device_logs WHERE device_id = ?").bind(id).execute(&st.pool).await.map_err(err)?.rows_affected();
    Ok(Json(json!({ "deleted": n })))
}

// ---------- screens ----------

const SCREEN_JSON: &str = "json_object('id', id, 'name', name, 'template', template, 'vars', vars, 'updated_at', updated_at)";

pub async fn list_screens(State(st): State<SharedState>) -> Res {
    let rows: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!("SELECT {SCREEN_JSON} FROM screens ORDER BY id")))
        .fetch_all(&st.pool).await.map_err(err)?;
    Ok(Json(json!(rows.iter().map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null)).collect::<Vec<_>>())))
}

pub async fn get_screen(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let row: Option<(String,)> = sqlx::query_as(AssertSqlSafe(format!("SELECT {SCREEN_JSON} FROM screens WHERE id = ?"))).bind(id)
        .fetch_optional(&st.pool).await.map_err(err)?;
    match row {
        Some((s,)) => Ok(Json(serde_json::from_str(&s).unwrap_or(Value::Null))),
        None => Err(not_found("screen")),
    }
}

pub async fn patch_screen(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    let n = patch_table(&st.pool, "screens", id, &b, &["name", "template", "vars"]).await?;
    if n == 0 { return Err(not_found("screen")); }
    sqlx::query("UPDATE screens SET updated_at = datetime('now') WHERE id = ?").bind(id).execute(&st.pool).await.map_err(err)?;
    cache::invalidate(&st.cache, id).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_screen(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = sqlx::query("DELETE FROM screens WHERE id = ?").bind(id).execute(&st.pool).await.map_err(err)?.rows_affected();
    if n == 0 { return Err(not_found("screen")); }
    sqlx::query("DELETE FROM playlist_screens WHERE screen_id = ?").bind(id).execute(&st.pool).await.map_err(err)?;
    cache::invalidate(&st.cache, id).await;
    Ok(Json(json!({ "ok": true })))
}

// ---------- playlists ----------

pub async fn list_playlists(State(st): State<SharedState>) -> Res {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_object('id', p.id, 'name', p.name, 'mirror', p.mirror,
            'screen_ids', (SELECT json_group_array(screen_id) FROM playlist_screens ps
                           WHERE ps.playlist_id = p.id AND ps.visible = 1 ORDER BY ps.position))
         FROM playlists p ORDER BY p.id",
    )
    .fetch_all(&st.pool).await.map_err(err)?;
    Ok(Json(json!(rows.iter().map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null)).collect::<Vec<_>>())))
}

pub async fn get_playlist(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM playlists WHERE id = ?").bind(id)
        .fetch_optional(&st.pool).await.map_err(err)?;
    if exists.is_none() { return Err(not_found("playlist")); }
    let items: Vec<String> = sqlx::query_scalar(
        "SELECT json_object('item_id', ps.screen_id, 'screen_id', ps.screen_id, 'position', ps.position,
            'visible', ps.visible, 'name', s.name)
         FROM playlist_screens ps JOIN screens s ON s.id = ps.screen_id
         WHERE ps.playlist_id = ? ORDER BY ps.position",
    )
    .bind(id).fetch_all(&st.pool).await.map_err(err)?;
    let name: String = sqlx::query_scalar("SELECT name FROM playlists WHERE id = ?").bind(id)
        .fetch_one(&st.pool).await.unwrap_or_default();
    Ok(Json(json!({
        "id": id, "name": name,
        "items": items.iter().map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null)).collect::<Vec<_>>()
    })))
}

pub async fn patch_playlist(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    if let Some(name) = str_field(&b, "name") {
        let n = sqlx::query("UPDATE playlists SET name = ? WHERE id = ?").bind(name).bind(id)
            .execute(&st.pool).await.map_err(err)?.rows_affected();
        if n == 0 { return Err(not_found("playlist")); }
    }
    if let Some(ids) = b.get("screen_ids").and_then(|v| v.as_array()) {
        sqlx::query("DELETE FROM playlist_screens WHERE playlist_id = ?").bind(id).execute(&st.pool).await.map_err(err)?;
        for (pos, sid) in ids.iter().filter_map(|v| v.as_i64()).enumerate() {
            sqlx::query("INSERT OR IGNORE INTO playlist_screens (playlist_id, screen_id, position) VALUES (?, ?, ?)")
                .bind(id).bind(sid).bind(pos as i64).execute(&st.pool).await.map_err(err)?;
        }
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_playlist(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = sqlx::query("DELETE FROM playlists WHERE id = ?").bind(id).execute(&st.pool).await.map_err(err)?.rows_affected();
    if n == 0 { return Err(not_found("playlist")); }
    sqlx::query("DELETE FROM playlist_screens WHERE playlist_id = ?").bind(id).execute(&st.pool).await.map_err(err)?;
    let _ = sqlx::query("UPDATE devices SET playlist_id = NULL WHERE playlist_id = ?").bind(id).execute(&st.pool).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn add_playlist_item(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    let sid = b.get("screen_id").and_then(|v| v.as_i64()).ok_or_else(|| bad("screen_id required"))?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM playlists WHERE id = ?").bind(id)
        .fetch_optional(&st.pool).await.map_err(err)?;
    if exists.is_none() { return Err(not_found("playlist")); }
    let pos: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_screens WHERE playlist_id = ?")
        .bind(id).fetch_one(&st.pool).await.unwrap_or(0);
    sqlx::query("INSERT OR REPLACE INTO playlist_screens (playlist_id, screen_id, position) VALUES (?, ?, ?)")
        .bind(id).bind(sid).bind(pos).execute(&st.pool).await.map_err(err)?;
    Ok(Json(json!({ "ok": true, "position": pos })))
}

/// Toggle visibility (Terminus playlist item visibility). item_id == screen_id.
pub async fn patch_playlist_item(State(st): State<SharedState>, Path((id, item_id)): Path<(i64, i64)>, Json(b): Json<Value>) -> Res {
    let visible = b.get("visible").and_then(|v| v.as_i64()).unwrap_or(1);
    let n = sqlx::query("UPDATE playlist_screens SET visible = ? WHERE playlist_id = ? AND screen_id = ?")
        .bind(visible).bind(id).bind(item_id).execute(&st.pool).await.map_err(err)?.rows_affected();
    if n == 0 { return Err(not_found("playlist item")); }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_playlist_item(State(st): State<SharedState>, Path((id, item_id)): Path<(i64, i64)>) -> Res {
    let n = sqlx::query("DELETE FROM playlist_screens WHERE playlist_id = ? AND screen_id = ?")
        .bind(id).bind(item_id).execute(&st.pool).await.map_err(err)?.rows_affected();
    if n == 0 { return Err(not_found("playlist item")); }
    Ok(Json(json!({ "ok": true })))
}

// ---------- firmware ----------

pub async fn list_firmware(State(st): State<SharedState>) -> Res {
    let rows: Vec<(i64, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, version, url, notes, created_at FROM firmware ORDER BY id DESC")
        .fetch_all(&st.pool).await.map_err(err)?;
    Ok(Json(json!(rows.into_iter().map(|(id, version, url, notes, created_at)| json!({
        "id": id, "version": version, "url": url, "notes": notes, "created_at": created_at
    })).collect::<Vec<_>>())))
}

pub async fn get_firmware(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let row: Option<(i64, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, version, url, notes, created_at FROM firmware WHERE id = ?")
        .bind(id).fetch_optional(&st.pool).await.map_err(err)?;
    match row {
        Some((id, version, url, notes, created_at)) => Ok(Json(json!({
            "id": id, "version": version, "url": url, "notes": notes, "created_at": created_at
        }))),
        None => Err(not_found("firmware")),
    }
}

pub async fn create_firmware(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let version = str_field(&b, "version").ok_or_else(|| bad("version required"))?;
    let url = str_field(&b, "url").ok_or_else(|| bad("url required"))?;
    let notes = str_field(&b, "notes").unwrap_or("");
    let id = sqlx::query("INSERT INTO firmware (version, url, notes, created_at) VALUES (?, ?, ?, datetime('now'))")
        .bind(version).bind(url).bind(notes).execute(&st.pool).await.map_err(err)?.last_insert_rowid();
    Ok(Json(json!({ "id": id })))
}

pub async fn patch_firmware(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    let n = patch_table(&st.pool, "firmware", id, &b, &["version", "url", "notes"]).await?;
    if n == 0 { return Err(not_found("firmware")); }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_firmware(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = sqlx::query("DELETE FROM firmware WHERE id = ?").bind(id).execute(&st.pool).await.map_err(err)?.rows_affected();
    if n == 0 { return Err(not_found("firmware")); }
    Ok(Json(json!({ "ok": true })))
}

/// Bulk wipe (Terminus `DELETE /bulk/firmware`).
pub async fn wipe_firmware(State(st): State<SharedState>) -> Res {
    let n = sqlx::query("DELETE FROM firmware").execute(&st.pool).await.map_err(err)?.rows_affected();
    Ok(Json(json!({ "deleted": n })))
}

// ---------- models ----------

pub async fn list_models(State(st): State<SharedState>) -> Res {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_object('id', id, 'name', name, 'label', label, 'width', width, 'height', height, 'bit_depth', bit_depth) FROM models ORDER BY id")
        .fetch_all(&st.pool).await.map_err(err)?;
    Ok(Json(json!(rows.iter().map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null)).collect::<Vec<_>>())))
}

pub async fn get_model(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT json_object('id', id, 'name', name, 'label', label, 'width', width, 'height', height, 'bit_depth', bit_depth) FROM models WHERE id = ?")
        .bind(id).fetch_optional(&st.pool).await.map_err(err)?;
    match row {
        Some((s,)) => Ok(Json(serde_json::from_str(&s).unwrap_or(Value::Null))),
        None => Err(not_found("model")),
    }
}

pub async fn create_model(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let name = str_field(&b, "name").ok_or_else(|| bad("name required"))?;
    let label = str_field(&b, "label").unwrap_or(name);
    let width = b.get("width").and_then(|v| v.as_i64()).unwrap_or(800);
    let height = b.get("height").and_then(|v| v.as_i64()).unwrap_or(480);
    let bit_depth = b.get("bit_depth").and_then(|v| v.as_i64()).unwrap_or(2);
    let id = sqlx::query("INSERT INTO models (name, label, width, height, bit_depth) VALUES (?, ?, ?, ?, ?)")
        .bind(name).bind(label).bind(width).bind(height).bind(bit_depth)
        .execute(&st.pool).await.map_err(err)?.last_insert_rowid();
    Ok(Json(json!({ "id": id })))
}

pub async fn patch_model(State(st): State<SharedState>, Path(id): Path<i64>, Json(b): Json<Value>) -> Res {
    let n = patch_table(&st.pool, "models", id, &b, &["name", "label", "width", "height", "bit_depth"]).await?;
    if n == 0 { return Err(not_found("model")); }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_model(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = sqlx::query("DELETE FROM models WHERE id = ?").bind(id).execute(&st.pool).await.map_err(err)?.rows_affected();
    if n == 0 { return Err(not_found("model")); }
    Ok(Json(json!({ "ok": true })))
}
