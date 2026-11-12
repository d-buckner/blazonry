# BRAND.md — blazonry in the Bloud house

**Status:** direction locked for the bloud-os release.
**Relationship model:** *house brand*. Bloud OS is the house; blazonry is a product
that lives in it. blazonry keeps its own identity (name, wordmark, palette,
register) and obeys the house rules below. Products are siblings, not
re-skins: nothing here asks blazonry to look like the Bloud dashboard, only to
never contradict the house.

This file is the brand contract. `DESIGN.md` remains the visual system for the
admin UI; `PRODUCT.md` remains the product definition. Where they overlap,
this file decides brand questions only.

---

## 1. The house, in one paragraph

Bloud is *your friendly personal cloud*: self-hosting without the
sysadmin homework. The house voice is plainspoken, first-person, dry, and
honest to the point of self-incrimination ("Please don't expose bloud to the
public internet yet"; "read it before piping it to a shell"). The house never
hypes, never inflates a number, never promises what it can't verify. Its
enemy is not a competitor — it's the boring misery of wiring YAML at 11pm.

blazonry already speaks this dialect. The README's "The docs lie; the firmware
doesn't" and the house's "Not the installing part. Everything after." are the
same register: wry, concrete, earned confidence.

## 2. House voice rules (shared with `bloud`)

1. **Plain words, short sentences.** Contractions on. No marketing adjectives
   ("seamless", "powerful", "cutting-edge" are banned in both repos).
2. **First person singular is allowed.** The maintainer is a person, not a
   logo. "I built this because it didn't work" beats "we deliver solutions".
3. **Punch at absurdity, never at people.** "Chromium. For a 4-level
   grayscale image." is house-legal: it attacks an architecture, and the
   README pairs it with "Respect to the Terminus team". A line that mocks a
   named person or shames a user for their setup is not.
4. **Measured claims only.** Every number is reproducible from the repo
   (pull sizes, render times, memory). If it can't be re-measured, it doesn't
   ship. Honesty about status (alpha, license TBD) is a feature of the brand.
5. **Own the trade-offs.** Stance sections ("Why not just fix Terminus?",
   "The hardware is Seeed's") stay — the house respects a product that says
   what it is *for* and *not for*.

## 3. Visual kinship (measured, not vibes)

The two products already share a palette core; this is the evidence and the
rule that keeps it true.

| Role | Bloud dashboard | blazonry admin | Verdict |
|---|---|---|---|
| paper / bg | `#FAF9F6` | `#F4F2ED` | same warm-paper family; keep both, do not converge |
| ink / text | `#1C1917` | `#1C1A16` | effectively identical |
| elevated | `#FFFFFF` | — | blazonry stays paper-only (e-ink has no elevation) |
| semantic accent | amber `#B45309` | red `#B23A2E` | **per-product, semantic-only** (see §5) |
| dark mode | full (`#1C1917` ground) | none (paper is the concept) | blazonry stays light-only; do not add dark mode |

**Mark grammar (shared):** flat, two-tone, monochrome, rounded geometry,
negative space as the primary device. The Bloud cloud-carved-`b` and blazonry's
monitor glyph (`web-app/src/icon.svg`, `#1a1a1a` tile / `#f5f4f0` glyph)
already conform. Any future product icon follows the same grammar: one solid
rounded shape, one negative-space letterform or glyph, two tones from the
ink/paper pair — no gradients, no third color, no drop shadows.

**Naming:** product names are lowercase, always, even mid-sentence: `bloud`,
`blazonry`. The org renders as "Bloud OS" only where GitHub capitalizes the org
field; prose says "the bloud-os org" or "Bloud OS" as a proper noun for the
organization, never for a product.

## 4. blazonry identity (what stays blazonry-only)

- **Wordmark:** lowercase `blazonry`. The old plan set it in TRMNL16-Bold (the
  vendor's device font) — rejected in #35: the brand face must not be the
  vendor's typeface. Direction: a real grotesque wordmark (Space Grotesk 700
  leading the shortlist) with the ink-dot dither accent; TRMNL16 stays *inside*
  e-ink previews only, where it honestly depicts the device. Device font
  depicts the device; it is not the brand.
- **Type:** admin chrome is system-ui grotesque + ui-monospace, self-hosted /
  offline-first — no Google Fonts, ever. This intentionally differs from the
  Bloud dashboard's Newsreader/Inter: the constraint (offline binary,
  across-a-room legibility) is real and the difference is fine. House prose
  (org README, docs sites) may use Newsreader; product UIs keep their own
  type systems.
- **Dither law:** the dot-matrix dither is blazonry's signature imagery and lives
  only inside e-ink previews (DESIGN.md §4). It is not a house pattern — do
  not export it to other bloud-os products.
- **Split voice:** admin UI is warm, calm, plain (family surface); README and
  marketing carry the opinionated register. This split is blazonry's, and it
  survives the move unchanged.

## 5. The accent question, decided

Bloud's amber and blazonry's red are **not** a conflict and will not be merged.
The house rule is: *one semantic accent per product, never decorative, never
the primary action.* Both products already obey it (blazonry: needs-attention /
next-up / offline only, ≤3 per view; Bloud: focus ring + danger states).
A future third product picks its own accent under the same rule.

## 6. Release checklist — moving to bloud-os

Brand-visible items only; the git/container mechanics live elsewhere.

- [x] Org profile: name "Bloud OS", description "your friendly personal
      cloud" (already set) — publish the org README as the
      `bloud-os/.github` profile repo (house position in §1's voice, voice
      rules §2, token spec, one-accent rule §5).
- [x] Repo name `blazonry` (lowercase), description in house voice, one line,
      no hype: *"a TRMNL BYOS server in Rust — one 13 MB binary, your LAN,
      your screens."* (set on Forgejo; mirror on the GitHub org repo).
- [ ] Topics: `self-hosted`, `e-ink`, `trmnl`, `rust`, `dashboard` (+
      `bloud-os` if the org adopts a family topic). Not settable via the `fj`
      CLI — set in the repo settings UI or via API token.
- [x] README: keep as-is except (a) container image path
      `ghcr.io/bloud-os/blazonry` → the bloud-os registry path once the
      image moves — done: images now publish to `ghcr.io/bloud-os/blazonry`;
      (b) License section now states AGPL-3.0 + dual-licensing
      (decided Nov 2026) — keep it in sync with `LICENSE`.
- [ ] Org avatar and blazonry icon: both already conform to §3's mark grammar;
      no redraw needed.
- [x] Honesty audit before publish: resolved Nov 2026 — the engine is AGPL-3.0
      + commercial dual-license, the same license as blazonry. The org README's
      "Licensing stance" says so plainly; the house brand survives on exactly
      this kind of answer.

## 7. What must never happen

- blazonry's wordmark or dither used as bloud-os org chrome (the org has no
  bitmap face; the cloud mark is the org's only imagery).
- Combative README lines aimed at people, in either repo.
- A benchmark, star count, user count, or testimonial that can't be re-run or
  shown. No customers, pricing, or press exist — none may be implied.
- Converging the two palettes into one "brand system". Kinship, not identity.
