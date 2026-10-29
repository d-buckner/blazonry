//! SQLite schema + bootstrap. sqlx runtime API (no compile-time macros —
//! keeps builds DB-free).

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
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
    sqlx::query(SEED_MODELS).execute(&pool).await?;
    for stmt in COLUMN_MIGRATIONS {
        // idempotent: ignore "duplicate column name" on re-run
        let _ = sqlx::query(*stmt).execute(&pool).await;
    }
    Ok(pool)
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

/// Next screen id for a device: playlist rotation by poll counter.
pub async fn next_screen_id(pool: &SqlitePool, playlist_id: i64, poll_index: u64) -> anyhow::Result<Option<i64>> {
    let rows: Vec<i64> = sqlx::query_scalar(
        "SELECT screen_id FROM playlist_screens WHERE playlist_id = ? ORDER BY position",
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(rows[(poll_index as usize) % rows.len()]))
}

pub async fn load_screen(pool: &SqlitePool, id: i64) -> anyhow::Result<(String, String, String)> {
    let row = sqlx::query("SELECT name, template, vars FROM screens WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok((row.get(0), row.get(1), row.get(2)))
}
