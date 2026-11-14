# SPDX-License-Identifier: AGPL-3.0-only
"""Render the repo social preview (og:image) — 1200x630 PNG.

Reproducible: python3 scripts/social_card.py [--out docs/img/social-card.png]
Uses the same fonts the product inlines (Space Grotesk brand face, TRMNL16
device face) and the DESIGN.md palette. The dither appears here as marketing
imagery, which is legal: the dither law scopes it out of admin *chrome*, and
the right side of this card depicts an actual e-ink screen.
"""
import argparse, functools, http.server, pathlib, socketserver, threading
from playwright.sync_api import sync_playwright

ROOT = pathlib.Path(__file__).resolve().parent.parent
HTML = """<!DOCTYPE html><html><head><meta charset="utf-8"><style>
@font-face{font-family:'Brand';src:url('/assets/brand/SpaceGrotesk-Bold.woff2');font-weight:700}
@font-face{font-family:'Device';src:url('/assets/framework/fonts/TRMNL16-Bold.woff2');font-weight:700}
:root{--paper:#F4F2ED;--paper2:#ECE8DF;--ink:#1C1A16;--ink2:#575146;--ink3:#6E685C;
--ln:#E0DBCF;--ln2:#C9C3B4;--dot:#D9D3C5;
--mono:ui-monospace,'SF Mono',Menlo,Consolas,monospace;
--body:system-ui,-apple-system,'Segoe UI',Roboto,sans-serif}
*{box-sizing:border-box;margin:0}
body{width:1200px;height:630px;background:var(--paper);color:var(--ink);
font-family:var(--body);-webkit-font-smoothing:antialiased;display:flex;overflow:hidden}
.left{flex:1.15;display:flex;flex-direction:column;justify-content:center;padding:0 64px 0 72px}
.wm{font-family:'Brand';font-weight:700;font-size:88px;letter-spacing:-.02em;line-height:1}
.wm::after{content:"";display:inline-block;width:22px;height:22px;background:var(--ink);margin-left:16px;transform:translateY(-4px)}
.pitch{font-size:26px;line-height:1.45;color:var(--ink2);margin-top:26px;max-width:520px}
.pitch b{color:var(--ink);font-weight:650}
.specline{font-family:var(--mono);font-size:14px;color:var(--ink3);letter-spacing:.05em;
border-top:1px solid var(--ln);margin-top:44px;padding-top:16px}
.right{flex:1;display:flex;align-items:center;justify-content:center;padding:56px 64px 56px 24px}
.panel{width:100%;aspect-ratio:5/3;background:var(--paper2);
background-image:radial-gradient(var(--dot) 1px,transparent 1.5px);background-size:5px 5px;
border:1px solid var(--ln2);box-shadow:10px 10px 0 var(--ln);padding:44px 48px;
display:flex;flex-direction:column;justify-content:space-between}
.panel .top{display:flex;justify-content:space-between;align-items:baseline}
.panel .clk{font-family:'Device';font-size:64px;line-height:1}
.panel .temp{font-family:'Device';font-size:34px;color:var(--ink2)}
.panel .row{font-family:'Device';font-size:24px;color:var(--ink2);margin-top:6px}
.panel .foot{display:flex;justify-content:space-between;font-family:var(--mono);
font-size:12px;color:var(--ink3);text-transform:uppercase;letter-spacing:.1em;
border-top:1px solid var(--ln2);padding-top:14px}
</style></head><body>
<div class="left">
  <div class="wm">blazonry</div>
  <div class="pitch">A <b>TRMNL BYOS server</b> in Rust. Your LAN, your screens, your server.</div>
  <div class="specline">4 gray levels · 800×480 · native render · no cloud · no database server</div>
</div>
<div class="right"><div class="panel">
  <div>
    <div class="top"><div class="clk">7:40</div><div class="temp">21&deg;</div></div>
    <div class="row">Good morning &mdash; school at 8:15</div>
  </div>
  <div class="foot"><span>kitchen</span><span>updated 4 min ago</span></div>
</div></div>
</body></html>"""

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "docs" / "img" / "social-card.png"))
    args = ap.parse_args()
    page_path = ROOT / "scripts" / "_social_card.html"
    page_path.write_text(HTML)
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(ROOT))
    class Srv(socketserver.TCPServer): allow_reuse_address = True
    with Srv(("127.0.0.1", 0), handler) as srv:
        port = srv.server_address[1]
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        with sync_playwright() as p:
            b = p.chromium.launch(
                executable_path="/workspace/pw-browsers/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell",
                args=["--no-sandbox", "--disable-gpu"])
            pg = b.new_page(viewport={"width": 1200, "height": 630})
            pg.goto(f"http://127.0.0.1:{port}/scripts/_social_card.html", wait_until="networkidle")
            pg.evaluate("document.fonts.ready")
            pg.wait_for_timeout(300)
            loaded = pg.evaluate("[...document.fonts].filter(f=>f.status==='loaded').map(f=>f.family)")
            assert "Brand" in loaded and "Device" in loaded, f"fonts failed: {loaded}"
            pg.screenshot(path=args.out)
            b.close()
    page_path.unlink()
    srv.shutdown()
    print(f"wrote {args.out}")

if __name__ == "__main__":
    main()
