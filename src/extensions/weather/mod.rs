// SPDX-License-Identifier: AGPL-3.0-only
//! Weather extension — TRMNL's #1 plugin rebuilt: big current temp + label,
//! a row of day cards (hi/lo/precip), from merged WeatherSource snapshots.
//!
//! Requirement: WeatherSource, cardinality exactly 1 (one screen shows one
//! place; a second location = a second extension).
//!
//! Settings (JSON):
//!   provider_id: i64      the weather provider to show (default: first)
//!   days: i64             forecast days 0..=4 (default 3)
//!   title: string

use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub mod open_meteo;

pub const MANIFEST: &str = include_str!("manifest.json");

pub const WEATHER_TEMPLATE: &str = include_str!("template.liquid");

pub struct Weather;

impl super::Extension for Weather {
    fn template(&self) -> String {
        WEATHER_TEMPLATE.to_string()
    }

    fn vars(
        &self,
        req: super::VarsRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> {
        Box::pin(async move {
            let now = req.local_time()?;
            let Some(snap) = req.snapshots.first() else {
                return Ok(no_data_vars(if req.provider_ids.is_empty() {
                    "No weather source is connected."
                } else {
                    "Waiting for first weather refresh..."
                }));
            };
            Ok(if snap.shape != "weather" {
                no_data_vars("Resolved provider does not publish weather")
            } else {
                weather_vars(&snap.payload, &req.settings, now)
            })
        })
    }
}

/// Snapshot payload (open_meteo shape) -> Liquid vars. `now` is the local
/// instant the rebuild is for, so the day labels and the footer do not read a
/// clock of their own.
pub fn weather_vars(
    snap: &Value,
    settings: &Value,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Value {
    let max_days = settings
        .get("days")
        .and_then(|v| v.as_i64())
        .unwrap_or(3)
        .clamp(0, 4) as usize;
    let days: Vec<Value> = snap["days"]
        .as_array()
        .map(|ds| {
            ds.iter()
                .take(max_days)
                .map(|d| {
                    let date = d["date"].as_str().unwrap_or("");
                    let label = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                        .map(|dt| {
                            if dt == now.date_naive() {
                                "Today".to_string()
                            } else {
                                dt.format("%a").to_string()
                            }
                        })
                        .unwrap_or_else(|_| date.to_string());
                    let sub = match d["precip"].as_i64() {
                        Some(p) if p > 0 => format!("{p}% precip"),
                        _ => d["label"].as_str().unwrap_or("").to_string(),
                    };
                    // Today's card is inverted (black ground); the rest sit on light gray.
                    let is_today = label == "Today";
                    json!({ "label": label, "hi": d["hi"], "lo": d["lo"], "sub": sub,
            "card_bg": if is_today { 0 } else { 2 },
            "fg": if is_today { 3 } else { 0 },
            "sub_fg": if is_today { 2 } else { 1 } })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "title": settings.get("title").and_then(|v| v.as_str())
            .unwrap_or(snap["location"].as_str().unwrap_or("Weather")),
        "have_data": true,
        "units": snap["units"].as_str().unwrap_or("F"),
        "current": snap["current"],
        "days": days,
        "footer": format!("Updated {}", now.format("%-l:%M %p").to_string().to_lowercase()),
    })
}

pub fn no_data_vars(message: &str) -> Value {
    json!({ "title": "Weather", "have_data": false, "message": message, "footer": "" })
}
