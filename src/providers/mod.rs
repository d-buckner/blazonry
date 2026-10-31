//! Providers: leaf data dependencies implementing capability shapes.
//!
//! Design (AFFiNE: "mntr: Providers & Extensions Design"): providers publish
//! normalized snapshots on their own refresh cadence (default 1h); extensions
//! declare a shape + cardinality and consume merged snapshots. Shapes are
//! typed interfaces, not vendors — a screen never knows if CalDAV or Google
//! answered.
//!
//! The interface contract, in one sentence: **providers own the payload, the
//! framework owns the envelope.** A provider's `fetch` returns a normalized
//! payload for exactly one shape; the framework stamps provenance (provider
//! id), freshness (`fetched_at`), and change detection (`content_hash`) —
//! providers never see any of that. Storage is one shape-generic table
//! (`provider_snapshots`), so adding a provider kind costs one module + one
//! registry line, and adding a *shape* costs one `Snapshot` variant with its
//! payload round-trip — no tables, no vendor literals in SQL.
//!
//! One module per provider under `providers/` (today: `caldav`, `open_meteo`).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use sqlx::SqlitePool;
use std::future::Future;
use std::pin::Pin;

pub mod caldav;
pub mod open_meteo;

pub const DEFAULT_REFRESH_SECS: i64 = 3600;

/// One normalized calendar event — the CalendarSource shape's row type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub uid: String,
    /// ISO UTC datetime, or `YYYY-MM-DD` when all_day.
    pub start: String,
    pub all_day: bool,
    pub title: String,
    pub location: String,
    pub description: String,
}

/// What a provider publishes. Shapes are typed interfaces, not vendors.
///
/// Every variant round-trips through a JSON payload (`payload` /
/// `from_payload`): the DB, the hash, and cross-provider merges all speak
/// payload, never the variant's internals. Adding a shape = one variant +
/// its arms in `shape`, `payload`, `from_payload`, `item_count`.
#[derive(Debug, Clone)]
pub enum Snapshot {
    Calendar(Vec<CalendarEvent>),
    /// WeatherSource: one JSON object (current + days), see open_meteo.
    Weather(serde_json::Value),
}

impl Snapshot {
    /// Shape string this snapshot publishes ("calendar" | "weather").
    pub fn shape(&self) -> &'static str {
        match self {
            Snapshot::Calendar(_) => "calendar",
            Snapshot::Weather(_) => "weather",
        }
    }

    /// Canonical payload (envelope-free: no fetched_at, no provider id).
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Snapshot::Calendar(events) => serde_json::to_value(events)
                .expect("CalendarEvent is Serialize by construction"),
            Snapshot::Weather(v) => v.clone(),
        }
    }

    /// Inverse of `payload` — unknown shapes fail here, at the boundary.
    pub fn from_payload(shape: &str, payload: serde_json::Value) -> anyhow::Result<Self> {
        match shape {
            "calendar" => Ok(Snapshot::Calendar(serde_json::from_value(payload)?)),
            "weather" => Ok(Snapshot::Weather(payload)),
            other => anyhow::bail!("unknown snapshot shape '{other}'"),
        }
    }

    /// Rows this snapshot holds (admin display: events for calendar, 1 for weather).
    pub fn item_count(&self) -> usize {
        match self {
            Snapshot::Calendar(events) => events.len(),
            Snapshot::Weather(_) => 1,
        }
    }

    /// Content hash for change detection: sha256 over the canonical payload.
    /// serde_json serializes object keys in sorted order, so the hash is
    /// stable regardless of how the provider built the value.
    pub fn content_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.payload().to_string());
        let out = h.finalize();
        out.iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub const MIGRATE: &str = r#"
CREATE TABLE IF NOT EXISTS providers (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  shape TEXT NOT NULL DEFAULT '',
  name TEXT NOT NULL,
  config TEXT NOT NULL DEFAULT '{}',
  refresh_secs INTEGER NOT NULL DEFAULT 3600,
  last_ok_at TEXT,
  last_error TEXT
);
CREATE TABLE IF NOT EXISTS provider_snapshots (
  provider_id INTEGER PRIMARY KEY,
  shape TEXT NOT NULL,
  payload TEXT NOT NULL,
  items INTEGER NOT NULL DEFAULT 0,
  content_hash TEXT NOT NULL,
  fetched_at TEXT NOT NULL
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
/// Implementations live one-per-module (see [`caldav`]). A provider returns
/// ONLY its normalized payload — the framework owns freshness, provenance,
/// and change detection (see module docs).
pub trait Provider: Send + Sync {
    fn kind(&self) -> &'static str;
    /// Shape this kind publishes: "calendar" | "weather".
    fn shape(&self) -> &'static str;
    /// Validate a config JSON at create/patch time — bad config fails in the
    /// admin UI, not at render time.
    fn validate(&self, config: &serde_json::Value) -> anyhow::Result<()>;
    /// Fetch a fresh snapshot for one configured instance.
    fn fetch(&self, config: serde_json::Value)
        -> Pin<Box<dyn Future<Output = anyhow::Result<Snapshot>> + Send>>;
}

/// The provider registry: kind string -> impl. One line per provider module.
pub fn registry() -> &'static std::collections::HashMap<&'static str, Box<dyn Provider>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<HashMap<&'static str, Box<dyn Provider>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(caldav::CalDav), Box::new(open_meteo::OpenMeteo)];
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
// `providers` + `provider_snapshots` table owner: CRUD for the admin API.
// ---------------------------------------------------------------------------

use serde_json::{json, Value};
use sqlx::AssertSqlSafe;

const PROVIDER_COLS: &str = "id, kind, shape, name, config, refresh_secs, last_ok_at, last_error";

fn provider_json(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "id": row.get::<i64, _>(0),
        "kind": row.get::<String, _>(1),
        "shape": row.get::<String, _>(2),
        "name": row.get::<String, _>(3),
        "config": serde_json::from_str::<Value>(&row.get::<String, _>(4)).unwrap_or(Value::Null),
        "refresh_secs": row.get::<i64, _>(5),
        "last_ok_at": row.get::<Option<String>, _>(6),
        "last_error": row.get::<Option<String>, _>(7),
    })
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(AssertSqlSafe(format!("SELECT {PROVIDER_COLS} FROM providers ORDER BY id")))
        .fetch_all(pool).await?;
    Ok(rows.iter().map(provider_json).collect())
}

/// Create a provider instance. `shape` is derived from the registry —
/// callers cannot store a kind/shape mismatch (enforced at the boundary).
pub async fn create(pool: &SqlitePool, kind: &str, name: &str, config: &Value, refresh_secs: i64) -> anyhow::Result<i64> {
    let shape = lookup(kind)
        .ok_or_else(|| anyhow::anyhow!("unknown provider kind '{kind}'"))?
        .shape();
    let id = sqlx::query("INSERT INTO providers (kind, shape, name, config, refresh_secs) VALUES (?, ?, ?, ?, ?)")
        .bind(kind).bind(shape).bind(name).bind(config.to_string()).bind(refresh_secs)
        .execute(pool).await?
        .last_insert_rowid();
    Ok(id)
}

pub async fn get(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Value>> {
    let row = sqlx::query(AssertSqlSafe(format!("SELECT {PROVIDER_COLS} FROM providers WHERE id = ?")))
        .bind(id).fetch_optional(pool).await?;
    Ok(row.map(|r| provider_json(&r)))
}

/// Snapshot item count for the admin view (events for calendar, 1 for weather).
pub async fn snapshot_items(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<usize>> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT items FROM provider_snapshots WHERE provider_id = ?")
        .bind(id).fetch_optional(pool).await?
        .map(|n| n as usize))
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
            .bind(secs).bind(id)
            .execute(pool).await?;
    }
    if let Some(config) = config {
        sqlx::query("UPDATE providers SET config = ? WHERE id = ?")
            .bind(config.to_string()).bind(id)
            .execute(pool).await?;
    }
    Ok(())
}

/// All provider ids publishing a shape, in creation order (selector default:
/// `all`). Shape-generic: no vendor kind appears in SQL below this line.
pub async fn provider_ids_by_shape(pool: &SqlitePool, shape: &str) -> anyhow::Result<Vec<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM providers WHERE shape = ? ORDER BY id")
        .bind(shape).fetch_all(pool).await?)
}

/// Validate that every id in `ids` exists and publishes `shape`
/// (cardinality check for extension settings; one function for all shapes).
pub async fn ids_with_shape_exist(pool: &SqlitePool, shape: &str, ids: &[i64]) -> anyhow::Result<bool> {
    if ids.is_empty() {
        return Ok(true);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut q = sqlx::query(AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM providers WHERE shape = ? AND id IN ({placeholders})")));
    q = q.bind(shape);
    for id in ids {
        q = q.bind(*id);
    }
    let n: i64 = q.fetch_one(pool).await?.get(0);
    Ok(n == ids.len() as i64)
}

/// Merged calendar events across providers, sorted by start (all-day first
/// within a day falls out of the `start` sort: `YYYY-MM-DD` < any datetime).
/// Rows: (start, all_day, title, location, description, source_name).
pub async fn merged_calendar_rows(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<Vec<(String, bool, String, String, String, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut q = sqlx::query(AssertSqlSafe(format!(
        "SELECT ps.payload, p.name FROM provider_snapshots ps \
         JOIN providers p ON p.id = ps.provider_id \
         WHERE ps.provider_id IN ({placeholders}) AND ps.shape = 'calendar'"
    )));
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    let mut out: Vec<(String, bool, String, String, String, String)> = Vec::new();
    for r in rows {
        let payload: String = r.get(0);
        let source: String = r.get(1);
        let snap = Snapshot::from_payload("calendar", serde_json::from_str(&payload)?)?;
        let Snapshot::Calendar(events) = snap else { unreachable!("shape-filtered query") };
        for e in events {
            out.push((e.start, e.all_day, e.title, e.location, e.description, source.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

pub async fn delete(pool: &SqlitePool, id: i64) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM provider_snapshots WHERE provider_id = ?").bind(id)
        .execute(pool).await?;
    sqlx::query("DELETE FROM providers WHERE id = ?").bind(id)
        .execute(pool).await?;
    Ok(())
}

/// Backfill `shape` for rows created before the column existed. Idempotent;
/// runs once at startup so the shape column is a total invariant everywhere
/// downstream (no fallback inference in queries).
pub async fn backfill_shapes(pool: &SqlitePool) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id, kind FROM providers WHERE shape = ''").fetch_all(pool).await?;
    for r in rows {
        let id: i64 = r.get(0);
        let kind: String = r.get(1);
        let shape = lookup(&kind).map(|p| p.shape()).unwrap_or("");
        sqlx::query("UPDATE providers SET shape = ? WHERE id = ?").bind(shape).bind(id)
            .execute(pool).await?;
    }
    Ok(())
}

/// One-time upgrade: fold the pre-snapshot-store tables (`provider_events`,
/// `provider_weather`) into `provider_snapshots`, then drop them. Fresh DBs
/// never had them — absence is the normal case, checked via sqlite_master.
pub async fn migrate_legacy_snapshots(pool: &SqlitePool) -> anyhow::Result<()> {
    let has: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('provider_events','provider_weather')")
        .fetch_one(pool).await?;
    if has == 0 {
        return Ok(());
    }
    // Calendar rows -> payload. Old table may hold several rows per provider.
    if table_exists(pool, "provider_events").await? {
        let rows = sqlx::query(
            "SELECT provider_id, uid, start, all_day, title, location, description FROM provider_events ORDER BY provider_id, start")
            .fetch_all(pool).await?;
        let mut by_provider: std::collections::BTreeMap<i64, Vec<CalendarEvent>> = Default::default();
        for r in rows {
            use sqlx::Row;
            by_provider.entry(r.get(0)).or_default().push(CalendarEvent {
                uid: r.get(1), start: r.get(2), all_day: r.get::<i64, _>(3) != 0,
                title: r.get(4), location: r.get(5), description: r.get(6),
            });
        }
        for (pid, events) in by_provider {
            let snap = Snapshot::Calendar(events);
            sqlx::query("INSERT OR IGNORE INTO provider_snapshots (provider_id, shape, payload, items, content_hash, fetched_at) \
                         VALUES (?, ?, ?, ?, ?, datetime('now'))")
                .bind(pid).bind(snap.shape()).bind(snap.payload().to_string())
                .bind(snap.item_count() as i64).bind(snap.content_hash())
                .execute(pool).await?;
        }
        sqlx::query("DROP TABLE provider_events").execute(pool).await?;
    }
    if table_exists(pool, "provider_weather").await? {
        let rows = sqlx::query("SELECT provider_id, payload FROM provider_weather").fetch_all(pool).await?;
        for r in rows {
            use sqlx::Row;
            let pid: i64 = r.get(0);
            let payload: String = r.get(1);
            let snap = Snapshot::Weather(serde_json::from_str(&payload).unwrap_or(Value::Null));
            sqlx::query("INSERT OR IGNORE INTO provider_snapshots (provider_id, shape, payload, items, content_hash, fetched_at) \
                         VALUES (?, ?, ?, ?, ?, datetime('now'))")
                .bind(pid).bind(snap.shape()).bind(snap.payload().to_string())
                .bind(snap.item_count() as i64).bind(snap.content_hash())
                .execute(pool).await?;
        }
        sqlx::query("DROP TABLE provider_weather").execute(pool).await?;
    }
    tracing::info!("legacy provider snapshot tables migrated into provider_snapshots");
    Ok(())
}

async fn table_exists(pool: &SqlitePool, name: &str) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?")
        .bind(name).fetch_one(pool).await? > 0)
}

// ---------------------------------------------------------------------------
// Snapshot store + refresh loop
// ---------------------------------------------------------------------------

/// Store one provider's snapshot, replacing the previous one. Returns
/// (item_count, changed): `changed` is false when the content hash matches
/// the stored snapshot, which lets callers skip downstream rebuilds (e-ink
/// full refreshes are the panel's rated lifetime — never spend one on
/// identical pixels).
pub async fn store_snapshot(pool: &SqlitePool, provider_id: i64, snap: &Snapshot) -> anyhow::Result<(usize, bool)> {
    let prev: Option<String> = sqlx::query_scalar("SELECT content_hash FROM provider_snapshots WHERE provider_id = ?")
        .bind(provider_id).fetch_optional(pool).await?;
    let hash = snap.content_hash();
    let changed = prev.as_deref() != Some(hash.as_str());
    if changed {
        sqlx::query("INSERT INTO provider_snapshots (provider_id, shape, payload, items, content_hash, fetched_at) \
                     VALUES (?, ?, ?, ?, ?, datetime('now')) \
                     ON CONFLICT(provider_id) DO UPDATE SET shape = excluded.shape, payload = excluded.payload, \
                     items = excluded.items, content_hash = excluded.content_hash, fetched_at = excluded.fetched_at")
            .bind(provider_id).bind(snap.shape()).bind(snap.payload().to_string())
            .bind(snap.item_count() as i64).bind(&hash)
            .execute(pool).await?;
    }
    Ok((snap.item_count(), changed))
}

/// Latest snapshot for one provider, decoded from its stored payload.
pub async fn load_snapshot(pool: &SqlitePool, provider_id: i64) -> anyhow::Result<Option<Snapshot>> {
    let row = sqlx::query("SELECT shape, payload FROM provider_snapshots WHERE provider_id = ?")
        .bind(provider_id).fetch_optional(pool).await?;
    let Some(r) = row else { return Ok(None) };
    let shape: String = r.get(0);
    let payload: String = r.get(1);
    match serde_json::from_str::<Value>(&payload) {
        Ok(v) => Snapshot::from_payload(&shape, v).map(Some),
        Err(e) => {
            tracing::warn!(provider = provider_id, %e, "stored snapshot payload unreadable; treating as absent");
            Ok(None)
        }
    }
}

/// Outcome of one provider refresh: how many items the snapshot holds, its
/// shape, and whether its content actually changed since the last refresh.
pub struct Refreshed {
    pub items: usize,
    pub shape: String,
    pub changed: bool,
}

/// Refresh one provider by id: fetch via its kind's impl, store snapshot,
/// record outcome. Failure keeps the last good snapshot (design: no backoff
/// machinery).
pub async fn refresh_provider(pool: &SqlitePool, id: i64) -> anyhow::Result<Refreshed> {
    let row = sqlx::query("SELECT kind, config FROM providers WHERE id = ?").bind(id)
        .fetch_optional(pool).await?
        .ok_or_else(|| anyhow::anyhow!("provider {id} not found"))?;
    let kind: String = row.get(0);
    let config: String = row.get(1);
    let result: anyhow::Result<Refreshed> = match lookup(&kind) {
        Some(p) => {
            let config = serde_json::from_str(&config).unwrap_or(serde_json::Value::Null);
            let snap = p.fetch(config).await?;
            let (items, changed) = store_snapshot(pool, id, &snap).await?;
            Ok(Refreshed { items, shape: snap.shape().to_string(), changed })
        }
        None => Err(anyhow::anyhow!("unknown provider kind {kind}")),
    };
    match result {
        Ok(r) => {
            sqlx::query("UPDATE providers SET last_ok_at = datetime('now'), last_error = NULL WHERE id = ?")
                .bind(id).execute(pool).await?;
            tracing::info!(provider = id, items = r.items, changed = r.changed, "provider refreshed");
            Ok(r)
        }
        Err(e) => {
            sqlx::query("UPDATE providers SET last_error = ? WHERE id = ?")
                .bind(e.to_string()).bind(id).execute(pool).await?;
            Err(e)
        }
    }
}

/// How long one provider fetch may take before the round gives up on it.
/// Provider HTTP clients time out at 30s; this wrapper catches everything
/// else (DNS hangs, wedged TCP) so one dead server can never stall the round.
pub const REFRESH_TIMEOUT_SECS: u64 = 45;

/// How many providers may fetch concurrently. Small on purpose: a household
/// box should not stampede its LAN or a rate-limited API.
pub const REFRESH_CONCURRENCY: usize = 4;

/// Background provider loop. Every `tick`:
/// 1. resolve the provider set actually referenced by extensions — providers
///    no screen consumes are never fetched (an orphan costs nothing);
/// 2. refresh due providers concurrently (bounded) with a per-provider timeout;
/// 3. rebuild only the extension screens whose consumed shape changed —
///    a weather refresh never repaints the calendar, and an unchanged
///    snapshot never repaints anything.
/// Clocks are live regardless: repainted every tick.
pub fn spawn_provider_loop(pool: SqlitePool, backends: crate::cache::Backends, cache: crate::cache::BmpCache, tick: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(tick);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let referenced = match crate::extensions::referenced_provider_ids(&pool).await {
                Ok(set) => set,
                Err(e) => { tracing::warn!(%e, "provider reference resolution failed"); continue }
            };
            let rows = sqlx::query("SELECT id, refresh_secs, last_ok_at FROM providers").fetch_all(&pool).await;
            let Ok(rows) = rows else { continue };
            let due: Vec<i64> = rows.into_iter().filter_map(|row| {
                let id: i64 = row.get(0);
                if !referenced.contains(&id) { return None }
                let secs: i64 = row.get(1);
                let last: Option<String> = row.get(2);
                let due = match last {
                    None => true,
                    Some(t) => chrono::NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S")
                        .map(|t| (Utc::now().naive_utc() - t).num_seconds() >= secs)
                        .unwrap_or(true),
                };
                due.then_some(id)
            }).collect();

            let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(REFRESH_CONCURRENCY));
            let mut set = tokio::task::JoinSet::new();
            for id in due {
                let pool = pool.clone();
                let sem = sem.clone();
                set.spawn(async move {
                    let _permit = match sem.acquire().await { Ok(p) => p, Err(_) => return (id, None) };
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(REFRESH_TIMEOUT_SECS),
                        refresh_provider(&pool, id),
                    ).await {
                        Ok(Ok(r)) => (id, if r.changed { Some(r.shape) } else { None }),
                        Ok(Err(e)) => { tracing::warn!(%e, provider = id, "provider refresh failed"); (id, None) }
                        Err(_) => { tracing::warn!(provider = id, timeout = REFRESH_TIMEOUT_SECS, "provider refresh timed out"); (id, None) }
                    }
                });
            }
            let mut changed_shapes: std::collections::HashSet<String> = Default::default();
            while let Some(Ok((_id, shape))) = set.join_next().await {
                if let Some(s) = shape { changed_shapes.insert(s); }
            }
            if !changed_shapes.is_empty() {
                match crate::extensions::rebuild_extensions_shapes(&pool, &backends, &cache, &changed_shapes).await {
                    Ok(n) => tracing::info!(shapes = ?changed_shapes, screens = n, "extension rebuild"),
                    Err(e) => tracing::warn!(%e, "extension rebuild failed"),
                }
            }
            // Clocks are live regardless of provider activity: repaint every tick.
            if let Err(e) = crate::extensions::rebuild_clocks(&pool, &backends, &cache).await {
                tracing::warn!(%e, "clock rebuild failed");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cal(title: &str, start: &str) -> CalendarEvent {
        CalendarEvent { uid: title.into(), start: start.into(), all_day: start.len() == 10, title: title.into(), location: "".into(), description: "".into() }
    }

    #[test]
    fn calendar_roundtrips_through_payload() {
        let snap = Snapshot::Calendar(vec![cal("yoga", "2026-11-01"), cal("call", "2026-11-01T17:00:00Z")]);
        let back = Snapshot::from_payload("calendar", snap.payload()).unwrap();
        let Snapshot::Calendar(evs) = back else { panic!("wrong variant") };
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].title, "call");
    }

    #[test]
    fn weather_roundtrips_and_rejects_unknown_shape() {
        let snap = Snapshot::Weather(json!({"current": {"temp": 55}}));
        let back = Snapshot::from_payload("weather", snap.payload()).unwrap();
        assert_eq!(back.payload()["current"]["temp"], 55);
        assert!(Snapshot::from_payload("stocks", json!({})).is_err());
    }

    #[test]
    fn content_hash_is_key_order_independent() {
        let a = Snapshot::Weather(json!({"x": 1, "y": {"p": 2, "q": 3}}));
        let b = Snapshot::Weather(json!({"y": {"q": 3, "p": 2}, "x": 1}));
        assert_eq!(a.content_hash(), b.content_hash());
        let c = Snapshot::Weather(json!({"x": 2, "y": {"p": 2, "q": 3}}));
        assert_ne!(a.content_hash(), c.content_hash());
    }

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(MIGRATE).execute(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn store_snapshot_flags_change_only_on_content_diff() {
        let pool = test_pool().await;
        let snap = Snapshot::Calendar(vec![cal("a", "2026-11-01")]);
        let (n, changed) = store_snapshot(&pool, 1, &snap).await.unwrap();
        assert_eq!((n, changed), (1, true));
        // identical content again: stored hash matches, nothing to rebuild
        let (n2, changed2) = store_snapshot(&pool, 1, &snap).await.unwrap();
        assert_eq!((n2, changed2), (1, false));
        // content changed: flagged again
        let snap2 = Snapshot::Calendar(vec![cal("a", "2026-11-01"), cal("b", "2026-11-02")]);
        let (_, changed3) = store_snapshot(&pool, 1, &snap2).await.unwrap();
        assert!(changed3);
        let loaded = load_snapshot(&pool, 1).await.unwrap().unwrap();
        assert_eq!(loaded.item_count(), 2);
    }

    #[tokio::test]
    async fn shape_queries_are_vendor_generic() {
        let pool = test_pool().await;
        let a = create(&pool, "caldav", "Family", &json!({"url": "http://x/"}), 3600).await.unwrap();
        let b = create(&pool, "open_meteo", "Home", &json!({"latitude": 1.0, "longitude": 2.0}), 3600).await.unwrap();
        assert_eq!(provider_ids_by_shape(&pool, "calendar").await.unwrap(), vec![a]);
        assert_eq!(provider_ids_by_shape(&pool, "weather").await.unwrap(), vec![b]);
        assert!(ids_with_shape_exist(&pool, "calendar", &[a]).await.unwrap());
        assert!(!ids_with_shape_exist(&pool, "calendar", &[b]).await.unwrap());
        assert!(!ids_with_shape_exist(&pool, "weather", &[a, b]).await.unwrap());
        assert!(ids_with_shape_exist(&pool, "calendar", &[]).await.unwrap());
    }

    #[tokio::test]
    async fn merged_calendar_rows_merges_and_sorts_across_providers() {
        let pool = test_pool().await;
        let a = create(&pool, "caldav", "Work", &json!({"url": "http://x/"}), 3600).await.unwrap();
        let b = create(&pool, "caldav", "Home", &json!({"url": "http://y/"}), 3600).await.unwrap();
        store_snapshot(&pool, a, &Snapshot::Calendar(vec![cal("standup", "2026-11-01T14:00:00Z")])).await.unwrap();
        store_snapshot(&pool, b, &Snapshot::Calendar(vec![cal("checkup", "2026-11-01"), cal("dinner", "2026-11-01T01:00:00Z")])).await.unwrap();
        let rows = merged_calendar_rows(&pool, &[a, b]).await.unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
        // all-day (date-only start) sorts first, then timed by start
        assert_eq!(titles, vec!["checkup", "dinner", "standup"]);
        assert_eq!(rows[0].5, "Home");
    }
}
