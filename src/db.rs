// SPDX-License-Identifier: AGPL-3.0-only
//! SQLite schema + bootstrap. sqlx runtime API (no compile-time macros —
//! keeps builds DB-free).
//!
//! ## SQL ownership map (issue #9)
//!
//! All raw SQL for a table lives in its owner module; other modules call the
//! owner's typed functions. Read-only cross-table joins are allowed in the
//! *driving* table's owner (e.g. playlists joins screens for names).
//!
//! | table(s)                       | owner                    |
//! |--------------------------------|--------------------------|
//! | devices, device_logs           | `db` (this module)       |
//! | screens                        | `screens`                |
//! | playlists, playlist_screens    | `playlists`              |
//! | users, sessions                | `auth`                   |
//! | firmware                       | `firmware`               |
//! | models                         | `models` (seed DDL here) |
//! | providers, provider_snapshots  | `providers`              |
//! | extensions                     | `extensions`             |
//!
//! DDL for all tables stays here (MIGRATE / COLUMN_MIGRATIONS / SEED_MODELS);
//! providers/extensions contribute their own DDL constants, executed by `connect`.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

pub const MIGRATE: &str = r#"
CREATE TABLE IF NOT EXISTS devices (
  id INTEGER PRIMARY KEY,
  mac TEXT UNIQUE NOT NULL,
  name TEXT NOT NULL DEFAULT 'device',
  api_key TEXT NOT NULL,
  refresh_interval INTEGER NOT NULL DEFAULT 900,
  playlist_id INTEGER,
  last_poll_at TEXT
);
CREATE TABLE IF NOT EXISTS screens (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  template TEXT NOT NULL,
  vars TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS playlists (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS playlist_screens (
  playlist_id INTEGER NOT NULL,
  screen_id INTEGER NOT NULL,
  position INTEGER NOT NULL,
  PRIMARY KEY (playlist_id, screen_id)
);
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY,
  email TEXT UNIQUE NOT NULL,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'unverified'
);
CREATE TABLE IF NOT EXISTS sessions (
  token TEXT PRIMARY KEY,
  user_id INTEGER NOT NULL,
  expires_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS firmware (
  id INTEGER PRIMARY KEY,
  version TEXT NOT NULL,
  url TEXT NOT NULL,
  notes TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS device_logs (
  id INTEGER PRIMARY KEY,
  device_id INTEGER NOT NULL,
  message TEXT NOT NULL,
  level TEXT,
  source_path TEXT,
  created_at TEXT
);
"#;

/// Column additions applied idempotently (SQLite has no ADD COLUMN IF NOT EXISTS;
/// a duplicate-column error is expected and ignored).
pub const COLUMN_MIGRATIONS: &[&str] = &[
    "ALTER TABLE providers ADD COLUMN shape TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE devices ADD COLUMN firmware_version TEXT",
    "ALTER TABLE devices ADD COLUMN model TEXT",
    "ALTER TABLE devices ADD COLUMN battery_voltage REAL",
    "ALTER TABLE devices ADD COLUMN rssi INTEGER",
    "ALTER TABLE devices ADD COLUMN width INTEGER",
    "ALTER TABLE devices ADD COLUMN height INTEGER",
    "ALTER TABLE devices ADD COLUMN poll_count INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE devices ADD COLUMN current_screen_id INTEGER",
    "ALTER TABLE screens ADD COLUMN updated_at TEXT",
    "ALTER TABLE firmware ADD COLUMN created_at TEXT",
    "ALTER TABLE playlists ADD COLUMN mirror INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE playlist_screens ADD COLUMN visible INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE screens ADD COLUMN render_backend TEXT",
];

/// Seed the device-model registry once (Terminus /api/models parity: og + x).
pub const SEED_MODELS: &str = r#"
CREATE TABLE IF NOT EXISTS models (
  id INTEGER PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  label TEXT NOT NULL,
  width INTEGER NOT NULL DEFAULT 800,
  height INTEGER NOT NULL DEFAULT 480,
  bit_depth INTEGER NOT NULL DEFAULT 2
);
INSERT OR IGNORE INTO models (name, label, width, height, bit_depth) VALUES
  ('trmnl_gen_1', 'TRMNL OG', 800, 480, 1),
  ('trmnl_gen_2', 'TRMNL X', 800, 480, 2);
"#;

pub async fn connect(path: &str) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(path)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;
    sqlx::query(MIGRATE).execute(&pool).await?;
    sqlx::query(crate::extensions::MIGRATE).execute(&pool).await?;
    sqlx::query(SEED_MODELS).execute(&pool).await?;
    for stmt in COLUMN_MIGRATIONS {
        // idempotent: ignore "duplicate column name" on re-run
        let _ = sqlx::query(*stmt).execute(&pool).await;
    }
    // Shape column is a total invariant after this (pre-shape rows backfilled
    // from the registry), and pre-snapshot-store data is carried forward.
    crate::providers::backfill_shapes(&pool).await?;
    crate::providers::migrate_legacy_snapshots(&pool).await?;
    Ok(pool)
}

/// Shared PATCH helper: build `SET a=?, b=?` from present keys only.
/// Table/column names come from `allowed` constants, never user input.
pub async fn patch_table(pool: &SqlitePool, table: &'static str, id: i64, body: &serde_json::Value, allowed: &[&'static str]) -> Result<u64, sqlx::Error> {
    use sqlx::AssertSqlSafe;
    let mut sets: Vec<&str> = Vec::new();
    for k in allowed {
        if body.get(*k).is_some() {
            sets.push(k);
        }
    }
    if sets.is_empty() {
        return Ok(0);
    }
    let mut q = sqlx::query(AssertSqlSafe(format!(
        "UPDATE {table} SET {} WHERE id = ?",
        sets.iter().map(|s| format!("{s} = ?")).collect::<Vec<_>>().join(", ")
    )));
    for k in &sets {
        let v = body.get(*k).unwrap();
        q = if v.is_i64() || v.is_u64() {
            q.bind(v.as_i64())
        } else if v.is_f64() {
            q.bind(v.as_f64())
        } else if v.is_boolean() {
            q.bind(v.as_bool().map(|b| b as i64))
        } else if v.is_null() {
            q.bind(None::<String>)
        } else {
            q.bind(v.as_str().map(|s| s.to_string()))
        };
    }
    let n = q.bind(id).execute(pool).await?.rows_affected();
    Ok(n)
}

/// Register-or-fetch a device by MAC. Unknown MACs are auto-paired on first
/// contact (LAN trust model, same default as Terminus BYOS) and get a key.
pub async fn upsert_device(pool: &SqlitePool, mac: &str) -> anyhow::Result<(i64, String, Option<i64>, i64)> {
    let existing: Option<(i64, String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, api_key, playlist_id, refresh_interval FROM devices WHERE mac = ?",
    )
    .bind(mac)
    .fetch_optional(pool)
    .await?;
    if let Some(row) = existing {
        return Ok(row);
    }
    let key = format!("{:064x}", {
        let mut bytes = [0u8; 32];
        use std::io::Read;
        std::fs::File::open("/dev/urandom").expect("/dev/urandom")
            .read_exact(&mut bytes).expect("urandom read");
        u128::from_be_bytes(bytes[..16].try_into().unwrap()) ^ u128::from_be_bytes(bytes[16..].try_into().unwrap())
    });
    let id = sqlx::query("INSERT INTO devices (mac, api_key) VALUES (?, ?)")
        .bind(mac)
        .bind(&key)
        .execute(pool)
        .await?
        .last_insert_rowid();
    Ok((id, key, None, 900))
}

// ---------------------------------------------------------------------------
// `devices` + `device_logs` table owner helpers (issue #9).
// Schema DDL stays in MIGRATE/COLUMN_MIGRATIONS above; these are the only
// functions allowed to write raw SQL against devices/device_logs.
// ---------------------------------------------------------------------------

/// Telemetry from a /api/display poll: COALESCE so absent headers keep old values.
#[allow(clippy::too_many_arguments)]
pub async fn record_telemetry(
    pool: &SqlitePool,
    id: i64,
    fw_version: Option<&str>,
    rssi: Option<i64>,
    battery_voltage: Option<f64>,
    width: Option<i64>,
    height: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE devices SET last_poll_at = datetime('now'),
           firmware_version = COALESCE(?, firmware_version),
           rssi = COALESCE(?, rssi),
           battery_voltage = COALESCE(?, battery_voltage),
           width = COALESCE(?, width),
           height = COALESCE(?, height)
         WHERE id = ?",
    )
    .bind(fw_version).bind(rssi).bind(battery_voltage).bind(width).bind(height)
    .bind(id)
    .execute(pool).await?;
    Ok(())
}

/// Record setup headers (fw version + model) without touching poll state.
pub async fn record_setup(pool: &SqlitePool, id: i64, fw_version: Option<&str>, model: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE devices SET firmware_version = COALESCE(?, firmware_version), model = COALESCE(?, model) WHERE id = ?",
    )
    .bind(fw_version).bind(model).bind(id)
    .execute(pool).await?;
    Ok(())
}

/// Advance the rotation counter; returns the new poll_count.
pub async fn advance_poll(pool: &SqlitePool, id: i64) -> Result<i64, sqlx::Error> {
    Ok(sqlx::query_scalar(
        "UPDATE devices SET poll_count = poll_count + 1, last_poll_at = datetime('now') WHERE id = ? RETURNING poll_count",
    )
    .bind(id)
    .fetch_one(pool)
    .await?)
}

pub async fn set_current_screen(pool: &SqlitePool, id: i64, screen_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE devices SET current_screen_id = ? WHERE id = ?")
        .bind(screen_id).bind(id)
        .execute(pool).await?;
    Ok(())
}

/// (api_key, current_screen_id, refresh_interval, playlist_id) for /display/current.
pub async fn device_state_by_mac(pool: &SqlitePool, mac: &str) -> Result<Option<(String, Option<i64>, i64, Option<i64>)>, sqlx::Error> {
    Ok(sqlx::query_as(
        "SELECT api_key, current_screen_id, refresh_interval, playlist_id FROM devices WHERE mac = ?",
    )
    .bind(mac)
    .fetch_optional(pool)
    .await?)
}

pub async fn device_id_by_token(pool: &SqlitePool, token: &str) -> Result<Option<i64>, sqlx::Error> {
    Ok(sqlx::query_scalar("SELECT id FROM devices WHERE api_key = ?").bind(token)
        .fetch_optional(pool).await?)
}

pub async fn insert_device_log(pool: &SqlitePool, device_id: i64, message: &str, level: Option<&str>, source_path: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO device_logs (device_id, message, level, source_path, created_at) VALUES (?, ?, ?, ?, datetime('now'))",
    )
    .bind(device_id).bind(message).bind(level).bind(source_path)
    .execute(pool).await?;
    Ok(())
}

pub async fn set_device_playlist_by_mac(pool: &SqlitePool, mac: &str, playlist_id: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("UPDATE devices SET playlist_id = ? WHERE mac = ?")
        .bind(playlist_id).bind(mac)
        .execute(pool).await?.rows_affected())
}

pub async fn set_device_name_by_mac(pool: &SqlitePool, mac: &str, name: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE devices SET name = ? WHERE mac = ?").bind(name).bind(mac)
        .execute(pool).await?;
    Ok(())
}

pub async fn delete_device(pool: &SqlitePool, id: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM devices WHERE id = ?").bind(id)
        .execute(pool).await?.rows_affected())
}

pub async fn wipe_device_logs(pool: &SqlitePool, device_id: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM device_logs WHERE device_id = ?").bind(device_id)
        .execute(pool).await?.rows_affected())
}

/// Unassign devices when their playlist is deleted.
pub async fn clear_playlist_devices(pool: &SqlitePool, playlist_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE devices SET playlist_id = NULL WHERE playlist_id = ?")
        .bind(playlist_id).execute(pool).await?;
    Ok(())
}

pub async fn device_list_json(pool: &SqlitePool) -> Result<Vec<serde_json::Value>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT json_object(
            'id', id, 'mac', mac, 'name', name, 'api_key', api_key,
            'playlist_id', playlist_id, 'last_poll_at', last_poll_at,
            'firmware_version', firmware_version, 'model', model,
            'battery_voltage', battery_voltage, 'rssi', rssi,
            'width', width, 'height', height, 'poll_count', poll_count,
            'current_screen_id', current_screen_id, 'refresh_interval', refresh_interval
         ) FROM devices ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(s,)| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)).collect())
}

pub async fn device_json(pool: &SqlitePool, id: i64) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT json_object(
            'id', id, 'mac', mac, 'name', name, 'api_key', api_key,
            'playlist_id', playlist_id, 'last_poll_at', last_poll_at,
            'firmware_version', firmware_version, 'model', model,
            'battery_voltage', battery_voltage, 'rssi', rssi,
            'width', width, 'height', height, 'poll_count', poll_count,
            'current_screen_id', current_screen_id, 'refresh_interval', refresh_interval
         ) FROM devices WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(s,)| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)))
}

