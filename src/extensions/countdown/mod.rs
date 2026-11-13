// SPDX-License-Identifier: AGPL-3.0-only
//! Countdown extension — TRMNL's #3 plugin rebuilt: days remaining until a
//! date, big-number layout. No provider: the content depends only on the
//! household date, so it repaints once a day (the provider loop triggers a
//! rebuild when the local date rolls over) — unlike a clock, it never forces
//! a minute-level refresh cycle.
//!
//! Settings (JSON):
//!   title: string        what we're counting to (e.g. "Housewarming")
//!   date:  "YYYY-MM-DD"  target date (LA-local)

use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub const MANIFEST: &str = include_str!("manifest.json");

pub const COUNTDOWN_TEMPLATE: &str = include_str!("template.liquid");

pub struct Countdown;

/// Target date vs `today` -> Liquid vars.
pub fn countdown_vars(settings: &Value, today: chrono::NaiveDate) -> anyhow::Result<Value> {
    let date = settings
        .get("date")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("countdown requires a \"date\" setting (YYYY-MM-DD)"))?;
    let target = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("countdown date '{date}' is not YYYY-MM-DD: {e}"))?;
    let delta = target.signed_duration_since(today).num_days();
    let (days, unit) = match delta {
        d if d > 0 => (d.to_string(), "days to go".to_string()),
        0 => ("0".to_string(), "today".to_string()),
        d => ((-d).to_string(), "days ago".to_string()),
    };
    Ok(json!({
        "title": settings.get("title").and_then(|v| v.as_str()).unwrap_or("Countdown"),
        "days": days,
        "unit": unit,
    }))
}

/// No provider: the framework calls this once per local-day rollover.
impl super::Extension for Countdown {
    fn template(&self) -> String {
        COUNTDOWN_TEMPLATE.to_string()
    }

    fn vars(
        &self,
        req: super::VarsRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> {
        Box::pin(async move { countdown_vars(&req.settings, req.local_date()?) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn date(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn future_date_counts_days_to_go() {
        let v = countdown_vars(
            &json!({ "title": "Trip", "date": "2027-01-01" }),
            date("2026-12-22"),
        )
        .unwrap();
        assert_eq!(v["days"], "10");
        assert_eq!(v["unit"], "days to go");
        assert_eq!(v["title"], "Trip");
    }

    #[test]
    fn today_and_past_dates() {
        let v = countdown_vars(&json!({ "date": "2026-12-22" }), date("2026-12-22")).unwrap();
        assert_eq!(v["unit"], "today");
        let v = countdown_vars(&json!({ "date": "2026-12-19" }), date("2026-12-22")).unwrap();
        assert_eq!(v["days"], "3");
        assert_eq!(v["unit"], "days ago");
    }

    #[test]
    fn missing_or_bad_date_is_a_config_error() {
        assert!(countdown_vars(&json!({}), date("2026-12-22")).is_err());
        assert!(countdown_vars(&json!({ "date": "Feb 3 2027" }), date("2026-12-22")).is_err());
    }
}
