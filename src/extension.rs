//! Extensions: consumers of provider shapes. The first one is the Family
//! Calendar — TRMNL's most popular plugin (Google Calendar, #4 overall on
//! trmnl.com/integrations; its "Default" agenda layout is the canonical
//! calendar screen): upcoming events grouped by day, merged from all picked
//! calendar providers, sorted by start.
//!
//! Settings (JSON):
//!   provider_ids: [i64]   explicit pick list (empty = all caldav providers)
//!   days: i64             window in days (default 3, like TRMNL Default)
//!   max_events: usize     cap (default 12)
//!   time_format: "24h" | "12h" (default "12h")
//!   include_description: bool (default false)
//!
//! Cardinality: CalendarSource 1..n — zero resolved providers is a config
//! error surfaced at create time, not a blank screen at 7am.

use crate::provider::la;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use serde_json::{json, Value};
use sqlx::AssertSqlSafe;
use sqlx::Row;
use sqlx::SqlitePool;

pub const MIGRATE: &str = crate::provider::MIGRATE;

/// The agenda template: Liquid -> Screen JSON, fed by merged events.
/// Day headers are LA-local dates; times arrive pre-formatted so the
/// template stays dumb (and the golden-corpus renderer sees plain text).
pub const AGENDA_TEMPLATE: &str = r##"{
  "name": "{{ title }}",
  "parts": [
    { "type": "text", "text": "{{ title }}", "style": { "font_px": 34, "bold": true }, "padding": [16, 20, 4, 20] },
    { "type": "divider", "padding": [4, 20, 8, 20] },
{% for day in days %}
    { "type": "text", "text": "{{ day.label }}", "style": { "font_px": 22, "bold": true }, "padding": [8, 20, 2, 20] },
{% for e in day.events %}
    { "type": "flex", "direction": "row", "gap": 14, "padding": [2, 20, 2, 20], "children": [
      { "type": "text", "text": "{{ e.time }}", "style": { "font_px": 22 }, "width": 92 },
      { "type": "text", "text": "{{ e.title }}", "style": { "font_px": 22 }, "grow": 1 }{% if e.extra %},
      { "type": "text", "text": "{{ e.extra }}", "style": { "font_px": 18, "fg": 1 } }{% endif %}
    ] },
{% endfor %}
{% endfor %}
{% if empty %}
    { "type": "text", "text": "Nothing scheduled. Enjoy it.", "style": { "font_px": 26, "fg": 1 }, "padding": [16, 20, 0, 20] },
{% endif %}
    { "type": "text", "text": "{{ footer }}", "style": { "font_px": 16, "fg": 1 }, "padding": [10, 20, 0, 20] }
  ]
}"##;

/// Merged snapshot rows -> Liquid vars for AGENDA_TEMPLATE.
/// `rows` items: (start, all_day, title, location, description, source_name).
pub fn agenda_vars(rows: &[(String, bool, String, String, String, String)], settings: &Value) -> Value {
    let days_window = settings.get("days").and_then(|v| v.as_i64()).unwrap_or(3).max(1);
    let max_events = settings.get("max_events").and_then(|v| v.as_i64()).unwrap_or(12).max(1) as usize;
    let h24 = settings.get("time_format").and_then(|v| v.as_str()) == Some("24h");
    let include_desc = settings.get("include_description").and_then(|v| v.as_bool()).unwrap_or(false);

    let tz = la();
    let now = Utc::now();
    let today = now.with_timezone(&tz).date_naive();
    let win_end = today.checked_add_days(chrono::Days::new(days_window as u64 - 1)).unwrap_or(today);

    // Group by LA-local date, dropping past timed events (TRMNL behavior:
    // today's agenda starts at "now"; all-day events stay all day).
    let mut by_day: std::collections::BTreeMap<NaiveDate, Vec<Value>> = Default::default();
    let mut count = 0usize;
    for (start, all_day, title, location, description, source) in rows {
        let date = if *all_day {
            NaiveDate::parse_from_str(start, "%Y-%m-%d").ok()
        } else {
            DateTime::parse_from_rfc3339(start).ok().map(|dt| dt.with_timezone(&tz).date_naive())
        };
        let Some(date) = date else { continue };
        if date < today || date > win_end {
            continue;
        }
        let dt = if *all_day {
            "All day".to_string()
        } else {
            match DateTime::parse_from_rfc3339(start) {
                Ok(dt) => {
                    let l = dt.with_timezone(&tz);
                    if h24 { l.format("%H:%M").to_string() } else { l.format("%-l:%M %p").to_string().to_lowercase() }
                }
                Err(_) => "?".into(),
            }
        };
        // Past timed events today are history, not agenda.
        if !*all_day && date == today {
            if let Ok(dt) = DateTime::parse_from_rfc3339(start) {
                if dt < now {
                    continue;
                }
            }
        }
        let mut extra = location.clone();
        if extra.is_empty() && include_desc {
            extra = description.chars().take(40).collect();
        }
        if extra.is_empty() {
            extra = source.clone();
        }
        let entry = json!({ "time": dt, "title": title, "extra": extra });
        by_day.entry(date).or_default().push(entry);
        count += 1;
        if count >= max_events {
            break;
        }
    }

    let days: Vec<Value> = by_day.into_iter().map(|(d, events)| {
        let label = if d == today {
            "Today".to_string()
        } else {
            d.format("%a %b %-d").to_string()
        };
        json!({ "label": label, "events": events })
    }).collect();

    json!({
        "title": settings.get("title").and_then(|v| v.as_str()).unwrap_or("Family Calendar"),
        "days": days,
        "empty": days.is_empty(),
        "footer": format!("Updated {}", now.with_timezone(&tz).format("%-l:%M %p").to_string().to_lowercase()),
    })
}

/// Resolve an extension's provider pick list: explicit ids, or all providers
/// of the shape when unset (selector: "all" | explicit list — no tags).
async fn resolved_provider_ids(pool: &SqlitePool, settings: &Value) -> anyhow::Result<Vec<i64>> {
    if let Some(ids) = settings.get("provider_ids").and_then(|v| v.as_array()) {
        let ids: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    Ok(sqlx::query_scalar("SELECT id FROM providers WHERE kind = 'caldav' ORDER BY id")
        .fetch_all(pool).await?)
}

/// Rebuild every extension's bound screen from current snapshots.
/// Returns the number of extensions rebuilt.
pub async fn rebuild_extensions(pool: &SqlitePool, cache: &crate::cache::BmpCache) -> anyhow::Result<usize> {
    let exts = sqlx::query("SELECT id, kind, name, settings, screen_id FROM extensions").fetch_all(pool).await?;
    let mut n = 0;
    for ext in exts {
        let kind: String = ext.get(1);
        if kind != "family_calendar" {
            continue;
        }
        let name: String = ext.get(2);
        let settings: String = ext.get(3);
        let screen_id: Option<i64> = ext.get(4);
        let settings: Value = serde_json::from_str(&settings).unwrap_or(json!({}));
        let ids = resolved_provider_ids(pool, &settings).await?;
        if ids.is_empty() {
            tracing::warn!(extension = name, "no calendar providers resolved; leaving screen as-is");
            continue;
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let mut q = sqlx::query(AssertSqlSafe(format!(
            "SELECT pe.start, pe.all_day, pe.title, pe.location, pe.description, p.name \
             FROM provider_events pe JOIN providers p ON p.id = pe.provider_id \
             WHERE pe.provider_id IN ({placeholders}) ORDER BY pe.start"
        )));
        for id in &ids {
            q = q.bind(id);
        }
        let rows = q.fetch_all(pool).await?;
        let merged: Vec<(String, bool, String, String, String, String)> = rows.iter().map(|r| {
            (r.get(0), r.get::<i64, _>(1) != 0, r.get(2), r.get(3), r.get(4), r.get(5))
        }).collect();
        let vars = agenda_vars(&merged, &settings);
        let vars_json = serde_json::to_string(&vars)?;
        let Some(sid) = screen_id else { continue };
        sqlx::query("UPDATE screens SET vars = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&vars_json).bind(sid).execute(pool).await?;
        for bpp in [1u16, 2u16] {
            if let Err(e) = crate::cache::render_and_cache(pool, &crate::render::Renderer::new()?, cache, sid, bpp).await {
                tracing::warn!(%e, screen = sid, "extension re-render failed");
            }
        }
        n += 1;
    }
    Ok(n)
}

/// Create a family_calendar extension: validates cardinality (1..n), creates
/// its screen row with the agenda template, and renders it immediately.
pub async fn create_family_calendar(pool: &SqlitePool, cache: &crate::cache::BmpCache, name: &str, settings: Value) -> anyhow::Result<i64> {
    let ids = resolved_provider_ids(pool, &settings).await?;
    if ids.is_empty() {
        anyhow::bail!("family_calendar requires 1..n calendar providers; none configured or picked");
    }
    let vars = json!({ "title": "Family Calendar", "days": [], "empty": true, "footer": "" });
    let row = sqlx::query("INSERT INTO screens (name, template, vars, updated_at) VALUES (?, ?, ?, datetime('now'))")
        .bind(name).bind(AGENDA_TEMPLATE).bind(vars.to_string())
        .execute(pool).await?;
    let sid = row.last_insert_rowid();
    let id = sqlx::query("INSERT INTO extensions (kind, name, settings, screen_id) VALUES ('family_calendar', ?, ?, ?)")
        .bind(name).bind(settings.to_string()).bind(sid)
        .execute(pool).await?
        .last_insert_rowid();
    // Immediate first paint from existing snapshots.
    rebuild_extensions(pool, cache).await?;
    Ok(id)
}
