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
    fn template(&self) -> String {
        AGENDA_TEMPLATE.to_string()
    }

    fn vars(
        &self,
        req: super::VarsRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> {
        Box::pin(async move {
            let now = req.local_time()?;
            Ok(agenda_vars(
                &merged_rows(&req.snapshots)?,
                &req.settings,
                now,
            ))
        })
    }
}

/// One calendar event with the name of the provider it came from.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub start: String,
    pub all_day: bool,
    pub title: String,
    pub location: String,
    pub description: String,
    pub source: String,
}

/// Merge the calendar snapshots into one list sorted by start. All-day first
/// within a day falls out of the `start` sort: `YYYY-MM-DD` < any datetime.
fn merged_rows(snapshots: &[super::SnapshotInput]) -> anyhow::Result<Vec<EventRow>> {
    let mut out: Vec<EventRow> = Vec::new();
    for s in snapshots.iter().filter(|s| s.shape == "calendar") {
        let events: Vec<crate::providers::CalendarEvent> =
            serde_json::from_value(s.payload.clone())
                .map_err(|e| anyhow::anyhow!("{}: calendar payload is unreadable: {e}", s.name))?;
        out.extend(events.into_iter().map(|e| EventRow {
            start: e.start,
            all_day: e.all_day,
            title: e.title,
            location: e.location,
            description: e.description,
            source: s.name.clone(),
        }));
    }
    out.sort_by(|a, b| a.start.cmp(&b.start));
    Ok(out)
}

/// Merged event rows -> Liquid vars for AGENDA_TEMPLATE. `now` is the local
/// instant the rebuild is for, so the day grouping and the footer do not read
/// a clock of their own.
pub fn agenda_vars(
    rows: &[EventRow],
    settings: &Value,
    now: chrono::DateTime<chrono::FixedOffset>,
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

    let today = now.date_naive();
    // Events carry their own UTC instant; the rebuild's offset is what makes a
    // day boundary local.
    let tz = *now.offset();
    let win_end = today
        .checked_add_days(chrono::Days::new(days_window as u64 - 1))
        .unwrap_or(today);

    // Group by local date, dropping past timed events (TRMNL behavior:
    // today's agenda starts at "now"; all-day events stay all day).
    let mut by_day: std::collections::BTreeMap<chrono::NaiveDate, Vec<Value>> = Default::default();
    let mut count = 0usize;
    for EventRow {
        start,
        all_day,
        title,
        location,
        description,
        source,
    } in rows
    {
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
        "today": now.format("%a %b %-d").to_string(),
        "days": days,
        "empty": days.is_empty(),
        "footer": format!("Updated {}", now.format("%-l:%M %p").to_string().to_lowercase()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::SnapshotInput;

    fn now() -> chrono::DateTime<chrono::FixedOffset> {
        chrono::DateTime::parse_from_rfc3339("2026-11-01T14:00:00-08:00").unwrap()
    }

    fn snap(name: &str, events: Vec<Value>) -> SnapshotInput {
        SnapshotInput {
            provider_id: 1,
            name: name.to_string(),
            shape: "calendar".to_string(),
            fetched_at: "2026-11-01T17:00:00Z".to_string(),
            payload: Value::Array(events),
        }
    }

    fn event(title: &str, start: &str, all_day: bool) -> Value {
        json!({"uid": title, "start": start, "all_day": all_day, "title": title,
               "location": "", "description": ""})
    }

    #[test]
    fn merged_rows_sorts_across_providers() {
        let rows = merged_rows(&[
            snap(
                "Work",
                vec![event("standup", "2026-11-01T14:00:00Z", false)],
            ),
            snap(
                "Home",
                vec![
                    event("checkup", "2026-11-01", true),
                    event("dinner", "2026-11-01T01:00:00Z", false),
                ],
            ),
        ])
        .unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        // all-day (date-only start) sorts first, then timed by start
        assert_eq!(titles, vec!["checkup", "dinner", "standup"]);
        assert_eq!(rows[0].source, "Home");
    }

    #[test]
    fn agenda_keeps_only_what_is_still_ahead() {
        let rows = merged_rows(&[snap(
            "Work",
            vec![
                event("all day yesterday", "2026-10-31", true),
                event("all day today", "2026-11-01", true),
                event("past timed", "2026-11-01T16:00:00Z", false),
                event("future timed", "2026-11-01T23:00:00Z", false),
            ],
        )])
        .unwrap();
        let v = agenda_vars(&rows, &json!({}), now());
        let titles: Vec<&str> = v["days"][0]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["all day today", "future timed"]);
    }
}
