// SPDX-License-Identifier: AGPL-3.0-only
//! Render backend abstraction (issue #14).
//!
//! Two interchangeable backends produce panel BMP bytes from the same
//! (template, vars) pair:
//!
//! - [`NativeBackend`] — the default: Liquid -> JSON screen tree -> taffy +
//!   tiny-skia. Fast (ms), no external process, compiled into every build.
//! - browser backend (`browser` feature) — Liquid -> HTML -> real headless
//!   Chromium with the real trmnl-framework stylesheet. Slower (seconds),
//!   but renders arbitrary recipe JS/canvas with 100% framework fidelity.
//!
//! Selection is global (`--render-backend`) with a per-screen override in
//! `screens.render_backend`; a screen whose backend is `browser` on a core
//! build falls back to native with a warning.

use crate::bmp;
use crate::render::Renderer;
use crate::{HEIGHT, WIDTH};

/// Renders one screen (Liquid template + JSON vars) to panel BMP bytes.
#[async_trait::async_trait]
pub trait RenderBackend: Send + Sync {
    /// Stable name for logs/admin: "native" | "browser".
    fn name(&self) -> &'static str;
    /// Render at the given colour depth (1 or 2 bpp) and encode BMP.
    async fn render_bmp(&self, template: &str, vars_json: &str, bpp: u16) -> anyhow::Result<Vec<u8>>;
}

/// The always-available native renderer (Liquid -> Screen -> taffy/tiny-skia).
pub struct NativeBackend {
    renderer: Renderer,
}

impl NativeBackend {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { renderer: Renderer::new()? })
    }
}

#[async_trait::async_trait]
impl RenderBackend for NativeBackend {
    fn name(&self) -> &'static str {
        "native"
    }

    async fn render_bmp(&self, template: &str, vars_json: &str, bpp: u16) -> anyhow::Result<Vec<u8>> {
        let vars: serde_json::Value =
            serde_json::from_str(vars_json).unwrap_or(serde_json::json!({}));
        let screen = crate::template::render_screen(template, &vars)?;
        let indices = self.renderer.render(&screen);
        Ok(match bpp {
            1 => bmp::encode_1bit_bmp(WIDTH, HEIGHT, &indices),
            _ => bmp::encode_2bit_bmp(WIDTH, HEIGHT, &indices),
        })
    }
}

/// Parse vars leniently (bad JSON -> empty object), shared by backends.
pub fn parse_vars(vars_json: &str) -> serde_json::Value {
    serde_json::from_str(vars_json).unwrap_or(serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn native_backend_renders_golden_path() {
        let b = NativeBackend::new().unwrap();
        let tmpl = json!({
            "name": "t",
            "parts": [{"type": "text", "text": "{{ title }}", "style": {"font_px": 42, "bold": true}}]
        })
        .to_string();
        let bmp = b.render_bmp(&tmpl, r#"{"title":"hi"}"#, 2).await.unwrap();
        assert_eq!(&bmp[0..2], b"BM");
        assert!(bmp.len() > 1000);
    }
}
