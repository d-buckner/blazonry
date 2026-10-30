//! Extensions: consumers of provider shapes. One module per extension under
//! `extensions/` (today: `family_calendar`). Each extension declares its
//! required shape + cardinality and turns merged snapshots into a screen.
//!
//! The framework here is generic: selector resolution (all | explicit pick
//! list — no tags, no expressions), snapshot merge, and rebuild dispatch.
//! Adding an extension = new module + one match arm in [`rebuild_extensions`]
//! and [`create`].

use serde_json::Value;
use sqlx::AssertSqlSafe;
use sqlx::SqlitePool;

pub mod family_calendar;

pub const MIGRATE: &str = crate::providers::MIGRATE;

/// Resolve an extension's provider pick list: explicit ids, or all providers
/// of the shape when unset.
pub async fn resolved_provider_ids(pool: &SqlitePool, settings: &Value) -> anyhow::Result<Vec<i64>> {
    if let Some(ids) = settings.get("provider_ids").and_then(|v| v.as_array()) {
        let ids: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    Ok(sqlx::query_scalar("SELECT id FROM providers WHERE kind = 'caldav' ORDER BY id")
        .fetch_all(pool).await?)
}

/// Merged calendar snapshot rows for a resolved provider set, sorted by start.
/// Rows: (start, all_day, title, location, description, source_name).
pub async fn merged_calendar_rows(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<Vec<(String, bool, String, String, String, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut q = sqlx::query(AssertSqlSafe(format!(
        "SELECT pe.start, pe.all_day, pe.title, pe.location, pe.description, p.name \
         FROM provider_events pe JOIN providers p ON p.id = pe.provider_id \
         WHERE pe.provider_id IN ({placeholders}) ORDER BY pe.start"
    )));
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    use sqlx::Row;
    Ok(rows.iter().map(|r| {
        (r.get(0), r.get::<i64, _>(1) != 0, r.get(2), r.get(3), r.get(4), r.get(5))
    }).collect())
}

/// Re-render one extension screen into the BMP cache (both panel depths).
async fn repaint(pool: &SqlitePool, cache: &crate::cache::BmpCache, screen_id: i64) {
    for bpp in [1u16, 2u16] {
        match crate::render::Renderer::new() {
            Ok(r) => {
                if let Err(e) = crate::cache::render_and_cache(pool, &r, cache, screen_id, bpp).await {
                    tracing::warn!(%e, screen = screen_id, "extension re-render failed");
                }
            }
            Err(e) => tracing::warn!(%e, "renderer init failed during extension repaint"),
        }
    }
}

/// Rebuild every extension's bound screen from current snapshots.
/// Returns the number of extensions rebuilt.
pub async fn rebuild_extensions(pool: &SqlitePool, cache: &crate::cache::BmpCache) -> anyhow::Result<usize> {
    let exts = sqlx::query("SELECT kind, name, settings, screen_id FROM extensions").fetch_all(pool).await?;
    let mut n = 0;
    for ext in exts {
        use sqlx::Row;
        let kind: String = ext.get(0);
        let name: String = ext.get(1);
        let settings: String = ext.get(2);
        let screen_id: Option<i64> = ext.get(3);
        let settings: Value = serde_json::from_str(&settings).unwrap_or(serde_json::json!({}));
        let Some(sid) = screen_id else { continue };
        match kind.as_str() {
            "family_calendar" => {
                let ids = resolved_provider_ids(pool, &settings).await?;
                if ids.is_empty() {
                    tracing::warn!(extension = name, "no calendar providers resolved; leaving screen as-is");
                    continue;
                }
                let merged = merged_calendar_rows(pool, &ids).await?;
                let vars = family_calendar::agenda_vars(&merged, &settings);
                let vars_json = serde_json::to_string(&vars)?;
                sqlx::query("UPDATE screens SET vars = ?, updated_at = datetime('now') WHERE id = ?")
                    .bind(&vars_json).bind(sid).execute(pool).await?;
                repaint(pool, cache, sid).await;
                n += 1;
            }
            other => tracing::warn!(extension = name, kind = other, "unknown extension kind; skipping"),
        }
    }
    Ok(n)
}

/// Create an extension by kind: validate cardinality, create its screen row,
/// render immediately from existing snapshots. Returns (extension_id, screen_id).
pub async fn create(pool: &SqlitePool, cache: &crate::cache::BmpCache, kind: &str, name: &str, settings: Value) -> anyhow::Result<(i64, i64)> {
    match kind {
        "family_calendar" => family_calendar::create(pool, cache, name, settings).await,
        other => anyhow::bail!("unknown extension kind '{other}' (supported: family_calendar)"),
    }
}
