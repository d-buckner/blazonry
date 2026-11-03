// SPDX-License-Identifier: AGPL-3.0-only
//! Extensions: consumers of provider shapes. One module per extension under
//! `extensions/` (today: `calendar`). Each extension declares its
//! required shape + cardinality and turns merged snapshots into a screen.
//!
//! The framework here is generic: selector resolution (all | explicit pick
//! list — no tags, no expressions), snapshot merge, and rebuild dispatch.
//! Adding an extension = new module + one match arm in [`rebuild_extension`]
//! and [`create`].
//!
//! Two framework services read the same kind→shape table so consumers never
//! re-derive it: [`referenced_provider_ids`] (which providers any screen
//! actually consumes — the refresh loop fetches nothing else) and
//! [`rebuild_extensions_shapes`] (rebuild only screens whose consumed shape
//! changed).

use serde_json::Value;
use sqlx::SqlitePool;

pub mod calendar;
pub mod countdown;
pub mod weather;

pub const MIGRATE: &str = crate::providers::MIGRATE;

/// The shape an extension kind consumes, if any (countdown computes from the
/// calendar date; no provider).
/// This is the single kind→shape table — reference resolution and rebuild
/// filtering both read it, so they can never disagree.
pub fn consumes_shape(kind: &str) -> Option<&'static str> {
    match kind {
        "calendar" => Some("calendar"),
        "weather" => Some("weather"),
        _ => None,
    }
}

/// Resolve an extension's provider pick list for a shape: explicit ids, or
/// all providers of the shape when unset.
pub async fn resolved_provider_ids(
    pool: &SqlitePool,
    settings: &Value,
    shape: &str,
) -> anyhow::Result<Vec<i64>> {
    if let Some(ids) = settings.get("provider_ids").and_then(|v| v.as_array()) {
        let ids: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    crate::providers::provider_ids_by_shape(pool, shape).await
}

/// The set of provider ids actually referenced by existing extensions.
/// A provider no extension consumes is never refreshed — an orphan costs
/// nothing. Weather's singular `provider_id` setting is folded in here so
/// the loop sees one uniform reference set.
pub async fn referenced_provider_ids(
    pool: &SqlitePool,
) -> anyhow::Result<std::collections::HashSet<i64>> {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    for ext in all_rows(pool).await? {
        let Some(shape) = consumes_shape(&ext.kind) else {
            continue;
        };
        let mut ids = resolved_provider_ids(pool, &ext.settings, shape).await?;
        if let Some(pid) = ext.settings.get("provider_id").and_then(|v| v.as_i64()) {
            ids.push(pid);
        }
        set.extend(ids);
    }
    Ok(set)
}

/// Merged calendar snapshot rows for a resolved provider set, sorted by start.
/// Rows: (start, all_day, title, location, description, source_name).
pub async fn merged_calendar_rows(
    pool: &SqlitePool,
    ids: &[i64],
) -> anyhow::Result<Vec<(String, bool, String, String, String, String)>> {
    crate::providers::merged_calendar_rows(pool, ids).await
}

/// Re-render one extension screen into the BMP cache (both panel depths).
async fn repaint(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    screen_id: i64,
) {
    for bpp in [1u16, 2u16] {
        if let Err(e) = crate::cache::render_and_cache(pool, backends, cache, screen_id, bpp).await
        {
            tracing::warn!(%e, screen = screen_id, "extension re-render failed");
        }
    }
}

/// Rebuild every extension's bound screen from current snapshots.
/// Returns the number of extensions rebuilt.
pub async fn rebuild_extensions(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
) -> anyhow::Result<usize> {
    let exts = all_rows(pool).await?;
    let mut n = 0;
    for ext in exts {
        if rebuild_extension(pool, backends, cache, ext).await? {
            n += 1
        }
    }
    Ok(n)
}

/// Rebuild only extensions consuming one of `shapes` — a weather refresh
/// never repaints the calendar, and providerless extensions (no shape) are
/// never touched here. Returns the number of extensions rebuilt.
pub async fn rebuild_extensions_shapes(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    shapes: &std::collections::HashSet<String>,
) -> anyhow::Result<usize> {
    let exts = all_rows(pool).await?;
    let mut n = 0;
    for ext in exts {
        let consumed = consumes_shape(&ext.kind).map(str::to_string);
        let Some(consumed) = consumed else { continue };
        if !shapes.contains(&consumed) {
            continue;
        }
        if rebuild_extension(pool, backends, cache, ext).await? {
            n += 1
        }
    }
    Ok(n)
}

/// Rebuild one extension's bound screen from current snapshots.
/// Returns whether the screen was rebuilt (false = skipped, e.g. no
/// providers resolved — the last paint stays up).
async fn rebuild_extension(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    ext: ExtRow,
) -> anyhow::Result<bool> {
    let ExtRow {
        kind,
        name,
        settings,
        screen_id,
    } = ext;
    let Some(sid) = screen_id else {
        return Ok(false);
    };
    match kind.as_str() {
        "calendar" => {
            let ids = resolved_provider_ids(pool, &settings, "calendar").await?;
            if ids.is_empty() {
                tracing::warn!(
                    extension = name,
                    "no calendar providers resolved; leaving screen as-is"
                );
                return Ok(false);
            }
            let merged = merged_calendar_rows(pool, &ids).await?;
            let vars = calendar::agenda_vars(&merged, &settings);
            let vars_json = serde_json::to_string(&vars)?;
            crate::screens::set_vars(pool, sid, &vars_json).await?;
            repaint(pool, backends, cache, sid).await;
            Ok(true)
        }
        "weather" => {
            let Some(pid) = weather::resolved_provider_id(pool, &settings).await? else {
                tracing::warn!(
                    extension = name,
                    "no weather provider resolved; leaving screen as-is"
                );
                return Ok(false);
            };
            match crate::providers::load_snapshot(pool, pid).await? {
                Some(snap) => {
                    let Snapshot::Weather(payload) = snap else {
                        tracing::warn!(
                            extension = name,
                            "resolved provider does not publish weather; leaving screen as-is"
                        );
                        return Ok(false);
                    };
                    let vars = weather::weather_vars(&payload, &settings);
                    crate::screens::set_vars(pool, sid, &serde_json::to_string(&vars)?).await?;
                    repaint(pool, backends, cache, sid).await;
                    Ok(true)
                }
                None => {
                    let vars = weather::no_data_vars("Waiting for first weather refresh...");
                    crate::screens::set_vars(pool, sid, &serde_json::to_string(&vars)?).await?;
                    repaint(pool, backends, cache, sid).await;
                    Ok(true)
                }
            }
        }
        "countdown" => {
            let vars = countdown::countdown_vars(&settings)?;
            crate::screens::set_vars(pool, sid, &serde_json::to_string(&vars)?).await?;
            repaint(pool, backends, cache, sid).await;
            Ok(true)
        }
        other => {
            tracing::warn!(
                extension = name,
                kind = other,
                "unknown extension kind; skipping"
            );
            Ok(false)
        }
    }
}

use crate::providers::Snapshot;

/// Re-render providerless extensions whose content depends on the local
/// date (countdown). Called once per local-day rollover, never per minute:
/// a screen whose content changes daily must not force a minute refresh
/// cycle (e-ink wear + battery).
pub async fn rebuild_daily(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
) -> anyhow::Result<()> {
    let exts = all_rows(pool).await?;
    for ext in exts {
        if consumes_shape(&ext.kind).is_some() {
            continue;
        }
        if ext.screen_id.is_none() {
            continue;
        };
        if let Err(e) = rebuild_extension(pool, backends, cache, ext).await {
            tracing::warn!(%e, "daily rebuild failed");
        }
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
    let rows = sqlx::query("SELECT kind, name, settings, screen_id FROM extensions")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| ExtRow {
            kind: r.get(0),
            name: r.get(1),
            settings: serde_json::from_str(r.get(2)).unwrap_or(serde_json::json!({})),
            screen_id: r.get(3),
        })
        .collect())
}

pub async fn kind_of(pool: &SqlitePool, id: i64) -> anyhow::Result<String> {
    Ok(
        sqlx::query_scalar("SELECT kind FROM extensions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows =
        sqlx::query("SELECT id, kind, name, settings, screen_id FROM extensions ORDER BY id")
            .fetch_all(pool)
            .await?;
    use sqlx::Row;
    Ok(rows.into_iter().map(|r| serde_json::json!({
        "id": r.get::<i64, _>(0),
        "kind": r.get::<String, _>(1),
        "name": r.get::<String, _>(2),
        "settings": serde_json::from_str::<Value>(&r.get::<String, _>(3)).unwrap_or(Value::Null),
        "screen_id": r.get::<Option<i64>, _>(4),
    })).collect())
}

pub async fn insert(
    pool: &SqlitePool,
    kind: &str,
    name: &str,
    settings: &Value,
    screen_id: i64,
) -> anyhow::Result<i64> {
    Ok(
        sqlx::query("INSERT INTO extensions (kind, name, settings, screen_id) VALUES (?, ?, ?, ?)")
            .bind(kind)
            .bind(name)
            .bind(settings.to_string())
            .bind(screen_id)
            .execute(pool)
            .await?
            .last_insert_rowid(),
    )
}

pub async fn exists(pool: &SqlitePool, id: i64) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM extensions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?
            > 0,
    )
}

pub async fn set_name(pool: &SqlitePool, id: i64, name: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE extensions SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_settings(pool: &SqlitePool, id: i64, settings: &Value) -> anyhow::Result<()> {
    sqlx::query("UPDATE extensions SET settings = ? WHERE id = ?")
        .bind(settings.to_string())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete an extension and its bound screen (screen teardown via its owner).
pub async fn delete(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<i64>> {
    let sid: Option<i64> = sqlx::query_scalar("SELECT screen_id FROM extensions WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .flatten();
    sqlx::query("DELETE FROM extensions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if let Some(sid) = sid {
        crate::screens::delete(pool, sid).await?;
    }
    Ok(sid)
}

/// Create an extension by kind: validate cardinality, create its screen row,
/// render immediately from existing snapshots. Returns (extension_id, screen_id).
pub async fn create(
    pool: &SqlitePool,
    backends: &crate::cache::Backends,
    cache: &crate::cache::BmpCache,
    kind: &str,
    name: &str,
    settings: Value,
) -> anyhow::Result<(i64, i64)> {
    match kind {
        "calendar" => calendar::create(pool, backends, cache, name, settings).await,
        "weather" => weather::create(pool, backends, cache, name, settings).await,
        "countdown" => countdown::create(pool, backends, cache, name, settings).await,
        other => anyhow::bail!(
            "unknown extension kind '{other}' (supported: calendar, weather, countdown)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(crate::providers::MIGRATE)
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn referenced_ids_exclude_orphans_and_honor_explicit_picks() {
        let pool = test_pool().await;
        let cal1 =
            crate::providers::create(&pool, "caldav", "A", &json!({"url": "http://x/"}), 3600)
                .await
                .unwrap();
        let cal2 =
            crate::providers::create(&pool, "caldav", "B", &json!({"url": "http://y/"}), 3600)
                .await
                .unwrap();
        let met = crate::providers::create(
            &pool,
            "open_meteo",
            "W",
            &json!({"latitude": 1.0, "longitude": 2.0}),
            3600,
        )
        .await
        .unwrap();

        // No extensions: nothing is referenced, even though providers exist.
        assert!(referenced_provider_ids(&pool).await.unwrap().is_empty());

        // A calendar extension with default selector references all calendar
        // providers but never the weather one.
        insert(&pool, "calendar", "Cal", &json!({}), 1)
            .await
            .unwrap();
        let refs = referenced_provider_ids(&pool).await.unwrap();
        assert!(refs.contains(&cal1) && refs.contains(&cal2) && !refs.contains(&met));

        // Explicit pick narrows the set; the un-picked provider is orphaned.
        set_settings(&pool, 1, &json!({"provider_ids": [cal1]}))
            .await
            .unwrap();
        let refs = referenced_provider_ids(&pool).await.unwrap();
        assert!(refs.contains(&cal1) && !refs.contains(&cal2));

        // Weather's singular provider_id is folded into the same set.
        insert(&pool, "weather", "Wx", &json!({"provider_id": met}), 2)
            .await
            .unwrap();
        let refs = referenced_provider_ids(&pool).await.unwrap();
        assert!(refs.contains(&cal1) && refs.contains(&met) && !refs.contains(&cal2));
    }
}
