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

use serde_json::{json, Value};
use sqlx::SqlitePool;

pub const WEATHER_TEMPLATE: &str = r##"{
  "name": "{{ title }}",
  "parts": [
    { "type": "text", "text": "{{ title }}", "style": { "font_px": 30, "bold": true }, "padding": [16, 20, 0, 20] },
{% if have_data %}
    { "type": "flex", "direction": "row", "gap": 18, "align": "center", "padding": [6, 20, 0, 20], "children": [
      { "type": "text", "text": "{{ current.temp }}{{ units }}", "style": { "font_px": 96, "bold": true } },
      { "type": "flex", "direction": "column", "gap": 4, "children": [
        { "type": "text", "text": "{{ current.label }}", "style": { "font_px": 30 } },
        { "type": "text", "text": "feels {{ current.feels }}{{ units }}  {{ current.wind }} mph  {{ current.humidity }}%", "style": { "font_px": 20, "fg": 1 } }
      ] }
    ] },
    { "type": "divider", "padding": [10, 20, 6, 20] },
    { "type": "flex", "direction": "row", "gap": 12, "padding": [0, 20, 0, 20], "children": [
{% for d in days %}
      { "type": "flex", "direction": "column", "gap": 2, "grow": 1, "border": true, "padding": [8, 10, 8, 10], "children": [
        { "type": "text", "text": "{{ d.label }}", "style": { "font_px": 20, "bold": true } },
        { "type": "text", "text": "{{ d.hi }}/{{ d.lo }}", "style": { "font_px": 24 } },
        { "type": "text", "text": "{{ d.sub }}", "style": { "font_px": 16, "fg": 1 } }
      ] }{% if forloop.last %}{% else %},{% endif %}
{% endfor %}
{% if days.size > 0 %}
      , { "type": "text", "text": "", "grow": 1 }
{% endif %}
    ] },
{% else %}
    { "type": "text", "text": "{{ message }}", "style": { "font_px": 26, "fg": 1 }, "padding": [16, 20, 0, 20] },
{% endif %}
    { "type": "text", "text": "{{ footer }}", "style": { "font_px": 16, "fg": 1 }, "padding": [10, 20, 0, 20] }
  ]
}"##;

/// Snapshot payload (open_meteo shape) -> Liquid vars.
pub fn weather_vars(snap: &Value, settings: &Value) -> Value {
    let max_days = settings.get("days").and_then(|v| v.as_i64()).unwrap_or(3).clamp(0, 4) as usize;
    let tz = crate::providers::la();
    let now = chrono::Utc::now().with_timezone(&tz);
    let days: Vec<Value> = snap["days"].as_array().map(|ds| ds.iter().take(max_days).map(|d| {
        let date = d["date"].as_str().unwrap_or("");
        let label = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map(|dt| if dt == now.date_naive() { "Today".to_string() } else { dt.format("%a").to_string() })
            .unwrap_or_else(|_| date.to_string());
        let sub = match d["precip"].as_i64() {
            Some(p) if p > 0 => format!("{p}% precip"),
            _ => d["label"].as_str().unwrap_or("").to_string(),
        };
        json!({ "label": label, "hi": d["hi"], "lo": d["lo"], "sub": sub })
    }).collect()).unwrap_or_default();

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

/// Resolve the weather provider id: explicit `provider_id`, else first one.
pub async fn resolved_provider_id(pool: &SqlitePool, settings: &Value) -> anyhow::Result<Option<i64>> {
    if let Some(id) = settings.get("provider_id").and_then(|v| v.as_i64()) {
        return Ok(Some(id));
    }
    Ok(crate::providers::provider_ids_by_shape(pool, "weather").await?.into_iter().next())
}

/// Create a weather extension: cardinality exactly 1 checked here, screen
/// created with the template, first paint from an existing snapshot if any.
pub async fn create(pool: &SqlitePool, backends: &crate::cache::Backends, cache: &crate::cache::BmpCache, name: &str, settings: Value) -> anyhow::Result<(i64, i64)> {
    let pid = resolved_provider_id(pool, &settings).await?;
    let Some(pid) = pid else {
        anyhow::bail!("weather requires exactly 1 weather provider; none configured — create a provider first");
    };
    // Fail out-of-existence: an explicit id that doesn't exist is a config error.
    if !crate::providers::ids_with_shape_exist(pool, "weather", &[pid]).await? {
        anyhow::bail!("provider_id {pid} is not a known weather provider");
    }
    let vars = match crate::providers::load_snapshot(pool, pid).await? {
        Some(crate::providers::Snapshot::Weather(snap)) => weather_vars(&snap, &settings),
        Some(_) => no_data_vars("Resolved provider does not publish weather"),
        None => no_data_vars("Waiting for first weather refresh..."),
    };
    let sid = crate::screens::create(pool, name, WEATHER_TEMPLATE, &vars.to_string()).await?;
    let id = super::insert(pool, "weather", name, &settings, sid).await?;
    super::rebuild_extensions(pool, backends, cache).await?;
    Ok((id, sid))
}
