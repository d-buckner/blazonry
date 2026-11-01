// SPDX-License-Identifier: AGPL-3.0-only
//! Golden-image regression over a screen corpus.
//!
//! Issue #4: these are the contract for the taffy migration. Each screen in
//! `tests/golden/screens/*.json` renders to the 2-bit index buffer; the
//! buffer must be byte-identical to `tests/golden/<name>.bin`.
//!
//! Regenerate deliberately: `UPDATE_GOLDEN=1 cargo test --test golden`
//! then review the diff of the .bin files (or render them to PNG).

use mntr::render::Renderer;
use mntr::screen::Screen;
use std::fs;
use std::path::Path;

fn corpus() -> Vec<(String, Screen)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/screens");
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .expect("golden corpus dir")
        .map(|e| e.expect("entry"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let src = fs::read_to_string(&path).expect("read screen");
        let screen: Screen = serde_json::from_str(&src)
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        out.push((path.file_stem().unwrap().to_string_lossy().into_owned(), screen));
    }
    assert!(!out.is_empty(), "golden corpus is empty");
    out
}

#[test]
fn golden_screens_render_byte_identical() {
    let renderer = Renderer::new().expect("renderer");
    let update = std::env::var("UPDATE_GOLDEN").is_ok();
    let golden_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let actual_dir = golden_dir.join("actual");

    let mut failures = Vec::new();
    for (name, screen) in corpus() {
        let buf = renderer.render(&screen);
        let golden = golden_dir.join(format!("{name}.bin"));
        if update || !golden.exists() {
            fs::write(&golden, &buf).expect("write golden");
            continue;
        }
        let expected = fs::read(&golden).expect("read golden");
        if expected != buf {
            fs::create_dir_all(&actual_dir).ok();
            fs::write(actual_dir.join(format!("{name}.bin")), &buf).ok();
            let diff = expected
                .iter()
                .zip(buf.iter())
                .filter(|(a, b)| a != b)
                .count();
            failures.push(format!("{name}: {diff} differing pixels (actual in tests/golden/actual/)"));
        }
    }
    assert!(failures.is_empty(), "golden mismatches:\n{}", failures.join("\n"));
}
