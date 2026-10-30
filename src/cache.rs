//! Render cache + job loop.
//!
//! Devices poll every ~15 min; rendering on every poll is waste. The job loop
//! re-renders every screen on an interval (this is where live-data fetchers
//! will hang — weather, sleep, etc. will update `vars` before each pass) and
//! swaps the cache atomically. `/api/display` becomes an O(1) map read with
//! an on-demand fallback render on cold cache.

use crate::bmp::encode_2bit_bmp;
use crate::render::Renderer;
use crate::template::render_screen;
use crate::{HEIGHT, WIDTH};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cache key: (screen_id, bpp). OG panel wants 1bpp, X wants 2bpp.
pub type BmpCache = Arc<RwLock<HashMap<(i64, u16), Arc<Vec<u8>>>>>;

pub fn new_cache() -> BmpCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Render one screen template+vars to BMP bytes at the given depth (1 or 2 bpp).
pub fn render_bmp(renderer: &Renderer, template: &str, vars_json: &str, bpp: u16) -> anyhow::Result<Vec<u8>> {
    let vars: serde_json::Value = serde_json::from_str(vars_json).unwrap_or(serde_json::json!({}));
    let screen = render_screen(template, &vars)?;
    let indices = renderer.render(&screen);
    Ok(match bpp {
        1 => crate::bmp::encode_1bit_bmp(WIDTH, HEIGHT, &indices),
        _ => encode_2bit_bmp(WIDTH, HEIGHT, &indices),
    })
}

/// Load + render a screen from the DB, inserting into the cache.
pub async fn render_and_cache(pool: &SqlitePool, renderer: &Renderer, cache: &BmpCache, screen_id: i64, bpp: u16) -> anyhow::Result<Arc<Vec<u8>>> {
    let (template, vars) = crate::screens::template_vars(pool, screen_id).await?;
    let bmp = render_bmp(renderer, &template, &vars, bpp)?;
    let bmp = Arc::new(bmp);
    cache.write().await.insert((screen_id, bpp), bmp.clone());
    Ok(bmp)
}

/// Get BMP for a screen: cache hit or render-on-miss.
pub async fn cached_bmp(pool: &SqlitePool, renderer: &Renderer, cache: &BmpCache, screen_id: i64, bpp: u16) -> anyhow::Result<Arc<Vec<u8>>> {
    if let Some(bmp) = cache.read().await.get(&(screen_id, bpp)) {
        return Ok(bmp.clone());
    }
    render_and_cache(pool, renderer, cache, screen_id, bpp).await
}

/// Invalidate one screen (call after screen update/delete).
pub async fn invalidate(cache: &BmpCache, screen_id: i64) {
    cache.write().await.retain(|(id, _), _| *id != screen_id);
}

/// Background job loop: periodically re-render every screen so the cache is
/// warm and (later) fresh with live vars. Replaces Sidekiq + poller procs.
pub fn spawn_job_loop(pool: SqlitePool, renderer: Arc<Renderer>, cache: BmpCache, interval: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let ids = match crate::screens::all_ids(&pool).await {
                Ok(v) => v,
                Err(e) => { tracing::warn!(%e, "job loop: screen list failed"); continue; }
            };
            let mut fresh = HashMap::new();
            for id in ids {
                match crate::screens::template_vars(&pool, id).await {
                    Ok((template, vars)) => {
                        for bpp in [1u16, 2u16] {
                            match render_bmp(&renderer, &template, &vars, bpp) {
                                Ok(bmp) => { fresh.insert((id, bpp), Arc::new(bmp)); }
                                Err(e) => tracing::warn!(%e, screen = id, "job loop: render failed"),
                            }
                        }
                    }
                    Err(e) => tracing::warn!(%e, screen = id, "job loop: load failed"),
                }
            }
            let n = fresh.len();
            *cache.write().await = fresh;
            tracing::debug!(screens = n, "job loop: cache refreshed");
        }
    })
}
