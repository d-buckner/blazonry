// SPDX-License-Identifier: AGPL-3.0-only
//! Pins the Home Assistant mapping to a captured live dashboard config.
//!
//! The fixtures under `tests/fixtures/` came from a real Home Assistant
//! instance with the household edited out. If upstream changes the shape of a
//! Lovelace config, or a state stops mapping to a word a person would say, a
//! test here fails instead of the wall display going quietly blank.

use blazonry::extensions::dashboard::{DASHBOARD_TEMPLATE, dashboard_vars};
use blazonry::providers::home_assistant::{build_tiles, flatten_cards, pick_view, tile_json};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).expect("fixture missing — see tests/fixtures/");
    serde_json::from_str(&raw).expect("fixture is not valid JSON")
}

fn views() -> Vec<Value> {
    fixture("ha_dashboard.json")["views"]
        .as_array()
        .cloned()
        .unwrap()
}

fn states() -> BTreeMap<String, Value> {
    fixture("ha_states.json")
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["entity_id"].as_str().unwrap().to_string(), s.clone()))
        .collect()
}

/// The payload a provider run would publish for one captured tab.
fn payload(view: &Value) -> Value {
    let states = states();
    let cards = flatten_cards(view);
    let (tiles, omitted) = build_tiles(&cards, &states, &BTreeMap::new());
    json!({
        "dashboard": "lovelace",
        "dashboard_title": "Overview",
        "view": view["path"],
        "view_title": view["title"],
        "tiles": tiles.iter().map(tile_json).collect::<Vec<_>>(),
        "omitted": omitted,
    })
}

#[test]
fn every_captured_tab_is_selectable_by_path_title_and_index() {
    let vs = views();
    for (i, v) in vs.iter().enumerate() {
        let path = v["path"].as_str().unwrap();
        let title = v["title"].as_str().unwrap();
        assert_eq!(pick_view(&vs, Some(path)).unwrap()["path"], path, "by path");
        assert_eq!(
            pick_view(&vs, Some(title)).unwrap()["path"],
            path,
            "by title"
        );
        assert_eq!(
            pick_view(&vs, Some(&i.to_string())).unwrap()["path"],
            path,
            "by index"
        );
    }
    assert_eq!(
        pick_view(&vs, None).unwrap()["path"],
        vs[0]["path"],
        "nothing selected"
    );
    assert!(
        pick_view(&vs, Some("no-such-tab")).is_err(),
        "a stale tab name must fail loudly"
    );
}

#[test]
fn containers_flatten_to_leaf_cards() {
    let drawn = [
        "grid",
        "sections",
        "section",
        "vertical-stack",
        "horizontal-stack",
    ];
    for v in views() {
        let cards = flatten_cards(&v);
        assert!(!cards.is_empty(), "tab {} produced no cards", v["path"]);
        for c in &cards {
            let t = c["type"].as_str().unwrap_or("");
            assert!(
                !drawn.contains(&t),
                "container {t} survived flattening in {}",
                v["path"]
            );
        }
    }
}

#[test]
fn no_card_vanishes_between_the_config_and_the_payload() {
    // The invariant behind the fail-loud rule: every leaf card the capture held
    // ends up either as a tile or as a counted omission. A tab that maps to
    // nothing is then an error the operator sees, not a blank panel.
    for v in views() {
        let cards = flatten_cards(&v);
        let p = payload(&v);
        let tiles = p["tiles"].as_array().unwrap().len();
        let omitted: u64 = p["omitted"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|v| v.as_u64())
            .sum();
        assert_eq!(
            cards.len(),
            tiles + omitted as usize,
            "tab {} lost cards: {} flattened, {} tiles, {omitted} omitted",
            v["path"],
            cards.len(),
            tiles
        );
    }
}

#[test]
fn cards_a_panel_cannot_draw_are_counted_by_type() {
    let mut seen = std::collections::BTreeSet::new();
    for v in views() {
        let p = payload(&v);
        seen.extend(p["omitted"].as_object().unwrap().keys().cloned());
    }
    // Graphs, a vendor map card, action buttons and shortcuts are all in the
    // capture; none of them can be read on an e-ink panel.
    for kind in [
        "history-graph",
        "custom:some_vendor-card",
        "button",
        "shortcut",
    ] {
        assert!(seen.contains(kind), "{kind} not counted in {seen:?}");
    }
}

#[test]
fn states_become_words_a_person_would_say() {
    let p = payload(views().iter().find(|v| v["path"] == "home").unwrap());
    let find = |title: &str| -> Value {
        p["tiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["title"] == title)
            .cloned()
            .unwrap_or_else(|| panic!("no tile titled {title}: {:?}", p["tiles"]))
    };
    assert_eq!(
        find("Today")["value"],
        "Partly cloudy",
        "weather states are spelled out"
    );
    assert_eq!(
        find("Today")["sub"],
        "56\u{b0}F",
        "the reading the card adds stays visible"
    );
    assert_eq!(
        find("Smoke")["sub"],
        "μg/m³",
        "a long unit moves to the sub line"
    );
    assert_eq!(find("Doors")["value"], "Locked");
    assert_eq!(find("Doors")["on"], false, "a locked door is at rest");
    assert_eq!(find("Basement")["value"], "Off", "a light at rest says Off");

    // The printing tab is all unavailable entities in this capture.
    let p = payload(views().iter().find(|v| v["path"] == "printing").unwrap());
    for t in p["tiles"].as_array().unwrap() {
        assert_eq!(
            t["value"], "Unavailable",
            "{}: unavailable is not Off",
            t["title"]
        );
        assert_eq!(t["on"], false);
    }
}

#[test]
fn every_captured_tab_renders_a_screen() {
    for v in views() {
        let p = payload(&v);
        for cols in [2u64, 3, 4] {
            let vars = dashboard_vars(&p, &json!({ "columns": cols }));
            let report = blazonry::ext_dev::check(DASHBOARD_TEMPLATE, &vars.to_string());
            assert!(
                report.ok,
                "tab {} at {cols} columns: {:?} {}",
                v["path"],
                report.stage,
                report.error.unwrap_or_default()
            );
        }
    }
}

#[test]
fn nothing_is_lost_between_the_payload_and_the_panel() {
    // Tiles placed + tiles that did not fit must account for every renderable
    // tile in the payload, so the footer can never under-report.
    let v = views().into_iter().find(|v| v["path"] == "home").unwrap();
    let p = payload(&v);
    let vars = dashboard_vars(&p, &json!({ "columns": 4 }));
    let placed: usize = vars["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["tiles"]
                .as_array()
                .map(|t| t.iter().filter(|c| c["blank"] != true).count())
                .unwrap_or(0)
        })
        .sum();
    let footer = vars["footer"].as_str().unwrap();
    // Only the "did not fit" figure belongs in this sum; the "cannot be shown"
    // figure counts cards that never became tiles at all.
    let leftover: usize = footer
        .split(" · ")
        .filter(|part| part.contains("did not fit"))
        .filter_map(|part| part.split(' ').next().and_then(|n| n.parse::<usize>().ok()))
        .sum();
    let renderable = p["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] != "heading")
        .count();
    assert_eq!(
        placed + leftover,
        renderable,
        "footer under-counts: {footer}"
    );
    assert!(placed > 0);

    // The same accounting when the panel is deliberately too small, so the
    // "did not fit" figure is exercised rather than always zero.
    let vars = dashboard_vars(&p, &json!({ "columns": 4, "max_tiles": 3 }));
    let footer = vars["footer"].as_str().unwrap();
    let placed: usize = vars["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["tiles"]
                .as_array()
                .map(|t| t.iter().filter(|c| c["blank"] != true).count())
                .unwrap_or(0)
        })
        .sum();
    let leftover: usize = footer
        .split(" · ")
        .filter(|part| part.contains("did not fit"))
        .filter_map(|part| part.split(' ').next().and_then(|n| n.parse::<usize>().ok()))
        .sum();
    assert_eq!(placed, 3, "max_tiles was not honoured");
    assert_eq!(
        placed + leftover,
        renderable,
        "footer under-counts when cards are cut: {footer}"
    );
}
