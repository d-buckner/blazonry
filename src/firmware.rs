// SPDX-License-Identifier: AGPL-3.0-only
//! `firmware` table owner: all SQL touching `firmware` lives here (issue #9).

use serde_json::Value;
use sqlx::SqlitePool;

type Row = (i64, String, String, String, Option<String>);

fn row_json((id, version, url, notes, created_at): Row) -> Value {
    serde_json::json!({ "id": id, "version": version, "url": url, "notes": notes, "created_at": created_at })
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<Value>, sqlx::Error> {
    let rows: Vec<Row> =
        sqlx::query_as("SELECT id, version, url, notes, created_at FROM firmware ORDER BY id DESC")
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(row_json).collect())
}

pub async fn get(pool: &SqlitePool, id: i64) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<Row> =
        sqlx::query_as("SELECT id, version, url, notes, created_at FROM firmware WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(row_json))
}

/// Newest row: (version, url, notes).
pub async fn latest(pool: &SqlitePool) -> Result<Option<(String, String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT version, url, notes FROM firmware ORDER BY id DESC LIMIT 1")
        .fetch_optional(pool)
        .await
}

pub async fn create(
    pool: &SqlitePool,
    version: &str,
    url: &str,
    notes: &str,
) -> Result<i64, sqlx::Error> {
    Ok(sqlx::query(
        "INSERT INTO firmware (version, url, notes, created_at) VALUES (?, ?, ?, datetime('now'))",
    )
    .bind(version)
    .bind(url)
    .bind(notes)
    .execute(pool)
    .await?
    .last_insert_rowid())
}

pub async fn patch(pool: &SqlitePool, id: i64, body: &Value) -> Result<u64, sqlx::Error> {
    crate::db::patch_table(pool, "firmware", id, body, &["version", "url", "notes"]).await
}

pub async fn delete(pool: &SqlitePool, id: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM firmware WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected())
}

pub async fn wipe(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM firmware")
        .execute(pool)
        .await?
        .rows_affected())
}
