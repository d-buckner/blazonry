// SPDX-License-Identifier: AGPL-3.0-only
//! Home Assistant provider: one dashboard tab, read from a real HA instance.
//! Publishes the `dashboard` shape.
//!
//! Two protocols, because HA splits them: the dashboard layout is only
//! reachable over the WebSocket API (`get_panels`, `lovelace/config`) — there
//! is no REST endpoint for Lovelace config — while entity values come from the
//! ordinary REST `GET /api/states`. Markdown cards hold Jinja, so each one
//! costs a `POST /api/template`.
//!
//! The payload is *display-ready*: every card the panel can express becomes one
//! tile of text, with states already humanized (a door contact reporting "on"
//! reads "Open"). Cards that cannot survive the trip to a 4-level e-ink panel —
//! graphs, maps, pictures, third-party cards, action-only buttons — are counted
//! in `omitted` rather than vanishing, so an upstream schema change shows up as
//! a number on the screen instead of a quietly empty panel. A view that yields
//! no tiles at all is an error, not a blank screen.
//!
//! Config: { url, token, dashboard?: url_path, view?: path | title | index }
//!
//! This module is the only place in blazonry that reads Lovelace card config.
//! `mapping_tests.rs` pins the shapes against a captured HA 2026.11.1
//! config, so an upstream change fails a test here rather than the wall at 7am.
//! Lovelace `grid_options` is deliberately NOT read: it sizes a scrollable web
//! page whose width the operator chose, and a panel is always 800x480.

use crate::providers::{Provider, Snapshot};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::pin::Pin;

pub struct HomeAssistant;

pub const MANIFEST: &str = include_str!("home_assistant.manifest.json");

/// What one card contributes to the screen. `heading` is a section label,
/// `tile` a title/value pair, `group` a block of key/value rows, `progress` a
/// filled bar, `text` a free-form line (rendered Markdown).
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub kind: &'static str,
    pub title: String,
    pub value: String,
    pub unit: String,
    pub sub: String,
    /// Active state — the extension inverts the tile to show it, because a
    /// panel has no colour to tint with.
    pub on: bool,
    /// Filled bar for `progress` tiles.
    pub progress: Option<f32>,
    /// Key/value rows for `group` tiles.
    pub rows: Vec<(String, String)>,
}

fn validate(config: &Value) -> anyhow::Result<()> {
    let url = config.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
        anyhow::anyhow!("config.url is required (for example https://ha.lan:8123)")
    })?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("config.url must be an http(s) URL");
    }
    let token = config
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("config.token is required (a long-lived access token)"))?;
    if token.trim().is_empty() {
        anyhow::bail!("config.token is empty");
    }
    for key in ["dashboard", "view"] {
        if let Some(v) = config.get(key)
            && !v.is_null()
            && v.as_str().is_none()
        {
            anyhow::bail!("config.{key} must be a string");
        }
    }
    Ok(())
}

impl Provider for HomeAssistant {
    fn kind(&self) -> &'static str {
        "home_assistant"
    }

    fn manifest(&self) -> &'static crate::extensions::ProviderManifest {
        static M: std::sync::OnceLock<crate::extensions::ProviderManifest> =
            std::sync::OnceLock::new();
        M.get_or_init(|| {
            crate::extensions::ProviderManifest::parse(
                MANIFEST,
                "dashboard/home_assistant.manifest.json",
            )
            .expect("bundled provider manifest is unreadable")
        })
    }

    fn shape(&self) -> &'static str {
        "dashboard"
    }

    /// The one kind with something to browse: Lovelace config is exposed over
    /// no REST endpoint, so this is the only way to offer real tab names
    /// instead of free text. The answer fills this kind's own `dashboard` and
    /// `view` fields, and says which one drives the other.
    fn browse(
        &self,
        config: Value,
    ) -> Option<Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send>>> {
        Some(Box::pin(async move {
            let want = |key: &str| -> anyhow::Result<String> {
                config[key]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("Fill in the {} field first.", key))
            };
            let base = want("url")?;
            let token = want("token")?;
            let dashboard = config["dashboard"].as_str().unwrap_or("lovelace");
            let raw = browse(&base, &token, dashboard).await?;

            let dashboards: Vec<Value> = raw["dashboards"]
                .as_array()
                .map(|ds| {
                    ds.iter()
                        .map(|d| {
                            let path = d["url_path"].as_str().unwrap_or("");
                            json!({ "value": path, "label": format!("{} ({})", d["title"].as_str().unwrap_or(path), path) })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let tabs: Vec<Value> = raw["tabs"]
                .as_array()
                .map(|ts| {
                    ts.iter()
                        .map(|t| {
                            json!({ "value": t["path"], "label": t["title"].as_str().unwrap_or("(untitled)") })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "driver": "dashboard",
                "fields": { "dashboard": dashboards, "view": tabs },
                "note": format!(
                    "Found {} tab(s) in “{}”.",
                    tabs.len(),
                    raw["dashboard_title"].as_str().unwrap_or(dashboard)
                ),
            }))
        }))
    }

    fn validate(&self, config: &Value) -> anyhow::Result<()> {
        validate(config)
    }

    fn fetch(
        &self,
        config: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<Snapshot>> + Send>> {
        Box::pin(async move {
            validate(&config)?;
            let base = config["url"].as_str().unwrap().trim_end_matches('/');
            let token = config["token"].as_str().unwrap();
            let dashboard = config
                .get("dashboard")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("lovelace");
            let view_sel = config
                .get("view")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());

            let (dash_title, dash_config, _) = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                ws_dashboard(base, token, dashboard),
            )
            .await
            .map_err(|_| anyhow::anyhow!("home assistant websocket timed out after 30s"))??;

            let view = pick_view(views_of(&dash_config), view_sel)?;
            let cards = flatten_cards(view);
            let states = rest_states(base, token).await?;

            // Markdown cards hold Jinja, so they need a round trip each. Bounded:
            // a tab full of Markdown blocks must not stall the refresh loop.
            let mut rendered: BTreeMap<String, String> = BTreeMap::new();
            for card in cards
                .iter()
                .filter(|c| c["type"].as_str() == Some("markdown"))
            {
                let src = card["content"].as_str().unwrap_or("");
                if src.is_empty() || rendered.contains_key(src) || rendered.len() >= 4 {
                    continue;
                }
                match rest_template(base, token, src).await {
                    Ok(text) => {
                        rendered.insert(src.to_string(), text);
                    }
                    Err(e) => tracing::warn!(%e, "home assistant markdown card did not render"),
                }
            }

            let (tiles, omitted) = build_tiles(&cards, &states, &rendered);
            if tiles.is_empty() {
                anyhow::bail!(
                    "dashboard '{dashboard}' view '{}' has {} card(s) and none can be shown on a panel ({})",
                    view_title(view),
                    cards.len(),
                    omitted
                        .iter()
                        .map(|(k, n)| format!("{n} {k}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            Ok(Snapshot::Dashboard(json!({
                "dashboard": dashboard,
                "dashboard_title": dash_title,
                "view": view.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                "view_title": view_title(view),
                "tiles": tiles.iter().map(tile_json).collect::<Vec<_>>(),
                "omitted": omitted,
            })))
        })
    }
}

// ---------------------------------------------------------------------------
// Layout: view -> ordered cards
// ---------------------------------------------------------------------------

fn views_of(dash_config: &Value) -> &[Value] {
    dash_config["views"]
        .as_array()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

fn view_title(view: &Value) -> String {
    view.get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Dashboard")
        .to_string()
}

/// Pick the tab to render: by `path`, then by title (case-insensitive), then by
/// zero-based index, else the first view.
pub fn pick_view<'a>(views: &'a [Value], sel: Option<&str>) -> anyhow::Result<&'a Value> {
    if views.is_empty() {
        anyhow::bail!("dashboard has no views");
    }
    let Some(sel) = sel else {
        return Ok(&views[0]);
    };
    if let Some(v) = views.iter().find(|v| v["path"].as_str() == Some(sel)) {
        return Ok(v);
    }
    if let Some(v) = views.iter().find(|v| {
        v["title"]
            .as_str()
            .is_some_and(|t| t.eq_ignore_ascii_case(sel))
    }) {
        return Ok(v);
    }
    if let Ok(i) = sel.parse::<usize>()
        && let Some(v) = views.get(i)
    {
        return Ok(v);
    }
    anyhow::bail!(
        "no view matching '{sel}' (available: {})",
        views
            .iter()
            .map(|v| format!("{}: {}", v["path"].as_str().unwrap_or("?"), view_title(v)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Walk a view into the cards it shows, in order. Sections and grids are
/// containers, not content, so they flatten away; the panel lays its own grid.
/// Section dashboards hang `sections` off the view, legacy dashboards `cards`.
pub fn flatten_cards(view: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for key in ["sections", "cards"] {
        for child in view[key].as_array().into_iter().flatten() {
            walk_cards(child, &mut out);
        }
    }
    out
}

fn walk_cards(node: &Value, out: &mut Vec<Value>) {
    const CONTAINERS: &[&str] = &["grid", "sections", "vertical-stack", "horizontal-stack"];
    if CONTAINERS.contains(&node["type"].as_str().unwrap_or("")) {
        for key in ["cards", "sections"] {
            for child in node[key].as_array().into_iter().flatten() {
                walk_cards(child, out);
            }
        }
        return;
    }
    out.push(node.clone());
}

// ---------------------------------------------------------------------------
// Card -> tile
// ---------------------------------------------------------------------------

/// Map cards to tiles, and count what could not be mapped by card type.
///
/// There is no blocklist of card types. A card we know nothing about that names
/// a single `entity` still gets a tile from that entity's state, which is the
/// friendlier failure mode when HA ships a new card type: the value shows up
/// plainly instead of disappearing. Cards that draw (graphs, maps, pictures)
/// carry `entities`/`image_entity` rather than `entity`, so they fall out and
/// are counted.
pub fn build_tiles(
    cards: &[Value],
    states: &BTreeMap<String, Value>,
    rendered: &BTreeMap<String, String>,
) -> (Vec<Tile>, BTreeMap<String, u64>) {
    let mut tiles = Vec::new();
    let mut omitted: BTreeMap<String, u64> = BTreeMap::new();
    for card in cards {
        let kind = card["type"].as_str().unwrap_or("unknown");
        let tile = match kind {
            "heading" => Some(Tile {
                kind: "heading",
                title: String::new(),
                value: card["heading"].as_str().unwrap_or("").to_string(),
                unit: String::new(),
                sub: String::new(),
                on: false,
                progress: None,
                rows: Vec::new(),
            }),
            "entities" | "glance" => entity_group(card, states),
            "gauge" => gauge_tile(card, states),
            "markdown" => Some(Tile {
                kind: "text",
                title: card["title"].as_str().unwrap_or("").to_string(),
                value: rendered
                    .get(card["content"].as_str().unwrap_or(""))
                    .cloned()
                    .unwrap_or_else(|| "not rendered".to_string()),
                unit: String::new(),
                sub: String::new(),
                on: false,
                progress: None,
                rows: Vec::new(),
            }),
            // Action-only: nothing to read, and a panel cannot press it.
            "button" | "shortcut" if card.get("entity").is_none() => None,
            "media-control" => media_tile(card, states),
            _ if card.get("entity").is_some() => entity_tile(card, states),
            _ => None,
        };
        match tile {
            Some(mut t) => {
                if t.title.is_empty() {
                    t.title = card_name(card, states);
                }
                tiles.push(t);
            }
            None => *omitted.entry(kind.to_string()).or_default() += 1,
        }
    }
    (tiles, omitted)
}

/// A tile as it appears in the published payload.
pub fn tile_json(t: &Tile) -> Value {
    json!({
        "kind": t.kind,
        "title": t.title,
        "value": t.value,
        "unit": t.unit,
        "sub": t.sub,
        "on": t.on,
        "progress": t.progress,
        "rows": t.rows.iter().map(|(k, v)| json!({"k": k, "v": v})).collect::<Vec<_>>(),
    })
}

/// Card's own `name`, else the entity's friendly name, else the entity id.
fn card_name(card: &Value, states: &BTreeMap<String, Value>) -> String {
    if let Some(n) = card.get("name").and_then(|v| v.as_str()) {
        return n.to_string();
    }
    let entity = card["entity"].as_str().unwrap_or("");
    states
        .get(entity)
        .and_then(|s| s["attributes"]["friendly_name"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| entity.to_string())
}

fn entity_tile(card: &Value, states: &BTreeMap<String, Value>) -> Option<Tile> {
    let entity = card["entity"].as_str()?;
    let st = states.get(entity)?;
    let (value, unit, sub, on) = humanize(entity, st);
    Some(Tile {
        kind: "tile",
        title: card
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        value,
        unit,
        sub,
        on,
        progress: None,
        rows: Vec::new(),
    })
}

/// Value and unit as one string. Symbols glue to the number (`50%`, `69.6°F`);
/// spelled-out units take a space, because `6405.9mi` and `39psi` misread as
/// one token.
pub fn with_unit(value: &str, unit: &str) -> String {
    if unit.is_empty() {
        return value.to_string();
    }
    if unit.starts_with('\u{b0}') || unit.starts_with('%') {
        format!("{value}{unit}")
    } else {
        format!("{value} {unit}")
    }
}

/// A gauge becomes a filled bar. HA puts the range on the card, not the entity.
fn gauge_tile(card: &Value, states: &BTreeMap<String, Value>) -> Option<Tile> {
    let entity = card["entity"].as_str()?;
    let st = states.get(entity)?;
    let Some(num) = st["state"].as_str().and_then(|s| s.parse::<f32>().ok()) else {
        return entity_tile(card, states);
    };
    let min = card["min_value"].as_f64().unwrap_or(0.0) as f32;
    let max = card["max_value"].as_f64().unwrap_or(100.0) as f32;
    let frac = if max > min {
        ((num - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let unit = st["attributes"]["unit_of_measurement"]
        .as_str()
        .unwrap_or("")
        .to_string();
    Some(Tile {
        kind: "progress",
        title: String::new(),
        value: with_unit(&trim_num(num as f64), &unit),
        unit: String::new(),
        sub: String::new(),
        on: false,
        progress: Some(frac),
        rows: Vec::new(),
    })
}

fn media_tile(card: &Value, states: &BTreeMap<String, Value>) -> Option<Tile> {
    let entity = card["entity"].as_str()?;
    let st = states.get(entity)?;
    let (value, _, _, on) = humanize(entity, st);
    Some(Tile {
        kind: "tile",
        title: String::new(),
        value,
        unit: String::new(),
        sub: st["attributes"]["media_title"]
            .as_str()
            .or_else(|| st["attributes"]["app_name"].as_str())
            .unwrap_or("")
            .to_string(),
        on,
        progress: None,
        rows: Vec::new(),
    })
}

/// An `entities` or `glance` card becomes one block of key/value rows.
fn entity_group(card: &Value, states: &BTreeMap<String, Value>) -> Option<Tile> {
    let mut rows = Vec::new();
    for item in card["entities"].as_array().into_iter().flatten() {
        let (entity, named) = match item {
            Value::String(s) => (s.as_str(), None),
            Value::Object(o) => (
                o.get("entity")?.as_str()?,
                o.get("name").and_then(|v| v.as_str()),
            ),
            _ => continue,
        };
        let Some(st) = states.get(entity) else {
            continue;
        };
        let key = named
            .map(str::to_string)
            .or_else(|| {
                st["attributes"]["friendly_name"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| entity.to_string());
        let (value, unit, _, _) = humanize(entity, st);
        rows.push((key, with_unit(&value, &unit)));
    }
    if rows.is_empty() {
        return None;
    }
    Some(Tile {
        kind: "group",
        title: card["title"].as_str().unwrap_or("").to_string(),
        value: String::new(),
        unit: String::new(),
        sub: String::new(),
        on: false,
        progress: None,
        rows,
    })
}

// ---------------------------------------------------------------------------
// State humanization
// ---------------------------------------------------------------------------

/// `binary_sensor` states are "on"/"off", which says nothing on a wall. Say
/// what the device class means instead.
const BINARY_WORDS: &[(&str, &str, &str)] = &[
    ("battery", "Low", "Normal"),
    ("cold", "Cold", "Normal"),
    ("connectivity", "Offline", "Online"),
    ("door", "Open", "Closed"),
    ("garage_door", "Open", "Closed"),
    ("heat", "Hot", "Normal"),
    ("light", "Bright", "Dark"),
    ("lock", "Unlocked", "Locked"),
    ("moisture", "Wet", "Dry"),
    ("motion", "Motion", "Clear"),
    ("occupancy", "Present", "Empty"),
    ("opening", "Open", "Closed"),
    ("person", "Detected", "Clear"),
    ("problem", "Problem", "OK"),
    ("presence", "Home", "Away"),
    ("smoke", "Smoke!", "Clear"),
    ("sound", "Sound", "Quiet"),
    ("vibration", "Vibrating", "Still"),
    ("window", "Open", "Closed"),
];

/// HA weather states are machine tokens; the panel has room for the phrase.
const WEATHER_WORDS: &[(&str, &str)] = &[
    ("clear-night", "Clear night"),
    ("cloudy", "Cloudy"),
    ("exceptional", "Extreme weather"),
    ("fog", "Fog"),
    ("hail", "Hail"),
    ("lightning", "Lightning"),
    ("lightning-rainy", "Thunderstorms"),
    ("partlycloudy", "Partly cloudy"),
    ("pouring", "Pouring rain"),
    ("rainy", "Rain"),
    ("snowy", "Snow"),
    ("snowy-rainy", "Sleet"),
    ("sunny", "Sunny"),
    ("windy", "Windy"),
    ("windy-variant", "Windy"),
];

/// HA state -> (value, unit, sub, on). Presentation-ready text: a panel has
/// four grey levels and no hover text.
pub fn humanize(entity: &str, st: &Value) -> (String, String, String, bool) {
    let domain = entity.split('.').next().unwrap_or("");
    let raw = st["state"].as_str().unwrap_or("");
    let attrs = &st["attributes"];
    let unit = attrs["unit_of_measurement"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // A device that stopped reporting is not "Off". Say so.
    if matches!(raw, "unavailable" | "unknown") {
        return (words(raw), String::new(), String::new(), false);
    }

    let numeric = raw.parse::<f64>().ok();
    let on = raw == "on";
    let pct = |key: &str| -> String {
        attrs[key]
            .as_f64()
            .map(|v| format!("{:.0}%", v))
            .unwrap_or_default()
    };

    let value = match domain {
        "binary_sensor" => {
            let dc = attrs["device_class"].as_str().unwrap_or("");
            let (yes, no) = BINARY_WORDS
                .iter()
                .find(|(d, _, _)| *d == dc)
                .map(|(_, y, n)| (*y, *n))
                .unwrap_or(("On", "Off"));
            (if on { yes } else { no }).to_string()
        }
        "cover" => match raw {
            "open" | "opening" => "Open",
            "closed" | "closing" => "Closed",
            "stopped" => "Stopped",
            other => other,
        }
        .to_string(),
        "lock" => match raw {
            "locked" => "Locked",
            "unlocked" => "Unlocked",
            "jamming" => "Jammed",
            other => other,
        }
        .to_string(),
        "person" | "zone" => match raw {
            "home" => "Home".to_string(),
            "not_home" => "Away".to_string(),
            other => words(other),
        },
        "device_tracker" => match raw {
            "home" => "Home".to_string(),
            "not_home" => "Away".to_string(),
            other => words(other),
        },
        "sun" => match raw {
            "above_horizon" => "Day".to_string(),
            "below_horizon" => "Night".to_string(),
            other => words(other),
        },
        "update" => match raw {
            "on" => "Update ready".to_string(),
            "off" => "Up to date".to_string(),
            other => words(other),
        },
        "weather" => WEATHER_WORDS
            .iter()
            .find(|(s, _)| *s == raw)
            .map(|(_, w)| w.to_string())
            .unwrap_or_else(|| words(raw)),
        "climate" => attrs["current_temperature"]
            .as_f64()
            .map(|t| format!("{}°", trim_num(t)))
            .unwrap_or_else(|| words(raw)),
        "light" | "switch" | "input_boolean" | "fan" | "group" | "remote" | "counter" => {
            if on { "On" } else { "Off" }.to_string()
        }
        "media_player" => match raw {
            "playing" => "Playing",
            "paused" => "Paused",
            "idle" | "standby" => "Idle",
            "off" => "Off",
            _ => "On",
        }
        .to_string(),
        "vacuum" => match raw {
            "cleaning" => "Cleaning".to_string(),
            "docked" => "Docked".to_string(),
            "paused" => "Paused".to_string(),
            "returning" => "Returning".to_string(),
            other => words(other),
        },
        "alarm_control_panel" => match raw {
            "armed_away" => "Armed away".to_string(),
            "armed_home" => "Armed home".to_string(),
            "armed_night" => "Armed night".to_string(),
            "armed_vacation" => "Armed vacation".to_string(),
            "disarmed" => "Disarmed".to_string(),
            "triggered" => "TRIGGERED".to_string(),
            other => words(other),
        },
        _ => numeric.map(trim_num).unwrap_or_else(|| words(raw)),
    };

    let sub = match domain {
        "light" => {
            // brightness is null while off; 1..=254 while on.
            attrs["brightness"]
                .as_f64()
                .map(|b| format!("{:.0}%", (b / 254.0 * 100.0).round()))
                .unwrap_or_default()
        }
        "cover" => pct("current_position"),
        "climate" => words(attrs["hvac_action"].as_str().unwrap_or("")),
        "weather" => format!(
            "{}{}",
            attrs["temperature"]
                .as_f64()
                .map(trim_num)
                .unwrap_or_default(),
            attrs["temperature_unit"].as_str().unwrap_or("°")
        ),
        "media_player" => attrs["media_title"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    };

    // A long unit next to a 34px number is unreadable across a room, so only
    // short units ride the value line; longer ones go on the sub line.
    let long_unit = if unit.chars().count() > 3 {
        unit.clone()
    } else {
        String::new()
    };
    let unit = if long_unit.is_empty() {
        unit
    } else {
        String::new()
    };
    let sub = if sub.is_empty() { long_unit } else { sub };

    (value, unit, sub, active(domain, raw, attrs))
}

/// Inverted means "not at rest" — the panel has no colour to tint with, so the
/// fill carries the signal. Each domain decides its own resting state.
fn active(domain: &str, raw: &str, attrs: &Value) -> bool {
    match domain {
        "light" | "switch" | "input_boolean" | "fan" | "group" | "remote" | "counter"
        | "binary_sensor" | "update" => raw == "on",
        "lock" => raw != "locked",
        "cover" => matches!(raw, "open" | "opening"),
        "person" | "zone" | "device_tracker" => raw == "home",
        "vacuum" => raw == "cleaning",
        "media_player" => !matches!(raw, "off" | "idle" | "standby"),
        "alarm_control_panel" => raw != "disarmed",
        "climate" => !attrs["hvac_action"]
            .as_str()
            .is_none_or(|a| a == "idle" || a == "off"),
        _ => false,
    }
}

/// `partly_cloudy` -> `Partly cloudy`.
fn words(s: &str) -> String {
    let text = s.replace(['_', '-'], " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `21.0` -> `21`, `69.62` -> `69.7`. Panels have no room for false precision.
fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 {
        return format!("{}", n as i64);
    }
    format!("{:.1}", n)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

fn ws_url(base: &str) -> String {
    let ws = base
        .strip_prefix("https://")
        .map(|r| format!("wss://{r}"))
        .unwrap_or_else(|| format!("ws://{}", base.trim_start_matches("http://")));
    format!("{}/api/websocket", ws.trim_end_matches('/'))
}

/// Connect, authenticate, ask for the panel list and one dashboard's config.
/// Command ids must strictly increase, or HA answers `id_reuse`.
async fn ws_dashboard(
    base: &str,
    token: &str,
    dashboard: &str,
) -> anyhow::Result<(String, Value, Value)> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(base))
        .await
        .map_err(|e| {
            anyhow::anyhow!("home assistant websocket connect to {}: {e}", ws_url(base))
        })?;

    let mut commands = vec![
        json!({"type": "get_panels"}),
        json!({"type": "lovelace/config", "url_path": dashboard, "force": false}),
    ];
    let mut results: Vec<Value> = Vec::new();
    let mut id = 0u64;
    let mut need_auth = true;

    loop {
        if need_auth {
            ws.send(Message::Text(
                json!({"type": "auth", "access_token": token})
                    .to_string()
                    .into(),
            ))
            .await?;
            need_auth = false;
        } else if let Some(mut cmd) = commands.pop() {
            // `pop` runs the list in reverse; ids still increase.
            id += 1;
            cmd["id"] = json!(id);
            ws.send(Message::Text(cmd.to_string().into())).await?;
        } else if results.len() >= 2 {
            break;
        }
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let Ok(m) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match m["type"].as_str().unwrap_or("") {
                    "auth_failed" | "auth_invalid" => {
                        anyhow::bail!("home assistant rejected the access token")
                    }
                    "result" => {
                        if m["success"].as_bool() == Some(false) {
                            anyhow::bail!(
                                "home assistant: {}",
                                m["error"]["message"].as_str().unwrap_or("command failed")
                            );
                        }
                        results.push(m["result"].clone());
                    }
                    _ => {}
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => anyhow::bail!("home assistant websocket: {e}"),
            None => anyhow::bail!("home assistant websocket closed early"),
        }
    }

    // results came back in reverse command order: [lovelace/config, get_panels].
    let dash_config = results.first().cloned().unwrap_or(Value::Null);
    let panels = results.get(1).cloned().unwrap_or(Value::Null);
    let panel = panels
        .as_object()
        .and_then(|p| {
            p.values().find(|v| {
                v["component_name"].as_str() == Some("lovelace")
                    && v["url_path"].as_str() == Some(dashboard)
            })
        })
        .cloned();
    let Some(panel) = panel else {
        let known: Vec<String> = panels
            .as_object()
            .map(|p| {
                p.values()
                    .filter(|v| v["component_name"].as_str() == Some("lovelace"))
                    .filter_map(|v| v["url_path"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        anyhow::bail!(
            "no dashboard '{dashboard}' (available: {})",
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        );
    };
    Ok((
        panel["title"].as_str().unwrap_or(dashboard).to_string(),
        dash_config,
        panels,
    ))
}

/// Live dashboard and tab list for the admin UI. Lovelace config is not
/// exposed over REST, so this is the only way to offer real tab names instead
/// of free text.
pub async fn browse(base: &str, token: &str, dashboard: &str) -> anyhow::Result<Value> {
    let (dash_title, dash_config, panels) = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        ws_dashboard(base, token, dashboard),
    )
    .await
    .map_err(|_| anyhow::anyhow!("home assistant websocket timed out after 30s"))??;

    let dashboards: Vec<Value> = panels
        .as_object()
        .map(|p| {
            p.values()
                .filter(|v| v["component_name"].as_str() == Some("lovelace"))
                .map(|v| {
                    json!({
                        "url_path": v["url_path"].as_str().unwrap_or(""),
                        "title": v["title"].as_str().unwrap_or(""),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let tabs: Vec<Value> = views_of(&dash_config)
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!({
                "path": v["path"].as_str().map(str::to_string).unwrap_or_else(|| i.to_string()),
                "title": view_title(v),
                "icon": v["icon"].as_str().unwrap_or(""),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "dashboard": dashboard,
        "dashboard_title": dash_title,
        "dashboards": dashboards,
        "tabs": tabs,
    }))
}

/// `GET /api/states` keyed by entity id.
async fn rest_states(base: &str, token: &str) -> anyhow::Result<BTreeMap<String, Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client
        .get(format!("{base}/api/states"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?
        .error_for_status()?;
    let states: Vec<Value> = resp.json().await?;
    Ok(states
        .into_iter()
        .filter_map(|s| {
            let id = s["entity_id"].as_str()?.to_string();
            Some((id, s))
        })
        .collect())
}

/// `POST /api/template` — Lovelace Markdown cards hold Jinja, so they only
/// become text once HA renders them.
async fn rest_template(base: &str, token: &str, template: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client
        .post(format!("{base}/api/template"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "text/plain;charset=UTF-8")
        .body(template.to_string())
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("template render failed: {}", resp.status());
    }
    Ok(resp.text().await?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rendered_markdown_becomes_the_tile_value() {
        let cards = vec![json!({ "type": "markdown", "title": "Parked At",
                                 "content": "{{ states('sensor.loc') }}" })];
        let st = states(&[("sensor.loc", "x", json!({}))]);

        let mut done = BTreeMap::new();
        done.insert(
            "{{ states('sensor.loc') }}".to_string(),
            "Ravenna".to_string(),
        );
        let (tiles, _) = build_tiles(&cards, &st, &done);
        assert_eq!(tiles[0].kind, "text");
        assert_eq!(tiles[0].title, "Parked At");
        assert_eq!(
            tiles[0].value, "Ravenna",
            "template result never reached the tile"
        );

        // The template call failed or was skipped: say so rather than show nothing.
        let (tiles, _) = build_tiles(&cards, &st, &BTreeMap::new());
        assert_eq!(tiles[0].value, "not rendered");
    }

    #[test]
    fn symbols_glue_to_the_number_and_words_get_a_space() {
        assert_eq!(with_unit("50", "%"), "50%");
        assert_eq!(with_unit("69.6", "\u{b0}F"), "69.6\u{b0}F");
        assert_eq!(with_unit("40", "psi"), "40 psi");
        assert_eq!(with_unit("6405.9", "mi"), "6405.9 mi");
        assert_eq!(with_unit("Off", ""), "Off");
    }

    #[test]
    fn validate_requires_url_and_token() {
        assert!(validate(&json!({})).is_err());
        assert!(validate(&json!({"url": "ha.lan", "token": "t"})).is_err());
        assert!(validate(&json!({"url": "https://ha.lan", "token": "  "})).is_err());
        assert!(validate(&json!({"url": "https://ha.lan", "token": "t"})).is_ok());
        assert!(
            validate(&json!({"url": "https://ha.lan", "token": "t", "view": 3})).is_err(),
            "view must be a string or absent"
        );
    }

    #[test]
    fn ws_url_tracks_scheme_and_base_path() {
        assert_eq!(
            ws_url("https://ha.lan:8123"),
            "wss://ha.lan:8123/api/websocket"
        );
        assert_eq!(ws_url("http://ha.lan/ha"), "ws://ha.lan/ha/api/websocket");
    }

    #[test]
    fn pick_view_by_path_then_title_then_index() {
        let views = vec![
            json!({"path": "home", "title": "Home"}),
            json!({"path": "security", "title": "Security"}),
        ];
        assert_eq!(pick_view(&views, None).unwrap()["path"], "home");
        assert_eq!(
            pick_view(&views, Some("security")).unwrap()["path"],
            "security"
        );
        assert_eq!(
            pick_view(&views, Some("SECURITY")).unwrap()["path"],
            "security"
        );
        assert_eq!(pick_view(&views, Some("1")).unwrap()["path"], "security");
        let err = pick_view(&views, Some("nope")).unwrap_err().to_string();
        assert!(err.contains("home: Home"), "error lists tabs: {err}");
    }

    #[test]
    fn sections_and_grids_flatten_away() {
        let view = json!({"sections": [
            {"type": "grid", "cards": [
                {"type": "heading", "heading": "Lights"},
                {"type": "tile", "entity": "light.a"}
            ]},
            {"type": "grid", "cards": [{"type": "tile", "entity": "light.b"}]}
        ]});
        let flat = flatten_cards(&view);
        let kinds: Vec<&str> = flat.iter().map(|c| c["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["heading", "tile", "tile"]);
    }

    #[test]
    fn unmappable_cards_are_counted_not_dropped_silently() {
        let cards = vec![
            json!({"type": "heading", "heading": "H"}),
            json!({"type": "map", "entities": []}),
            json!({"type": "history-graph"}),
            json!({"type": "custom:valetudo-map-card", "vacuum": "vacuum.r"}),
            json!({"type": "button", "name": "Preheat"}),
            json!({"type": "tile", "entity": "light.a"}),
            // Unknown to us, but it names an entity, so its state is shown.
            json!({"type": "custom:some-new-card", "entity": "sensor.x"}),
        ];
        let states = states(&[("light.a", "on", json!({})), ("sensor.x", "7", json!({}))]);
        let (tiles, omitted) = build_tiles(&cards, &states, &BTreeMap::new());
        assert_eq!(tiles.len(), 3, "heading + light tile + unknown-card tile");
        assert_eq!(omitted["map"], 1);
        assert_eq!(omitted["history-graph"], 1);
        assert_eq!(omitted["custom:valetudo-map-card"], 1);
        assert_eq!(omitted["button"], 1);
        assert!(!omitted.contains_key("custom:some-new-card"));
    }

    #[test]
    fn entities_card_becomes_one_group_block() {
        let card = json!({"type": "entities", "title": "Sensors", "entities": [
            "sensor.temp", {"entity": "light.a", "name": "Custom name"}
        ]});
        let states = states(&[
            (
                "sensor.temp",
                "19.4",
                json!({"unit_of_measurement": "°C", "friendly_name": "Temp"}),
            ),
            ("light.a", "off", json!({})),
        ]);
        let group = entity_group(&card, &states).unwrap();
        assert_eq!(group.kind, "group");
        assert_eq!(group.title, "Sensors");
        assert_eq!(group.rows[0], ("Temp".to_string(), "19.4°C".to_string()));
        assert_eq!(
            group.rows[1],
            ("Custom name".to_string(), "Off".to_string())
        );
    }

    #[test]
    fn gauge_uses_the_card_range_not_the_entity() {
        let card = json!({"type": "gauge", "entity": "sensor.p", "min_value": 10, "max_value": 60});
        let states = states(&[("sensor.p", "35", json!({"unit_of_measurement": "%"}))]);
        let t = gauge_tile(&card, &states).unwrap();
        assert_eq!(t.kind, "progress");
        assert_eq!(t.progress, Some(0.5));
        assert_eq!(t.value, "35%");
    }

    #[test]
    fn binary_sensors_say_what_they_mean() {
        let dc = |state: &str, class: &str| json!({"state": state, "attributes": {"device_class": class}});
        assert_eq!(
            humanize("binary_sensor.front_door", &dc("on", "door")).0,
            "Open"
        );
        assert_eq!(
            humanize("binary_sensor.front_door", &dc("off", "door")).0,
            "Closed"
        );
        assert_eq!(
            humanize("binary_sensor.hall", &dc("on", "motion")).0,
            "Motion"
        );
        assert_eq!(
            humanize("binary_sensor.leak", &dc("off", "moisture")).0,
            "Dry"
        );
        assert_eq!(humanize("binary_sensor.x", &dc("on", "whatever")).0, "On");
    }

    #[test]
    fn a_silent_device_is_not_reported_off() {
        for state in ["unavailable", "unknown"] {
            let (v, _, _, on) = humanize("light.bed", &json!({"state": state, "attributes": {}}));
            assert_eq!((v.as_str(), on), (words(state).as_str(), false));
        }
    }

    #[test]
    fn domains_get_panel_words_and_units() {
        let s = |state: &str, attrs: Value| json!({"state": state, "attributes": attrs});
        let (v, u, sub, on) = humanize("light.bed", &s("on", json!({"brightness": 127})));
        assert_eq!(
            (v.as_str(), u.as_str(), sub.as_str(), on),
            ("On", "", "50%", true)
        );
        let (v, _, _, _) = humanize("light.off", &s("off", json!({"brightness": null})));
        assert_eq!(v, "Off");
        assert_eq!(
            humanize(
                "sensor.temp",
                &s("69.62", json!({"unit_of_measurement": "°F"}))
            )
            .0,
            "69.6"
        );
        let (v, u, _, _) = humanize(
            "sensor.humidity",
            &s("40.0", json!({"unit_of_measurement": "%"})),
        );
        assert_eq!((v.as_str(), u.as_str()), ("40", "%"));
        // A long unit cannot ride the big number line.
        let (v, u, sub, _) = humanize(
            "sensor.pm",
            &s("16.05", json!({"unit_of_measurement": "µg/m³"})),
        );
        assert_eq!(
            (v.as_str(), u.as_str(), sub.as_str()),
            ("16.1", "", "µg/m³")
        );
        assert_eq!(
            humanize("lock.front", &s("unlocked", json!({}))).0,
            "Unlocked"
        );
        assert_eq!(
            humanize(
                "weather.home",
                &s(
                    "partlycloudy",
                    json!({"temperature": 56, "temperature_unit": "°F"})
                )
            )
            .0,
            "Partly cloudy"
        );
        assert_eq!(humanize("person.dan", &s("home", json!({}))).0, "Home");
        assert_eq!(
            humanize("cover.blinds", &s("closed", json!({"current_position": 0}))).2,
            "0%"
        );
        assert_eq!(
            humanize("sensor.status", &s("power_off", json!({}))).0,
            "Power off"
        );
    }

    #[test]
    fn words_and_numbers_are_panel_sized() {
        assert_eq!(words("partly_cloudy"), "Partly cloudy");
        assert_eq!(words(""), "");
        assert_eq!(trim_num(21.0), "21");
        assert_eq!(trim_num(21.44), "21.4");
    }

    fn states(items: &[(&str, &str, Value)]) -> BTreeMap<String, Value> {
        items
            .iter()
            .map(|(id, state, attrs)| {
                (
                    (*id).to_string(),
                    json!({"entity_id": id, "state": state, "attributes": attrs}),
                )
            })
            .collect()
    }
}
