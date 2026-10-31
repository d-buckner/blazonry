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
{% if have_data %}
    { "type": "flex", "direction": "row", "gap": 28, "align": "center", "style": { "bg": 0 }, "padding": [18, 28, 18, 28], "children": [
      { "type": "text", "text": "{{ current.temp }}{{ units }}", "style": { "font_px": 104, "bold": true, "fg": 3 } },
      { "type": "flex", "direction": "column", "gap": 6, "grow": 1, "children": [
        { "type": "text", "text": "{{ title }}", "style": { "font_px": 30, "bold": true, "fg": 3 } },
        { "type": "text", "text": "{{ current.label }}", "style": { "font_px": 26, "fg": 2 } },
        { "type": "text", "text": "feels {{ current.feels }}{{ units}}   {{ current.wind }} mph   {{ current.humidity }}%", "style": { "font_px": 18, "fg": 2 } }
      ] }
    ] },
    { "type": "flex", "direction": "row", "gap": 14, "padding": [18, 24, 0, 24], "children": [
{% for d in days %}
      { "type": "flex", "direction": "column", "gap": 4, "grow": 1, "style": { "bg": {{ d.card_bg }} }, "padding": [12, 16, 14, 16], "children": [
        { "type": "text", "text": "{{ d.label }}", "style": { "font_px": 20, "bold": true, "fg": {{ d.fg }} } },
        { "type": "text", "text": "{{ d.hi }}/{{ d.lo }}", "style": { "font_px": 28, "bold": true, "fg": {{ d.fg }} } },
        { "type": "text", "text": "{{ d.sub }}", "style": { "font_px": 16, "fg": {{ d.sub_fg }} } }
      ] }{% if forloop.last %}{% else %},{% endif %}
{% endfor %}
{% if days.size > 0 %}
      , { "type": "text", "text": "", "grow": 1 }
{% endif %}
    ] },
{% else %}
    { "type": "text", "text": "{{ title }}", "style": { "font_px": 30, "bold": true }, "padding": [16, 24, 0, 24] },
    { "type": "text", "text": "{{ message }}", "style": { "font_px": 26, "fg": 1 }, "padding": [16, 24, 0, 24] },
{% endif %}
    { "type": "text", "text": "{{ footer }}", "style": { "font_px": 15, "fg": 1 }, "padding": [12, 24, 0, 24] }
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
        // Today's card is inverted (black ground); the rest sit on light gray.
        let is_today = label == "Today";
        json!({ "label": label, "hi": d["hi"], "lo": d["lo"], "sub": sub,
            "card_bg": if is_today { 0 } else { 2 },
            "fg": if is_today { 3 } else { 0 },
            "sub_fg": if is_today { 2 } else { 1 } })
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
