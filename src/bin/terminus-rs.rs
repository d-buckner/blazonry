//! terminus-rs server: one binary, SQLite, device plane + admin API.
//! `terminus-rs serve [--db path] [--addr 0.0.0.0:2301] [--job-secs 300]`

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
    let db_path = arg_after(&args, "--db").unwrap_or_else(|| "terminus.db".into());
    let addr = arg_after(&args, "--addr").unwrap_or_else(|| "0.0.0.0:2301".into());
    let job_secs: u64 = arg_after(&args, "--job-secs").and_then(|s| s.parse().ok()).unwrap_or(300);

    let pool = terminus_rs::db::connect(&db_path).await?;
    let renderer = Arc::new(terminus_rs::render::Renderer::new()?);
    let cache = terminus_rs::cache::new_cache();
    let state = Arc::new(terminus_rs::api::AppState {
        pool: pool.clone(),
        renderer: renderer.clone(),
        cache: cache.clone(),
    });

    // background refresh loop (replaces Sidekiq + poller procs)
    terminus_rs::cache::spawn_job_loop(pool.clone(), renderer.clone(), cache.clone(),
        std::time::Duration::from_secs(job_secs));

    let admin = Router::new()
        .route("/screens", get(terminus_rs::admin::list_screens).post(terminus_rs::api::create_screen))
        .route("/screens/{id}", get(terminus_rs::admin::get_screen)
            .patch(terminus_rs::admin::patch_screen)
            .delete(terminus_rs::admin::delete_screen))
        .route("/screens/{id}/preview", get(terminus_rs::api::preview_screen))
        .route("/playlists", get(terminus_rs::admin::list_playlists).post(terminus_rs::api::create_playlist))
        .route("/playlists/{id}", get(terminus_rs::admin::get_playlist)
            .patch(terminus_rs::admin::patch_playlist)
            .delete(terminus_rs::admin::delete_playlist))
        .route("/playlists/{id}/items", post(terminus_rs::admin::add_playlist_item))
        .route("/playlists/{id}/items/{item_id}", patch(terminus_rs::admin::patch_playlist_item)
            .delete(terminus_rs::admin::delete_playlist_item))
        .route("/devices", get(terminus_rs::admin::list_devices))
        .route("/devices/{id}", get(terminus_rs::admin::get_device)
            .patch(terminus_rs::admin::patch_device)
            .delete(terminus_rs::admin::delete_device))
        .route("/devices/{id}/logs", delete(terminus_rs::admin::wipe_device_logs))
        .route("/devices/import", post(terminus_rs::admin::import_devices))
        .route("/devices/assign", post(terminus_rs::api::assign_device))
        .route("/firmware", get(terminus_rs::admin::list_firmware).post(terminus_rs::admin::create_firmware))
        .route("/firmware/{id}", get(terminus_rs::admin::get_firmware)
            .patch(terminus_rs::admin::patch_firmware)
            .delete(terminus_rs::admin::delete_firmware))
        .route("/firmware_wipe", delete(terminus_rs::admin::wipe_firmware))
        .route("/models", get(terminus_rs::admin::list_models).post(terminus_rs::admin::create_model))
        .route("/models/{id}", get(terminus_rs::admin::get_model)
            .patch(terminus_rs::admin::patch_model)
            .delete(terminus_rs::admin::delete_model))
        .route("/users/verify", post(terminus_rs::auth::verify_user))
        .route_layer(middleware::from_fn_with_state(state.clone(), terminus_rs::auth::require_admin));

    let app = Router::new()
        // device plane (TRMNL firmware wire contract)
        .route("/api/display", get(terminus_rs::api::display))
        .route("/api/display/current", get(terminus_rs::api::display_current))
        .route("/api/current_screen", get(terminus_rs::api::display_current))
        .route("/api/setup", get(terminus_rs::api::setup))
        .route("/api/log", post(terminus_rs::api::log_create))
        .route("/api/logs", get(terminus_rs::api::logs))
        .route("/api/latest_firmware", get(terminus_rs::api::firmware_latest))
        .route("/images/{name}", get(terminus_rs::api::serve_image))
        // auth (open by design)
        .route("/admin/register", post(terminus_rs::auth::register))
        .route("/admin/login", post(terminus_rs::auth::login))
        // admin plane (bearer session required)
        .nest("/admin", admin)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!("terminus-rs listening on http://{bound}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}
