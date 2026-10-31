# DESIGN.md — mntr admin visual system

**Status:** direction locked, home/overview prototype built and reviewed.
**Direction:** *The Bitmap* — restrained register.
**Prototype:** `prototypes/bitmap.html` (self-contained HTML/CSS).
**Port target:** `web-app/src/` (Svelte 5).
**Surface:** admin home / overview — "what's on my wall right now."

---

## 1. Thesis

The admin UI is **calm paper**. The only place the loud, dithered, bitmap character appears is *inside the previews of the e-ink screens themselves* — because that is literally what an e-ink panel looks like. Everything around those previews (nav, labels, cards, buttons) is a quiet humanist grotesque on hairline rules.

This is the whole idea, and the thing to protect in every future screen:

> **The bitmap face and dither are the *content* of the displays, not the *chrome* of the admin.**

The first pass over-indexed on "bold" and rendered the whole admin as a brutalist poster. That was rejected. The correct register is restrained: the e-ink previews carry all the personality; the admin recedes so a family can read it across a room.

## 2. Palette

| Token | Hex | Use |
|---|---|---|
| `--paper` | `#F4F2ED` | page ground (the e-ink paper) |
| `--paper2` | `#ECE8DF` | screen-preview fill, "next-up" row fill |
| `--ink` | `#1C1A16` | primary text, primary button, ink tags |
| `--ink2` | `#575146` | secondary text, meta, labels (~6:1) |
| `--ink3` | `#6E685C` | tertiary meta, ids, footer (~4.6:1) |
| `--line` | `#E0DBCF` | hairline rules, dividers |
| `--line2` | `#C9C3B4` | stronger hairline (card borders, button shadow) |
| `--red` | `#B23A2E` | **semantic accent only** (see §5) |
| `--dot` | `#D9D3C5` | dither dot colour inside previews |

The cream ground is **intentional** — it is the e-ink material, not a default beige. Do not "modernise" it to pure white or grey.

## 3. Type

Three faces, strict roles:

- **`--display` — Pixelify Sans (bitmap).** Only for: the `mntr` wordmark, and text *inside e-ink screen previews* (the clock, "Good morning", "7:40", "21°", "Today", recently-shown words). Never on admin chrome.
- **`--body` — Hanken Grotesk.** All admin chrome: nav, buttons, card titles, section labels, body copy, event names.
- **`--mono` — JetBrains Mono.** Data and tracked micro-labels: status keys, timecodes, measurements, ids, footer.

Scale (functional-text floor is **11px** — this is a family surface read across a room):

| Role | Size |
|---|---|
| body | 15px |
| card title (h3) | 16px / 700 |
| section label (h2) | 14px / 700 / uppercase |
| card meta, status keys | 12–13px |
| micro-labels, tags, ids, footer | 11px / uppercase / tracked |
| e-ink preview content | 17–46px (bitmap) |

Section labels are deliberately small and differentiated by **weight + uppercase + tracking**, not size. That is an editorial device, not an oversight.

## 4. Dither

The dot-matrix dither is the signature and the **only** imagery. It lives **only** inside `.screen` previews, as a light radial-gradient dot field:

```css
background: var(--paper2);
background-image: radial-gradient(var(--dot) 1px, transparent 1.5px);
background-size: 5px 5px;
```

Never use dither as page chrome, backgrounds, icons, or borders. Keep it light — it should read as "e-ink texture," not noise.

## 5. Red — semantic, occasional

Red means **"look, this matters right now."** It is never decoration and never the primary action.

Allowed uses (target ≤3 per view):
1. **Needs-attention count** when > 0 (status panel).
2. **The next-up event** — red time + a small red square marker.
3. **A display in a problem state** — the offline card's status.

The primary button is **ink**, not red. Selection is ink (border + subtle offset shadow), not red.

## 6. Components

- **Header** — sticky, hairline bottom rule. Bitmap wordmark + small mono tagline; text nav (active = ink + 2px ink underline); ink `+ Add display` button with a 2px `--line2` offset shadow; family chip (ink avatar square).
- **Hero** — `1.5fr / 1fr`. Left: a large e-ink preview (5:3) of the featured display, `NOW ON KITCHEN` ink tag + "updated N min ago". Right: bordered **Status** panel — hairline rows, `label / value`, needs-attention in red.
- **Spec line** — one hairline-ruled mono line: `4 gray levels / 800×480 panel / native render / LAN only · no cloud`. Reinforces the self-hosted story quietly.
- **Display card** — hairline border; top row = id + status dot; e-ink preview thumbnail (5:3); name (grotesk) + one-line status; footer `View display →` + dim mono spec. States: `active` (ink dot), `low` (hollow dot), `off` (red dot + red label). Selected = ink border + `3px 3px 0 var(--line)` shadow.
- **Today rail** — bordered panel; hairline event rows (`time / name + who`); next-up row gets `--paper2` fill + red time + red square. Family legend of initial squares at the foot.
- **Recently shown** — strip of small e-ink preview tiles (4:3), tracked cap + bitmap word.
- **Footer** — quiet mono line, bitmap `mntr`.

## 7. Layout & responsiveness

- Content max-width `1320px`, centred, `26px` gutters.
- Breakpoints: `≤1000px` hero and split collapse to one column; `≤680px` display grid → 1 col, recently-shown → 3 cols, nav hides, gutters `16px`.
- All grid/flex children carry `min-width:0` (prevents content blowout). `html,body{overflow-x:hidden}` as a guard.
- Verified at 1440, 820, and a true 390px viewport (rendered in a 390px iframe — headless Chrome clamps `--window-size` to a 500px minimum, so iframe is the reliable way to test narrow widths).

## 8. Accessibility

- Functional text ≥ 11px; body 15px.
- Tertiary gray kept ≥ 4.5:1 on paper (the earlier `#948D7E` at 2.9:1 was a real failure and is fixed).
- `prefers-reduced-motion` respected; motion is limited to subtle hover/press on controls.
- Themed `::selection`, `:focus-visible` (2px ink outline), and scrollbar.

## 9. Known intentional detector exceptions

`impeccable detect` flags four items that are deliberate, not defects:
- **cream-palette** — the e-ink paper is the concept (§2).
- **tiny-text 11px** — e-ink *screen-content* costume (depicting text on a physical 800×480 panel), analogous to the code-context exemption.
- **flat-type-hierarchy** — section labels use weight + case, not size (§3).
- **cramped-padding (status rows)** — the rule is a list divider, not a box wrapping text; 13px inset is intentional.

## 10a. Port status (shipped to `web-app/`)

The overview is ported and verified against the real backend with seeded data. Deltas from the prototype, all deliberate:

- **Fonts are self-hosted, no Google Fonts.** mntr is offline-first / no cloud round-trip, so the UI must not fetch fonts. The wordmark uses the **real device font TRMNL16-Bold** (vendored in `assets/framework/fonts/`, inlined as base64 `@font-face` at build time in `build.js`). Body = system-ui stack; data = ui-monospace. The prototype's Pixelify/Hanken/JetBrains were demo-only.
- **Screen previews are real rendered images**, fetched authed from `/admin/screens/:id/preview` (native renderer output) — the e-ink look is authentic automatically; the dot-field is only the placeholder when a device has no current screen. Required a one-line backend change: `current_screen_id` + `refresh_interval` added to `device_list_json`.
- **The fictional "Today" calendar and "Recently shown" strip are dropped** — there is no calendar or per-display history in the data model, so inventing them would be dishonest. The right rail is now an **Attention** panel listing offline / low-battery devices (the real "needs attention" items), with a calm "All displays healthy" empty state.
- **Live state is derived, never fabricated:** online = last poll within `max(2×refresh_interval, 30 min)`; low battery = `voltage < 3.3 V`; needs-attention = offline + low-battery count. Battery shows voltage, not a fake percentage.
- **Shared header restyled** to the new system (applies to all tabs). The other tabs' content (Screens/Playlists/Firmware/Models/Users) still uses the legacy table styles — an incremental port; the overview was the target surface.

## 10. Porting notes (Svelte)

- Lift §2 tokens into a `:root` / `app.css`; keep the three-face split exactly.
- Build a `<ScreenPreview>` component (the `.screen` dot-field wrapper) reused by hero, cards, and recently-shown — it is the single home of the dither.
- Build `<StatusRow>`, `<DisplayCard>` (with `status` + `selected` props), `<EventRow>` (with `next` prop).
- Red is a prop/state semantic (`needsAttention`, `next`, `offline`), never a style default.
- The bitmap face must stay scoped to `ScreenPreview` content + wordmark; treat any use outside that as a bug.
