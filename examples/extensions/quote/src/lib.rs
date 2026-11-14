// SPDX-License-Identifier: AGPL-3.0-only
//! A daily quote, as a runtime extension. The whole plugin is this file plus
//! `manifest.json` and `template.liquid`.
//!
//! Build it and check it with:
//!
//! ```text
//! ext_host build examples/extensions/quote
//! ext_host probe examples/extensions/quote
//! ```

use blazonry_ext_api::{Extension, Host, VarsRequest, export_extension};
use chrono::Datelike;
use serde_json::{Value, json};
use std::num::NonZeroUsize;

const MANIFEST: &str = include_str!("../manifest.json");
const TEMPLATE: &str = include_str!("../template.liquid");

/// The renderer paints one line per text node and never wraps, so the extension
/// cuts the quote itself. `host.wrap` measures with the fonts the renderer will
/// paint with, so the break lands where the panel breaks it. Measure the weight the
/// template paints: bold DejaVu is wider, and measuring regular under-fills a line.
const QUOTE_PX: f32 = 38.0;
const QUOTE_BOLD: bool = true;
const QUOTE_MAX_PX: f32 = 672.0;

struct Quote;

impl Extension for Quote {
    fn manifest(&self) -> Value {
        // The manifest ships beside the code, so one that will not parse is a
        // build-time bug.
        serde_json::from_str(MANIFEST).unwrap_or_else(|e| panic!("manifest.json: {e}"))
    }

    fn template(&self) -> String {
        TEMPLATE.to_string()
    }

    fn vars(&self, req: VarsRequest, host: &Host) -> Result<Value, String> {
        let title = req
            .settings
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Quote of the day");
        let quotes = req
            .settings
            .get("quotes")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let lines: Vec<&str> = quotes
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let Some(count) = NonZeroUsize::new(lines.len()) else {
            return Ok(json!({
                "title": title,
                "lines": ["Add one quote per line in the settings."],
                "author": "",
                "footer": "",
            }));
        };
        // One quote per day, chosen by the day number, so the screen changes
        // daily and never at random.
        let day = req.local_date()?.num_days_from_ce();
        let line = lines[day.rem_euclid(count.get() as i32) as usize];
        // "the line | an attribution". The separator is in the manifest hint,
        // because a settings field holds one string.
        let (text, author) = match line.split_once('|') {
            Some((q, who)) => (q.trim(), who.trim()),
            None => (line, ""),
        };
        let quote = format!("“{text}”");
        Ok(json!({
            "title": title,
            "lines": host.wrap(&quote, QUOTE_MAX_PX, QUOTE_PX, QUOTE_BOLD),
            "author": author,
            "footer": req.local_date()?.format("%a %b %-d").to_string(),
        }))
    }
}

export_extension!(Quote);
