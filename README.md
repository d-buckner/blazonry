# mntr

TRMNL BYOS server in Rust. No Chrome, no Postgres/Valkey/Sidekiq, no premium
tiers — one binary, SQLite, Liquid templates, native renderer.

## Spike result (verified 2026-10-29)

`cargo run --bin demo` → Liquid template + vars → Screen tree → tiny-skia
greyscale → Floyd–Steinberg 4-level dither → 2-bit BMP (96,070 B, base64
128,096 B) in ~62ms on a 48-core box. Output visually verified: header,
key/value rows, progress bar, 7-bar chart all legible at 800×480.

## The GPUI finding (important)

GPUI 0.2.2 on crates.io cannot be used headless as a screen rasterizer:

- `Scene`, `Quad`, sprites, `Window::scene` are all `pub(crate)` — the paint
  output is unreadable from outside the crate.
- The `test-support` platform's `WindowOperations::draw()` is a no-op sink.
- The Linux `headless` platform bails on `open_window` entirely.

So the renderer is hand-rolled (tiny-skia + fontdue) but keeps GPUI's
vocabulary — measure/paint two-pass, flex grow/align — so a future GPUI with
a software backend (or a fork) drops in without redesign. Zed's own headless
needs (if any) would come from their vendored gpui, not the published crate.

## Layout

- `src/screen.rs` — the screen vocabulary (text, flex, keyed_value, divider,
  progress, bars). ~15 primitives cover the TRMNL screen families.
- `src/template.rs` — Liquid (rust `liquid` crate) renders a JSON screen
  tree; JSON is the compile target, Liquid the authoring surface.
- `src/render.rs` — measure/paint software renderer.
- `src/dither.rs` — greyscale → 4-level FS dither.
- `src/bmp.rs` — 2-bit bottom-up BMP writer (device wire format).

## Spec conformance (tests/spec_conformance.rs)

Ground truth vendored in `tests/spec/` (fetched 2026-10-29): TRMNL BYOS docs,
private Display API docs, and Terminus `config/routes.rb` + the actual
`app/actions/api/{display,setup,log}` sources. 13 tests, one per contract
line. Findings this pass fixed:

- **`ID` header**: firmware sends `ID: <mac>`, not `idempotency-key`. We
  only read the latter — a real device would have 401'd at setup. Now both.
- **`refresh_rate` must be a NUMBER of seconds.** Firmware reads a string as
  0 and wakes immediately (battery killer). We were sending ms; now seconds.
- **status codes**: `0` = draw image, `202` = not registered yet (was 200);
  unknown access-token = `{status:500, error:"Device not found",
  reset_firmware:true}` (device wipes + re-pairs), not 401.
- **filename stability**: device skips download when filename matches;
  filename is now `screen-{id}-{fnv1a-of-bmp}`, stable until content changes.
- **`image_url` now real**: `/images/{filename}.bmp` serves the cached BMP;
  payload also carries inline base64 `image`.
- **`POST /api/log`** (Terminus parity): device by Access-Token, full
  firmware log schema, 204/404/422 problem-details responses; rows stored
  in `device_logs`.
- **`GET /api/display/current`** + legacy `/api/current_screen`: non-
  advancing mirror via per-device `poll_count` + `current_screen_id`.
- **Telemetry ingest**: FW-Version, Model, RSSI, Battery-Voltage, Width/
  Height stored on the device row (Terminus header schema parity).
- **`update_firmware`** only true when the device's FW-Version differs from
  the newest firmware row (was always true — would OTA-loop devices).

## Admin API (auth: Bearer from /admin/login)

Full CRUD, Terminus `/api/*` parity (INTEGER ids instead of UUIDs):
devices (list/show/patch/delete, bulk import, log wipe), screens
(list/show/patch/delete + PNG preview), playlists (list/show/patch/
delete, items add/hide/remove), firmware (CRUD + wipe — feeds
/api/latest_firmware OTA gating), models (seeded og/x registry).
Patch semantics: only present keys are updated; `playlist_id: null`
unassigns. Deleting a screen cascades out of playlists; deleting a
playlist unassigns its devices.

## Next

1. TRMNL wire compat pass against a real device (partial updates, custom
   templates via `markup` pass-through later if ever needed).
2. Web UI on top of the admin API (CRUD is complete: devices, screens,
   playlists + item visibility, firmware, models, bulk import/wipe).

## Verification

`cargo test` = 4 unit tests (BMP header/bit-packing, dither) + 1 e2e test
that spawns the real binary on an ephemeral port and drives the whole flow:
admin gate 401 → register (first=admin, second=unverified) → verify → login
(bad pw 401, unverified 403) → create screen/playlist → device pair/assign/
poll (valid 800×480 2-bit BMP, rotation, wrong token reset_firmware) → PNG
preview; + 13 spec conformance tests (see above). Preview output visually
verified legible.

Note: port 2300 on the Hermes box is already taken (something serves a 404
page there); dev default is 2301.
