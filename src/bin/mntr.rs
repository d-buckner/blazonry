//! mntr server: one binary, SQLite, device plane + admin API.
//! `mntr serve [--db path] [--addr 0.0.0.0:2301] [--job-secs 300]`

use axum::middleware;
use axum::routing::{delete, get, patch, post};
use axum::Router;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()),
    ).init();

    let args: Vec<String> = std::env::args().collect();
    let db_path = arg_after(&args, "--db").unwrap_or_else(|| "mntr.db".into());
    let addr = arg_after(&args, "--addr").unwrap_or_else(|| "0.0.0.0:2301".into());
    let job_secs: u64 = arg_after(&args, "--job-secs").and_then(|s| s.parse().ok()).unwrap_or(300);

    let pool = mntr::db::connect(&db_path).await?;

    // Render backends (issue #14): native always; browser when this build has
    // the `browser` feature AND a Chromium binary is present. --render-backend
    // browser makes it the global default; per-screen render_backend overrides.
    #[cfg_attr(not(feature = "browser"), allow(unused_mut))]
    let mut backends = mntr::cache::Backends::native_only()?;
    let want_browser = arg_after(&args, "--render-backend").as_deref() == Some("browser");
    #[cfg(feature = "browser")]
    if want_browser || std::env::var("MNTR_BROWSER_PATH").is_ok() {
        match mntr::browser::find_browser() {
            Some(chrome) => match mntr::browser::BrowserBackend::launch(&chrome).await {
                Ok(b) => {
                    let b = std::sync::Arc::new(b);
                    tracing::info!(chrome, "browser render backend ready");
                    if want_browser { backends.default = b.clone(); }
                    backends.browser = Some(b);
                }
                Err(e) => tracing::warn!(%e, "browser backend requested but launch failed; native only"),
            },
            None => tracing::warn!("browser backend requested but no Chromium found (set MNTR_BROWSER_PATH)"),
        }
    }
    #[cfg(not(feature = "browser"))]
    if want_browser {
        tracing::warn!("--render-backend browser requested but this build lacks the 'browser' feature; using native");
    }

    let cache = mntr::cache::new_cache();
    let auth_cfg = mntr::auth_core::AuthConfig::from_env()?;
    tracing::info!(mode = auth_cfg.mode_str(), "admin auth mode");
    let state = Arc::new(mntr::api::AppState {
        pool: pool.clone(),
        backends: backends.clone(),
        cache: cache.clone(),
        auth: Arc::new(auth_cfg),
        pending_oidc: Arc::new(mntr::auth_core::PendingOidc::new()),
    });

    // background refresh loop (replaces Sidekiq + poller procs)
    mntr::cache::spawn_job_loop(pool.clone(), backends.clone(), cache.clone(),
        std::time::Duration::from_secs(job_secs));
    // provider refresh loop: per-provider cadence (default 1h) -> snapshots
    // -> extension screen rebuilds
    mntr::providers::spawn_provider_loop(pool.clone(), backends.clone(), cache.clone(),
        std::time::Duration::from_secs(60));

    let admin = Router::new()
        .route("/screens", get(mntr::admin::list_screens).post(mntr::api::create_screen))
        .route("/screens/{id}", get(mntr::admin::get_screen)
            .patch(mntr::admin::patch_screen)
            .delete(mntr::admin::delete_screen))
        .route("/screens/{id}/preview", get(mntr::api::preview_screen))
        .route("/playlists", get(mntr::admin::list_playlists).post(mntr::api::create_playlist))
        .route("/playlists/{id}", get(mntr::admin::get_playlist)
            .patch(mntr::admin::patch_playlist)
            .delete(mntr::admin::delete_playlist))
        .route("/playlists/{id}/items", post(mntr::admin::add_playlist_item))
        .route("/playlists/{id}/items/{item_id}", patch(mntr::admin::patch_playlist_item)
            .delete(mntr::admin::delete_playlist_item))
        .route("/devices", get(mntr::admin::list_devices))
        .route("/devices/{id}", get(mntr::admin::get_device)
            .patch(mntr::admin::patch_device)
            .delete(mntr::admin::delete_device))
        .route("/devices/{id}/logs", delete(mntr::admin::wipe_device_logs))
        .route("/devices/import", post(mntr::admin::import_devices))
        .route("/devices/assign", post(mntr::api::assign_device))
        .route("/firmware", get(mntr::admin::list_firmware).post(mntr::admin::create_firmware))
        .route("/firmware/{id}", get(mntr::admin::get_firmware)
            .patch(mntr::admin::patch_firmware)
            .delete(mntr::admin::delete_firmware))
        .route("/firmware_wipe", delete(mntr::admin::wipe_firmware))
        .route("/models", get(mntr::admin::list_models).post(mntr::admin::create_model))
        .route("/models/{id}", get(mntr::admin::get_model)
            .patch(mntr::admin::patch_model)
            .delete(mntr::admin::delete_model))
        .route("/users/verify", post(mntr::auth::verify_user))
        .route("/users", get(mntr::auth::list_users))
        .route("/me", get(mntr::auth::me))
        .route("/providers", get(mntr::admin_provider::list_providers).post(mntr::admin_provider::create_provider))
        .route("/providers/{id}", get(mntr::admin_provider::get_provider)
            .patch(mntr::admin_provider::patch_provider)
            .delete(mntr::admin_provider::delete_provider))
        .route("/providers/{id}/refresh", post(mntr::admin_provider::refresh_provider_now))
        .route("/extensions", get(mntr::admin_provider::list_extensions).post(mntr::admin_provider::create_extension))
        .route("/extensions/{id}", patch(mntr::admin_provider::patch_extension)
            .delete(mntr::admin_provider::delete_extension))
        .route_layer(middleware::from_fn_with_state(state.clone(), mntr::auth::require_admin));

    let app = Router::new()
        // admin SPA (static; auth happens in JS against /admin/me)
        .route("/", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
             include_str!("../../web/index.html"))
        }))
        // PWA assets (embedded; same single-binary rule). The SW must be a
        // top-level file and must never itself be cached by the browser.
        .route("/sw.js", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
              (axum::http::header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")],
             include_str!("../../web/sw.js"))
        }))
        .route("/manifest.webmanifest", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "application/manifest+json"),
              (axum::http::header::CACHE_CONTROL, "no-cache")],
             include_str!("../../web/manifest.webmanifest"))
        }))
        .route("/icons/icon.svg", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
             &include_bytes!("../../web/icons/icon.svg")[..])
        }))
        .route("/icons/icon-192.png", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "image/png")],
             &include_bytes!("../../web/icons/icon-192.png")[..])
        }))
        .route("/icons/icon-512.png", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "image/png")],
             &include_bytes!("../../web/icons/icon-512.png")[..])
        }))
        // device plane (TRMNL firmware wire contract)
        .route("/api/display", get(mntr::api::display))
        .route("/api/display/current", get(mntr::api::display_current))
        .route("/api/current_screen", get(mntr::api::display_current))
        .route("/api/setup", get(mntr::api::setup))
        .route("/api/log", post(mntr::api::log_create))
        .route("/api/logs", get(mntr::api::logs))
        .route("/api/latest_firmware", get(mntr::api::firmware_latest))
        .route("/images/{name}", get(mntr::api::serve_image))
        // auth (open by design)
        .route("/admin/register", post(mntr::auth::register))
        .route("/admin/login", post(mntr::auth::login))
        .route("/admin/auth/mode", get(mntr::auth::auth_mode))
        // OIDC browser flow (open by design)
        .route("/auth/oidc/login", get(mntr::auth::oidc_login))
        .route("/auth/oidc/callback", get(mntr::auth::oidc_callback))
        // admin plane (bearer session required)
        .nest("/admin", admin)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!("mntr listening on http://{bound}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}
