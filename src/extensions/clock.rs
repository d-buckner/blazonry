//! Clock extension — TRMNL's stock Clock plugin rebuilt: big time, date
//! line, optional weekday emphasis. No provider: time is computed at every
//! rebuild tick (the provider loop repaints extensions each minute, which is
//! what keeps the clock live).
//!
//! Settings (JSON):
//!   time_format: "12h" | "24h"   (default "12h")
//!   show_seconds: bool            (default false)
//!   date_format: strftime string  (default "%A, %B %-d")
//!   title: string                 (optional small label above the time)

use serde_json::{json, Value};
use sqlx::SqlitePool;

pub const CLOCK_TEMPLATE: &str = r##"{
  "name": "Clock",
  "parts": [
    { "type": "flex", "direction": "row", "align": "center", "height": 480, "children": [
      { "type": "flex", "direction": "column", "align": "center", "gap": 10, "grow": 1, "children": [
{% if title %}
        { "type": "text", "text": "{{ title }}", "style": { "font_px": 24, "fg": 1 } },
{% endif %}
        { "type": "text", "text": "{{ time }}", "style": { "font_px": 150, "bold": true } },
        { "type": "flex", "direction": "row", "style": { "bg": 0 }, "padding": [8, 22, 8, 22], "children": [
          { "type": "text", "text": "{{ date }}", "style": { "font_px": 28, "bold": true, "fg": 3 } }
        ] }
      ] }
    ] }
  ]
}"##;

/// Current LA wall-clock -> Liquid vars.
pub fn clock_vars(settings: &Value) -> Value {
    let tz = crate::providers::la();
    let now = chrono::Utc::now().with_timezone(&tz);
    let h24 = settings.get("time_format").and_then(|v| v.as_str()) == Some("24h");
    let secs = settings.get("show_seconds").and_then(|v| v.as_bool()).unwrap_or(false);
    let fmt = if h24 { "%H:%M" } else { "%-l:%M" };
    let fmt = if secs { format!("{fmt}:%S") } else { fmt.to_string() };
    let mut time = now.format(&fmt).to_string();
    if !h24 {
        time.push_str(if now.format("%p").to_string() == "AM" { " AM" } else { " PM" });
    }
    let date_fmt = settings.get("date_format").and_then(|v| v.as_str()).unwrap_or("%A, %B %-d");
    json!({
        "title": settings.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        "time": time,
        "date": now.format(date_fmt).to_string(),
    })
}

/// Create a clock extension: no provider requirement (pure computation).
pub async fn create(pool: &SqlitePool, backends: &crate::cache::Backends, cache: &crate::cache::BmpCache, name: &str, settings: Value) -> anyhow::Result<(i64, i64)> {
    let vars = clock_vars(&settings);
    let sid = crate::screens::create(pool, name, CLOCK_TEMPLATE, &vars.to_string()).await?;
    let id = super::insert(pool, "clock", name, &settings, sid).await?;
    super::rebuild_extensions(pool, backends, cache).await?;
    Ok((id, sid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_hour_default() {
        let v = clock_vars(&json!({}));
        let t = v["time"].as_str().unwrap();
        assert!(t.ends_with("AM") || t.ends_with("PM"), "12h suffix: {t}");
        assert_eq!(t.matches(':').count(), 1, "no seconds by default: {t}");
    }

    #[test]
    fn twentyfour_with_seconds() {
        let v = clock_vars(&json!({"time_format": "24h", "show_seconds": true}));
        let t = v["time"].as_str().unwrap();
        assert_eq!(t.matches(':').count(), 2, "hh:mm:ss: {t}");
        assert!(!t.contains("AM") && !t.contains("PM"));
    }
}
