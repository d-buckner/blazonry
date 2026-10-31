//! Render cache + job loop.
//!
//! Devices poll every ~15 min; rendering on every poll is waste. The job loop
//! re-renders every screen on an interval (this is where live-data fetchers
//! will hang — weather, sleep, etc. will update `vars` before each pass) and
//! swaps the cache atomically. `/api/display` becomes an O(1) map read with
//! an on-demand fallback render on cold cache.
//!
//! Browser renders cost seconds (vs ms native), so each cache entry also
//! stores a content hash of (template, vars, backend): the job loop skips
//! re-rendering anything whose hash is unchanged.

use crate::backend::{NativeBackend, RenderBackend};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cache key: (screen_id, bpp). OG panel wants 1bpp, X wants 2bpp.
pub type BmpCache = Arc<RwLock<HashMap<(i64, u16), Entry>>>;

/// One cached render: the BMP bytes plus the content hash they came from.
pub struct Entry {
    pub bmp: Arc<Vec<u8>>,
    pub hash: String,
}

/// Content hash for cache-skip: template + vars + backend name.
pub fn content_hash(template: &str, vars_json: &str, backend: &str) -> String {
    let mut h = Sha256::new();
    h.update(template.as_bytes());
    h.update([0u8]);
    h.update(vars_json.as_bytes());
    h.update([0u8]);
    h.update(backend.as_bytes());
    let out = h.finalize();
    out.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Backend registry: native always, browser when the `browser` feature found
/// a Chromium binary at startup.
#[derive(Clone)]
pub struct Backends {
    pub native: Arc<dyn RenderBackend>,
    pub browser: Option<Arc<dyn RenderBackend>>,
    /// Global default for screens with no per-screen preference
    /// (`--render-backend` at startup).
    pub default: Arc<dyn RenderBackend>,
}

impl Backends {
    pub fn native_only() -> anyhow::Result<Self> {
        let native: Arc<dyn RenderBackend> = std::sync::Arc::new(NativeBackend::new()?);
        Ok(Self { native: native.clone(), browser: None, default: native })
    }

    /// Resolve the backend for a screen preference ("native" | "browser" | "").
    /// Unknown/browser-on-core falls back to native with a warning.
    pub fn for_pref(&self, pref: &str) -> Arc<dyn RenderBackend> {
        match pref {
            "browser" => match &self.browser {
                Some(b) => b.clone(),
                None => {
                    tracing::debug!("screen wants browser backend; core build -> native");
                    self.native.clone()
                }
            },
            "native" => self.native.clone(),
            _ => self.default.clone(),
        }
    }
}

pub fn new_cache() -> BmpCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Render one screen template+vars to BMP bytes at the given depth (1 or 2 bpp).
pub async fn render_bmp(backend: &dyn RenderBackend, template: &str, vars_json: &str, bpp: u16) -> anyhow::Result<Vec<u8>> {
    backend.render_bmp(template, vars_json, bpp).await
}

/// Load + render a screen from the DB, inserting into the cache.
pub async fn render_and_cache(pool: &SqlitePool, backends: &Backends, cache: &BmpCache, screen_id: i64, bpp: u16) -> anyhow::Result<Arc<Vec<u8>>> {
    let (template, vars, pref) = crate::screens::template_vars_backend(pool, screen_id).await?;
    let backend = backends.for_pref(&pref);
    let bmp = Arc::new(render_bmp(backend.as_ref(), &template, &vars, bpp).await?);
    let hash = content_hash(&template, &vars, backend.name());
    cache.write().await.insert((screen_id, bpp), Entry { bmp: bmp.clone(), hash });
    Ok(bmp)
}

/// Get BMP for a screen: cache hit or render-on-miss.
pub async fn cached_bmp(pool: &SqlitePool, backends: &Backends, cache: &BmpCache, screen_id: i64, bpp: u16) -> anyhow::Result<Arc<Vec<u8>>> {
    if let Some(e) = cache.read().await.get(&(screen_id, bpp)) {
        return Ok(e.bmp.clone());
    }
    render_and_cache(pool, backends, cache, screen_id, bpp).await
}

/// Invalidate one screen (call after screen update/delete).
pub async fn invalidate(cache: &BmpCache, screen_id: i64) {
    cache.write().await.retain(|(id, _), _| *id != screen_id);
}

/// Background job loop: periodically re-render every screen so the cache is
/// warm and (later) fresh with live vars. Unchanged content (hash match) is
/// carried over without re-rendering — essential for the browser backend.
pub fn spawn_job_loop(pool: SqlitePool, backends: Backends, cache: BmpCache, interval: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let ids = match crate::screens::all_ids(&pool).await {
                Ok(v) => v,
                Err(e) => { tracing::warn!(%e, "job loop: screen list failed"); continue; }
            };
            let mut fresh: HashMap<(i64, u16), Entry> = HashMap::new();
            let mut skipped = 0usize;
            for id in ids {
                match crate::screens::template_vars_backend(&pool, id).await {
                    Ok((template, vars, pref)) => {
                        let backend = backends.for_pref(&pref);
                        let hash = content_hash(&template, &vars, backend.name());
                        for bpp in [1u16, 2u16] {
                            // unchanged content: carry the previous render over
                            let hit = cache.read().await.get(&(id, bpp))
                                .filter(|e| e.hash == hash)
                                .map(|e| e.bmp.clone());
                            if let Some(bmp) = hit {
                                fresh.insert((id, bpp), Entry { bmp, hash: hash.clone() });
                                skipped += 1;
                                continue;
                            }
                            match render_bmp(backend.as_ref(), &template, &vars, bpp).await {
                                Ok(bmp) => { fresh.insert((id, bpp), Entry { bmp: Arc::new(bmp), hash: hash.clone() }); }
                                Err(e) => tracing::warn!(%e, screen = id, backend = backend.name(), "job loop: render failed"),
                            }
                        }
                    }
                    Err(e) => tracing::warn!(%e, screen = id, "job loop: load failed"),
                }
            }
            let n = fresh.len();
            *cache.write().await = fresh;
            tracing::debug!(screens = n, skipped_unchanged = skipped, "job loop: cache refreshed");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_changes_with_every_input() {
        let a = content_hash("t1", "{}", "native");
        assert_eq!(a, content_hash("t1", "{}", "native"));
        assert_ne!(a, content_hash("t2", "{}", "native"));
        assert_ne!(a, content_hash("t1", "{\"x\":1}", "native"));
        assert_ne!(a, content_hash("t1", "{}", "browser"));
    }
}
