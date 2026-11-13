// SPDX-License-Identifier: AGPL-3.0-only
//! Home Assistant extension — one tab, re-laid out for e-ink.
//!
//! Requirement: HomeAssistant shape, cardinality exactly 1 (one screen shows
//! one tab; a second tab is a second extension).
//!
//! This is deliberately *not* a screenshot of Lovelace. A panel is 800x480, has
//! four grey levels, no colour, no scroll, and is read from across a room, so
//! the layout is the panel's own:
//!
//! - **Fill carries state.** An active tile (light on, door open, unlocked,
//!   vacuum cleaning) is inverted black-on-white. Colour is unavailable, and
//!   "which of these nine is on" is the only question a wall display gets.
//! - **Uniform grid.** Lovelace `grid_options` sizes a scrollable page whose
//!   width the operator chose; here every tile is one cell of a `columns` grid,
//!   so type size stays legible instead of shrinking to fit the widest card.
//! - **Fit before emit.** The renderer never wraps text, so every string is
//!   measured and ellipsized to its cell (`crate::render::fit`).
//! - **Nothing disappears quietly.** Cards that did not fit, and cards the
//!   provider could not express, are counted in the footer.
//!
//! Settings (JSON):
//!   provider_id: i64          the Home Assistant source (default: first)
//!   title: string             header title (default: the tab's own title)
//!   columns: i64              tiles per row, 2..=4 (default 4)
//!   max_tiles: i64            cap beyond the pixel budget, 0 = as many as fit
//!   show_headings: bool       keep Lovelace section headings (default true)

use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

/// The provider module is named for its kind, as in every folder. Here the
/// extension and its only provider share a kind, so the names nest.
#[allow(clippy::module_inception)]
pub mod home_assistant;
#[cfg(test)]
#[path = "mapping_tests.rs"]
mod mapping_tests;

use crate::render::{fit, fit_size, text_width};

/// Panel geometry, in the renderer's pixels.
const WIDTH: f32 = 800.0;
const HEIGHT: f32 = 480.0;
/// Page inset, left and right.
const PAD: f32 = 20.0;
/// Gap between rows and between tiles in a row.
const GAP: f32 = 6.0;

const HEADER_H: f32 = 48.0;
const FOOTER_H: f32 = 25.0;
const HEADING_H: f32 = 24.0;
/// One tile row: padding + title line + value line. The sub sits on the value
/// line right-aligned, so it costs no vertical space.
const TILE_H: f32 = 70.0;
/// A `group` block: padding + title + one key/value line.
const GROUP_BASE_H: f32 = 35.0;
const GROUP_ROW_H: f32 = 23.0;
/// A `progress` or `text` block.
const BLOCK_H: f32 = 63.0;

const TITLE_PX: f32 = 15.0;
const VALUE_PX: f32 = 28.0;
/// Floor for a value that would otherwise have to be cut: long enough to read
/// across a room, short enough that a two-word state still lands whole.
const VALUE_MIN_PX: f32 = 18.0;
const SUB_PX: f32 = 14.0;
const BLOCK_PX: f32 = 17.0;
/// Horizontal padding inside a tile.
const TILE_PAD_X: f32 = 10.0;

/// Liquid -> Screen JSON. Every row part ends with a comma; the footer is the
/// only untrailing part, so the loop never has to know if it is last.
pub const MANIFEST: &str = include_str!("manifest.json");

pub const HOME_ASSISTANT_TEMPLATE: &str = include_str!("template.liquid");

pub struct HomeAssistantTab;

/// Greedy two-line word wrap: `Some(lines)` when the text splits into at most
/// two lines that each fit at `px`. `None` means the caller must ellipsize.
fn wrap2(text: &str, max_px: f32, px: f32) -> Option<Vec<String>> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if cur.is_empty() || text_width(&candidate, px, true) <= max_px {
            cur = candidate;
            continue;
        }
        if lines.len() + 1 >= 2 {
            return None;
        }
        lines.push(std::mem::take(&mut cur));
        cur = word.to_string();
        if text_width(&cur, px, true) > max_px {
            return None;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    (lines.len() == 2).then_some(lines)
}

/// Snapshot payload -> Liquid vars. Fills rows until the panel is full, then
/// counts what is left over instead of dropping it silently.
pub fn tab_vars(snap: &Value, settings: &Value) -> Value {
    let cols = settings
        .get("columns")
        .and_then(|v| v.as_i64())
        .unwrap_or(4)
        .clamp(2, 4) as usize;
    let show_headings = settings
        .get("show_headings")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let max_tiles = settings
        .get("max_tiles")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0) as usize;

    let tiles: Vec<&Value> = snap["tiles"]
        .as_array()
        .map(|t| t.iter())
        .unwrap()
        .collect();
    let budget = HEIGHT - HEADER_H - FOOTER_H - GAP;
    let block_w = WIDTH - 2.0 * PAD - 28.0;

    let mut rows: Vec<Value> = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let mut heading: Option<String> = None;
    let mut used = 0.0f32;
    let mut shown = 0usize;
    let mut stopped_at = 0usize;

    // A row is only started when the whole row fits, and a heading is only
    // emitted once something follows it: a heading at the bottom of a panel
    // would name a section the reader never gets.
    fn pay_heading(rows: &mut Vec<Value>, heading: &mut Option<String>, used: &mut f32) {
        if let Some(text) = heading.take() {
            *used += HEADING_H + GAP;
            rows.push(json!({"heading": true, "text": text}));
        }
    }

    // Short rows are padded with blank cells: a lone tile with `grow` would
    // otherwise stretch across the whole panel.
    fn close_row(
        rows: &mut Vec<Value>,
        pending: &mut Vec<Value>,
        used: &mut f32,
        cols: usize,
        budget: f32,
        pad: bool,
    ) {
        if pending.is_empty() {
            return;
        }
        // Width is a property of the row, not of the grid: an unpadded last row
        // is wider per tile, and fitting it at grid width would cut text that
        // had room.
        let n = if pad { cols } else { pending.len() }.max(1);
        let cell_w = (WIDTH - 2.0 * PAD - GAP * (n as f32 - 1.0)) / n as f32 - 2.0 * TILE_PAD_X;
        if pending.is_empty() {
            return;
        }
        // One value size for the whole row: mixed sizes side by side read as a
        // bug, so the row takes the largest size every value in it fits at.
        let avail = |c: &Value| {
            let sub = c["sub"].as_str().unwrap_or("");
            let taken = if sub.is_empty() {
                0.0
            } else {
                text_width(sub, SUB_PX, false) + 8.0
            };
            (cell_w - taken).max(48.0)
        };
        let vpx = pending
            .iter()
            .map(|c| {
                fit_size(
                    c["value"].as_str().unwrap_or(""),
                    avail(c),
                    VALUE_PX,
                    VALUE_MIN_PX,
                    true,
                )
                .1
            })
            .fold(VALUE_PX, f32::min);
        // A value that will not fit on one line splits onto two instead of
        // being cut: "Partly clo…" says less than two short lines.
        let mut extra = 0.0f32;
        for c in pending.iter_mut() {
            let title = std::mem::replace(&mut c["title"], json!(""));
            c["title"] = json!(fit(title.as_str().unwrap_or(""), cell_w, TITLE_PX, false));
            let v = std::mem::replace(&mut c["value"], json!(""));
            let v = v.as_str().unwrap_or("").to_string();
            let w = avail(c);
            c["vpx"] = json!(vpx);
            if text_width(&v, vpx, true) <= w {
                c["value"] = json!(v);
            } else if let Some(lines) = wrap2(&v, w, vpx) {
                extra = extra.max(vpx * 1.35);
                c["value"] = json!("");
                c["lines"] = json!(lines);
            } else {
                c["value"] = json!(fit(&v, w, vpx, true));
            }
            let sub = c["sub"].as_str().unwrap_or("").to_string();
            c["sub"] = json!(fit(&sub, cell_w * 0.45, SUB_PX, false));
        }
        let mut h = TILE_H + extra;
        if *used + h + GAP > budget {
            // The wrap cost more than the panel had left; fall back to one line.
            for c in pending.iter_mut() {
                if c["lines"].is_array() {
                    let v = c["lines"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|l| l.as_str().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join(" ");
                    c["value"] = json!(fit(&v, avail(c), vpx, true));
                    c["lines"] = json!(null);
                }
            }
            h = TILE_H;
        }
        *used += h + GAP;
        let mut cells = std::mem::take(pending);
        if pad {
            while cells.len() < cols {
                cells.push(json!({"blank": true}));
            }
        }
        rows.push(json!({"tiles": cells, "height": h}));
    }

    for (i, t) in tiles.iter().enumerate() {
        if max_tiles > 0 && shown >= max_tiles {
            stopped_at = i;
            break;
        }
        let kind = t["kind"].as_str().unwrap_or("tile");
        let title = t["title"].as_str().unwrap_or("");
        let value = home_assistant::with_unit(
            t["value"].as_str().unwrap_or(""),
            t["unit"].as_str().unwrap_or(""),
        );
        let sub = t["sub"].as_str().unwrap_or("").to_string();

        if kind == "heading" {
            if show_headings && !value.is_empty() {
                // A section starts a fresh row. Letting a row straddle a
                // heading would print the label under tiles from the section
                // before it, which reads as a lie on the panel.
                close_row(&mut rows, &mut pending, &mut used, cols, budget, true);
                heading = Some(fit(&value, WIDTH - 2.0 * PAD, 16.0, true));
            }
            continue;
        }

        // A tile joins the grid; a row is only started when it fits whole.
        if !matches!(kind, "group" | "progress" | "text") {
            if pending.is_empty() {
                let need = TILE_H
                    + GAP
                    + if heading.is_some() {
                        HEADING_H + GAP
                    } else {
                        0.0
                    };
                if used + need > budget {
                    stopped_at = i;
                    break;
                }
                pay_heading(&mut rows, &mut heading, &mut used);
            }
            let on = t["on"].as_bool().unwrap_or(false);
            pending.push(json!({
                "title": title,
                "value": value,
                "sub": sub,
                "bg": if on { 0 } else { 2 },
                "fg": if on { 3 } else { 0 },
                "fg2": if on { 2 } else { 1 },
            }));
            shown += 1;
            if pending.len() >= cols {
                close_row(&mut rows, &mut pending, &mut used, cols, budget, true);
            }
            continue;
        }

        // Full-width block: close any partial row, then see if it fits.
        close_row(&mut rows, &mut pending, &mut used, cols, budget, true);
        let head_h = if heading.is_some() {
            HEADING_H + GAP
        } else {
            0.0
        };
        let (h, node) = match kind {
            "group" => {
                let all = t["rows"]
                    .as_array()
                    .map(|r| r.iter())
                    .unwrap()
                    .collect::<Vec<_>>();
                let room =
                    ((budget - used - head_h - GROUP_BASE_H - GAP) / GROUP_ROW_H).max(0.0) as usize;
                let keep = if all.is_empty() {
                    0
                } else {
                    room.max(1).min(all.len())
                };
                (
                    GROUP_BASE_H + GROUP_ROW_H * keep as f32,
                    json!({
                        "group": true,
                        "title": fit(title, block_w, TITLE_PX, true),
                        "height": GROUP_BASE_H + GROUP_ROW_H * keep as f32,
                        "rows": all[..keep].iter().map(|r| json!({
                            "k": fit(r["k"].as_str().unwrap_or(""), block_w * 0.62, BLOCK_PX, false),
                            "v": fit(r["v"].as_str().unwrap_or(""), block_w * 0.34, BLOCK_PX, true),
                        })).collect::<Vec<_>>(),
                    }),
                )
            }
            "progress" => (
                BLOCK_H,
                json!({
                    "bar": t["progress"].as_f64().unwrap_or(0.0),
                    "title": fit(title, block_w * 0.6, TITLE_PX, false),
                    "value": fit(&value, block_w * 0.35, BLOCK_PX, true),
                    "height": BLOCK_H,
                }),
            ),
            _ => (
                BLOCK_H,
                json!({
                    "text": true,
                    "title": fit(title, block_w, TITLE_PX, false),
                    "value": fit(&value, block_w, 19.0, true),
                    "height": BLOCK_H,
                }),
            ),
        };
        if used + head_h + h + GAP > budget {
            stopped_at = i;
            break;
        }
        pay_heading(&mut rows, &mut heading, &mut used);
        used += h + GAP;
        rows.push(node);
    }
    // The last row keeps the width its siblings would have only when it holds a
    // single tile; two tiles alone on a panel should fill it, not sit in the
    // left half of a four-column grid.
    let pad_last = pending.len() < 2;
    close_row(&mut rows, &mut pending, &mut used, cols, budget, pad_last);
    if stopped_at == 0 && rows.is_empty() {
        stopped_at = tiles.len();
    }
    let leftover = if stopped_at > 0 {
        tiles.len() - stopped_at
    } else {
        0
    };

    let mut missing: Vec<String> = Vec::new();
    if leftover > 0 {
        missing.push(format!("{leftover} did not fit"));
    }
    if let Some(omitted) = snap["omitted"].as_object() {
        let n: u64 = omitted.values().filter_map(|v| v.as_u64()).sum();
        if n > 0 {
            missing.push(format!("{n} cannot be shown on a panel"));
        }
    }

    let tz = crate::providers::la();
    let now = chrono::Utc::now().with_timezone(&tz);
    let view = snap["view_title"].as_str().unwrap_or("Dashboard");
    let mut footer = format!(
        "Home Assistant · {view} · updated {}",
        now.format("%-l:%M %p").to_string().to_lowercase()
    );
    if !missing.is_empty() {
        footer.push_str(&format!(" · {}", missing.join(" · ")));
    }

    json!({
        "title": settings.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .unwrap_or(view),
        "have_data": true,
        "clock": now.format("%-l:%M").to_string().to_lowercase(),
        "rows": rows,
        "footer": footer,
    })
}

pub fn no_data_vars(message: &str) -> Value {
    json!({
        "title": "Dashboard",
        "have_data": false,
        "message": message,
        "clock": "",
        "rows": [],
        "footer": "",
    })
}

/// The provider data one rebuild reads is resolved by the framework; this only
/// turns the snapshot on hand into panel rows.
impl super::Extension for HomeAssistantTab {
    fn template(&self) -> &'static str {
        HOME_ASSISTANT_TEMPLATE
    }

    fn vars(&self, cx: super::Ctx) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> {
        Box::pin(async move {
            let Some(pid) = cx.provider_ids.first().copied() else {
                return Ok(no_data_vars("No dashboard source is connected."));
            };
            Ok(
                match crate::providers::load_snapshot(&cx.pool, pid).await? {
                    Some(crate::providers::Snapshot::HomeAssistant(snap)) => {
                        tab_vars(&snap, &cx.settings)
                    }
                    Some(_) => no_data_vars("Resolved provider does not publish a dashboard"),
                    None => no_data_vars("Waiting for first dashboard refresh..."),
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tile(kind: &str, title: &str, value: &str) -> Value {
        json!({"kind": kind, "title": title, "value": value, "unit": "", "sub": "",
               "on": false, "progress": null, "rows": []})
    }

    #[test]
    fn fills_rows_and_counts_what_did_not_fit() {
        let mut tiles = vec![tile("heading", "", "Lights")];
        for i in 0..30 {
            tiles.push(tile("tile", &format!("Light {i}"), "On"));
        }
        let snap = json!({"view_title": "Home", "tiles": tiles, "omitted": {"map": 2}});
        let v = tab_vars(&snap, &json!({"columns": 4}));
        assert!(v["have_data"].as_bool().unwrap());
        let rows = v["rows"].as_array().unwrap();
        let placed: usize = rows
            .iter()
            .map(|r| r["tiles"].as_array().map(|t| t.len()).unwrap_or(0))
            .sum();
        assert!(
            placed > 0 && placed < 30,
            "some but not all tiles placed: {placed}"
        );
        assert_eq!(rows[0]["heading"], true);
        let footer = v["footer"].as_str().unwrap();
        assert!(footer.contains("did not fit"), "{footer}");
        assert!(footer.contains("2 cannot be shown"), "{footer}");
    }

    #[test]
    fn every_row_fits_the_panel() {
        let tiles: Vec<Value> = (0..40)
            .map(|i| tile("tile", &format!("T{i}"), "On"))
            .collect();
        let snap = json!({"view_title": "Home", "tiles": tiles, "omitted": {}});
        let v = tab_vars(&snap, &json!({"columns": 3}));
        let rows = v["rows"].as_array().unwrap();
        // header + rows + footer must stay inside 480px.
        let mut used = HEADER_H + FOOTER_H;
        for r in rows {
            used += if r["heading"].as_bool().unwrap_or(false) {
                HEADING_H
            } else {
                r["height"].as_f64().unwrap_or(BLOCK_H as f64) as f32
            };
            used += GAP;
        }
        assert!(
            used <= HEIGHT,
            "layout overflowed the panel: {used}px of {HEIGHT}px in {} rows",
            rows.len()
        );
    }

    #[test]
    fn long_strings_are_measured_to_their_cell_not_left_to_overlap() {
        let snap = json!({"view_title": "Home", "omitted": {}, "tiles": [
            {"kind": "tile", "title": "Bedroom Skylight Left Hand Side", "value": "Unverifiable",
             "unit": "", "sub": "Some long secondary line", "on": false, "progress": null, "rows": []}
        ]});
        let v = tab_vars(&snap, &json!({"columns": 4}));
        let t = &v["rows"][0]["tiles"][0];
        let cell_w = (WIDTH - 2.0 * PAD - GAP * 3.0) / 4.0 - 2.0 * TILE_PAD_X;
        assert!(
            crate::render::text_width(t["title"].as_str().unwrap(), TITLE_PX, false) <= cell_w,
            "title escaped its cell: {}",
            t["title"]
        );
        assert!(t["title"].as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn group_block_keeps_only_the_rows_that_fit() {
        let mut rows = Vec::new();
        for i in 0..40 {
            rows.push(json!({"k": format!("Sensor {i}"), "v": i}));
        }
        let snap = json!({"view_title": "Home", "omitted": {}, "tiles": [
            {"kind": "group", "title": "Sensors", "value": "", "unit": "", "sub": "",
             "on": false, "progress": null, "rows": rows}
        ]});
        let v = tab_vars(&snap, &json!({}));
        let kept = v["rows"][0]["rows"].as_array().unwrap().len();
        assert!(kept > 0 && kept < 40, "kept {kept} of 40 rows");
    }

    #[test]
    fn a_short_last_row_is_padded_so_one_tile_cannot_span_the_panel() {
        let tiles: Vec<Value> = (0..5)
            .map(|i| tile("tile", &format!("T{i}"), "On"))
            .collect();
        let snap = json!({"view_title": "Home", "tiles": tiles, "omitted": {}});
        let v = tab_vars(&snap, &json!({"columns": 4}));
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let last = rows[1]["tiles"].as_array().unwrap();
        assert_eq!(last.len(), 4, "short row padded to the full grid");
        assert_eq!(last[0]["title"], "T4");
        assert!(
            last[1..].iter().all(|c| c["blank"] == true),
            "padding cells carry no content: {last:?}"
        );
    }

    /// The real template, through the real pipeline, for every row kind the
    /// layout can emit — including a short row's blank filler cells.
    #[test]
    fn the_template_survives_the_full_pipeline_for_every_row_kind() {
        let snap = json!({"view_title": "Home", "omitted": {"map": 1}, "tiles": [
            {"kind": "heading", "title": "", "value": "Weather", "unit": "", "sub": "",
             "on": false, "progress": null, "rows": []},
            {"kind": "tile", "title": "Today", "value": "Cloudy", "unit": "", "sub": "55°F",
             "on": false, "progress": null, "rows": []},
            {"kind": "tile", "title": "Living Room", "value": "On", "unit": "", "sub": "",
             "on": true, "progress": null, "rows": []},
            {"kind": "group", "title": "Sensors", "value": "", "unit": "", "sub": "",
             "on": false, "progress": null,
             "rows": [{"k": "Temp", "v": "19.4 °F"}, {"k": "Humidity", "v": "41%"}]},
            {"kind": "progress", "title": "Battery", "value": "80%", "unit": "", "sub": "",
             "on": false, "progress": 0.8, "rows": []},
            {"kind": "text", "title": "Note", "value": "Trash tonight", "unit": "", "sub": "",
             "on": false, "progress": null, "rows": []},
            {"kind": "tile", "title": "Alone", "value": "Open", "unit": "", "sub": "",
             "on": true, "progress": null, "rows": []}
        ]});
        for cols in [2u64, 3, 4] {
            let vars = tab_vars(&snap, &json!({"columns": cols}));
            let report = crate::ext_dev::check(HOME_ASSISTANT_TEMPLATE, &vars.to_string());
            assert!(
                report.ok,
                "{cols} cols: {:?} {}",
                report.stage,
                report.error.unwrap_or_default()
            );
        }
    }

    #[test]
    fn the_no_data_template_renders_too() {
        let report = crate::ext_dev::check(
            HOME_ASSISTANT_TEMPLATE,
            &no_data_vars("Waiting for first dashboard refresh...").to_string(),
        );
        assert!(
            report.ok,
            "{:?} {}",
            report.stage,
            report.error.unwrap_or_default()
        );
    }

    #[test]
    fn a_two_word_value_wraps_rather_than_being_cut() {
        let snap = json!({"view_title": "Home", "omitted": {}, "tiles": [
            {"kind": "tile", "title": "Today", "value": "Partly cloudy", "unit": "", "sub": "55\u{b0}F",
             "on": false, "progress": null, "rows": []}
        ]});
        let v = tab_vars(&snap, &json!({"columns": 4}));
        let row = &v["rows"][0];
        let t = &row["tiles"][0];
        assert_eq!(t["lines"].as_array().map(|l| l.len()), Some(2), "{t}");
        assert_eq!(t["lines"][0], "Partly");
        assert_eq!(t["lines"][1], "cloudy");
        assert!(
            row["height"].as_f64().unwrap() > TILE_H as f64,
            "the row paid for the line it added"
        );
    }

    #[test]
    fn one_long_word_still_ellipsizes_at_the_floor() {
        assert_eq!(wrap2("Supercalifragilistic", 120.0, 28.0), None);
        assert_eq!(
            wrap2("two words", 400.0, 28.0),
            None,
            "one line is not a wrap"
        );
        // The property, not one lucky width: every line fits, and nothing is lost.
        let got = wrap2("Partly cloudy", 140.0, 28.0).expect("splits in two");
        assert_eq!(got.join(" "), "Partly cloudy");
        for l in &got {
            assert!(text_width(l, 28.0, true) <= 140.0, "{l} overflows");
        }
    }

    #[test]
    fn a_section_starts_a_fresh_row_so_a_label_never_sits_under_foreign_tiles() {
        let snap = json!({"view_title": "Home", "omitted": {}, "tiles": [
            {"kind": "tile", "title": "A", "value": "1", "unit": "", "sub": "",
             "on": false, "progress": null, "rows": []},
            {"kind": "tile", "title": "B", "value": "2", "unit": "", "sub": "",
             "on": false, "progress": null, "rows": []},
            {"kind": "heading", "title": "", "value": "Lights", "unit": "", "sub": "",
             "on": false, "progress": null, "rows": []},
            {"kind": "tile", "title": "C", "value": "3", "unit": "", "sub": "",
             "on": false, "progress": null, "rows": []}
        ]});
        let v = tab_vars(&snap, &json!({"columns": 4}));
        let rows = v["rows"].as_array().unwrap();
        let shape: Vec<String> = rows
            .iter()
            .map(|r| {
                if r["heading"].as_bool().unwrap_or(false) {
                    format!("heading:{}", r["text"].as_str().unwrap_or("?"))
                } else {
                    format!(
                        "tiles:{}",
                        r["tiles"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|c| c["blank"] != true)
                            .map(|c| c["title"].as_str().unwrap_or("?"))
                            .collect::<Vec<_>>()
                            .join("+")
                    )
                }
            })
            .collect();
        assert_eq!(
            shape,
            vec!["tiles:A+B", "heading:Lights", "tiles:C"],
            "{shape:?}"
        );
    }

    #[test]
    fn footer_names_the_tab_and_the_source() {
        let snap = json!({"view_title": "Security", "tiles": [tile("tile", "Alarm", "Armed away")], "omitted": {}});
        let v = tab_vars(&snap, &json!({"title": "Wall"}));
        assert_eq!(v["title"], "Wall", "settings.title overrides the tab name");
        assert!(v["footer"].as_str().unwrap().contains("Security"));
        assert!(v["footer"].as_str().unwrap().starts_with("Home Assistant"));
    }

    #[test]
    fn empty_snapshot_is_an_explicit_wait_not_a_blank_screen() {
        let v = no_data_vars("Waiting for first dashboard refresh...");
        assert_eq!(v["have_data"], false);
        assert_eq!(v["message"], "Waiting for first dashboard refresh...");
    }
}
