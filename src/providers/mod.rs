//! Providers: leaf data dependencies implementing capability shapes.
//!
//! Design (AFFiNE: "mntr: Providers & Extensions Design"): providers publish
//! normalized snapshots on their own refresh cadence (default 1h); extensions
//! declare a shape + cardinality and consume merged snapshots. Shapes are
//! typed interfaces, not vendors — a screen never knows if CalDAV or Google
//! answered.
//!
//! One module per provider under `providers/` (today: `caldav`). Each provider
//! registers a `Provider` impl in [`registry`]: kind string, config
//! validation, and a fetch that returns normalized shape rows. Adding a
//! provider = new module + one line in the registry.

use chrono::Utc;
use serde::Serialize;
use sqlx::Row;
use sqlx::SqlitePool;
use std::future::Future;
use std::pin::Pin;

pub mod caldav;

pub const DEFAULT_REFRESH_SECS: i64 = 3600;

/// One normalized calendar event — the CalendarSource shape's row type.
#[derive(Debug, Clone, Serialize)]
pub struct CalendarEvent {
    pub uid: String,
    /// ISO UTC datetime, or `YYYY-MM-DD` when all_day.
    pub start: String,
    pub all_day: bool,
    pub title: String,
    pub location: String,
    pub description: String,
}

pub const MIGRATE: &str = r#"
CREATE TABLE IF NOT EXISTS providers (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  config TEXT NOT NULL DEFAULT '{}',
  refresh_secs INTEGER NOT NULL DEFAULT 3600,
  last_ok_at TEXT,
  last_error TEXT
);
CREATE TABLE IF NOT EXISTS provider_events (
  provider_id INTEGER NOT NULL,
  uid TEXT NOT NULL,
  start TEXT NOT NULL,
  all_day INTEGER NOT NULL DEFAULT 0,
  title TEXT NOT NULL,
  location TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS extensions (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  settings TEXT NOT NULL DEFAULT '{}',
  screen_id INTEGER
);
"#;

/// Household timezone: every wall-clock interpretation happens here.
pub fn la() -> chrono_tz::Tz {
    "America/Los_Angeles".parse().expect("static tz")
}

/// A provider kind (not an instance): validates config and fetches snapshots.
/// Implementations live one-per-module (see [`caldav`]).
pub trait Provider: Send + Sync {
    fn kind(&self) -> &'static str;
    /// Validate a config JSON at create/patch time — bad config fails in the
    /// admin UI, not at render time.
    fn validate(&self, config: &serde_json::Value) -> anyhow::Result<()>;
    /// Fetch a fresh CalendarSource snapshot for one configured instance.
    fn fetch(&self, config: serde_json::Value)
        -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<CalendarEvent>>> + Send>>;
}

/// The provider registry: kind string -> impl. One line per provider module.
pub fn registry() -> &'static std::collections::HashMap<&'static str, Box<dyn Provider>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<HashMap<&'static str, Box<dyn Provider>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(caldav::CalDav)];
        providers.into_iter().map(|p| (p.kind(), p)).collect()
    })
}

pub fn lookup(kind: &str) -> Option<&'static dyn Provider> {
    registry().get(kind).map(|b| b.as_ref())
}

/// Validate a provider config for a kind (admin API entry point).
pub fn validate_config(kind: &str, config: &serde_json::Value) -> anyhow::Result<()> {
    lookup(kind)
        .ok_or_else(|| anyhow::anyhow!("unknown provider kind '{kind}' (known: {})", registry().keys().cloned().collect::<Vec<_>>().join(", ")))?
        .validate(config)
}

// ---------------------------------------------------------------------------
// `providers` + `provider_events` table owner: CRUD for the admin API.
// ---------------------------------------------------------------------------

use serde_json::{json, Value};
use sqlx::AssertSqlSafe;

const PROVIDER_COLS: &str = "id, kind, name, config, refresh_secs, last_ok_at, last_error";

fn provider_json(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "id": row.get::<i64, _>(0),
        "kind": row.get::<String, _>(1),
        "name": row.get::<String, _>(2),
        "config": serde_json::from_str::<Value>(&row.get::<String, _>(3)).unwrap_or(Value::Null),
        "refresh_secs": row.get::<i64, _>(4),
        "last_ok_at": row.get::<Option<String>, _>(5),
        "last_error": row.get::<Option<String>, _>(6),
    })
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(AssertSqlSafe(format!("SELECT {PROVIDER_COLS} FROM providers ORDER BY id")))
        .fetch_all(pool).await?;
    Ok(rows.iter().map(provider_json).collect())
}

pub async fn create(pool: &SqlitePool, kind: &str, name: &str, config: &Value, refresh_secs: i64) -> anyhow::Result<i64> {
    let id = sqlx::query("INSERT INTO providers (kind, name, config, refresh_secs) VALUES (?, ?, ?, ?)")
        .bind(kind).bind(name).bind(config.to_string()).bind(refresh_secs)
        .execute(pool).await?
        .last_insert_rowid();
    Ok(id)
}

pub async fn get(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Value>> {
    let row = sqlx::query(AssertSqlSafe(format!("SELECT {PROVIDER_COLS} FROM providers WHERE id = ?")))
        .bind(id).fetch_optional(pool).await?;
    Ok(row.map(|r| provider_json(&r)))
}

pub async fn event_count(pool: &SqlitePool, id: i64) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM provider_events WHERE provider_id = ?")
        .bind(id).fetch_one(pool).await?)
}

pub async fn exists(pool: &SqlitePool, id: i64) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM providers WHERE id = ?")
        .bind(id).fetch_one(pool).await? > 0)
}

pub async fn kind_of(pool: &SqlitePool, id: i64) -> anyhow::Result<String> {
    Ok(sqlx::query_scalar("SELECT kind FROM providers WHERE id = ?").bind(id).fetch_one(pool).await?)
}

/// PATCH helper: update present fields only (name / refresh_secs / config).
pub async fn patch(pool: &SqlitePool, id: i64, name: Option<&str>, refresh_secs: Option<i64>, config: Option<&Value>) -> anyhow::Result<()> {
    if let Some(name) = name {
        sqlx::query("UPDATE providers SET name = ? WHERE id = ?").bind(name).bind(id)
            .execute(pool).await?;
    }
    if let Some(secs) = refresh_secs {
        sqlx::query("UPDATE providers SET refresh_secs = ? WHERE id = ?")
            .bind(secs).bind(id).execute(pool).await?;
    }
    if let Some(config) = config {
        sqlx::query("UPDATE providers SET config = ? WHERE id = ?")
            .bind(config.to_string()).bind(id).execute(pool).await?;
    }
    Ok(())
}

/// All calendar-provider ids in creation order (selector default: `all`).
pub async fn calendar_provider_ids(pool: &SqlitePool) -> anyhow::Result<Vec<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM providers WHERE kind = 'caldav' ORDER BY id")
        .fetch_all(pool).await?)
}

/// Merged snapshot rows across providers (cross-table read join lives with
/// the provider_events owner): (start, all_day, title, location, description, source_name).
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
    Ok(rows.iter().map(|r| {
        (r.get(0), r.get::<i64, _>(1) != 0, r.get(2), r.get(3), r.get(4), r.get(5))
    }).collect())
}

pub async fn delete(pool: &SqlitePool, id: i64) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM provider_events WHERE provider_id = ?").bind(id)
        .execute(pool).await?;
    sqlx::query("DELETE FROM providers WHERE id = ?").bind(id)
        .execute(pool).await?;
    Ok(())
}

/// Validate that every id in `ids` exists as a caldav provider (cardinality
/// check for extension settings).
pub async fn all_caldav_ids_exist(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<bool> {
    if ids.is_empty() {
        return Ok(true);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut q = sqlx::query(AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM providers WHERE kind = 'caldav' AND id IN ({placeholders})")));
    for id in ids {
        q = q.bind(*id);
    }
    let n: i64 = q.fetch_one(pool).await?.get(0);
    Ok(n == ids.len() as i64)
}

// ---------------------------------------------------------------------------
// Snapshot store + refresh loop
// ---------------------------------------------------------------------------

/// Replace one provider's snapshot atomically-ish (delete+insert).
pub async fn store_snapshot(pool: &SqlitePool, provider_id: i64, events: &[CalendarEvent]) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM provider_events WHERE provider_id = ?").bind(provider_id)
        .execute(&mut *tx).await?;
    for e in events {
        sqlx::query("INSERT INTO provider_events (provider_id, uid, start, all_day, title, location, description) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(provider_id).bind(&e.uid).bind(&e.start).bind(e.all_day as i64)
            .bind(&e.title).bind(&e.location).bind(&e.description)
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Refresh one provider by id: fetch via its kind's impl, store snapshot,
/// record outcome. Failure keeps the last good snapshot (design: no backoff
/// machinery).
pub async fn refresh_provider(pool: &SqlitePool, id: i64) -> anyhow::Result<usize> {
    let row = sqlx::query("SELECT kind, config FROM providers WHERE id = ?").bind(id)
        .fetch_optional(pool).await?
        .ok_or_else(|| anyhow::anyhow!("provider {id} not found"))?;
    let kind: String = row.get(0);
    let config: String = row.get(1);
    let result: anyhow::Result<usize> = match lookup(&kind) {
        Some(p) => {
            let config = serde_json::from_str(&config).unwrap_or(serde_json::Value::Null);
            let events = p.fetch(config).await?;
            let n = events.len();
            store_snapshot(pool, id, &events).await?;
            Ok(n)
        }
        None => Err(anyhow::anyhow!("unknown provider kind {kind}")),
    };
    match result {
        Ok(n) => {
            sqlx::query("UPDATE providers SET last_ok_at = datetime('now'), last_error = NULL WHERE id = ?")
                .bind(id).execute(pool).await?;
            tracing::info!(provider = id, events = n, "provider refreshed");
            Ok(n)
        }
        Err(e) => {
            sqlx::query("UPDATE providers SET last_error = ? WHERE id = ?")
                .bind(e.to_string()).bind(id).execute(pool).await?;
            Err(e)
        }
    }
}

/// Background provider loop: every `tick`, refresh providers whose interval
/// elapsed, then rebuild extension screens if anything changed.
pub fn spawn_provider_loop(pool: SqlitePool, cache: crate::cache::BmpCache, tick: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(tick);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let rows = sqlx::query("SELECT id, refresh_secs, last_ok_at FROM providers").fetch_all(&pool).await;
            let Ok(rows) = rows else { continue };
            let mut changed = false;
            for row in rows {
                let id: i64 = row.get(0);
                let secs: i64 = row.get(1);
                let last: Option<String> = row.get(2);
                let due = match last {
                    None => true,
                    Some(t) => chrono::NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S")
                        .map(|t| (Utc::now().naive_utc() - t).num_seconds() >= secs)
                        .unwrap_or(true),
                };
                if due {
                    match refresh_provider(&pool, id).await {
                        Ok(_) => changed = true,
                        Err(e) => tracing::warn!(%e, provider = id, "provider refresh failed"),
                    }
                }
            }
            if changed {
                if let Err(e) = crate::extensions::rebuild_extensions(&pool, &cache).await {
                    tracing::warn!(%e, "extension rebuild failed");
                }
            }
        }
    })
}
