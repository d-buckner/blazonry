#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Drive the blazonry admin SPA in headless Chromium against a freshly
spawned server on an ephemeral DB: register admin, create screen + playlist,
walk every tab, screenshot each, fail on JS console errors.
Run: /workspace/pio-venv/bin/python scripts/ui_check.py"""
import contextlib, os, socket, subprocess, sys, tempfile, time
from playwright.sync_api import sync_playwright

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = "/workspace/pw-browsers/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell"

def free_port():
    with contextlib.closing(socket.socket()) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]

d = tempfile.mkdtemp(prefix="blazonry-ui-")
port = free_port()
srv = subprocess.Popen(
    [os.path.join(ROOT, "target/debug/blazonry"), "serve",
     "--db", os.path.join(d, "ui.db"), "--addr", f"127.0.0.1:{port}"],
    stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
for _ in range(50):
    with socket.socket() as s:
        s.settimeout(0.3)
        if s.connect_ex(("127.0.0.1", port)) == 0:
            break
    time.sleep(0.2)

errors = []
try:
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path=EXE, args=["--no-sandbox", "--disable-gpu"])
        pg = b.new_page(viewport={"width": 1280, "height": 900})
        pg.on("pageerror", lambda e: errors.append("pageerror: " + str(e)))
        pg.on("console", lambda m: errors.append("console: " + m.text) if m.type == "error" else None)
        pg.goto(f"http://127.0.0.1:{port}/", wait_until="networkidle")
        # first-run: the register form is the landing state and the first
        # account auto-logs in (issue #23)
        pg.wait_for_selector("#reg input[type=email]")
        pg.fill("#reg input[type=email]", "admin@home.lan")
        pg.fill("#reg input[type=password]", "longenough9")
        pg.click("#reg button.pri")
        pg.wait_for_selector("header nav")
        pg.wait_for_timeout(400)
        # first-run guided setup (#39): fresh DB must show the 3-step panel
        assert pg.locator(".welcome .wstep").count() == 3, "guided state missing on fresh DB"
        pg.screenshot(path=os.path.join(ROOT, "ui_devices.png"))
        pg.click("nav >> text=Screens")
        pg.wait_for_timeout(300)
        pg.click("text=New screen")
        pg.wait_for_selector("dialog[open]")
        pg.fill("dialog input", "hello")
        pg.fill("dialog textarea >> nth=1", '{"title":"Home HQ","body":"it works"}')
        pg.click("dialog button.pri")
        pg.wait_for_timeout(1500)
        pg.screenshot(path=os.path.join(ROOT, "ui_screens.png"))
        pg.click("nav >> text=Playlists")
        pg.click("text=New playlist")
        pg.wait_for_selector("dialog[open]")
        pg.fill("dialog input", "daily")
        pg.click("dialog button.pri")
        pg.wait_for_timeout(400)
        pg.select_option("select >> nth=0", index=0)
        pg.click("text=Add screen")
        pg.wait_for_selector("td >> text=hello", timeout=5000)
        pg.wait_for_timeout(300)
        pg.screenshot(path=os.path.join(ROOT, "ui_playlists.png"))
        pg.click("nav >> text=Firmware")
        pg.wait_for_timeout(300)
        pg.click("nav >> text=Models")
        pg.wait_for_timeout(300)
        pg.click("nav >> text=Users")
        pg.wait_for_timeout(300)
        pg.screenshot(path=os.path.join(ROOT, "ui_users.png"))

        # 390px sweep (issue #24): every tab must fit with no horizontal
        # overflow and every nav tab must be reachable.
        pg.set_viewport_size({"width": 390, "height": 800})
        pg.wait_for_timeout(300)
        for tab in ["Devices", "Screens", "Playlists", "Connections", "Firmware", "Models", "Users"]:
            link = pg.locator(f"header nav a:has-text('{tab}')")
            box = link.bounding_box()
            if box is None or box["x"] + box["width"] > 390 + 1:
                errors.append(f"mobile: nav tab {tab} unreachable at 390px")
            link.click()
            pg.wait_for_timeout(250)
            sw = pg.evaluate("document.documentElement.scrollWidth")
            cw = pg.evaluate("document.documentElement.clientWidth")
            if sw > cw + 1:
                errors.append(f"mobile: {tab} overflows at 390px (scrollWidth {sw} > clientWidth {cw})")
        pg.screenshot(path=os.path.join(ROOT, "ui_mobile_users.png"))
        b.close()
finally:
    srv.terminate()

# the boot /admin/me 401 before login is expected; anything else is a failure
real = [e for e in errors if "401" not in e]
print("JS errors:", real if real else "none")
sys.exit(1 if real else 0)
