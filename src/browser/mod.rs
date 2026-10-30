//! Headless-Chromium render backend (PoC for issue #14, `browser` feature).
//!
//! Renders arbitrary recipe HTML (including JS-built DOM) to an 800x480 PNG
//! via CDP, then feeds the existing dither/BMP path. This is the mntr-full
//! half of the two-offering split: core builds never compile this module.

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, Viewport};
use chromiumoxide::page::ScreenshotParams;
use futures_util::StreamExt;

/// One-shot browser session. PoC: launch per render (correct, slow).
/// Production version: pooled session + render cache keyed on
/// (template hash, data hash, backend) per issue #14.
pub struct CdpRenderer {
    browser: Browser,
}

impl CdpRenderer {
    /// Launch a headless shell. `chrome_path` is the version-pinned binary
    /// (on-demand fetch into ~/.cache/mntr/browsers/ in production; the PoC
    /// takes it from $MNTR_BROWSER_PATH or $CHROME_PATH).
    pub async fn launch(chrome_path: &str) -> Result<Self> {
        let config = BrowserConfig::builder()
            .chrome_executable(chrome_path)
            .no_sandbox()
            .arg("--disable-gpu")
            .arg("--hide-scrollbars")
            .arg("--force-device-scale-factor=1")
            .window_size(crate::WIDTH as u32, crate::HEIGHT as u32)
            .build()
            .map_err(anyhow::Error::msg)
            .context("browser config")?;
        let (browser, mut handler) = Browser::launch(config).await.context("browser launch")?;
        // Drive the CDP connection pump on its own task for the process lifetime.
        tokio::spawn(async move { while handler.next().await.is_some() {} });
        Ok(Self { browser })
    }

    /// Render HTML to an 800x480 greyscale buffer (1 byte/px, top-down).
    /// `settle_ms` lets JS-built DOM finish before capture.
    pub async fn render_grey(&self, html: &str, settle_ms: u64) -> Result<Vec<u8>> {
        let url = format!(
            "data:text/html;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, html)
        );
        let page = self
            .browser
            .new_page(&url)
            .await
            .context("new page")?;
        page.wait_for_navigation().await.context("navigation")?;
        tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;

        let shot = page
            .screenshot(
                ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .clip(Viewport {
                        x: 0.,
                        y: 0.,
                        width: crate::WIDTH as f64,
                        height: crate::HEIGHT as f64,
                        scale: 1.,
                    })
                    .build(),
            )
            .await
            .context("screenshot")?;
        page.close().await.ok();

        let img = image::load_from_memory(&shot).context("decode png")?;
        let rgba = img.to_rgba8();
        anyhow::ensure!(
            rgba.width() == crate::WIDTH as u32 && rgba.height() == crate::HEIGHT as u32,
            "screenshot {}x{}, expected {}x{}",
            rgba.width(),
            rgba.height(),
            crate::WIDTH,
            crate::HEIGHT
        );
        // Luma, Rec.601 — same weighting e-ink pipelines expect.
        let grey = rgba
            .pixels()
            .map(|p| {
                let [r, g, b, a] = p.0;
                if a == 0 {
                    255 // transparent -> paper white
                } else {
                    (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u8
                }
            })
            .collect();
        Ok(grey)
    }
}
