//! Extensions: consumers of provider shapes. One module per extension under
//! `extensions/` (today: `family_calendar`). Each extension declares its
//! required shape + cardinality and turns merged snapshots into a screen.
//!
//! The framework here is generic: selector resolution (all | explicit pick
//! list — no tags, no expressions), snapshot merge, and rebuild dispatch.
//! Adding an extension = new module + one match arm in [`rebuild_extensions`]
//! and [`create`].

use serde_json::Value;
use sqlx::SqlitePool;

pub mod family_calendar;
pub mod weather;
pub mod clock;

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
    Ok(crate::providers::calendar_provider_ids(pool).await?)
}

/// Merged calendar snapshot rows for a resolved provider set, sorted by start.
/// Rows: (start, all_day, title, location, description, source_name).
pub async fn merged_calendar_rows(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<Vec<(String, bool, String, String, String, String)>> {
    crate::providers::merged_calendar_rows(pool, ids).await
}

/// Re-render one extension screen into the BMP cache (both panel depths).
async fn repaint(pool: &SqlitePool, backends: &crate::cache::Backends, cache: &crate::cache::BmpCache, screen_id: i64) {
    for bpp in [1u16, 2u16] {
        if let Err(e) = crate::cache::render_and_cache(pool, backends, cache, screen_id, bpp).await {
            tracing::warn!(%e, screen = screen_id, "extension re-render failed");
        }
    }
}

/// Rebuild every extension's bound screen from current snapshots.
/// Returns the number of extensions rebuilt.
pub async fn rebuild_extensions(pool: &SqlitePool, backends: &crate::cache::Backends, cache: &crate::cache::BmpCache) -> anyhow::Result<usize> {
    let exts = all_rows(pool).await?;
    let mut n = 0;
    for ext in exts {
        let ExtRow { kind, name, settings, screen_id } = ext;
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
                crate::screens::set_vars(pool, sid, &vars_json).await?;
                repaint(pool, backends, cache, sid).await;
                n += 1;
            }
            "weather" => {
                let Some(pid) = weather::resolved_provider_id(pool, &settings).await? else {
                    tracing::warn!(extension = name, "no weather provider resolved; leaving screen as-is");
                    continue;
                };
                match crate::providers::weather_snapshot(pool, pid).await? {
                    Some(snap) => {
                        let vars = weather::weather_vars(&snap, &settings);
                        crate::screens::set_vars(pool, sid, &serde_json::to_string(&vars)?).await?;
                        repaint(pool, backends, cache, sid).await;
                        n += 1;
                    }
                    None => {
                        let vars = weather::no_data_vars("Waiting for first weather refresh...");
                        crate::screens::set_vars(pool, sid, &serde_json::to_string(&vars)?).await?;
                        repaint(pool, backends, cache, sid).await;
                    }
                }
            }
            "clock" => {
                let vars = clock::clock_vars(&settings);
                crate::screens::set_vars(pool, sid, &serde_json::to_string(&vars)?).await?;
                repaint(pool, backends, cache, sid).await;
                n += 1;
            }
            other => tracing::warn!(extension = name, kind = other, "unknown extension kind; skipping"),
        }
    }
    Ok(n)
}

/// Re-render clock extensions (time is live: every provider-loop tick).
pub async fn rebuild_clocks(pool: &SqlitePool, backends: &crate::cache::Backends, cache: &crate::cache::BmpCache) -> anyhow::Result<()> {
    let exts = all_rows(pool).await?;
    for ext in exts {
        if ext.kind != "clock" { continue }
        let Some(sid) = ext.screen_id else { continue };
        let vars = clock::clock_vars(&ext.settings);
        crate::screens::set_vars(pool, sid, &serde_json::to_string(&vars)?).await?;
        repaint(pool, backends, cache, sid).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `extensions` table owner: CRUD for the admin API.
// ---------------------------------------------------------------------------

pub struct ExtRow {
    pub kind: String,
    pub name: String,
    pub settings: Value,
    pub screen_id: Option<i64>,
}

async fn all_rows(pool: &SqlitePool) -> anyhow::Result<Vec<ExtRow>> {
    use sqlx::Row;
    let rows = sqlx::query("SELECT kind, name, settings, screen_id FROM extensions").fetch_all(pool).await?;
    Ok(rows.into_iter().map(|r| ExtRow {
        kind: r.get(0),
        name: r.get(1),
        settings: serde_json::from_str(r.get(2)).unwrap_or(serde_json::json!({})),
        screen_id: r.get(3),
    }).collect())
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query("SELECT id, kind, name, settings, screen_id FROM extensions ORDER BY id")
        .fetch_all(pool).await?;
    use sqlx::Row;
    Ok(rows.into_iter().map(|r| serde_json::json!({
        "id": r.get::<i64, _>(0),
        "kind": r.get::<String, _>(1),
        "name": r.get::<String, _>(2),
        "settings": serde_json::from_str::<Value>(&r.get::<String, _>(3)).unwrap_or(Value::Null),
        "screen_id": r.get::<Option<i64>, _>(4),
    })).collect())
}

pub async fn insert(pool: &SqlitePool, kind: &str, name: &str, settings: &Value, screen_id: i64) -> anyhow::Result<i64> {
    Ok(sqlx::query("INSERT INTO extensions (kind, name, settings, screen_id) VALUES (?, ?, ?, ?)")
        .bind(kind).bind(name).bind(settings.to_string()).bind(screen_id)
        .execute(pool).await?
        .last_insert_rowid())
}

pub async fn exists(pool: &SqlitePool, id: i64) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM extensions WHERE id = ?")
        .bind(id).fetch_one(pool).await? > 0)
}

pub async fn set_name(pool: &SqlitePool, id: i64, name: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE extensions SET name = ? WHERE id = ?").bind(name).bind(id)
        .execute(pool).await?;
    Ok(())
}

pub async fn set_settings(pool: &SqlitePool, id: i64, settings: &Value) -> anyhow::Result<()> {
    sqlx::query("UPDATE extensions SET settings = ? WHERE id = ?")
        .bind(settings.to_string()).bind(id)
        .execute(pool).await?;
    Ok(())
}

/// Delete an extension and its bound screen (screen teardown via its owner).
pub async fn delete(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<i64>> {
    let sid: Option<i64> = sqlx::query_scalar("SELECT screen_id FROM extensions WHERE id = ?")
        .bind(id).fetch_optional(pool).await?.flatten();
    sqlx::query("DELETE FROM extensions WHERE id = ?").bind(id)
        .execute(pool).await?;
    if let Some(sid) = sid {
        crate::screens::delete(pool, sid).await?;
    }
    Ok(sid)
}

/// Create an extension by kind: validate cardinality, create its screen row,
/// render immediately from existing snapshots. Returns (extension_id, screen_id).
pub async fn create(pool: &SqlitePool, backends: &crate::cache::Backends, cache: &crate::cache::BmpCache, kind: &str, name: &str, settings: Value) -> anyhow::Result<(i64, i64)> {
    match kind {
        "family_calendar" => family_calendar::create(pool, backends, cache, name, settings).await,
        "weather" => weather::create(pool, backends, cache, name, settings).await,
        "clock" => clock::create(pool, backends, cache, name, settings).await,
        other => anyhow::bail!("unknown extension kind '{other}' (supported: family_calendar, weather, clock)"),
    }
}
