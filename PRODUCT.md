# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

- **Setup persona (one technical person):** typically one technically-comfortable member of a household who installs the server, pairs the displays, and authors the screens/recipes (Liquid + JSON, their own data sources). This is the only person who needs to touch the technical surface, and it is usually one person, not the whole family.
- **Primary daily audience: the whole family.** Partners, kids, and grandparents use the displays and the admin UI day to day — glancing at the family calendar, seeing what is on each screen, switching a playlist, checking the weather. They are not technical. The UI must look great and be genuinely easy for them, legible across a room and across ages.

The admin UI is a shared family surface, not a sysadmin panel. It is designed for the whole family first; the technical setup and screen-authoring surface is one person's job and must not dominate the everyday experience.

## Product Purpose

blazonry is a self-hosted server for TRMNL-compatible e-ink displays. Point a device at your own server and you control the whole stack — screens, playlists, pairing, firmware/OTA, users, previews — from one binary on a box you already run.

It exists because the obvious path — pointing a TRMNL at your own calendar/data on your own LAN — did not work on the hosted cloud or on the official BYOS server. Success means a family's own information — calendar, weather, chores, photos — reliably on the displays on their wall, owned end to end, and easy enough that anyone in the house can use it.

## Positioning

**The self-hoster's better half.** blazonry is a respectful alternative to TRMNL's cloud and Terminus (the official BYOS server), not a clone. It offers the half self-hosters actually want: your data, your server, your screens, no vendor in the middle. It conforms to the same TRMNL wire contract, so the hardware can't tell the difference.

It does not try to be TRMNL (no curated plugin catalog, no hosted mobile apps). It wins on ownership and simplicity rather than feature parity.

## Operating Context

- A family home. The displays live in shared spaces (kitchen, hallway, family room) and are read by everyone, often from across the room.
- Runs on a home server / NAS / Raspberry Pi on the household LAN. State is a single SQLite file; backup is `cp`.
- Setup ritual: open the admin UI and register — the first account becomes admin; later accounts wait for admin verification. On the device: Settings → Server URL → the server address; the device pairs automatically and appears on the Devices tab.
- Authoring: a screen is a Liquid template plus JSON variables; a playlist orders screens and is assigned to devices; devices poll and render.
- The rendered output targets a 4-level grayscale, 800×480 e-ink panel.

## Capabilities and Constraints

- **Device plane:** the full TRMNL wire contract (`/api/display`, `/api/setup`, `/api/log`, `/api/latest_firmware`, image serving), verified against the real firmware in QEMU. Wire-compatible with firmware 1.6.10+ (OG and X panels).
- **Admin web UI** (Svelte 5, SSR-prerendered, inlined into the binary): Devices, Screens (with live previews), Playlists, Firmware, Models, Users.
- **Admin API:** bearer-auth CRUD mirroring Terminus `/api/*` (bulk device import, playlist item visibility, firmware wipe, log wipe). Uses INTEGER ids where Terminus uses UUIDs.
- **No plugin marketplace:** data sources are the user's own HTTP endpoints; screens are Liquid the user controls. A TRMNL-recipe importer is planned, not built.
- **Deliberate stances (held, not limitations):** core function never depends on a vendor cloud; no lock-in or dark patterns.
- **License: AGPL-3.0 with commercial dual-licensing available** (decided Nov 2026; `LICENSE` in repo).

## Brand Commitments

- **Name:** `blazonry` — the heraldic craft of declaring, in words, what is
  displayed. A real word (never unvowelled), lowercase always, used as-is.
  Renamed from `mntr` Nov 2026: the unvowelled trick read as a TRMNL knock-off;
  the product is an independent alternative that speaks TRMNL's wire protocol.
- **Voice is split.** The admin UI is warm, calm, and plain — clear and encouraging enough for any age in the house. The opinionated, direct, occasionally combative voice lives in the README and marketing, not in the product UI.

## Evidence on Hand

- `README.md`: detailed product narrative with measured comparisons (image size, memory) against the official Terminus stack. Those measured numbers are real; do not invent additional benchmarks.
- `docs/img/`: real native screen renders and admin UI captures used by the README.
- `tests/`: `cargo test` (unit + e2e + spec_conformance), `scripts/e2e_sim.py` (real firmware in QEMU), `scripts/ui_check.py` (headless-Chromium UI e2e).
- `prototypes/`: four prior admin-UX directions (The Wall, Composing Room, Poll Desk, Proof Room) with captures in `.taste-skill/`. The prototype notes record a prior verdict: dither used as UI chrome was rejected for fighting readability, and Direction 4 "Proof Room" (paper/ink discipline, one accent, dither only inside the screen proofs) was the concluded direction. Treat this as prior exploration to confirm in new-work, not a locked decision.
- **Absences future work must not fabricate:** no customers, testimonials, case studies, pricing, or press; no license decision yet.

## Product Principles

1. **Ownership is the product.** The family's displays, data, and screens stay theirs. When two goods conflict, give the user more control, not less.
2. **Self-hosting is the default, not the hard mode.** Remove every step the user shouldn't have to take; the product runs on the household's own server and never depends on a vendor cloud to work.
3. **Never build a cage.** No forced accounts, telemetry, or lock-in — leaving must stay as easy as arriving.
4. **Designed for the whole family, not just the person who set it up.** The everyday experience must be beautiful and obvious to a non-technical person of any age; the technical setup and authoring surface stays available but never dominates.
5. **The ceiling is the author's imagination, not a catalog.** Any data source, any screen; keep the format open so the person who sets it up can make anything.

## Accessibility & Inclusion

The displays and the admin UI are read by the whole family — kids and grandparents included — often from across a room. Strong contrast, large legible type, and never relying on color alone to convey state are real requirements, not nice-to-haves. No formal standard was mandated. The device output is a 4-level grayscale e-ink panel (no color, limited contrast), a constraint on the rendered screens (user-authored content) that reinforces not relying on color alone.
