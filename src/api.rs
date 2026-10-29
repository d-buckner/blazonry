//! Device plane: the endpoints TRMNL firmware actually calls.
//!
//! Wire contract (from firmware + Terminus compat docs):
//!   GET /api/display   headers: idempotency-key = device MAC, access-token
//!        -> { status, image: <base64 2-bit BMP>, filename, refresh_rate,
//!             next_refresh_in, image_url, ... }
//!   GET /api/logs      header: idempotency-key -> device log lines
//!   GET /api/setup     same headers; registration handshake
//!
//! Auth model: MAC auto-pairs on first contact (LAN trust), access-token is
//! checked once paired. Rotation: poll counter per device, modulo playlist.

use crate::cache::{self, BmpCache};
use crate::db;
use crate::render::Renderer;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use base64::Engine as _;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct AppState {
    pub pool: SqlitePool,
    pub renderer: Arc<Renderer>,
    pub cache: BmpCache,
    /// monotonic poll counter (rotation driver)
    pub polls: AtomicU64,
}
pub type SharedState = Arc<AppState>;

fn headers_ids(headers: &HeaderMap) -> Option<(String, String)> {
    let mac = headers.get("idempotency-key")?.to_str().ok()?.to_string();
    let token = headers.get("access-token").map(|v| v.to_str().unwrap_or("").to_string()).unwrap_or_default();
    Some((mac, token))
}

pub async fn display(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some((mac, token)) = headers_ids(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing idempotency-key").into_response();
    };
    let (id, api_key, playlist_id, refresh_interval) = match db::upsert_device(&st.pool, &mac).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if !token.is_empty() && token != api_key {
        return (StatusCode::UNAUTHORIZED, "bad access-token").into_response();
    }
    let _ = sqlx::query("UPDATE devices SET last_poll_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&st.pool)
        .await;

    let poll = st.polls.fetch_add(1, Ordering::Relaxed);
    let screen_id = match playlist_id {
        Some(pl) => db::next_screen_id(&st.pool, pl, poll).await.ok().flatten(),
        None => None,
    };
    let screen_id = match screen_id {
        Some(s) => s,
        None => {
            // No playlist/screen yet: send a friendly pairing screen.
            return axum::Json(serde_json::json!({
                "status": 200,
                "message": "terminus-rs: device paired, no screens yet",
                "refresh_rate": refresh_interval * 1000,
                "next_refresh_in": refresh_interval,
            }))
            .into_response();
        }
    };

    let rendered = match render_for_device(&st, screen_id).await {
        Ok(bmp_b64) => bmp_b64,
        Err(e) => {
            tracing::warn!(%e, screen = screen_id, "render failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response();
        }
    };
    axum::Json(serde_json::json!({
        "status": 200,
        "image": rendered,
        "filename": format!("terminus-{}.bmp", poll),
        "refresh_rate": refresh_interval * 1000,
        "refresh_count": 0,
        "next_refresh_in": refresh_interval,
        "image_url": null,
        "partial": null,
    }))
    .into_response()
}

async fn render_for_device(st: &SharedState, screen_id: i64) -> anyhow::Result<String> {
    let bmp = cache::cached_bmp(&st.pool, &st.renderer, &st.cache, screen_id).await?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&*bmp))
}

pub async fn setup(State(st): State<SharedState>, headers: HeaderMap) -> impl IntoResponse {
    let Some((mac, _)) = headers_ids(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing idempotency-key").into_response();
    };
    let (id, api_key, _, refresh_interval) = match db::upsert_device(&st.pool, &mac).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    tracing::info!(id, %mac, "device setup/pair");
    axum::Json(serde_json::json!({
        "status": 200,
        "message": "terminus-rs: paired",
        "api_key": api_key,
        "friendly_id": format!("{:06}", id % 1_000_000),
        "refresh_rate": refresh_interval * 1000,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct LogQ {
    #[serde(default)]
    pub messages: Option<String>,
}

pub async fn logs(headers: HeaderMap, Query(q): Query<LogQ>) -> impl IntoResponse {
    let mac = headers.get("idempotency-key").and_then(|v| v.to_str().ok()).unwrap_or("?");
    tracing::info!(%mac, messages = q.messages.as_deref().unwrap_or(""), "device log");
    axum::Json(serde_json::json!({ "status": 200 })).into_response()
}

// ---- minimal admin API (auth-gated in the router; see auth::require_admin) ----

pub async fn create_screen(
    State(st): State<SharedState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let name = body.get("name").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "name required".into()))?;
    let template = body.get("template").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "template required".into()))?;
    let vars = body.get("vars").map(|v| v.to_string()).unwrap_or_else(|| "{}".into());
    let id = sqlx::query("INSERT INTO screens (name, template, vars) VALUES (?, ?, ?)")
        .bind(name).bind(template).bind(&vars)
        .execute(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .last_insert_rowid();
    cache::invalidate(&st.cache, id).await;
    Ok(axum::Json(serde_json::json!({ "id": id })))
}

/// Render a screen by id and return it as a PNG (browser-viewable preview).
pub async fn preview_screen(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let bmp = cache::cached_bmp(&st.pool, &st.renderer, &st.cache, id).await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let png = bmp_to_png(&bmp).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(([(axum::http::header::CONTENT_TYPE, "image/png")], png).into_response())
}

/// Decode our own 2-bit BMP into a PNG (used by preview + tests).
pub fn bmp_to_png(bmp: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(&bmp[0..2] == b"BM", "not a BMP");
    let off = u32::from_le_bytes(bmp[10..14].try_into()?) as usize;
    let w = i32::from_le_bytes(bmp[18..22].try_into()?) as usize;
    let h = i32::from_le_bytes(bmp[22..26].try_into()?) as usize;
    let row = (w + 3) / 4;
    let pal: Vec<u8> = (0..4).map(|i| bmp[54 + 4 * i]).collect();
    let mut img = image::GrayImage::new(w as u32, h as u32);
    for y in 0..h {
        let r = &bmp[off + y * row..off + (y + 1) * row];
        for x in 0..w {
            let idx = (r[x / 4] >> (6 - 2 * (x % 4))) & 3;
            img.put_pixel(x as u32, (h - 1 - y) as u32, image::Luma([pal[idx as usize]]));
        }
    }
    let mut out = Vec::new();
    {
        use image::ImageEncoder;
        let encoder = image::codecs::png::PngEncoder::new(&mut out);
        encoder.write_image(img.as_raw(), w as u32, h as u32, image::ExtendedColorType::L8)?;
    }
    Ok(out)
}

/// Firmware list for devices: newest row wins.
pub async fn firmware_latest(
    State(st): State<SharedState>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT version, url, notes FROM firmware ORDER BY id DESC LIMIT 1")
        .fetch_optional(&st.pool).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(axum::Json(match row {
        Some((version, url, notes)) => serde_json::json!({
            "status": 200, "version": version, "firmware_url": url, "notes": notes,
            "update_firmware": true,
        }),
        None => serde_json::json!({ "status": 200, "update_firmware": false }),
    }))
}

pub async fn create_playlist(
    State(st): State<SharedState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let name = body.get("name").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "name required".into()))?;
    let ids: Vec<i64> = body.get("screen_ids").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
    let pid = sqlx::query("INSERT INTO playlists (name) VALUES (?)")
        .bind(name).execute(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .last_insert_rowid();
    for (pos, sid) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO playlist_screens (playlist_id, screen_id, position) VALUES (?, ?, ?)")
            .bind(pid).bind(*sid).bind(pos as i64)
            .execute(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(axum::Json(serde_json::json!({ "id": pid })))
}

pub async fn assign_device(
    State(st): State<SharedState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let mac = body.get("mac").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "mac required".into()))?;
    let playlist_id = body.get("playlist_id").and_then(|v| v.as_i64()).ok_or((StatusCode::BAD_REQUEST, "playlist_id required".into()))?;
    let n = sqlx::query("UPDATE devices SET playlist_id = ? WHERE mac = ?")
        .bind(playlist_id).bind(mac)
        .execute(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .rows_affected();
    if n == 0 { return Err((StatusCode::NOT_FOUND, "unknown mac".into())); }
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}

pub async fn list_devices(
    State(st): State<SharedState>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let rows: Vec<(i64, String, Option<String>, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT id, mac, name, playlist_id, last_poll_at FROM devices ORDER BY id",
    )
    .fetch_all(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let out: Vec<_> = rows.into_iter().map(|(id, mac, name, pl, poll)| serde_json::json!({
        "id": id, "mac": mac, "name": name, "playlist_id": pl, "last_poll_at": poll
    })).collect();
    Ok(axum::Json(serde_json::json!(out)))
}
