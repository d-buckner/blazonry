// SPDX-License-Identifier: AGPL-3.0-only
//! `playlists` + `playlist_screens` table owner: all SQL touching these
//! tables lives here (issue #9). The list/items queries join `screens` for
//! display names — read-only joins are allowed in the driving table's owner.

use serde_json::Value;
use sqlx::SqlitePool;

pub async fn list_json(pool: &SqlitePool) -> Result<Vec<Value>, sqlx::Error> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_object('id', p.id, 'name', p.name, 'mirror', p.mirror,
            'screen_ids', (SELECT json_group_array(screen_id) FROM playlist_screens ps
                           WHERE ps.playlist_id = p.id AND ps.visible = 1 ORDER BY ps.position),
            'items', (SELECT json_group_array(json_object(
                        'item_id', ps2.screen_id, 'screen_id', ps2.screen_id,
                        'position', ps2.position, 'visible', ps2.visible, 'name', s2.name))
                      FROM playlist_screens ps2 JOIN screens s2 ON s2.id = ps2.screen_id
                      WHERE ps2.playlist_id = p.id ORDER BY ps2.position))
         FROM playlists p ORDER BY p.id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null))
        .collect())
}

pub async fn exists(pool: &SqlitePool, id: i64) -> Result<bool, sqlx::Error> {
    let id_: Option<i64> = sqlx::query_scalar("SELECT id FROM playlists WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(id_.is_some())
}

pub async fn items_json(pool: &SqlitePool, playlist_id: i64) -> Result<Vec<Value>, sqlx::Error> {
    let items: Vec<String> = sqlx::query_scalar(
        "SELECT json_object('item_id', ps.screen_id, 'screen_id', ps.screen_id, 'position', ps.position,
            'visible', ps.visible, 'name', s.name)
         FROM playlist_screens ps JOIN screens s ON s.id = ps.screen_id
         WHERE ps.playlist_id = ? ORDER BY ps.position",
    )
    .bind(playlist_id).fetch_all(pool).await?;
    Ok(items
        .iter()
        .map(|s| serde_json::from_str::<Value>(s).unwrap_or(Value::Null))
        .collect())
}

pub async fn name(pool: &SqlitePool, id: i64) -> Result<String, sqlx::Error> {
    Ok(
        sqlx::query_scalar("SELECT name FROM playlists WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap_or_default(),
    )
}

/// Create a playlist with ordered screen ids; returns the new playlist id.
pub async fn create(pool: &SqlitePool, name: &str, screen_ids: &[i64]) -> Result<i64, sqlx::Error> {
    let pid = sqlx::query("INSERT INTO playlists (name) VALUES (?)")
        .bind(name)
        .execute(pool)
        .await?
        .last_insert_rowid();
    for (pos, sid) in screen_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_screens (playlist_id, screen_id, position) VALUES (?, ?, ?)",
        )
        .bind(pid)
        .bind(*sid)
        .bind(pos as i64)
        .execute(pool)
        .await?;
    }
    Ok(pid)
}

pub async fn set_name(pool: &SqlitePool, id: i64, name: &str) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("UPDATE playlists SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected())
}

/// Replace the full screen id list (positions reassigned in order).
/// One transaction: a mid-list failure can't leave the playlist emptied.
pub async fn set_screen_ids(pool: &SqlitePool, id: i64, ids: &[i64]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM playlist_screens WHERE playlist_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for (pos, sid) in ids.iter().enumerate() {
        sqlx::query("INSERT OR IGNORE INTO playlist_screens (playlist_id, screen_id, position) VALUES (?, ?, ?)")
            .bind(id).bind(*sid).bind(pos as i64)
            .execute(&mut *tx).await?;
    }
    tx.commit().await
}

/// Delete a playlist, its item rows, and device assignments.
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<u64, sqlx::Error> {
    let n = sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    if n > 0 {
        delete_playlist_refs(pool, id).await?;
        crate::db::clear_playlist_devices(pool, id).await?;
    }
    Ok(n)
}

pub async fn delete_playlist_refs(pool: &SqlitePool, playlist_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM playlist_screens WHERE playlist_id = ?")
        .bind(playlist_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_screen_refs(pool: &SqlitePool, screen_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM playlist_screens WHERE screen_id = ?")
        .bind(screen_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Append an item at the end; returns its position.
/// Single statement (INSERT ... SELECT MAX+1) so concurrent appends can
/// never read the same max and collide on a duplicate position (#22).
pub async fn add_item(
    pool: &SqlitePool,
    playlist_id: i64,
    screen_id: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query(
        "INSERT OR REPLACE INTO playlist_screens (playlist_id, screen_id, position) \
         SELECT ?, ?, COALESCE(MAX(position) + 1, 0) FROM playlist_screens WHERE playlist_id = ?",
    )
    .bind(playlist_id)
    .bind(screen_id)
    .bind(playlist_id)
    .execute(pool)
    .await?;
    let pos: i64 = sqlx::query_scalar(
        "SELECT position FROM playlist_screens WHERE playlist_id = ? AND screen_id = ?",
    )
    .bind(playlist_id)
    .bind(screen_id)
    .fetch_one(pool)
    .await?;
    Ok(pos)
}

pub async fn set_item_visible(
    pool: &SqlitePool,
    playlist_id: i64,
    screen_id: i64,
    visible: i64,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE playlist_screens SET visible = ? WHERE playlist_id = ? AND screen_id = ?",
    )
    .bind(visible)
    .bind(playlist_id)
    .bind(screen_id)
    .execute(pool)
    .await?
    .rows_affected())
}

pub async fn delete_item(
    pool: &SqlitePool,
    playlist_id: i64,
    screen_id: i64,
) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM playlist_screens WHERE playlist_id = ? AND screen_id = ?")
            .bind(playlist_id)
            .bind(screen_id)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

/// Next screen id for rotation: playlist items by position, modulo poll index.
pub async fn next_screen_id(
    pool: &SqlitePool,
    playlist_id: i64,
    poll_index: u64,
) -> anyhow::Result<Option<i64>> {
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
