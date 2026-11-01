// SPDX-License-Identifier: AGPL-3.0-only
//! Corpus probe: how much of the real TRMNL recipe corpus renders natively
//! through the markup front-end? Skips silently when the dump is absent so CI
//! stays green; run locally with `cargo test --test markup_corpus -- --nocapture`.

use std::collections::HashMap;
use std::path::Path;

/// Minimal plausible vars for common recipe shapes; unknown vars render empty.
fn probe_vars() -> serde_json::Value {
    serde_json::json!({
        "title": "Title", "name": "Name", "value": "42", "label": "Label",
        "items": [{"name": "A", "title": "A", "value": "1", "label": "x"},
                   {"name": "B", "title": "B", "value": "2", "label": "y"}],
        "days": [{"label": "Mon", "hi": 70, "lo": 50}],
        "stations": [{"name": "S1"}],
        "devices": [{"name": "R", "aq": 1, "co2": 400}],
        "trmnl": {"plugin_settings": {"instance_name": "P"}, "user": {"utc_offset": -25200}},
    })
}

#[test]
fn corpus_native_render_rate() {
    let dir = Path::new("/workspace/trmnl-dump/data/markup");
    if !dir.exists() {
        eprintln!("SKIP: trmnl-dump not present");
        return;
    }
    let vars = probe_vars();
    let mut ok = 0usize;
    let mut browser_required = 0usize; // contains img/svg/table/canvas/iframe/progress-bar
    let mut map_gaps: HashMap<String, usize> = HashMap::new(); // real mapper failures
    let mut liquid_err = 0usize;
    let mut total = 0usize;
    const BROWSER_ONLY: [&str; 7] = ["img", "svg", "table", "canvas", "iframe", "progress-bar", "video"];
    let files: Vec<_> = std::fs::read_dir(dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with("__markup_full.liquid"))
        .collect();
    for path in &files {
        total += 1;
        let src = std::fs::read_to_string(path).unwrap();
        let rendered = match mntr::template::render_str(&src, &vars) {
            Ok(r) => r,
            Err(_) => { liquid_err += 1; continue; }
        };
        match mntr::markup::markup_to_screen(&rendered, "probe") {
            Ok(s) if !s.parts.is_empty() => ok += 1,
            // empty after mapping = JS-driven content (script tags, data-*
            // views, comment-only) — nothing native to render; browser backend
            Ok(_) => browser_required += 1,
            Err(mntr::markup::Unsupported(what)) => {
                if BROWSER_ONLY.contains(&what.as_str()) {
                    browser_required += 1;
                } else {
                    *map_gaps.entry(what).or_default() += 1;
                }
            }
        }
    }
    let mut top: Vec<_> = map_gaps.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let mappable = ok + top.iter().map(|(_, n)| n).sum::<usize>();
    println!("\ncorpus: {} recipes (markup_full)", total);
    println!("  liquid render failed (partials/vars): {liquid_err}");
    println!("  browser-required (img/svg/table/...): {browser_required}");
    println!("  native map OK: {ok} ({:.0}% of mappable)", 100.0 * ok as f64 / mappable.max(1) as f64);
    for (what, n) in top.iter().take(12) {
        println!("  map_gap[{what}]: {n}");
    }
    // Floor assertion on the mappable subset: recipes whose content the native
    // mapper SHOULD express (no browser-only elements) must mostly work.
    // Raise as the mapper grows; never lower it.
    assert!(ok as f64 / mappable.max(1) as f64 > 0.85, "native rate too low: {ok}/{mappable}");
}
