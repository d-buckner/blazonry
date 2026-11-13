// SPDX-License-Identifier: AGPL-3.0-only
//! Calendar extension — TRMNL's most popular calendar plugin's
//! "Default" layout, rebuilt: upcoming events grouped by day, merged from
//! all picked calendar providers, sorted by start.
//!
//! Requirement: CalendarSource, cardinality 1..n — zero resolved providers
//! is a config error at create time, not a blank screen at 7am.
//!
//! Settings (JSON):
//!   provider_ids: [i64]   explicit pick list (empty = all caldav providers)
//!   days: i64             window in days (default 3, like TRMNL Default)
//!   max_events: usize     cap (default 12)
//!   time_format: "24h" | "12h" (default "12h")
//!   include_description: bool (default false)

use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub mod caldav;

/// The agenda template: Liquid -> Screen JSON, fed by merged events.
/// Day headers are LA-local dates; times arrive pre-formatted so the
/// template stays dumb (and the golden-corpus renderer sees plain text).
pub const MANIFEST: &str = include_str!("manifest.json");

pub const AGENDA_TEMPLATE: &str = include_str!("template.liquid");

pub struct Calendar;

impl super::Extension for Calendar {
    fn template(&self) -> &'static str {
        AGENDA_TEMPLATE
    }

    fn vars(&self, cx: super::Ctx) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> {
        Box::pin(async move {
            let merged = crate::providers::merged_calendar_rows(&cx.pool, &cx.provider_ids).await?;
            Ok(agenda_vars(&merged, &cx.settings))
        })
    }
}

/// Merged snapshot rows -> Liquid vars for AGENDA_TEMPLATE.
/// `rows` items: (start, all_day, title, location, description, source_name).
pub fn agenda_vars(
    rows: &[(String, bool, String, String, String, String)],
    settings: &Value,
) -> Value {
    let days_window = settings
        .get("days")
        .and_then(|v| v.as_i64())
        .unwrap_or(3)
        .max(1);
    let max_events = settings
        .get("max_events")
        .and_then(|v| v.as_i64())
        .unwrap_or(12)
        .max(1) as usize;
    let h24 = settings.get("time_format").and_then(|v| v.as_str()) == Some("24h");
    let include_desc = settings
        .get("include_description")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let tz = crate::providers::la();
    let now = chrono::Utc::now();
    let today = now.with_timezone(&tz).date_naive();
    let win_end = today
        .checked_add_days(chrono::Days::new(days_window as u64 - 1))
        .unwrap_or(today);

    // Group by LA-local date, dropping past timed events (TRMNL behavior:
    // today's agenda starts at "now"; all-day events stay all day).
    let mut by_day: std::collections::BTreeMap<chrono::NaiveDate, Vec<Value>> = Default::default();
    let mut count = 0usize;
    for (start, all_day, title, location, description, source) in rows {
        let date = if *all_day {
            chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d").ok()
        } else {
            chrono::DateTime::parse_from_rfc3339(start)
                .ok()
                .map(|dt| dt.with_timezone(&tz).date_naive())
        };
        let Some(date) = date else { continue };
        if date < today || date > win_end {
            continue;
        }
        let dt = if *all_day {
            "All day".to_string()
        } else {
            match chrono::DateTime::parse_from_rfc3339(start) {
                Ok(dt) => {
                    let l = dt.with_timezone(&tz);
                    if h24 {
                        l.format("%H:%M").to_string()
                    } else {
                        l.format("%-l:%M %p").to_string().to_lowercase()
                    }
                }
                Err(_) => "?".into(),
            }
        };
        // Past timed events today are history, not agenda.
        if !*all_day
            && date == today
            && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(start)
            && dt < now
        {
            continue;
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

    let days: Vec<Value> = by_day
        .into_iter()
        .map(|(d, events)| {
            let label = if d == today {
                "Today".to_string()
            } else {
                d.format("%a %b %-d").to_string()
            };
            json!({ "label": label, "events": events,
            "bg": if d == today { 1 } else { 2 },
            "fg": if d == today { 3 } else { 0 } })
        })
        .collect();

    json!({
        "title": settings.get("title").and_then(|v| v.as_str()).unwrap_or("Calendar"),
        "today": now.with_timezone(&tz).format("%a %b %-d").to_string(),
        "days": days,
        "empty": days.is_empty(),
        "footer": format!("Updated {}", now.with_timezone(&tz).format("%-l:%M %p").to_string().to_lowercase()),
    })
}
