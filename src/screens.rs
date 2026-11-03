// SPDX-License-Identifier: AGPL-3.0-only
//! `screens` table owner: all SQL touching `screens` lives here (issue #9).
//!
//! Other modules call these typed functions instead of writing raw SQL
//! against this table. Cross-table *read* joins are allowed in the driving
//! table's owner (see the ownership map in `db.rs`); writes always go through
//! the owner.

use serde_json::Value;
use sqlx::{AssertSqlSafe, SqlitePool};

const SCREEN_JSON: &str = "json_object('id', id, 'name', name, 'template', template, 'vars', vars, 'updated_at', updated_at, 'render_backend', COALESCE(render_backend, ''))";

pub async fn list_json(pool: &SqlitePool) -> Result<Vec<Value>, sqlx::Error> {
    let rows: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT {SCREEN_JSON} FROM screens ORDER BY id"
    )))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null))
        .collect())
}

pub async fn get_json(pool: &SqlitePool, id: i64) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {SCREEN_JSON} FROM screens WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(s,)| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null)))
}

/// Insert a screen row; returns the new id. `vars` must be valid JSON text.
pub async fn create(
    pool: &SqlitePool,
    name: &str,
    template: &str,
    vars: &str,
) -> Result<i64, sqlx::Error> {
    let id = sqlx::query(
        "INSERT INTO screens (name, template, vars, updated_at) VALUES (?, ?, ?, datetime('now'))",
    )
    .bind(name)
    .bind(template)
    .bind(vars)
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// PATCH helper: update present keys from `allowed`, then bump updated_at.
pub async fn patch(
    pool: &SqlitePool,
    id: i64,
    body: &Value,
    allowed: &[&'static str],
) -> Result<u64, sqlx::Error> {
    let n = crate::db::patch_table(pool, "screens", id, body, allowed).await?;
    if n > 0 {
        touch(pool, id).await?;
    }
    Ok(n)
}

pub async fn touch(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE screens SET updated_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace a screen's vars wholesale (extension repaint path).
pub async fn set_vars(pool: &SqlitePool, id: i64, vars_json: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE screens SET vars = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(vars_json)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Template + vars text for rendering (cache + job loop read path).
pub async fn template_vars(pool: &SqlitePool, id: i64) -> Result<(String, String), sqlx::Error> {
    use sqlx::Row;
    let row = sqlx::query("SELECT template, vars FROM screens WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok((row.get(0), row.get(1)))
}

/// Template + vars + per-screen backend preference ("" = global default).
pub async fn template_vars_backend(
    pool: &SqlitePool,
    id: i64,
) -> Result<(String, String, String), sqlx::Error> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT template, vars, COALESCE(render_backend, '') FROM screens WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok((row.get(0), row.get(1), row.get(2)))
}

pub async fn all_ids(pool: &SqlitePool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM screens ORDER BY id")
        .fetch_all(pool)
        .await
}

/// Delete a screen and its playlist references. Returns rows deleted from
/// `screens` (0 = unknown id).
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<u64, sqlx::Error> {
    let n = sqlx::query("DELETE FROM screens WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    if n > 0 {
        crate::playlists::delete_screen_refs(pool, id).await?;
    }
    Ok(n)
}
