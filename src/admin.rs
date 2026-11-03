// SPDX-License-Identifier: AGPL-3.0-only
//! Admin CRUD plane — Terminus `/api/*` parity (their config/routes.rb,
//! verified Oct 2026). Terminus ids are UUIDs; we use INTEGER rowids
//! (documented deviation). Everything here is auth-gated in the router.
//!
//! This module handles HTTP: parse request, call the table-owner module
//! (db/screens/playlists/firmware/models — see the ownership map in db.rs),
//! map errors to status codes. No raw SQL here (issue #9).
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
use crate::{cache, db, firmware, models, playlists, screens};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

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

// ---------- devices ----------

pub async fn list_devices(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(
        db::device_list_json(&st.pool).await.map_err(err)?
    )))
}

pub async fn get_device(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    match db::device_json(&st.pool, id).await.map_err(err)? {
        Some(v) => Ok(Json(v)),
        None => Err(not_found("device")),
    }
}

pub async fn patch_device(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    // playlist_id may be explicitly null (unassign)
    let n = db::patch_table(
        &st.pool,
        "devices",
        id,
        &b,
        &["name", "playlist_id", "refresh_interval", "model"],
    )
    .await
    .map_err(err)?;
    if n == 0 {
        return Err(not_found("device"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_device(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = db::delete_device(&st.pool, id).await.map_err(err)?;
    if n == 0 {
        return Err(not_found("device"));
    }
    Ok(Json(json!({ "ok": true })))
}

/// Bulk import: {devices: [{mac, name?, playlist_id?}]} — upsert by mac.
pub async fn import_devices(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let arr = b
        .get("devices")
        .and_then(|v| v.as_array())
        .ok_or_else(|| bad("devices array required"))?;
    let mut imported = 0;
    for d in arr {
        let Some(mac) = str_field(d, "mac") else {
            continue;
        };
        let (_id, key, _, _) = crate::db::upsert_device(&st.pool, mac)
            .await
            .map_err(|e| err(sqlx::Error::Protocol(e.to_string())))?;
        let _ = key;
        if let Some(name) = str_field(d, "name") {
            let _ = db::set_device_name_by_mac(&st.pool, mac, name).await;
        }
        if let Some(pl) = d.get("playlist_id").and_then(|v| v.as_i64()) {
            let _ = db::set_device_playlist_by_mac(&st.pool, mac, pl).await;
        }
        imported += 1;
    }
    Ok(Json(json!({ "imported": imported })))
}

pub async fn wipe_device_logs(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = db::wipe_device_logs(&st.pool, id).await.map_err(err)?;
    Ok(Json(json!({ "deleted": n })))
}

// ---------- screens ----------

pub async fn list_screens(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(
        screens::list_json(&st.pool).await.map_err(err)?
    )))
}

pub async fn get_screen(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    match screens::get_json(&st.pool, id).await.map_err(err)? {
        Some(v) => Ok(Json(v)),
        None => Err(not_found("screen")),
    }
}

pub async fn patch_screen(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    // Validate the post-patch state: if template or vars change, the screen
    // must still compile (#21).
    if b.get("template").is_some() || b.get("vars").is_some() {
        let cur = screens::get_json(&st.pool, id)
            .await
            .map_err(err)?
            .ok_or_else(|| not_found("screen"))?;
        let template = b
            .get("template")
            .and_then(|v| v.as_str())
            .or_else(|| cur["template"].as_str())
            .unwrap_or("");
        let vars = match b.get("vars") {
            Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
            Some(v) => v.to_string(),
            None => cur["vars"].as_str().unwrap_or("{}").to_string(),
        };
        crate::api::validate_screen(template, &vars).map_err(bad)?;
    }
    let n = screens::patch(
        &st.pool,
        id,
        &b,
        &["name", "template", "vars", "render_backend"],
    )
    .await
    .map_err(err)?;
    if n == 0 {
        return Err(not_found("screen"));
    }
    cache::invalidate(&st.cache, id).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_screen(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = screens::delete(&st.pool, id).await.map_err(err)?;
    if n == 0 {
        return Err(not_found("screen"));
    }
    cache::invalidate(&st.cache, id).await;
    Ok(Json(json!({ "ok": true })))
}

// ---------- playlists ----------

pub async fn list_playlists(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(
        playlists::list_json(&st.pool).await.map_err(err)?
    )))
}

pub async fn get_playlist(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    if !playlists::exists(&st.pool, id).await.map_err(err)? {
        return Err(not_found("playlist"));
    }
    let items = playlists::items_json(&st.pool, id).await.map_err(err)?;
    let name = playlists::name(&st.pool, id).await.unwrap_or_default();
    Ok(Json(json!({ "id": id, "name": name, "items": items })))
}

pub async fn patch_playlist(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    if let Some(name) = str_field(&b, "name") {
        let n = playlists::set_name(&st.pool, id, name).await.map_err(err)?;
        if n == 0 {
            return Err(not_found("playlist"));
        }
    }
    if let Some(ids) = b.get("screen_ids").and_then(|v| v.as_array()) {
        let ids: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
        playlists::set_screen_ids(&st.pool, id, &ids)
            .await
            .map_err(err)?;
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_playlist(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = playlists::delete(&st.pool, id).await.map_err(err)?;
    if n == 0 {
        return Err(not_found("playlist"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn add_playlist_item(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    let sid = b
        .get("screen_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| bad("screen_id required"))?;
    if !playlists::exists(&st.pool, id).await.map_err(err)? {
        return Err(not_found("playlist"));
    }
    let pos = playlists::add_item(&st.pool, id, sid).await.map_err(err)?;
    Ok(Json(json!({ "ok": true, "position": pos })))
}

/// Toggle visibility (Terminus playlist item visibility). item_id == screen_id.
pub async fn patch_playlist_item(
    State(st): State<SharedState>,
    Path((id, item_id)): Path<(i64, i64)>,
    Json(b): Json<Value>,
) -> Res {
    let visible = b.get("visible").and_then(|v| v.as_i64()).unwrap_or(1);
    let n = playlists::set_item_visible(&st.pool, id, item_id, visible)
        .await
        .map_err(err)?;
    if n == 0 {
        return Err(not_found("playlist item"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_playlist_item(
    State(st): State<SharedState>,
    Path((id, item_id)): Path<(i64, i64)>,
) -> Res {
    let n = playlists::delete_item(&st.pool, id, item_id)
        .await
        .map_err(err)?;
    if n == 0 {
        return Err(not_found("playlist item"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ---------- firmware ----------

pub async fn list_firmware(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(firmware::list(&st.pool).await.map_err(err)?)))
}

pub async fn get_firmware(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    match firmware::get(&st.pool, id).await.map_err(err)? {
        Some(v) => Ok(Json(v)),
        None => Err(not_found("firmware")),
    }
}

pub async fn create_firmware(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let version = str_field(&b, "version").ok_or_else(|| bad("version required"))?;
    let url = str_field(&b, "url").ok_or_else(|| bad("url required"))?;
    let notes = str_field(&b, "notes").unwrap_or("");
    let id = firmware::create(&st.pool, version, url, notes)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "id": id })))
}

pub async fn patch_firmware(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    let n = firmware::patch(&st.pool, id, &b).await.map_err(err)?;
    if n == 0 {
        return Err(not_found("firmware"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_firmware(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = firmware::delete(&st.pool, id).await.map_err(err)?;
    if n == 0 {
        return Err(not_found("firmware"));
    }
    Ok(Json(json!({ "ok": true })))
}

/// Bulk wipe (Terminus `DELETE /bulk/firmware`).
pub async fn wipe_firmware(State(st): State<SharedState>) -> Res {
    let n = firmware::wipe(&st.pool).await.map_err(err)?;
    Ok(Json(json!({ "deleted": n })))
}

// ---------- models ----------

pub async fn list_models(State(st): State<SharedState>) -> Res {
    Ok(Json(json!(models::list(&st.pool).await.map_err(err)?)))
}

pub async fn get_model(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    match models::get(&st.pool, id).await.map_err(err)? {
        Some(v) => Ok(Json(v)),
        None => Err(not_found("model")),
    }
}

pub async fn create_model(State(st): State<SharedState>, Json(b): Json<Value>) -> Res {
    let name = str_field(&b, "name").ok_or_else(|| bad("name required"))?;
    let label = str_field(&b, "label").unwrap_or(name);
    let width = b.get("width").and_then(|v| v.as_i64()).unwrap_or(800);
    let height = b.get("height").and_then(|v| v.as_i64()).unwrap_or(480);
    let bit_depth = b.get("bit_depth").and_then(|v| v.as_i64()).unwrap_or(2);
    let id = models::create(&st.pool, name, label, width, height, bit_depth)
        .await
        .map_err(err)?;
    Ok(Json(json!({ "id": id })))
}

pub async fn patch_model(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
    Json(b): Json<Value>,
) -> Res {
    let n = models::patch(&st.pool, id, &b).await.map_err(err)?;
    if n == 0 {
        return Err(not_found("model"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_model(State(st): State<SharedState>, Path(id): Path<i64>) -> Res {
    let n = models::delete(&st.pool, id).await.map_err(err)?;
    if n == 0 {
        return Err(not_found("model"));
    }
    Ok(Json(json!({ "ok": true })))
}
