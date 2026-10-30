#!/usr/bin/env python3
"""End-to-end validation: real TRMNL firmware in trmnl-sim against mntr.

Flow:
  1. boot mntr on host :2301 (fresh DB in a temp dir)
  2. seed admin user, one screen, one playlist via the admin API
  3. boot trmnl-sim headless with --control and port-forward 2301 so the
     device reaches the host at 10.0.2.2:2301
  4. drive the captive portal: connect WiFi + server http://10.0.2.2:2301
  5. wait for the device to poll; assert mntr saw setup + display
  6. POST /wake, fetch /screenshot PNG -> visual artifact of what the
     firmware drew from our rendered BMP

Requires: ../trmnl-sim/target/release/trmnl-sim, a firmware build
(.pio/build/TRMNL_X/FW*-TRMNL_X.bin or -trmnl), mntr cargo build.
"""
import argparse, json, os, re, subprocess, sys, tempfile, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

def http(url, body=None, headers=None, method=None, raw=False):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method or ("POST" if body else "GET"))
    req.add_header("Content-Type", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    with urllib.request.urlopen(req, timeout=60) as r:
        b = r.read()
        return r.status, (b if raw else (json.loads(b) if b and b[:1] in b"{[" else b))

def wait_port(proc, port, timeout=30):
    import socket
    t0 = time.time()
    while time.time() - t0 < timeout:
        if proc.poll() is not None:
            raise RuntimeError(f"process exited early:\n{proc.stderr.read().decode()[-2000:]}")
        with socket.socket() as s:
            s.settimeout(0.5)
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.3)
    raise RuntimeError(f"port {port} never opened")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--firmware", default=None, help="merged FW*.bin (default: newest TRMNL_X build)")
    ap.add_argument("--server-port", type=int, default=2301)
    ap.add_argument("--seconds", type=int, default=120)
    ap.add_argument("--shot", default=os.path.join(ROOT, "sim_screen.png"))
    args = ap.parse_args()

    fw = args.firmware
    if not fw:
        d = "/workspace/trmnl-firmware/.pio/build/TRMNL_X"
        cands = sorted(f for f in os.listdir(d) if f.startswith("FW") and f.endswith(".bin") and "littlefs" not in f)
        if not cands:
            sys.exit("no merged FW bin found; pass --firmware")
        fw = os.path.join(d, cands[-1])
    sim = "/workspace/trmnl-sim/target/release/trmnl-sim"
    print(f"[harness] firmware: {fw}")

    tmp = tempfile.mkdtemp(prefix="mntr-e2e-")
    procs = []
    def cleanup():
        for p in procs:
            p.terminate()
    import atexit; atexit.register(cleanup)
    server = subprocess.Popen(
        [os.path.join(ROOT, "target/debug/mntr"), "serve",
         "--db", os.path.join(tmp, "e2e.db"), "--addr", f"127.0.0.1:{args.server_port}"],
        stdout=open(os.path.join(tmp, "server.log"), "w"), stderr=subprocess.STDOUT)
    procs.append(server)
    wait_port(server, args.server_port)
    base = f"http://127.0.0.1:{args.server_port}"

    # seed: admin + screen + playlist
    _, r = http(base + "/admin/register", {"email": "e2e@t.c", "password": "longenough9"})
    auth = {"authorization": f"Bearer {r['token']}"}
    template = json.dumps({"name": "e2e", "parts": [
        {"type": "text", "text": "{{ title }}", "style": {"font_px": 48, "bold": True}, "padding": [30]},
        {"type": "text", "text": "{{ body }}", "style": {"font_px": 24}, "padding": [30]},
        {"type": "divider", "padding": [30]},
        {"type": "text", "text": "mntr + trmnl-sim", "style": {"font_px": 18}, "padding": [30]}]})
    _, r = http(base + "/admin/screens", {"name": "e2e", "template": template,
        "vars": {"title": "TERMINUS-RS", "body": "real firmware, no real hardware"}}, auth)
    sid = r["id"]
    _, r = http(base + "/admin/playlists", {"name": "p", "screen_ids": [sid]}, auth)
    # device MAC we will burn into the sim's eFuse so we can pre-assign
    DEV_MAC = "7C:DF:A1:12:34:56"

    # simulator: headless, control API, forward the server port to the guest
    import socket as _s
    with _s.socket() as _probe:
        _probe.bind(("127.0.0.1", 0))
        ctl_port = _probe.getsockname()[1]
    simlog = open(os.path.join(tmp, "sim.log"), "w")
    simp = subprocess.Popen(
        [sim, fw, "--headless", "--erase", "--fast-sleep", "--mac", DEV_MAC,
         "--control", f"127.0.0.1:{ctl_port}",
         "--host-port", f"{args.server_port}={args.server_port}"],
        stdout=simlog, stderr=subprocess.STDOUT)
    procs.append(simp)
    wait_port(simp, ctl_port)
    ctl = f"http://127.0.0.1:{ctl_port}"

    def portal(path, body=None):
        # the sim mirrors the captive portal on the portal port; use its API
        return http(ctl + path, body)

    # wait for the setup portal, then connect it to our server
    print("[harness] waiting for captive portal...")
    pu = ""
    for _ in range(180):
        st = http(ctl + "/status")[1]
        pu = (st.get("portal_url") or "").rstrip("/")
        if pu:
            break
        time.sleep(1)
    else:
        sys.exit("portal never came up; see " + os.path.join(tmp, "sim.log"))
    print(f"[harness] portal: {pu}")
    # same call the firmware's own Ruby client makes (spec/lib portal_connect)
    http(pu + "/connect", {"ssid": "TRMNL-Sim", "pswd": "simpass", "server": f"http://10.0.2.2:{args.server_port}"})

    # wait for the device to pair, assign its playlist, then wait for a poll
    print("[harness] waiting for device setup + playlist assign + first display poll...")
    deadline = time.time() + args.seconds
    polls = 0
    assigned = False
    while time.time() < deadline:
        _, devs = http(base + "/admin/devices", headers=auth)
        if devs and not assigned:
            st, _ = http(base + "/admin/devices/assign",
                         {"mac": DEV_MAC, "playlist_id": 1}, auth)
            assigned = (st == 200)
        if devs:
            polls = sum(1 for d in devs if d.get("last_poll_at"))
            if polls and assigned:
                break
        time.sleep(2)
    _, devs = http(base + "/admin/devices", headers=auth)
    print("[harness] devices:", json.dumps(devs, indent=2))
    if not polls:
        sys.exit("device never polled mntr")

    # force a wake + refresh, then screenshot what the firmware drew
    try:
        http(ctl + "/wake", {})
    except Exception:
        pass  # already awake
    time.sleep(8)
    png = http(ctl + "/screenshot", raw=True)[1]
    open(args.shot, "wb").write(png)
    print(f"[harness] screenshot -> {args.shot} ({len(png)} bytes)")

    _, reqs = http(ctl + "/console?since=0")
    print("[harness] done. sim log:", os.path.join(tmp, "sim.log"))
    simp.terminate(); server.terminate()

if __name__ == "__main__":
    main()
