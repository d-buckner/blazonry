#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Render TRMNL framework markup to PNG with vendored framework assets.

Serves the vendored trmnl-framework public/ dir over HTTP, wraps fixture
markup in trmnlp's exact page shell, waits for document.fonts.ready, then
screenshots at device size. Requires: playwright in pio-venv + the chromium
headless shell under /workspace/pw-browsers.
"""
import argparse, contextlib, functools, http.server, os, socket, socketserver, threading

FW = "/workspace/trmnl-framework/public"
SHELL = """<!DOCTYPE html>
<html><head>
<meta charset="utf-8">
<link rel="stylesheet" href="{base}/css/latest/plugins.css">
<script type="module" src="{base}/js/latest/plugins.js"></script>
</head>
<body class="environment trmnl">
<div class="{screen_classes}">
  <div class="view view--{view}">
{markup}
  </div>
</div>
</body></html>
"""

def free_port():
    with contextlib.closing(socket.socket()) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]

def serve(directory, port):
    h = functools.partial(http.server.SimpleHTTPRequestHandler, directory=directory)
    class Q(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        def log_message(self, *a): pass
    srv = Q(("127.0.0.1", port), h)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv

def render(markup, out, size=(800, 480), screen_classes="screen screen--fonts-trmnl", view="full"):
    # one origin for page + assets: @font-face is CORS-gated, cross-origin
    # font fetches fail silently (face enters 'error', glyphs fall back)
    site = "/workspace/mntr/.fw_render/site"
    os.makedirs(site, exist_ok=True)
    for d in ("css", "js", "fonts", "images"):
        src = os.path.join(FW, d)
        if os.path.isdir(src):
            os.symlink(src, os.path.join(site, d)) if not os.path.lexists(os.path.join(site, d)) else None
    page = SHELL.format(base="", screen_classes=screen_classes, view=view, markup=markup)
    with open(os.path.join(site, "page.html"), "w") as f:
        f.write(page)
    port = free_port()
    serve(site, port)

    from playwright.sync_api import sync_playwright
    exe = "/workspace/pw-browsers/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell"
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=exe, args=["--no-sandbox", "--disable-gpu"])
        pg = b.new_page(viewport={"width": size[0], "height": size[1]})
        pg.goto(f"http://127.0.0.1:{port}/page.html", wait_until="networkidle")
        pg.evaluate("document.fonts.ready")
        pg.wait_for_timeout(300)
        pg.screenshot(path=out)
        b.close()
    return out

if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("markup_file")
    ap.add_argument("--out", default="/workspace/mntr/fw_screen.png")
    ap.add_argument("--size", default="800x480")
    ap.add_argument("--screen-classes", default="screen screen--fonts-trmnl")
    ap.add_argument("--view", default="full")
    a = ap.parse_args()
    w, h = map(int, a.size.split("x"))
    render(open(a.markup_file).read(), a.out, (w, h), a.screen_classes, a.view)
    print("wrote", a.out)
