# mntr

**A TRMNL BYOS server in Rust. One 13 MB binary, zero containers, no Chrome.**

Point your [TRMNL](https://trmnl.com) (or any TRMNL-compatible ESP32 e-ink
display) at `http://your-server:2301` and you own the whole stack: screens,
playlists, device pairing, firmware OTA, users, previews — served from a
single static binary with an embedded web UI.

## Why this exists

I bought a TRMNL because I wanted a dashboard fed by *my* data. The first
thing I tried was the obvious one: my own calendar server on my own LAN.
It didn't work — not on the hosted cloud, and not on Terminus, the official
BYOS server. So I read the stack, and the "self-hosted lightweight display"
story fell apart:

| Terminus (official BYOS) | mntr |
|---|---|
| 3 containers: app (649 MB), Postgres (120 MB), Valkey (19 MB) — **~790 MB pulled** | **one 13 MB binary** |
| Ruby + Hanami + Node + esbuild + Puma + Sidekiq + Overmind + Rodauth | Rust, no runtime deps |
| Postgres + Valkey/Redis to babysit, migrate, back up | SQLite file. Copy it, you're backed up |
| Full **Chromium** install in the image to rasterize screens | native renderer (tiny-skia + fontdue), ~60 ms per 800×480 screen |
| Docker Compose, multiple processes, `overmind start` | `mntr serve` |

Chromium. For a **4-level grayscale, 800×480, 1-bit-class image**. That is
most of the image weight, and it's the most absurd part of the stack: a
~500 MB browser doing what a few thousand lines of software rasterizer do
in milliseconds. mntr renders natively: Liquid template → JSON screen tree
→ tiny-skia → Floyd–Steinberg dither → 2-bit BMP, verified against the
real firmware's parser (see [Testing](#testing)).

If you came here from "why can't I point my TRMNL at my own calendar":
that's the whole reason. The antidote to a 790 MB container stack that
still can't reach your LAN is a 13 MB binary that can.

### The hardware is Seeed's. The software is the product — and you can self-host a better one.

Once you see what you're actually buying, the math is not close:

- **TRMNL's value prop is software, not hardware.** The OG is $139 for a
  7.5" 800×480 4-level grayscale panel with an ESP32-C3; what the
  subscription and brand actually sell is the cloud: 1,400+ plugins,
  playlists, mobile apps, polish.
- **Seeed Studio builds the hardware.** The
  [TRMNL 7.5" (OG) DIY Kit](https://www.seeedstudio.com/TRMNL-7-5-Inch-OG-DIY-Kit-p-6481.html)
  is a literal Seeed × TRMNL co-development — the *same* 7.5" 800×480
  panel, a *better* MCU (ESP32-**S3** vs the OG's C3), a 2000 mAh battery,
  TRMNL firmware preinstalled, open-source enclosure designs — for
  **$47.99**. You are paying the OG's ~$90 premium for the logo, the
  injection-molded case, and the cloud you're about to stop using.
- **mntr is the software half of the deal, and it's the better half for a
  self-hoster.** No cloud round-trip, no plugin marketplace to live inside,
  no 790 MB container stack (see above): one 13 MB binary serving your
  screens from your LAN. The wire contract is identical, so the Seeed kit
  can't tell the difference.

**Seeed DIY kit + mntr ≈ $48 all-in. TRMNL OG + cloud ≈ $139 plus a
subscription.** Same panel, faster MCU, no vendor in the middle, and
software that fits in one binary instead of three containers. That is the
same value for a third of the money — and, on the self-hosting axis,
better software.

And the "plugin marketplace" objection is smaller than it looks: TRMNL
plugins are HTML/CSS/Liquid against HTTP data sources — a content format,
not a runtime. mntr speaks that format out of the box (Liquid templates +
your own endpoints), so any non-JS extension works here the day you write
it, with no marketplace, review process, or vendor in between — and a
paste-a-TRMNL-recipe-link importer (fetch the recipe, transpile its Liquid
to mntr's screen vocabulary) is a small, planned step.

What the OG + cloud still genuinely buys: mobile apps, someone else's
uptime, and a curated catalog if you want to browse instead of build. If
that's the product you want, the OG is fine hardware and the firmware is
open. But if you're a self-hoster who bought an e-ink display to put
*your* dashboards on *your* server, buy the panel from Seeed and run
mntr. Own the whole stack for less.

Other good Seeed options that speak mntr's protocol:
[reTerminal E1001/E1002](https://www.seeedstudio.com/reTerminal-E1002-p-6533.html)
(~$99–109, assembled, ESP32-S3, ESPHome/Home-Assistant-native — the E1002
is **full-color** E Ink Spectra 6 at that price) and
[XIAO ePaper kits](https://www.hackster.io/news/seeed-studio-s-epaper-offerings-shrink-in-size-not-in-quantity-meet-the-new-xiao-epaper-diy-kits-df9a3a29b638)
from **$16** for 2.13" panels.

## Quick start

```bash
cargo install --path .        # or: download the release binary
mntr serve                    # → http://0.0.0.0:2301
```

1. Open `http://your-server:2301`, register. **The first account becomes
   admin** (subsequent ones wait for verification — same model as Terminus).
2. On your TRMNL: Settings → Server URL → `http://your-server:2301`.
   The device pairs automatically and shows up on the Devices tab.
3. Create a screen (Liquid template + JSON vars), a playlist, assign it.
   Done — the device polls and renders.

No Docker, no database server, no reverse proxy required. The whole state
is one SQLite file (`--db path`), so backups are `cp`.

## What you get

- **Device plane** — full TRMNL wire contract: `/api/display`, `/api/setup`,
  `/api/log`, `/api/latest_firmware`, image serving. Verified against the
  real firmware in QEMU, not just the docs (see Testing).
- **Admin web UI** — Svelte 5, SSR-prerendered and inlined into the binary.
  Devices, screens (with live previews), playlists, firmware, models, users.
- **Admin API** — bearer-auth CRUD mirroring Terminus `/api/*`: bulk device
  import, playlist item visibility, firmware wipe, log wipe.
- **Native renderer** — ~15 screen primitives (text, keyed_value, flex,
  progress, bars, divider) cover the TRMNL screen families; Liquid is the
  authoring surface, a JSON screen tree is the compile target.
- **OTA gating** — `update_firmware` only when the device's reported version
  differs from the newest firmware row (no OTA loops).
- **Battery-friendly** — stable filenames so devices skip unchanged
  downloads; `refresh_rate` in seconds as the firmware actually expects.

## Layout

```
src/screen.rs    screen vocabulary (~15 primitives)
src/template.rs  Liquid → JSON screen tree
src/render.rs    measure/paint software renderer (tiny-skia + fontdue)
src/dither.rs    greyscale → 4-level Floyd–Steinberg dither
src/bmp.rs       2-bit bottom-up BMP writer (device wire format)
src/api.rs       device plane (TRMNL wire contract)
src/admin.rs     admin API (Terminus parity)
src/auth.rs      argon2 accounts, bearer sessions
web-app/         Svelte 5 source → node build.js → web/index.html
```

## Testing

Everything below is reproducible from a clean checkout with `cargo test`
plus two Python scripts; no physical hardware required.

### 1. Unit + end-to-end (`cargo test`, 23 tests)

- **Unit** — BMP header/bit-packing, dither output.
- **e2e** — spawns the real binary on an ephemeral port with a temp DB and
  drives the whole flow: admin gate 401 → register (first=admin,
  second=unverified) → verify → login (bad pw 401, unverified 403) →
  create screen/playlist → device pair/assign/poll → decodes the returned
  BMP and asserts a valid 800×480 2-bit image, playlist rotation, and the
  wrong-token `reset_firmware` path → PNG preview.
- **spec_conformance (13 tests)** — one test per contract line against
  ground truth vendored in `tests/spec/`: TRMNL BYOS docs, the private
  Display API docs, and Terminus `config/routes.rb` + the actual
  `app/actions/api/{display,setup,log}` sources.

### 2. Real firmware in QEMU (`scripts/e2e_sim.py`)

The docs lie; the firmware doesn't. This harness builds the **actual TRMNL
firmware** from source (PlatformIO, `pio run -e trmnl`) and runs it inside
QEMU via [trmnl-sim](https://github.com/mooselace/trmnl-sim), then drives it
against a freshly spawned mntr:

```bash
cargo build --release
python3 scripts/e2e_sim.py \
  --firmware .pio/build/trmnl/FW1.8.16-trmnl.bin --seconds 240
```

The simulated device boots, pairs over the QEMU gateway, polls, downloads
the BMP, and decodes it. This pass caught bugs no doc review would:

- firmware sends `ID: <mac>`, not `idempotency-key` (a real device 401'd at setup)
- `refresh_rate` must be a **number of seconds** — a string reads as 0 and
  the device wakes continuously (battery killer)
- unknown access-token must return `{status:500, reset_firmware:true}`, not 401
- `image_url` must be absolute (it goes straight into `HTTPClient::begin`)
- the OG panel demands exactly 48,062-byte 1bpp BMPs; a base64 image
  inlined in the JSON OOMs the ESP32 heap (max alloc ~63 KB) — never inline

The device's decoded screen, captured from the simulator:

![Screen rendered by real firmware in QEMU](docs/img/firmware-sim.png)

A native render of a full dashboard (header, key/value rows, progress bar,
7-bar week chart) at 800×480:

![Native screen render](docs/img/screen-render.png)

### 3. UI end-to-end (`scripts/ui_check.py`)

Spawns the server on an ephemeral port + temp DB, drives the admin UI in
headless Chromium: register → login → create screen with Liquid + vars →
walk every tab → assert **zero JS console errors** (the pre-login
`/admin/me` 401 is expected and filtered). Exits nonzero on any error.

![Devices tab](docs/img/ui_devices.png)

![Screens tab with live preview](docs/img/ui_screens.png)

![Playlists tab](docs/img/ui_playlists.png)

![Users tab](docs/img/ui_users.png)

## Design notes

**Why not GPUI?** The first attempt used GPUI 0.2.2 as a headless
rasterizer. It can't be: `Scene`, `Quad`, and paint output are all
`pub(crate)`, the test platform's `draw()` is a no-op sink, and the Linux
headless platform bails on `open_window`. So the renderer is hand-rolled
(tiny-skia + fontdue) but keeps GPUI's vocabulary — measure/paint two-pass,
flex grow/align — so a future GPUI with a software backend drops in without
redesign.

**Why not just fix Terminus?** Respect to the Terminus team — the wire
contract they published is what mntr conforms to. But the container-stack
direction is a product choice, not a bug, and my constraint (a dashboard
that talks to my LAN in one binary on the box that already runs everything)
isn't compatible with it. Different tool for a different constraint.

## Status

Wire-compatible with TRMNL firmware 1.6.10+ (OG and X panels). The admin
plane uses INTEGER ids where Terminus uses UUIDs. No plugin marketplace —
that's the point: your data sources are your own HTTP endpoints, and
screens are Liquid you control.

## License

TBD — see `LICENSE` once chosen (AGPL-3.0 + commercial dual-license is the
current direction).
