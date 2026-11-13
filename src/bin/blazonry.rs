// SPDX-License-Identifier: AGPL-3.0-only
//! blazonry server: one binary, SQLite, device plane + admin API.
//! `blazonry serve [--db path] [--addr 0.0.0.0:2301] [--job-secs 300]`

use axum::Router;
use axum::middleware;
use axum::routing::{delete, get, patch, post};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Dev-loop subcommands (no server, no DB): blazonry ext new|check|render
    {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        if argv.first().map(|s| s.as_str()) == Some("ext") {
            std::process::exit(blazonry::ext_dev::cli(&argv));
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let db_path = arg_after(&args, "--db").unwrap_or_else(|| "blazonry.db".into());
    let addr = arg_after(&args, "--addr").unwrap_or_else(|| "0.0.0.0:2301".into());
    let job_secs: u64 = arg_after(&args, "--job-secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    let pool = blazonry::db::connect(&db_path).await?;

    // Render backends (issue #14): native always; browser when this build has
    // the `browser` feature AND a Chromium binary is present. --render-backend
    // browser makes it the global default; per-screen render_backend overrides.
    #[cfg_attr(not(feature = "browser"), allow(unused_mut))]
    let mut backends = blazonry::cache::Backends::native_only()?;
    let want_browser = arg_after(&args, "--render-backend").as_deref() == Some("browser");
    #[cfg(feature = "browser")]
    if want_browser || std::env::var("BLAZONRY_BROWSER_PATH").is_ok() {
        match blazonry::browser::find_browser() {
            Some(chrome) => match blazonry::browser::BrowserBackend::launch(&chrome).await {
                Ok(b) => {
                    let b = std::sync::Arc::new(b);
                    tracing::info!(chrome, "browser render backend ready");
                    if want_browser {
                        backends.default = b.clone();
                    }
                    backends.browser = Some(b);
                }
                Err(e) => {
                    tracing::warn!(%e, "browser backend requested but launch failed; native only")
                }
            },
            None => tracing::warn!(
                "browser backend requested but no Chromium found (set BLAZONRY_BROWSER_PATH)"
            ),
        }
    }
    #[cfg(not(feature = "browser"))]
    if want_browser {
        tracing::warn!(
            "--render-backend browser requested but this build lacks the 'browser' feature; using native"
        );
    }

    let cache = blazonry::cache::new_cache();
    let auth_cfg = blazonry::auth_core::AuthConfig::from_env()?;
    tracing::info!(mode = auth_cfg.mode_str(), "admin auth mode");
    let state = Arc::new(blazonry::api::AppState {
        pool: pool.clone(),
        backends: backends.clone(),
        cache: cache.clone(),
        auth: Arc::new(auth_cfg),
        pending_oidc: Arc::new(blazonry::auth_core::PendingOidc::new()),
    });

    // background refresh loop (replaces Sidekiq + poller procs)
    blazonry::cache::spawn_job_loop(
        pool.clone(),
        backends.clone(),
        cache.clone(),
        std::time::Duration::from_secs(job_secs),
    );
    // provider refresh loop: per-provider cadence (default 1h) -> snapshots
    // -> extension screen rebuilds
    blazonry::providers::spawn_provider_loop(
        pool.clone(),
        backends.clone(),
        cache.clone(),
        std::time::Duration::from_secs(60),
    );

    let admin = Router::new()
        .route(
            "/screens",
            get(blazonry::admin::list_screens).post(blazonry::api::create_screen),
        )
        .route(
            "/screens/{id}",
            get(blazonry::admin::get_screen)
                .patch(blazonry::admin::patch_screen)
                .delete(blazonry::admin::delete_screen),
        )
        .route("/screens/{id}/preview", get(blazonry::api::preview_screen))
        .route("/screens/preview", post(blazonry::api::preview_draft))
        .route(
            "/playlists",
            get(blazonry::admin::list_playlists).post(blazonry::api::create_playlist),
        )
        .route(
            "/playlists/{id}",
            get(blazonry::admin::get_playlist)
                .patch(blazonry::admin::patch_playlist)
                .delete(blazonry::admin::delete_playlist),
        )
        .route(
            "/playlists/{id}/items",
            post(blazonry::admin::add_playlist_item),
        )
        .route(
            "/playlists/{id}/items/{item_id}",
            patch(blazonry::admin::patch_playlist_item)
                .delete(blazonry::admin::delete_playlist_item),
        )
        .route("/devices", get(blazonry::admin::list_devices))
        .route(
            "/devices/{id}",
            get(blazonry::admin::get_device)
                .patch(blazonry::admin::patch_device)
                .delete(blazonry::admin::delete_device),
        )
        .route(
            "/devices/{id}/logs",
            delete(blazonry::admin::wipe_device_logs),
        )
        .route("/devices/import", post(blazonry::admin::import_devices))
        .route("/devices/assign", post(blazonry::api::assign_device))
        .route(
            "/firmware",
            get(blazonry::admin::list_firmware).post(blazonry::admin::create_firmware),
        )
        .route(
            "/firmware/{id}",
            get(blazonry::admin::get_firmware)
                .patch(blazonry::admin::patch_firmware)
                .delete(blazonry::admin::delete_firmware),
        )
        .route("/firmware_wipe", delete(blazonry::admin::wipe_firmware))
        .route(
            "/models",
            get(blazonry::admin::list_models).post(blazonry::admin::create_model),
        )
        .route(
            "/models/{id}",
            get(blazonry::admin::get_model)
                .patch(blazonry::admin::patch_model)
                .delete(blazonry::admin::delete_model),
        )
        .route("/users/verify", post(blazonry::auth::verify_user))
        .route("/users", get(blazonry::auth::list_users))
        .route("/me", get(blazonry::auth::me))
        .route(
            "/providers",
            get(blazonry::admin_provider::list_providers)
                .post(blazonry::admin_provider::create_provider),
        )
        .route(
            "/providers/{id}",
            get(blazonry::admin_provider::get_provider)
                .patch(blazonry::admin_provider::patch_provider)
                .delete(blazonry::admin_provider::delete_provider),
        )
        .route(
            "/providers/{id}/refresh",
            post(blazonry::admin_provider::refresh_provider_now),
        )
        .route(
            "/providers/ha/tabs",
            post(blazonry::admin_provider::ha_tabs),
        )
        .route(
            "/extensions",
            get(blazonry::admin_provider::list_extensions)
                .post(blazonry::admin_provider::create_extension),
        )
        .route(
            "/extensions/{id}",
            patch(blazonry::admin_provider::patch_extension)
                .delete(blazonry::admin_provider::delete_extension),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            blazonry::auth::require_admin,
        ));

    let app = Router::new()
        // admin SPA (static; auth happens in JS against /admin/me)
        .route(
            "/",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    include_str!("../../web/index.html"),
                )
            }),
        )
        // PWA assets (embedded; same single-binary rule). The SW must be a
        // top-level file and must never itself be cached by the browser.
        .route(
            "/sw.js",
            get(|| async {
                (
                    [
                        (
                            axum::http::header::CONTENT_TYPE,
                            "text/javascript; charset=utf-8",
                        ),
                        (
                            axum::http::header::CACHE_CONTROL,
                            "no-cache, no-store, must-revalidate",
                        ),
                    ],
                    include_str!("../../web/sw.js"),
                )
            }),
        )
        .route(
            "/manifest.webmanifest",
            get(|| async {
                (
                    [
                        (
                            axum::http::header::CONTENT_TYPE,
                            "application/manifest+json",
                        ),
                        (axum::http::header::CACHE_CONTROL, "no-cache"),
                    ],
                    include_str!("../../web/manifest.webmanifest"),
                )
            }),
        )
        .route(
            "/icons/{name}",
            get(
                |axum::extract::Path(name): axum::extract::Path<String>| async move {
                    use axum::response::IntoResponse;
                    match name.as_str() {
                        "icon.svg" => (
                            [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                            &include_bytes!("../../web/icons/icon.svg")[..],
                        )
                            .into_response(),
                        "icon-192.png" => (
                            [(axum::http::header::CONTENT_TYPE, "image/png")],
                            &include_bytes!("../../web/icons/icon-192.png")[..],
                        )
                            .into_response(),
                        "icon-512.png" => (
                            [(axum::http::header::CONTENT_TYPE, "image/png")],
                            &include_bytes!("../../web/icons/icon-512.png")[..],
                        )
                            .into_response(),
                        "icon-192-maskable.png" => (
                            [(axum::http::header::CONTENT_TYPE, "image/png")],
                            &include_bytes!("../../web/icons/icon-192-maskable.png")[..],
                        )
                            .into_response(),
                        "icon-512-maskable.png" => (
                            [(axum::http::header::CONTENT_TYPE, "image/png")],
                            &include_bytes!("../../web/icons/icon-512-maskable.png")[..],
                        )
                            .into_response(),
                        "favicon-16.png" => (
                            [(axum::http::header::CONTENT_TYPE, "image/png")],
                            &include_bytes!("../../web/icons/favicon-16.png")[..],
                        )
                            .into_response(),
                        "favicon-32.png" => (
                            [(axum::http::header::CONTENT_TYPE, "image/png")],
                            &include_bytes!("../../web/icons/favicon-32.png")[..],
                        )
                            .into_response(),
                        _ => (axum::http::StatusCode::NOT_FOUND, "unknown icon").into_response(),
                    }
                },
            ),
        )
        // device plane (TRMNL firmware wire contract)
        .route("/api/display", get(blazonry::api::display))
        .route("/api/display/current", get(blazonry::api::display_current))
        .route("/api/current_screen", get(blazonry::api::display_current))
        .route("/api/setup", get(blazonry::api::setup))
        .route("/api/log", post(blazonry::api::log_create))
        .route("/api/logs", get(blazonry::api::logs))
        .route("/api/latest_firmware", get(blazonry::api::firmware_latest))
        .route("/images/{name}", get(blazonry::api::serve_image))
        // auth (open by design)
        .route("/admin/register", post(blazonry::auth::register))
        .route("/admin/login", post(blazonry::auth::login))
        .route("/admin/auth/mode", get(blazonry::auth::auth_mode))
        // OIDC browser flow (open by design)
        .route("/auth/oidc/login", get(blazonry::auth::oidc_login))
        .route("/auth/oidc/callback", get(blazonry::auth::oidc_callback))
        // admin plane (bearer session required)
        .nest("/admin", admin)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!("blazonry listening on http://{bound}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
