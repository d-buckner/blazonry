//! PoC driver for issue #14 with REAL TRMNL recipes from the dump:
//! render the recipe's Liquid through mntr's own compat engine, wrap it in
//! the trmnl-framework page shell (real CSS + fonts over localhost), capture
//! via CDP, dither, emit panel BMPs + PNGs.
//!
//!   MNTR_BROWSER_PATH=/path/to/chrome cargo run --features browser --bin recipe_shot \
//!     -- --dump /workspace/trmnl-dump/data --id 53263 --vars vars.json --out out_prefix

use anyhow::{Context, Result};
use mntr::browser::CdpRenderer;
use std::net::SocketAddr;

const SHELL: &str = r#"<!DOCTYPE html>
<html><head>
<meta charset="utf-8">
<link rel="stylesheet" href="/css/latest/plugins.css">
<script type="module" src="/js/latest/plugins.js"></script>
</head>
<body class="environment trmnl">
<div class="screen screen--fonts-trmnl">
  <div class="view view--full">
{markup}
  </div>
</div>
</body></html>
"#;

/// Minimal static file server (framework public/ dir) on an ephemeral port.
async fn serve_static(dir: std::path::PathBuf) -> Result<SocketAddr> {
    use axum::{extract::Path as AxPath, http::StatusCode, response::IntoResponse, routing::get, Router};
    let app = Router::new().route(
        "/{*path}",
        get(|AxPath(p): AxPath<String>, axum::extract::State(dir): axum::extract::State<std::path::PathBuf>| async move {
            let safe: std::path::PathBuf = p
                .split('/')
                .filter(|s| *s != ".." && !s.is_empty())
                .collect();
            match tokio::fs::read(dir.join(&safe)).await {
                Ok(bytes) => bytes.into_response(),
                Err(_) => (StatusCode::NOT_FOUND, format!("missing {p}")).into_response(),
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = app.with_state(dir);
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Ok(addr)
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut dump = Some(std::path::PathBuf::from("/workspace/trmnl-dump/data"));
    let mut fw = std::path::PathBuf::from("/workspace/trmnl-framework/public");
    let mut id = String::new();
    let mut vars = serde_json::json!({});
    let mut prefix = String::new();
    let mut settle_ms = 2500u64;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let (flag, mut inline) = a.split_once('=').map_or((a.as_str(), None), |(f, v)| (f, Some(v)));
        let mut val = || -> Result<String> {
            Ok(match inline.take() {
                Some(v) => v.to_string(),
                None => args.next().context("missing value")?,
            })
        };
        match flag {
            "--dump" => dump = Some(val()?.into()),
            "--fw" => fw = val()?.into(),
            "--id" => id = val()?,
            "--vars" => vars = serde_json::from_str(&val()?)?,
            "--settle" => settle_ms = val()?.parse()?,
            "--out" => prefix = val()?,
            other => anyhow::bail!("unknown arg {other}"),
        }
    }
    anyhow::ensure!(!id.is_empty() && !prefix.is_empty(), "--id and --out required");
    let dump = dump.context("--dump")?;
    let chrome = std::env::var("MNTR_BROWSER_PATH")
        .or_else(|_| std::env::var("CHROME_PATH"))
        .context("set MNTR_BROWSER_PATH")?;

    // 1. Load real recipe markup + render its Liquid through mntr's engine.
    let markup_path = dump.join("markup").join(format!("{id}__markup_full.liquid"));
    let src = std::fs::read_to_string(&markup_path)
        .with_context(|| format!("markup for {id}"))?;
    let html_body = mntr::template::render_str(&src, &vars).context("liquid render")?;

    // 2. Wrap in the framework shell and serve over localhost (font CORS).
    let page = SHELL.replace("{markup}", &html_body);
    let site = std::env::temp_dir().join(format!("mntr_recipe_{id}"));
    std::fs::create_dir_all(&site)?;
    for d in ["css", "js", "fonts", "images"] {
        let link = site.join(d);
        if !std::path::Path::new(&link).exists() {
            std::os::unix::fs::symlink(fw.join(d), &link).ok();
        }
    }
    std::fs::write(site.join("page.html"), &page)?;
    let addr = serve_static(site.clone()).await?;
    let url = if std::env::var("MNTR_DATA_URL").is_ok() {
        format!(
            "data:text/html;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &page)
        )
    } else {
        format!("http://{addr}/page.html")
    };
    eprintln!("serving {url} (markup {} bytes)", html_body.len());

    // 3. Capture via CDP; emit grey PNG (browser truth), dithered PNG, both BMPs.
    let t0 = std::time::Instant::now();
    let renderer = CdpRenderer::launch(&chrome).await?;
    let grey = renderer.render_url(&url, settle_ms).await?;
    let render_ms = t0.elapsed().as_millis();

    let grey_img = image::GrayImage::from_vec(mntr::WIDTH as u32, mntr::HEIGHT as u32, grey.clone()).unwrap();
    grey_img.save(format!("{prefix}_grey.png"))?;

    let indices = mntr::dither::floyd_steinberg_4(&grey, mntr::WIDTH, mntr::HEIGHT);
    let grey4: Vec<u8> = indices.iter().map(|&i| mntr::bmp::LEVELS[i as usize]).collect();
    image::GrayImage::from_vec(mntr::WIDTH as u32, mntr::HEIGHT as u32, grey4).unwrap()
        .save(format!("{prefix}_dither.png"))?;
    std::fs::write(format!("{prefix}_1bit.bmp"), mntr::bmp::encode_1bit_bmp(mntr::WIDTH, mntr::HEIGHT, &indices))?;
    std::fs::write(format!("{prefix}_2bit.bmp"), mntr::bmp::encode_2bit_bmp(mntr::WIDTH, mntr::HEIGHT, &indices))?;

    println!("recipe {id}: render_ms={render_ms} url={url}");
    Ok(())
}
