// SPDX-License-Identifier: AGPL-3.0-only
//! Headless-Chromium render backend (`browser` feature, issue #14).
//!
//! Production path: Liquid markup -> framework page shell -> served from a
//! loopback HTTP server together with the embedded trmnl-framework assets ->
//! real Chromium renders it -> CDP screenshot -> Rec.601 greyscale ->
//! existing dither/BMP encoders. A real origin (not data: URLs) is required
//! so @font-face and relative asset URLs resolve exactly like the cloud.
//!
//! One browser process is shared (pages are cheap, launches are not); renders
//! are serialized so a refresh burst can't fork-fest. The browser binary is
//! located via $MNTR_BROWSER_PATH / $CHROME_PATH / PATH — never downloaded,
//! never embedded (see issue #14 rationale).

mod assets;
pub mod assets_data;

pub use assets::{rewrite_vendor_urls, AssetStore, SHELL};

use crate::backend::RenderBackend;
use anyhow::{Context, Result};
use axum::extract::{Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, Viewport};
use chromiumoxide::page::ScreenshotParams;
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};

/// Locate a Chromium-family binary. Order: $MNTR_BROWSER_PATH, $CHROME_PATH,
/// well-known paths, PATH. Returns None on core-style installs.
pub fn find_browser() -> Option<String> {
    for var in ["MNTR_BROWSER_PATH", "CHROME_PATH"] {
        if let Ok(p) = std::env::var(var) {
            if std::path::Path::new(&p).exists() {
                return Some(p);
            }
        }
    }
    const CANDIDATES: &[&str] = &[
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/opt/chrome-headless-shell/chrome-headless-shell",
    ];
    for c in CANDIDATES {
        if std::path::Path::new(c).exists() {
            return Some((*c).to_string());
        }
    }
    for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome"] {
        if let Ok(out) = std::process::Command::new("which").arg(name).output() {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Shared browser + loopback server (assets + the page under render).
pub struct CdpRenderer {
    browser: Browser,
    base: String,
    slot: Arc<Mutex<String>>,
    /// Serialize page renders: one page at a time keeps memory flat and
    /// avoids Chrome contention under refresh bursts.
    render_lock: tokio::sync::Mutex<()>,
}

impl CdpRenderer {
    /// Launch headless Chromium and the loopback asset/page server.
    pub async fn launch(chrome_path: &str) -> Result<Self> {
        let store = Arc::new(AssetStore::from_embedded()?);
        let slot: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

        let app = Router::new()
            .route("/page.html", get({
                let slot = slot.clone();
                move || {
                    let slot = slot.clone();
                    async move {
                        let html = slot.lock().unwrap().clone();
                        ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
                    }
                }
            }))
            .route(
                "/{*path}",
                get(|AxPath(p): AxPath<String>, State(store): State<Arc<AssetStore>>| async move {
                    match store.get(&format!("/{p}")) {
                        Some(bytes) => (
                            [(header::CONTENT_TYPE, assets::content_type(&p))],
                            bytes.to_vec(),
                        )
                            .into_response(),
                        None => (StatusCode::NOT_FOUND, "not found").into_response(),
                    }
                }),
            )
            .with_state(store);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("bind asset server")?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::warn!(%e, "browser asset server exited");
            }
        });

        let config = BrowserConfig::builder()
            .chrome_executable(chrome_path)
            .no_sandbox()
            .arg("--disable-gpu")
            .arg("--hide-scrollbars")
            .arg("--force-device-scale-factor=1")
            .arg("--default-background-color=FFFFFFFF")
            .window_size(crate::WIDTH as u32, crate::HEIGHT as u32)
            .build()
            .map_err(anyhow::Error::msg)
            .context("browser config")?;
        let (browser, mut handler) = Browser::launch(config).await.context("browser launch")?;
        // Drive the CDP connection pump on its own task for the process lifetime.
        tokio::spawn(async move { while handler.next().await.is_some() {} });
        Ok(Self {
            browser,
            base: format!("http://{addr}"),
            slot,
            render_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Render recipe markup (already Liquid-resolved HTML) inside the
    /// framework shell to an 800x480 greyscale buffer.
    pub async fn render_markup(&self, markup: &str) -> Result<Vec<u8>> {
        let _guard = self.render_lock.lock().await;
        let page_html = SHELL.replace("{markup}", &rewrite_vendor_urls(markup));
        *self.slot.lock().unwrap() = page_html;
        let url = format!("{}/page.html", self.base);
        self.render_url(&url, 2500).await
    }

    /// Render any URL (http/file/data) to the greyscale buffer.
    pub async fn render_url(&self, url: &str, settle_ms: u64) -> Result<Vec<u8>> {
        let page = self.browser.new_page(url).await.context("new page")?;
        let nav = tokio::time::timeout(std::time::Duration::from_secs(20), page.wait_for_navigation()).await;
        if nav.is_err() {
            tracing::warn!("browser render: navigation timed out, capturing anyway");
        }
        // Settle: webfonts load lazily and document.fonts.status can read
        // 'loaded' before a freshly-registered load even flips it to
        // 'loading'. Kick the framework's face families, mark a done-flag when
        // those load promises settle, and poll the flag; then a grace period
        // so JS-built DOM/charts finish.
        {
            let _ = page
                .evaluate(
                    r#"(() => {
                        window.__mntrFontsDone = false;
                        if (!document.fonts) { window.__mntrFontsDone = true; return true; }
                        const kicks = [];
                        for (const fam of ['TRMNL12','TRMNL16','TRMNL21','Inter Variable'])
                          for (const w of ['normal','bold'])
                            kicks.push(document.fonts.load(w + ' 40px "' + fam + '"', 'MW0').catch(() => {}));
                        Promise.all(kicks).then(() => { window.__mntrFontsDone = true; });
                        return true; })()"#,
                )
                .await;
            let deadline = tokio::time::Instant::now()
                + std::time::Duration::from_millis(settle_ms + 5000);
            loop {
                let done = page
                    .evaluate("window.__mntrFontsDone === true")
                    .await
                    .ok()
                    .and_then(|r| r.value().and_then(|v| v.as_bool()))
                    .unwrap_or(true);
                if done || tokio::time::Instant::now() > deadline {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
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

    /// Loopback base URL of the embedded asset server.
    pub fn asset_base(&self) -> &str {
        &self.base
    }
}

/// Production backend: Liquid -> shell -> Chromium -> BMP.
pub struct BrowserBackend {
    renderer: Arc<CdpRenderer>,
}

impl BrowserBackend {
    pub async fn launch(chrome_path: &str) -> Result<Self> {
        Ok(Self { renderer: Arc::new(CdpRenderer::launch(chrome_path).await?) })
    }
}

#[async_trait::async_trait]
impl RenderBackend for BrowserBackend {
    fn name(&self) -> &'static str {
        "browser"
    }

    async fn render_bmp(&self, template: &str, vars_json: &str, bpp: u16) -> anyhow::Result<Vec<u8>> {
        let vars = crate::backend::parse_vars(vars_json);
        // Recipes emit HTML fragments (not the JSON screen tree): render the
        // Liquid to text, feed the browser directly.
        let markup = crate::template::render_str(template, &vars)?;
        let grey = self.renderer.render_markup(&markup).await?;
        let indices = crate::dither::floyd_steinberg_4(&grey, crate::WIDTH, crate::HEIGHT);
        Ok(match bpp {
            1 => crate::bmp::encode_1bit_bmp(crate::WIDTH, crate::HEIGHT, &indices),
            _ => crate::bmp::encode_2bit_bmp(crate::WIDTH, crate::HEIGHT, &indices),
        })
    }
}




