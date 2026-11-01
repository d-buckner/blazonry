// SPDX-License-Identifier: AGPL-3.0-only
//! Layout regression tests for the hand-rolled measure/paint pass.
//!
//! These assert on where ink actually lands in the rendered 800x480 buffer —
//! the only contract the panel cares about. They exist because `align`,
//! `padding` and `width` were all silently ignored at least once.
//!
//! Issue #4 replaces this pass with taffy; keep the assertions, they are the
//! golden-image seed for that migration.

use mntr::render::Renderer;
use mntr::screen::Screen;
use mntr::{HEIGHT, WIDTH};

fn render(json: &str) -> Vec<u8> {
    let screen: Screen = serde_json::from_str(json).expect("screen json");
    Renderer::new().expect("renderer").render(&screen)
}

/// Horizontal extent of dark ink (dither indices 0=black, 1=dark grey).
fn ink_cols(buf: &[u8]) -> (usize, usize) {
    let mut min = usize::MAX;
    let mut max = 0usize;
    for (i, &v) in buf.iter().enumerate() {
        if v <= 1 {
            let x = i % WIDTH;
            min = min.min(x);
            max = max.max(x);
        }
    }
    (min, max)
}

fn screen_of(part: &str) -> String {
    format!(r#"{{"name":"t","parts":[{part}]}}"#)
}

fn text(align: &str, extra: &str) -> String {
    screen_of(&format!(
        r#"{{"type":"text","text":"hello","align":"{align}","style":{{"font_px":40,"bold":true}}{extra}}}"#
    ))
}

#[test]
fn align_center_centers_a_leaf_text_part() {
    let (min, max) = ink_cols(&render(&text("center", "")));
    let center = (min + max) / 2;
    assert!(
        (400 - 12..=400 + 12).contains(&center),
        "expected ink centered on 800px, got span {min}..={max} (center {center})"
    );
}

#[test]
fn align_start_and_end_are_opposite_edges() {
    let (start_min, start_max) = ink_cols(&render(&text("start", "")));
    let (end_min, end_max) = ink_cols(&render(&text("end", "")));
    assert!(start_min < 12, "start-aligned ink began at {start_min}");
    assert!(end_max > WIDTH - 12, "end-aligned ink ended at {end_max}");
    assert!(end_min > start_max, "the two spans should not overlap");
}

/// Regression: `measure` used to add padding to a max-width-derived size, so a
/// padded full-width node measured 800+40 and painted off the right edge.
#[test]
fn padded_full_width_nodes_stay_inside_the_panel() {
    for kind in ["divider", "progress"] {
        let json = screen_of(&format!(
            r#"{{"type":"{kind}","value":0.5,"padding":[0,20,20,20]}}"#
        ));
        let (min, max) = ink_cols(&render(&json));
        // progress draws its frame as a 1px stroke, which straddles the box
        // edge by half a pixel; divider fills and lands exactly on it.
        let straddle = if kind == "progress" { 1 } else { 0 };
        assert!(
            min >= 20 - straddle,
            "{kind} ink started at x={min}, expected >= {}",
            20 - straddle
        );
        assert!(max < WIDTH, "{kind} ink reached x={max}, expected < {WIDTH}");
        assert!(
            max <= 780 + straddle,
            "{kind} ink reached x={max}, expected <= {}",
            780 + straddle
        );
    }
}

#[test]
fn unpadded_full_width_nodes_span_the_panel() {
    let (min, max) = ink_cols(&render(&screen_of(r#"{"type":"divider"}"#)));
    assert!(min <= 1 && max >= WIDTH - 2, "got {min}..={max}");
}

/// `width`/`height` are declared in the vocabulary and were never read.
#[test]
fn explicit_width_is_honored() {
    let (min, max) = ink_cols(&render(&screen_of(
        r#"{"type":"divider","width":400}"#,
    )));
    assert!(min <= 1, "expected the box to start at 0, ink at {min}");
    assert!(
        (395..=402).contains(&max),
        "expected ink to stop near x=400, got {max}"
    );
}

#[test]
fn explicit_height_is_honored() {
    // a 120px-tall divider box leaves the rows below it empty; an ignored
    // height would put the second divider at y=2.
    let json = screen_of(
        r#"{"type":"flex","direction":"column","children":[
             {"type":"divider","height":120},
             {"type":"divider"}]}"#,
    );
    let buf = render(&json);
    let rows: Vec<usize> = (0..HEIGHT)
        .filter(|&y| buf[y * WIDTH..(y + 1) * WIDTH].iter().any(|&v| v <= 1))
        .collect();
    assert!(rows.len() >= 2, "expected two inked rows, got {rows:?}");
    let gap = rows.iter().zip(rows.iter().skip(1)).map(|(a, b)| b - a).max().unwrap();
    assert!(gap >= 100, "second divider sat at row offset {gap}, expected >= 100");
}

#[test]
fn oversized_content_does_not_slide_off_the_left_edge() {
    // align end + content wider than the panel: the clamp keeps it at x=0.
    let long = "x".repeat(400);
    let json = screen_of(&format!(
        r#"{{"type":"text","text":"{long}","align":"end","style":{{"font_px":24}}}}"#
    ));
    let (min, _) = ink_cols(&render(&json));
    assert!(min <= 1, "oversized end-aligned ink started at x={min}");
}
