//! `models` table owner: all SQL touching `models` lives here (issue #9).
//! Seed DDL stays in db::SEED_MODELS (bootstrap is db.rs's job).

use serde_json::Value;
use sqlx::{AssertSqlSafe, SqlitePool};

const MODEL_JSON: &str = "json_object('id', id, 'name', name, 'label', label, 'width', width, 'height', height, 'bit_depth', bit_depth)";

pub async fn list(pool: &SqlitePool) -> Result<Vec<Value>, sqlx::Error> {
    let rows: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!("SELECT {MODEL_JSON} FROM models ORDER BY id")))
        .fetch_all(pool).await?;
    Ok(rows.iter().map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null)).collect())
}

pub async fn get(pool: &SqlitePool, id: i64) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(AssertSqlSafe(format!("SELECT {MODEL_JSON} FROM models WHERE id = ?")))
        .bind(id).fetch_optional(pool).await?;
    Ok(row.map(|(s,)| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null)))
}

pub async fn create(pool: &SqlitePool, name: &str, label: &str, width: i64, height: i64, bit_depth: i64) -> Result<i64, sqlx::Error> {
    Ok(sqlx::query("INSERT INTO models (name, label, width, height, bit_depth) VALUES (?, ?, ?, ?, ?)")
        .bind(name).bind(label).bind(width).bind(height).bind(bit_depth)
        .execute(pool).await?.last_insert_rowid())
}

pub async fn patch(pool: &SqlitePool, id: i64, body: &Value) -> Result<u64, sqlx::Error> {
    crate::db::patch_table(pool, "models", id, body, &["name", "label", "width", "height", "bit_depth"]).await
}

pub async fn delete(pool: &SqlitePool, id: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM models WHERE id = ?").bind(id)
        .execute(pool).await?.rows_affected())
}
