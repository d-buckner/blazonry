# terminus-rs

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

## Next

1. ~~axum device plane~~ done: `/api/display` (pair-by-MAC, access-token,
   playlist rotation, `next_refresh_in`), `/api/setup`, `/api/logs`.
   Verified end-to-end against a scripted fake device: pair → assign →
   poll rotation (screen 1 → screen 2, distinct BMPs) → 401 on bad token.
2. ~~SQLite schema~~ devices/screens/playlists + rotation.
3. Job loop: tokio interval fetchers → vars → render → cache BMP per device.
4. Admin API + minimal web UI; auth (argon2 + sessions). Admin plane is
   unauthenticated LAN-only for now.
5. TRMNL wire compat pass against a real device (partial updates, custom
   templates via `markup` pass-through later if ever needed).

Note: port 2300 on the Hermes box is already taken (something serves a 404
page there); dev server uses 2301.
