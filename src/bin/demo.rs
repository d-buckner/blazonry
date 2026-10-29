//! End-to-end spike: Liquid template + vars -> Screen -> greyscale -> dither
//! -> 2-bit BMP + PNG preview. `cargo run --bin demo`

use base64::Engine as _;
use serde_json::json;
use terminus_rs::bmp::encode_2bit_bmp;
use terminus_rs::render::Renderer;
use terminus_rs::template::render_screen;
use terminus_rs::{HEIGHT, WIDTH};

const TEMPLATE: &str = r##"{
  "name": "demo",
  "parts": [
    { "type": "text", "text": "{{ header }}", "style": { "font_px": 34, "bold": true }, "padding": [16, 20, 4, 20] },
    { "type": "divider", "padding": [4, 20, 8, 20] },
    { "type": "keyed_value", "key": "Outside", "value": "{{ temp }}°F {{ condition }}", "padding": [6, 20, 6, 20], "style": { "font_px": 26 } },
    { "type": "keyed_value", "key": "Sleep", "value": "{{ sleep }}", "padding": [6, 20, 6, 20], "style": { "font_px": 26 } },
    { "type": "keyed_value", "key": "Groceries", "value": "{{ groceries }} items", "padding": [6, 20, 6, 20], "style": { "font_px": 26 } },
    { "type": "progress", "value": {{ progress }}, "padding": [14, 20, 6, 20] },
    { "type": "bars", "values": [{{ bars }}], "labels": ["Mon","Tue","Wed","Thu","Fri","Sat","Sun"], "padding": [10, 20, 0, 20] }
  ]
}"##;

fn main() -> anyhow::Result<()> {
    let vars = json!({
        "header": "Home HQ — Thursday",
        "temp": 61,
        "condition": "drizzle",
        "sleep": "6h 12m",
        "groceries": 9,
        "progress": 0.62,
        "bars": "12, 18, 9, 22, 15, 30, 26"
    });

    let screen = render_screen(TEMPLATE, &vars)?;
    println!("parsed screen: {} parts", screen.parts.len());

    let renderer = Renderer::new()?;
    let t0 = std::time::Instant::now();
    let indices = renderer.render(&screen);
    let render_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let bmp = encode_2bit_bmp(WIDTH, HEIGHT, &indices);
    std::fs::write("out.bmp", &bmp)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bmp);
    println!("BMP {} bytes, base64 {} bytes, render {:.1}ms", bmp.len(), b64.len(), render_ms);

    // PNG preview of the dithered output for eyeballing
    let mut img = image::GrayImage::new(WIDTH as u32, HEIGHT as u32);
    for (i, &v) in indices.iter().enumerate() {
        img.put_pixel((i % WIDTH) as u32, (i / WIDTH) as u32,
            image::Luma([terminus_rs::bmp::LEVELS[v as usize]]));
    }
    img.save("out.png")?;
    println!("wrote out.bmp + out.png");
    Ok(())
}
