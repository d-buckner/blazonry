// SPDX-License-Identifier: AGPL-3.0-only
//! Admin auth: argon2id password hashing + opaque session tokens.
//! First registered user becomes admin (Terminus parity); everyone after is
//! unverified until an admin verifies them. Sessions are DB rows so a restart
//! doesn't log everyone out.

use argon2::Argon2;
use password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use axum::response::IntoResponse;
use sqlx::SqlitePool;

use crate::api::SharedState;
use crate::auth_core::AuthMode;

/// Public: which auth mode is this server in (SPA adapts its login screen).
/// `first_run` = no local users yet: the SPA leads with account creation.
pub async fn auth_mode(State(st): State<SharedState>) -> axum::Json<serde_json::Value> {
    let first_run = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(&st.pool)
        .await
        .map(|n| n == 0)
        .unwrap_or(false);
    axum::Json(serde_json::json!({ "mode": st.auth.mode_str(), "first_run": first_run }))
}

/// Find or provision a user row for an externally-authenticated identity
/// (LDAP bind / OIDC callback). First user is admin; `force_admin` (OIDC
/// admin_emails) also grants admin. password_hash is set to an unusable
/// marker so these accounts can never log in locally.
pub async fn upsert_external_user(pool: &SqlitePool, email: &str, force_admin: bool) -> anyhow::Result<(i64, String)> {
    let existing: Option<(i64, String)> = sqlx::query_as("SELECT id, role FROM users WHERE email = ?")
        .bind(email).fetch_optional(pool).await?;
    if let Some((id, role)) = existing {
        if force_admin && role != "admin" {
            sqlx::query("UPDATE users SET role = 'admin' WHERE id = ?").bind(id).execute(pool).await?;
            return Ok((id, "admin".into()));
        }
        // demote unverified -> user: external auth already proves identity
        if role == "unverified" {
            sqlx::query("UPDATE users SET role = 'user' WHERE id = ?").bind(id).execute(pool).await?;
            return Ok((id, "user".into()));
        }
        return Ok((id, role));
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(pool).await?;
    let role = if count == 0 || force_admin { "admin" } else { "user" };
    let id = sqlx::query("INSERT INTO users (email, password_hash, role) VALUES (?, ?, ?)")
        .bind(email).bind("!external!").bind(role)
        .execute(pool).await?
        .last_insert_rowid();
    Ok((id, role.to_string()))
}

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

async fn insert_session(pool: &SqlitePool, user_id: i64) -> anyhow::Result<String> {
    let token = crate::auth_core::random_hex(32);
    sqlx::query("INSERT INTO sessions (token, user_id, expires_at) VALUES (?, ?, datetime('now', '+30 days'))")
        .bind(&token).bind(user_id)
        .execute(pool).await?;
    Ok(token)
}

pub async fn register(State(st): State<SharedState>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if st.auth.mode != AuthMode::Local {
        return Err((StatusCode::NOT_FOUND, "registration is disabled in this auth mode".into()));
    }
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
    match st.auth.mode {
        AuthMode::Local => local_login(&st, &body).await,
        AuthMode::Ldap => ldap_login(&st, &body).await,
        AuthMode::Oidc => Err((StatusCode::BAD_REQUEST, "use /auth/oidc/login for OIDC sign-in".into())),
        AuthMode::None => Err((StatusCode::BAD_REQUEST, "auth is disabled on this server".into())),
    }
}

async fn local_login(st: &SharedState, body: &serde_json::Value) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("");
    let pw = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
    let row: Option<(i64, String, String)> = sqlx::query_as("SELECT id, password_hash, role FROM users WHERE email = ?")
        .bind(email).fetch_optional(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (id, hash, role) = row.ok_or((StatusCode::UNAUTHORIZED, "bad credentials".to_string()))?;
    if role == "unverified" {
        return Err((StatusCode::FORBIDDEN, "awaiting admin verification".into()));
    }
    if !verify_password(pw, &hash) {
        return Err((StatusCode::UNAUTHORIZED, "bad credentials".to_string()));
    }
    let token = insert_session(&st.pool, id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "id": id, "role": role, "token": token })))
}

async fn ldap_login(st: &SharedState, body: &serde_json::Value) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = st.auth.ldap.as_ref().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "ldap not configured".into()))?;
    let username = body.get("username").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
        .or_else(|| body.get("email").and_then(|v| v.as_str()).and_then(|e| e.split('@').next()))
        .filter(|s| !s.is_empty())
        .ok_or((StatusCode::BAD_REQUEST, "username required".into()))?;
    let password = body.get("password").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let username = username.to_string();
    let cfg = cfg.clone();
    // blocking client -> off the async runtime
    let email = tokio::task::spawn_blocking(move || crate::auth_core::ldap_verify(&cfg, &username, &password))
        .await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::UNAUTHORIZED, format!("{e}")))?;
    let (id, role) = upsert_external_user(&st.pool, &email, false).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let token = insert_session(&st.pool, id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(%email, %role, "ldap login");
    Ok(Json(serde_json::json!({ "id": id, "role": role, "token": token, "email": email })))
}

// ------------------------------------------------------------------ OIDC ----

/// Start the browser flow: build the authorization URL with state + PKCE +
/// nonce, remember them, and redirect the user to the provider.
pub async fn oidc_login(State(st): State<SharedState>) -> Result<axum::response::Response, (StatusCode, String)> {
    let cfg = st.auth.oidc.as_ref().ok_or((StatusCode::INTERNAL_SERVER_ERROR, "oidc not configured".into()))?;
    let url = crate::auth_core::start_login(cfg, &st.pending_oidc).await
        .map_err(|e| oidc_err(e))?;
    Ok(axum::response::Redirect::to(&url).into_response())
}

fn oidc_err(e: crate::auth_core::OidcError) -> (StatusCode, String) {
    use crate::auth_core::OidcError as E;
    match e {
        E::Upstream(m) => (StatusCode::BAD_GATEWAY, m),
        E::Auth(m) => (StatusCode::UNAUTHORIZED, m),
        E::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
    }
}

/// Provider redirects back here with ?code&state. Verify state, exchange the
/// code, verify the ID token (sig/iss/aud/exp/nonce), provision the user,
/// then hand the SPA a blazonry session token via URL fragment.
pub async fn oidc_callback(
    State(st): State<SharedState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let fail = |code: StatusCode, msg: String| -> axum::response::Response {
        (code, msg).into_response()
    };
    let cfg = match &st.auth.oidc { Some(c) => c, None => return fail(StatusCode::INTERNAL_SERVER_ERROR, "oidc not configured".into()) };
    let ident = match crate::auth_core::finish_login(cfg, &st.pending_oidc, &q).await {
        Ok(i) => i,
        Err(e) => return oidc_err(e).into_response(),
    };
    let (id, role) = match upsert_external_user(&st.pool, &ident.email, ident.force_admin).await {
        Ok(v) => v,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let token = match insert_session(&st.pool, id).await {
        Ok(t) => t,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    tracing::info!(email = %ident.email, %role, sub = %ident.sub, "oidc login");
    // SPA picks up #blazonry_token=... and stores it in sessionStorage
    axum::response::Redirect::to(&format!("/#blazonry_token={token}")).into_response()
}

pub async fn verify_user(State(st): State<SharedState>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let id = body.get("user_id").and_then(|v| v.as_i64()).ok_or((StatusCode::BAD_REQUEST, "user_id required".into()))?;
    sqlx::query("UPDATE users SET role = 'user' WHERE id = ? AND role = 'unverified'")
        .bind(id).execute(&st.pool).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Who am I (the SPA's boot check): email + role from the bearer token.
pub async fn me(State(st): State<SharedState>, req: axum::http::Request<axum::body::Body>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if st.auth.mode == AuthMode::None {
        return Ok(Json(serde_json::json!({ "email": "open", "role": "admin" })));
    }
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
    // `none`: admin plane is open by configuration (LAN-trust deployments).
    if st.auth.mode == AuthMode::None {
        return Ok(next.run(req).await);
    }
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
