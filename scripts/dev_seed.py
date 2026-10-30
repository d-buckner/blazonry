#!/usr/bin/env python3
"""Seed a local mntr for hands-on testing (browser + trmnl-sim).

Idempotent-ish: registers the admin if missing (first user becomes admin),
then creates the demo screens + playlist and prints the bearer token.

  python3 scripts/dev_seed.py [--base http://127.0.0.1:2301]
"""
import argparse, json, urllib.request, urllib.error

ap = argparse.ArgumentParser()
ap.add_argument("--base", default="http://127.0.0.1:2301")
ap.add_argument("--email", default="admin@mntr.local")
ap.add_argument("--password", default="mntr-demo-2026")
args = ap.parse_args()
BASE = args.base.rstrip("/")


def http(path, body=None, headers=None):
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(body).encode() if body is not None else None,
        method="POST" if body is not None else "GET",
    )
    req.add_header("Content-Type", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            b = r.read()
            return r.status, (json.loads(b) if b and b[:1] in b"{[" else b)
    except urllib.error.HTTPError as e:
        b = e.read()
        return e.code, (json.loads(b) if b and b[:1] in b"{[" else b)


# --- admin (first registered account wins the admin role) -------------------
st, r = http("/admin/register", {"email": args.email, "password": args.password})
if st == 200 and isinstance(r, dict) and r.get("token"):
    token = r["token"]
    print(f"[seed] registered {args.email} (role={r['role']}), logged in")
else:
    st, r = http("/admin/login", {"email": args.email, "password": args.password})
    if st != 200:
        raise SystemExit(f"[seed] could not register or log in: {st} {r}")
    token = r["token"]
    print(f"[seed] logged in as {args.email}")
auth = {"Authorization": f"Bearer {token}"}

# --- screens -----------------------------------------------------------------
# Liquid is the authoring surface, so the demo exercises the loop/if tags too.
DASHBOARD = """{
  "name": "dashboard",
  "parts": [
    {"type": "text", "text": "{{ header }}",
     "style": {"font_px": 34, "bold": true}, "padding": [16, 20, 4, 20]},
    {"type": "text", "text": "{{ subheader }}",
     "style": {"font_px": 18, "fg": 1}, "padding": [0, 20, 10, 20]},
    {"type": "divider", "padding": [0, 20, 10, 20]},
    {"type": "keyed_value", "key": "Outside", "value": "{{ temp }} and {{ condition }}",
     "padding": [6, 20, 6, 20], "style": {"font_px": 26}},
    {"type": "keyed_value", "key": "Today", "value": "{{ meetings }} meetings, {{ tasks }} tasks",
     "padding": [6, 20, 6, 20], "style": {"font_px": 26}},
    {"type": "keyed_value", "key": "Battery", "value": "{{ battery }}%",
     "padding": [6, 20, 10, 20], "style": {"font_px": 26}},
    {"type": "progress", "value": {{ battery_frac }}, "padding": [0, 20, 20, 20]},
    {"type": "text", "text": "SCREEN TIME",
     "style": {"font_px": 16, "bold": true, "fg": 1}, "padding": [0, 20, 4, 20]},
    {"type": "bars", "padding": [0, 20, 0, 20],
     "values": [{% for d in week %}{{ d.h }}{% unless forloop.last %},{% endunless %}{% endfor %}],
     "labels": [{% for d in week %}"{{ d.d }}"{% unless forloop.last %},{% endunless %}{% endfor %}]}
  ]
}"""

CENTERED = """{
  "name": "centered",
  "parts": [
    {"type": "text", "text": "{{ big }}", "align": "center",
     "style": {"font_px": 72, "bold": true}, "padding": [120, 20, 0, 20]},
    {"type": "text", "text": "{{ small }}", "align": "center",
     "style": {"font_px": 24, "fg": 1}, "padding": [16, 20, 0, 20]}
  ]
}"""

screens = [
    ("Dashboard", DASHBOARD, {
        "header": "mntr", "subheader": "one binary, no containers",
        "temp": "61°F", "condition": "clear", "meetings": 3, "tasks": 7, "battery": 82,
        "battery_frac": 0.82,
        "week": [{"d": "M", "h": 3.2}, {"d": "T", "h": 5.1}, {"d": "W", "h": 2.4},
                 {"d": "T", "h": 6.8}, {"d": "F", "h": 4.0}, {"d": "S", "h": 1.2},
                 {"d": "S", "h": 2.9}],
    }),
    ("Centered", CENTERED, {"big": "mntr works", "small": "align: center, no padding hacks"}),
]

ids = []
# re-runnable: drop any previous demo screens first
st, existing = http("/admin/screens", None, auth)
if st == 200:
    for s in existing:
        if s["name"] in {n for n, _, _ in screens}:
            req = urllib.request.Request(BASE + f"/admin/screens/{s['id']}", method="DELETE")
            req.add_header("Authorization", auth["Authorization"])
            urllib.request.urlopen(req, timeout=30)
            print(f"[seed] dropped old screen {s['name']} (id={s['id']})")
st, existing = http("/admin/playlists", None, auth)
if st == 200:
    for p in existing:
        if p["name"] == "Demo":
            req = urllib.request.Request(BASE + f"/admin/playlists/{p['id']}", method="DELETE")
            req.add_header("Authorization", auth["Authorization"])
            urllib.request.urlopen(req, timeout=30)
            print(f"[seed] dropped old playlist Demo (id={p['id']})")

for name, template, vars in screens:
    st, r = http("/admin/screens", {"name": name, "template": template, "vars": vars}, auth)
    if st != 200:
        raise SystemExit(f"[seed] screen {name!r} failed: {st} {r}")
    ids.append(r["id"])
    print(f"[seed] screen {name}: id={r['id']}")

st, pl = http("/admin/playlists", {"name": "Demo", "screen_ids": ids}, auth)
if st != 200:
    raise SystemExit(f"[seed] playlist failed: {st} {pl}")
print(f"[seed] playlist Demo: id={pl['id']} screens={ids}")

print(f"""
[seed] done.
  UI     {BASE}/
  login  {args.email} / {args.password}
  token  {token}

  assign a paired device to the playlist (mac comes from GET /admin/devices):
    curl -s -X POST {BASE}/admin/devices/assign \\
      -H 'Authorization: Bearer {token}' \\
      -H 'content-type: application/json' \\
      -d '{{"mac":"<device mac>","playlist_id":{pl["id"]}}}'
""")
