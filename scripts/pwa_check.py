# Verify PWA: SW registers, shell cache fills, login works through SW, and
# with the server dead the same context reloads to the cached shell + banner.
import subprocess, sys, time, socket, tempfile, os, signal
from playwright.sync_api import sync_playwright

REPO = "/workspace/mntr"
db = tempfile.mktemp(suffix=".db")
s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
addr = f"127.0.0.1:{port}"

proc = subprocess.Popen([f"{REPO}/target/debug/mntr", "serve", "--db", db, "--addr", addr],
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
while True:
    line = proc.stdout.readline()
    if not line or "listening" in line:
        break
ok = True
try:
    with sync_playwright() as p:
        b = p.chromium.launch(executable_path="/workspace/pw-browsers/chromium_headless_shell-1248/chrome-headless-shell-linux64/chrome-headless-shell", args=["--no-sandbox", "--disable-gpu"])
        browser = b
        ctx = browser.new_context()
        page = ctx.new_page()
        page.goto(f"http://{addr}/", wait_until="networkidle")

        reg = page.evaluate("() => navigator.serviceWorker.ready.then(r => r.active.scriptURL)")
        print("SW active:", reg)
        assert reg and reg.endswith("/sw.js")

        # wait for SW to control the page (claim)
        page.wait_for_timeout(500)

        mj = page.request.get(f"http://{addr}/manifest.webmanifest").json()
        print("manifest ok:", mj["name"], mj["display"], len(mj["icons"]), "icons")

        cached = page.evaluate("""() => caches.keys().then(ks =>
            Promise.all(ks.filter(k=>k.startsWith('mntr-shell-'))
                       .map(k=>caches.open(k).then(c=>c.keys().then(ks=>ks.map(x=>x.url))))))""")
        print("shell cache:", cached)
        assert any(any(u.rstrip('/') == f'http://{addr}' for u in cl) for cl in cached), "shell '/' not cached"

        # register + login through the SW path
        r = ctx.request.post(f"http://{addr}/admin/register",
                             data='{"email":"a@b.c","password":"longenough9"}',
                             headers={"content-type": "application/json"})
        assert r.status == 200, f"register {r.status}"
        page.fill('#login input[type=email]', 'a@b.c')
        page.fill('#login input[type=password]', 'longenough9')
        page.click('#login button.pri')
        page.wait_for_selector("header nav", timeout=5000)
        print("login through SW: ok")

        # kill the server; reload same context => offline shell + banner
        proc.send_signal(signal.SIGTERM); proc.wait(timeout=5); time.sleep(0.5)
        page.goto(f"http://{addr}/", wait_until="domcontentloaded")
        page.wait_for_timeout(1500)
        html = page.content()
        shell_ok = "mntr" in html and "Sign in" in html
        banner_ok = "Offline" in html
        print("offline reload: shell rendered =", shell_ok, "| offline banner =", banner_ok)
        assert shell_ok, "offline shell did not render"
        assert banner_ok, "offline banner missing"
        browser.close()
except AssertionError as e:
    print("FAIL:", e); ok = False
finally:
    if proc.poll() is None: proc.kill()
    for f in (db, db + "-shm", db + "-wal"):
        if os.path.exists(f): os.unlink(f)
sys.exit(0 if ok else 1)
