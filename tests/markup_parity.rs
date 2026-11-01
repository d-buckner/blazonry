//! Parity harness: render the same no-JS TRMNL recipes through BOTH backends
//! (native markup front-end vs real headless Chromium with the framework CSS)
//! and measure the visual gap.
//!
//! Requires: `--features browser` build + a Chromium binary
//! (MNTR_BROWSER_PATH or CHROME_PATH). Skips cleanly otherwise so default CI
//! is unaffected.
//!
//!   MNTR_BROWSER_PATH=/usr/bin/chromium cargo test --features browser \
//!     --test markup_parity -- --nocapture
//!
//! The two backends will never be pixel-identical (font hinting, dither seed),
//! so parity is measured as mean absolute difference over 4-level greys
//! (0..=3 scale), after both passes go through the same dither. Threshold is
//! a ceiling on *layout* gaps, not antialiasing noise: a wrong/missing block
//! spikes MAD, font jitter does not.

#[cfg(feature = "browser")]
use mntr::backend::RenderBackend;

/// Probe vars (same shape as the corpus probe test).
#[cfg(feature = "browser")]
fn probe_vars() -> serde_json::Value {
    serde_json::json!({
        "title": "Title", "name": "Name", "value": "42", "label": "Label",
        "items": [{"name": "A", "title": "A", "value": "1", "label": "x"}],
        "days": [{"label": "Mon", "hi": 70, "lo": 50}],
        "trmnl": {"plugin_settings": {"instance_name": "P"}, "user": {"utc_offset": -25200}},
    })
}

/// Decode BMP bytes (either backend) to a 4-level grey grid (0..=3) by
/// loading through `image` and quantizing.
#[cfg(feature = "browser")]
fn bmp_to_levels(bytes: &[u8]) -> Vec<u8> {
    let img = image::load_from_memory(bytes).expect("decode bmp").to_luma8();
    img.pixels()
        .map(|p| match p.0[0] {
            v if v < 64 => 0,
            v if v < 148 => 1,
            v if v < 220 => 2,
            _ => 3,
        })
        .collect()
}

/// Mean absolute difference on the 0..=3 scale, normalized to 0..=1.
#[cfg(feature = "browser")]
fn mad(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    let sum: u32 = a[..n].iter().zip(&b[..n]).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs()).sum();
    sum as f64 / (n as f64 * 3.0)
}

#[tokio::test]
async fn native_vs_browser_parity() {
    #[cfg(not(feature = "browser"))]
    {
        eprintln!("SKIP: build with --features browser for parity test");
        return;
    }
    #[cfg(feature = "browser")]
    {
        let Some(chrome) = mntr::browser::find_browser() else {
            eprintln!("SKIP: no Chromium found (set MNTR_BROWSER_PATH)");
            return;
        };
        let dir = std::path::Path::new("/workspace/trmnl-dump/data/markup");
        if !dir.exists() {
            eprintln!("SKIP: trmnl-dump not present");
            return;
        }

        let browser = mntr::browser::BrowserBackend::launch(&chrome).await.expect("launch browser backend");
        let vars = probe_vars();
        let vars_json = vars.to_string();

        // Sample: recipes that render natively (the parity claim only covers
        // those). Take a bounded sample so the test stays under a few minutes.
        let mut samples: Vec<(String, String, String)> = Vec::new();
        let files: Vec<_> = std::fs::read_dir(dir).unwrap()
            .filter_map(|e| e.ok()).map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with("__markup_full.liquid"))
            .collect();
        for path in files {
            if samples.len() >= 12 { break; }
            let src = std::fs::read_to_string(&path).unwrap();
            let globals = liquid::to_object(&vars).unwrap();
            let Ok(rendered) = mntr::liquid_compat::render_str(&src, &globals) else { continue };
            if !rendered.trim_start().starts_with('<') { continue; }
            if mntr::markup::markup_to_screen(&rendered, "p").is_err() { continue; }
            // skip near-empty renders (nothing to compare)
            let screen = mntr::markup::markup_to_screen(&rendered, "p").unwrap();
            if screen.parts.is_empty() { continue; }
            samples.push((path.file_name().unwrap().to_string_lossy().into_owned(), src, rendered));
        }
        assert!(!samples.is_empty(), "no natively-renderable recipes found");

        let mut scores = Vec::new();
        for (name, tmpl, html) in &samples {
            // native: markup -> IR -> dithered BMP (same path as ext render)
            let screen = mntr::markup::markup_to_screen(html, "parity").unwrap();
            let renderer = mntr::render::Renderer::new().unwrap();
            let indices = renderer.render(&screen);
            let native_bmp = mntr::bmp::encode_2bit_bmp(mntr::WIDTH, mntr::HEIGHT, &indices);
            // browser: same Liquid markup wrapped in the framework shell
            let browser_bmp = browser.render_bmp(tmpl, &vars_json, 2).await.unwrap();
            let d = mad(&bmp_to_levels(&native_bmp), &bmp_to_levels(&browser_bmp));
            println!("{name}: MAD {d:.3}");
            scores.push((name.clone(), d));
        }
        scores.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let median = scores[scores.len() / 2].1;
        let worst = scores.last().unwrap();
        println!("median MAD {median:.3}, worst {} {worst:?}", worst.0);
        // Layout-level parity: median under 15% of the grey range. Font/dither
        // noise alone sits well under this; a missing or misplaced block does not.
        assert!(median < 0.15, "median MAD too high: {median:.3}");
    }
}
