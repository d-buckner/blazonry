//! Admin auth: argon2id password hashing + opaque session tokens.
//! First registered user becomes admin (Terminus parity); everyone after is
//! unverified until an admin verifies them. Sessions are DB rows so a restart
//! doesn't log everyone out.

use argon2::Argon2;
use password_hash::{PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use sqlx::SqlitePool;
use std::io::Read;
use std::sync::Arc;

use crate::api::SharedState;

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    let hash = Argon2::default().hash_password(pw.as_bytes())?;
    Ok(hash.to_string())
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    // /dev/urandom is the zero-dependency CSPRNG on Linux; this targets LAN boxes.
    std::fs::File::open("/dev/urandom").expect("/dev/urandom")
        .read_exact(&mut bytes).expect("urandom read");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn insert_session(pool: &SqlitePool, user_id: i64) -> anyhow::Result<String> {
    let token = random_token();
    sqlx::query("INSERT INTO sessions (token, user_id, expires_at) VALUES (?, ?, datetime('now', '+30 days'))")
        .bind(&token).bind(user_id)
        .execute(pool).await?;
    Ok(token)
}

pub async fn register(State(st): State<SharedState>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let email = body.get("email").and_then(|v| v.as_str()).ok_or((StatusCode::BAD_REQUEST, "email required".into()))?;
    let pw = body.get("password").and_then(|v| v.as_str()).filter(|p| p.len() >= 8).ok_or((StatusCode::BAD_REQUEST, "password >= 8 chars required".into()))?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let role = if count == 0 { "admin" } else { "unverified" };
    let hash = hash_password(pw).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let id = sqlx::query("INSERT INTO users (email, password_hash, role) VALUES (?, ?, ?)")
        .bind(email).bind(&hash).bind(role)
        .execute(&st.pool).await
        .map_err(|e| if e.as_database_error().map(|d| d.is_unique_violation()).unwrap_or(false) {
            (StatusCode::CONFLICT, "email taken".into())
        } else { (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()) })?
        .last_insert_rowid();
    let token = insert_session(&st.pool, id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "id": id, "role": role, "token": token })))
}

pub async fn login(State(st): State<SharedState>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("");
    let pw = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
    let row: Option<(i64, String, String)> = sqlx::query_as("SELECT id, password_hash, role FROM users WHERE email = ?")
        .bind(email).fetch_optional(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (id, hash, role) = row.ok_or((StatusCode::UNAUTHORIZED, "bad credentials".to_string()))?;
    if role == "unverified" {
        return Err((StatusCode::FORBIDDEN, "awaiting admin verification".into()));
    }
    if !verify_password(pw, &hash) {
        return Err((StatusCode::UNAUTHORIZED, "bad credentials".into()));
    }
    let token = insert_session(&st.pool, id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "id": id, "role": role, "token": token })))
}

pub async fn verify_user(State(st): State<SharedState>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let id = body.get("user_id").and_then(|v| v.as_i64()).ok_or((StatusCode::BAD_REQUEST, "user_id required".into()))?;
    sqlx::query("UPDATE users SET role = 'user' WHERE id = ? AND role = 'unverified'")
        .bind(id).execute(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Who am I (the SPA's boot check): email + role from the bearer token.
pub async fn me(State(st): State<SharedState>, req: axum::http::Request<axum::body::Body>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let token = req.headers().get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT u.email, u.role FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token = ? AND s.expires_at > datetime('now') AND u.role IN ('admin','user')")
        .bind(token).fetch_optional(&st.pool).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (email, role) = row.ok_or((StatusCode::UNAUTHORIZED, "invalid or expired session".to_string()))?;
    Ok(Json(serde_json::json!({ "email": email, "role": role })))
}

/// User list for the admin verification UI.
pub async fn list_users(State(st): State<SharedState>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as("SELECT id, email, role FROM users ORDER BY id")
        .fetch_all(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!(rows.into_iter().map(|(id, email, role)| serde_json::json!({
        "id": id, "email": email, "role": role
    })).collect::<Vec<_>>())))
}

/// Admin middleware: requires `Authorization: Bearer <session token>` for a
/// user whose role is admin or user. 401 otherwise.
pub async fn require_admin(State(st): State<SharedState>, req: Request, next: Next) -> Result<Response, (StatusCode, String)> {
    let token = req.headers().get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if token.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "missing bearer token".into()));
    }
    let ok: Option<i64> = sqlx::query_scalar(
        "SELECT s.user_id FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token = ? AND s.expires_at > datetime('now') AND u.role IN ('admin','user')")
        .bind(token).fetch_optional(&st.pool).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match ok {
        Some(_) => Ok(next.run(req).await),
        None => Err((StatusCode::UNAUTHORIZED, "invalid or expired session".into())),
    }
}

/// One-time bootstrap helper used by tests/CLI: create admin directly.
pub struct AuthBootstrap;
pub type Shared = Arc<AuthBootstrap>;
