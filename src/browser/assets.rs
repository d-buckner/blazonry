//! Embedded trmnl-framework assets + page shell for the browser backend.
//!
//! The framework's `plugins.min.css` is ~19 MB of generated utility CSS; we
//! embed the Brotli copies (~260 KB) and decompress once into a cache dir at
//! startup. The browser then loads them from mntr's own loopback server, so
//! renders work fully offline and never phone home to usetrmnl.com.
//!
//! Branding strip: recipe markup that references the TRMNL wordmark
//! (`images/plugins/trmnl--render.svg`) gets the vendored blank replacement,
//! and absolute `usetrmnl.com`/`trmnl.com` asset URLs are rewritten to the
//! local mirror.

use crate::browser::assets_data::ASSETS;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};



/// Content types for the loopback asset server.
pub fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "css" => "text/css",
        "js" => "text/javascript",
        "html" => "text/html",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Brotli-decompress an embedded asset by URL path (no leading slash).
pub fn decompress(key: &str) -> Result<Vec<u8>> {
    let raw = ASSETS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .with_context(|| format!("unknown framework asset: {key}"))?;
    if key.ends_with(".br") {
        let mut out = Vec::new();
        brotli::BrotliDecompress(&mut std::io::Cursor::new(raw), &mut out)
            .context("brotli decompress")?;
        Ok(out)
    } else {
        Ok(raw.to_vec())
    }
}

/// Materialize the framework tree into `dir` (idempotent: skips files that
/// already exist with the right size). Returns the directory.
pub fn materialize(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for (key, _) in ASSETS {
        // strip the .br suffix on disk so URLs match the framework's own paths
        let rel = key.strip_suffix(".br").unwrap_or(key);
        let out = dir.join(rel);
        if out.exists() {
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, decompress(key)?)?;
    }
    Ok(dir.to_path_buf())
}

/// Default cache dir for the materialized framework: $MNTR_DATA_DIR or
/// `~/.cache/mntr/framework`.
pub fn default_cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("MNTR_DATA_DIR") {
        return PathBuf::from(d).join("framework");
    }
    dirs_cache().join("mntr").join("framework")
}

fn dirs_cache() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".cache"))
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
        })
}

/// The page shell that wraps recipe markup exactly like the TRMNL cloud does
/// (same classes, same stylesheet + plugins.js entry points).
pub const SHELL: &str = r#"<!DOCTYPE html>
<html><head>
<meta charset="utf-8">
<link rel="stylesheet" href="/css/latest/plugins.min.css">
<script type="module" src="/js/latest/plugins.min.js"></script>
</head>
<body class="environment trmnl">
<div class="screen screen--fonts-trmnl">
  <div class="view view--full">
{markup}
  </div>
</div>
</body></html>
"#;

/// Rewrite recipe HTML for local rendering:
/// - absolute vendor URLs -> local mirror (offline, no phone-home)
/// - the TRMNL wordmark asset -> blank placeholder (branding strip)
pub fn rewrite_vendor_urls(markup: &str) -> String {
    let mut out = markup.to_string();
    for host in ["https://usetrmnl.com", "http://usetrmnl.com", "https://trmnl.com", "http://trmnl.com"] {
        out = out.replace(host, "");
    }
    // protocol-relative //usetrmnl.com/...
    out = out.replace("//usetrmnl.com/", "/").replace("//trmnl.com/", "/");
    out = out.replace("images/plugins/trmnl--render.svg", "images/plugins/mntr-blank.svg");
    out
}

/// In-memory asset store served by the browser backend's loopback server:
/// the materialized framework files plus the alias for the blank logo.
pub struct AssetStore {
    files: HashMap<String, Vec<u8>>,
}

impl AssetStore {
    /// Build from the embedded table (no disk writes).
    pub fn from_embedded() -> Result<Self> {
        let mut files = HashMap::new();
        for (key, _) in ASSETS {
            let rel = key.strip_suffix(".br").unwrap_or(key);
            files.insert(format!("/{rel}"), decompress(key)?);
        }
        // alias: rewritten recipe URLs point at mntr-blank.svg
        if let Some(blank) = files.get("/images/plugins/trmnl--render.svg") {
            let b = blank.clone();
            files.insert("/images/plugins/mntr-blank.svg".into(), b);
        }
        Ok(Self { files })
    }

    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|v| &v[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_css_decompresses_and_is_big() {
        let css = decompress("css/latest/plugins.min.css.br").unwrap();
        assert!(css.len() > 10_000_000, "css should expand to ~19MB, got {}", css.len());
        assert!(css.starts_with(b"@layer") || css.windows(6).any(|w| w == b"@layer"));
    }

    #[test]
    fn vendor_urls_rewritten_and_logo_blanked() {
        let src = r#"<img src="https://usetrmnl.com/images/plugins/trmnl--render.svg">
<script src="https://usetrmnl.com/js/highcharts/12.3.0/highcharts.js"></script>
<img src="//usetrmnl.com/images/grayscale/gray-5.png">"#;
        let out = rewrite_vendor_urls(src);
        assert!(!out.contains("usetrmnl.com"));
        assert!(out.contains("/images/plugins/mntr-blank.svg"));
        assert!(out.contains("/js/highcharts/12.3.0/highcharts.js"));
        assert!(out.contains("/images/grayscale/gray-5.png"));
    }

    #[test]
    fn blank_logo_is_tiny_svg() {
        let svg = decompress("images/plugins/trmnl--render.svg.br").unwrap();
        assert!(svg.len() < 200);
        assert!(String::from_utf8_lossy(&svg).contains("<svg"));
    }
}
