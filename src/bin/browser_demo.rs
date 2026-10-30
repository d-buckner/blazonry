//! PoC driver for issue #14: render a JS-dependent recipe HTML through
//! headless Chromium, dither, and emit the panel BMPs.
//!
//!   MNTR_BROWSER_PATH=/path/to/chrome cargo run --features browser --bin browser_demo -- recipe.html out_prefix

use anyhow::{Context, Result};
use mntr::browser::CdpRenderer;
use mntr::dither::floyd_steinberg_4;
use mntr::{bmp, HEIGHT, WIDTH};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let html_path = args.next().context("usage: browser_demo <recipe.html> <out_prefix>")?;
    let prefix = args.next().unwrap_or_else(|| "browser_out".into());
    let chrome = std::env::var("MNTR_BROWSER_PATH")
        .or_else(|_| std::env::var("CHROME_PATH"))
        .context("set MNTR_BROWSER_PATH to a headless-capable chrome/chromium binary")?;

    let html = std::fs::read_to_string(&html_path)?;
    let t0 = std::time::Instant::now();
    let renderer = CdpRenderer::launch(&chrome).await?;
    let grey = renderer.render_grey(&html, 1500).await?;
    let render_ms = t0.elapsed().as_millis();

    let indices = floyd_steinberg_4(&grey, WIDTH, HEIGHT);
    let bmp2 = bmp::encode_2bit_bmp(WIDTH, HEIGHT, &indices);
    let bmp1 = bmp::encode_1bit_bmp(WIDTH, HEIGHT, &indices);
    std::fs::write(format!("{prefix}_2bit.bmp"), &bmp2)?;
    std::fs::write(format!("{prefix}_1bit.bmp"), &bmp1)?;

    // Also stash the raw PNG for eyeballing.
    let png = image::GrayImage::from_vec(WIDTH as u32, HEIGHT as u32, grey).unwrap();
    png.save(format!("{prefix}.png"))?;

    println!("render_ms={render_ms}");
    println!("bmp2_bytes={} bmp1_bytes={}", bmp2.len(), bmp1.len());
    Ok(())
}
