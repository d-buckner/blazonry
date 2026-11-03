// SPDX-License-Identifier: AGPL-3.0-only
//! Device plane: the endpoints TRMNL firmware actually calls.
//!
//! Wire contract (TRMNL BYOS docs + Terminus source, Oct 2026):
//!   GET /api/setup     headers: ID or idempotency-key = MAC, FW-Version, Model
//!        -> { status: 200, api_key, friendly_id, image_url, message }
//!   GET /api/display   headers: ID/idempotency-key, Access-Token, telemetry
//!        -> { status: 0, image_url, filename, refresh_rate (SECONDS, number),
//!             reset_firmware, update_firmware, firmware_url, special_function,
//!             maximum_compatibility, temperature_profile  (no inline image) }
//!        status 202 = not registered yet; unknown token -> 500 + reset_firmware
//!   POST /api/log      header: Access-Token; body {logs:[...]} -> 204
//!   GET /api/display/current  non-advancing mirror (BYOD/Chrome consumers)
//!
//! Rotation: per-device poll_count, modulo playlist; current_screen_id is
//! stored so /current can mirror without advancing.

use crate::cache::{self, BmpCache};
use crate::db;
use crate::{firmware, playlists};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use sqlx::SqlitePool;
use std::sync::Arc;

pub struct AppState {
    pub pool: SqlitePool,
    pub backends: crate::cache::Backends,
    pub cache: BmpCache,
    pub auth: Arc<crate::auth_core::AuthConfig>,
    pub pending_oidc: Arc<crate::auth_core::PendingOidc>,
}
pub type SharedState = Arc<AppState>;

/// Firmware sends `ID`; some BYOS clients use `idempotency-key`. Accept both.
fn device_mac(headers: &HeaderMap) -> Option<String> {
    headers
        .get("id")
        .or_else(|| headers.get("idempotency-key"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
}

fn header_f32(headers: &HeaderMap, name: &str) -> Option<f64> {
    header_str(headers, name).and_then(|s| s.parse().ok())
}

fn header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
    header_str(headers, name).and_then(|s| s.parse().ok())
}

/// Short stable content hash for filenames (FNV-1a 32-bit, hex).
fn content_hash(bytes: &[u8]) -> String {
    let mut h: u32 = 2166136261;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    format!("{h:08x}")
}

/// Advance the device's rotation and return (screen_id, poll).
async fn advance_device(st: &SharedState, device_id: i64, playlist_id: Option<i64>) -> Option<(i64, i64)> {
    let pl = playlist_id?;
    let poll = db::advance_poll(&st.pool, device_id).await.ok()?;
    let screen_id = playlists::next_screen_id(&st.pool, pl, poll as u64).await.ok().flatten()?;
    let _ = db::set_current_screen(&st.pool, device_id, screen_id).await;
    Some((screen_id, poll))
}

/// Bits-per-pixel the device's panel wants: OG (TRMNL 1) is 1bpp (firmware
/// hard-checks 48062-byte BMPs); X and newer accept 2bpp greyscale.
fn device_bpp(headers: &HeaderMap) -> u16 {
    match header_str(headers, "model").unwrap_or_default().to_ascii_lowercase().as_str() {
        m if m.contains("x") => 2,
        _ => 1,
    }
}

/// Absolute origin for image URLs, from the request's Host header.
/// The firmware feeds image_url straight into HTTPClient::begin(), which
/// rejects relative URLs — BYOS spec examples are absolute too.
fn base_url(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| format!("http://{h}"))
        .unwrap_or_default()
}

/// Shared payload builder for display + display/current.
async fn screen_payload(st: &SharedState, screen_id: i64, refresh_secs: i64, base: &str, bpp: u16) -> Option<serde_json::Value> {
    let bmp = cache::cached_bmp(&st.pool, &st.backends, &st.cache, screen_id, bpp).await.ok()?;
    let hash = content_hash(&bmp);
    // bpp lives in the filename: the firmware's image GET sends no headers
    let filename = format!("screen-{screen_id}-{hash}-{bpp}");
    // No inline base64 `image`: a 48-96KB string overflows the OG firmware's
    // heap (max_alloc ~63KB) and ArduinoJson fails to parse the response.
    // Official trmnl.app sends image_url only; the device downloads the BMP.
    let _ = &bmp;
    Some(serde_json::json!({
        "status": 0,
        "image_url": format!("{base}/images/{filename}.bmp"),
        "filename": filename,
        "refresh_rate": refresh_secs,
        "next_refresh_in": refresh_secs,
        "refresh_count": 0,
        "reset_firmware": false,
        "update_firmware": false,
        "firmware_url": null,
        "special_function": "none",
        "maximum_compatibility": false,
        "temperature_profile": "default",
    }))
}

pub async fn display(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(mac) = device_mac(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing ID header").into_response();
    };
    let token = header_str(&headers, "access-token").unwrap_or_default();
    let (id, api_key, playlist_id, refresh_interval) = match db::upsert_device(&st.pool, &mac).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Hosted TRMNL answers an unknown token with reset_firmware:true so the
    // device wipes and re-pairs; mirror that (docs warn to double-check keys).
    if !token.is_empty() && token != api_key {
        return axum::Json(serde_json::json!({
            "status": 500, "error": "Device not found", "reset_firmware": true,
        }))
        .into_response();
    }
    // Telemetry: store whatever the firmware reported.
    let _ = db::record_telemetry(
        &st.pool, id,
        header_str(&headers, "fw-version").as_deref(),
        header_i64(&headers, "rssi"),
        header_f32(&headers, "battery-voltage"),
        header_i64(&headers, "width"),
        header_i64(&headers, "height"),
    ).await;

    let (screen_id, _poll) = match advance_device(&st, id, playlist_id).await {
        Some(v) => v,
        None => {
            // Not registered to a playlist yet: firmware re-polls in seconds.
            return axum::Json(serde_json::json!({
                "status": 202,
                "message": "blazonry: device paired, no screens yet",
                "refresh_rate": refresh_interval,
                "reset_firmware": false,
                "update_firmware": false,
            }))
            .into_response();
        }
    };
    match screen_payload(&st, screen_id, refresh_interval, &base_url(&headers), device_bpp(&headers)).await {
        Some(p) => axum::Json(p).into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response(),
    }
}

/// Non-advancing mirror of the device's current screen (BYOD / extension use).
pub async fn display_current(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(mac) = device_mac(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing ID header").into_response();
    };
    let token = header_str(&headers, "access-token").unwrap_or_default();
    let row = db::device_state_by_mac(&st.pool, &mac).await.unwrap_or(None);
    let Some((api_key, mut current, refresh, playlist_id)) = row else {
        return axum::Json(serde_json::json!({ "status": 404, "message": "No screen currently being displayed" })).into_response();
    };
    if !token.is_empty() && token != api_key {
        return axum::Json(serde_json::json!({
            "status": 500, "error": "Device not found", "reset_firmware": true,
        }))
        .into_response();
    }
    // If never polled, peek at the playlist head without advancing.
    if current.is_none()
        && let Some(pl) = playlist_id {
            current = playlists::next_screen_id(&st.pool, pl, 0).await.ok().flatten();
        }
    match current {
        Some(sid) => match screen_payload(&st, sid, refresh, &base_url(&headers), device_bpp(&headers)).await {
            Some(mut p) => {
                // current-screen contract answers status 200, no advance
                p["status"] = serde_json::json!(200);
                axum::Json(p).into_response()
            }
            None => (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response(),
        },
        None => axum::Json(serde_json::json!({ "status": 404, "message": "No screen currently being displayed" })).into_response(),
    }
}

/// Serve a cached rendered BMP by filename (`screen-{id}-{hash}.bmp`).
pub async fn serve_image(
    State(st): State<SharedState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let Some(rest) = name.strip_suffix(".bmp") else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    // trailing "-{bpp}" segment selects the encoding (no headers on image GETs)
    let Some((stem, bpp_str)) = rest.rsplit_once('-') else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Ok(bpp) = bpp_str.parse::<u16>() else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if stem == "setup-logo" {
        let tmpl = "{\"name\":\"setup\",\"parts\":[{\"type\":\"text\",\"text\":\"blazonry\",\"style\":{\"font_px\":64,\"bold\":true},\"align\":\"center\",\"padding\":[180,200]}]}";
        let native = st.backends.for_pref("native");
        return match native.render_bmp(tmpl, "{}", bpp).await {
            Ok(bmp) => ([(axum::http::header::CONTENT_TYPE, "image/bmp")], bmp).into_response(),
            Err(_) => (StatusCode::NOT_FOUND, "render failed").into_response(),
        };
    }
    let Some(id_str) = stem.strip_prefix("screen-").and_then(|r| r.split('-').next()) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let Ok(screen_id) = id_str.parse::<i64>() else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match cache::cached_bmp(&st.pool, &st.backends, &st.cache, screen_id, bpp).await {
        Ok(bmp) => ([(axum::http::header::CONTENT_TYPE, "image/bmp")], bmp.to_vec()).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub async fn setup(State(st): State<SharedState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(mac) = device_mac(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing ID header").into_response();
    };
    let (id, api_key, _, refresh_interval) = match db::upsert_device(&st.pool, &mac).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let _ = db::record_setup(&st.pool, id, header_str(&headers, "fw-version").as_deref(), header_str(&headers, "model").as_deref()).await;
    tracing::info!(id, %mac, "device setup/pair");
    axum::Json(serde_json::json!({
        "status": 200,
        "message": "Welcome to blazonry!",
        "api_key": api_key,
        "friendly_id": format!("{:06}", id % 1_000_000),
        "image_url": format!("{}/images/setup-logo-{}.bmp", base_url(&headers), device_bpp(&headers)),
        "refresh_rate": refresh_interval,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct LogQ {
    #[serde(default)]
    pub messages: Option<String>,
}

pub async fn logs(headers: HeaderMap, Query(q): Query<LogQ>) -> impl IntoResponse {
    let mac = device_mac(&headers).unwrap_or_else(|| "?".into());
    tracing::info!(%mac, messages = q.messages.as_deref().unwrap_or(""), "device log (legacy GET)");
    axum::Json(serde_json::json!({ "status": 200 })).into_response()
}

/// POST /api/log — the real firmware contract: JSON array under `logs`,
/// device identified by Access-Token, 204 on accept (Terminus parity).
pub async fn log_create(
    State(st): State<SharedState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> impl IntoResponse {
    let token = header_str(&headers, "access-token").unwrap_or_default();
    let device_id = db::device_id_by_token(&st.pool, &token).await.unwrap_or(None);
    let Some(device_id) = device_id else {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({
            "type": "/problem_details#device_id", "status": 404,
            "detail": "invalid device id", "instance": "/api/log",
        }))).into_response();
    };
    let Some(entries) = body.get("logs").and_then(|v| v.as_array()) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(serde_json::json!({
            "type": "/problem_details#log_payload", "status": 422,
            "detail": "invalid payload", "instance": "/api/log",
        }))).into_response();
    };
    for e in entries {
        let Some(message) = e.get("message").and_then(|v| v.as_str()) else { continue };
        let _ = db::insert_device_log(&st.pool, device_id, message, e.get("level").and_then(|v| v.as_str()), e.get("source_path").and_then(|v| v.as_str())).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

// ---- minimal admin API (auth-gated in the router; see auth::require_admin) ----

pub async fn create_screen(
    State(st): State<SharedState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let name = body.get("name").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "name required".into()))?;
    let template = body.get("template").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "template required".into()))?;
    // vars may arrive as a JSON object or as a JSON-encoded string (the SPA's
    // textarea); store the raw JSON text either way, validated.
    let vars = match body.get("vars") {
        Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
        Some(v) => v.to_string(),
        None => "{}".into(),
    };
    serde_json::from_str::<serde_json::Value>(&vars)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("vars must be valid JSON: {e}")))?;
    // Compile the screen now — a saved screen must always render (#21).
    validate_screen(template, &vars)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let id = crate::screens::create(&st.pool, name, template, &vars).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    cache::invalidate(&st.cache, id).await;
    Ok(axum::Json(serde_json::json!({ "id": id })))
}

/// Compile a screen (Liquid template + vars JSON text) without persisting.
/// Returns the human-readable error on failure; bad state must not be
/// storable (#21 — APoSD: define errors out of existence downstream).
pub fn validate_screen(template: &str, vars: &str) -> Result<(), String> {
    let vars: serde_json::Value = serde_json::from_str(vars)
        .map_err(|e| format!("vars must be valid JSON: {e}"))?;
    crate::template::render_screen(template, &vars)
        .map(|_| ())
        .map_err(|e| format!("template does not compile: {e}"))
}

/// Draft preview: render a screen from the request body without saving it.
/// Same body shape as POST /admin/screens. Returns PNG (issue #21).
pub async fn preview_draft(
    State(st): State<SharedState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let template = body.get("template").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "template required".into()))?;
    let vars = match body.get("vars") {
        Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
        Some(v) => v.to_string(),
        None => "{}".into(),
    };
    validate_screen(template, &vars).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let pref = body.get("render_backend").and_then(|v| v.as_str()).unwrap_or("");
    let backend = st.backends.for_pref(pref);
    let bmp = cache::render_bmp(backend.as_ref(), template, &vars, 2).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let png = bmp_to_png(&bmp).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(([(axum::http::header::CONTENT_TYPE, "image/png")], png).into_response())
}

/// Render a screen by id and return it as a PNG (browser-viewable preview).
pub async fn preview_screen(
    State(st): State<SharedState>,
    Path(id): Path<i64>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let bmp = cache::cached_bmp(&st.pool, &st.backends, &st.cache, id, 2).await
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
    let row = w.div_ceil(4);
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

/// Firmware list for devices: newest row wins. `update_firmware` is only true
/// when a device is behind (Terminus: version mismatch gates the OTA flag).
pub async fn firmware_latest(
    State(st): State<SharedState>,
    headers: HeaderMap,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let row = firmware::latest(&st.pool).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let device_fw = header_str(&headers, "fw-version");
    Ok(axum::Json(match row {
        Some((version, url, notes)) => {
            let update = device_fw.as_deref().map(|v| v != version).unwrap_or(false);
            serde_json::json!({
                "status": 200, "version": version, "firmware_url": url, "notes": notes,
                "update_firmware": update,
            })
        }
        None => serde_json::json!({ "status": 200, "update_firmware": false }),
    }))
}

pub async fn create_playlist(
    State(st): State<SharedState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let name = body.get("name").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "name required".into()))?;
    let ids: Vec<i64> = body.get("screen_ids").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
    let pid = playlists::create(&st.pool, name, &ids).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(axum::Json(serde_json::json!({ "id": pid })))
}

pub async fn assign_device(
    State(st): State<SharedState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let mac = body.get("mac").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "mac required".into()))?;
    let playlist_id = body.get("playlist_id").and_then(|v| v.as_i64()).ok_or((StatusCode::BAD_REQUEST, "playlist_id required".into()))?;
    let n = db::set_device_playlist_by_mac(&st.pool, mac, playlist_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if n == 0 { return Err((StatusCode::NOT_FOUND, "unknown mac".into())); }
    Ok(axum::Json(serde_json::json!({ "ok": true })))
}
