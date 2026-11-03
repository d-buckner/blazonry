# Why blazonry

*The long version of the README's "Why blazonry" — the origin story, the
hardware economics, and where blazonry stands relative to Terminus.*

## The story

I bought a TRMNL because I wanted a dashboard fed by *my* data. The first
thing I tried was the obvious one: my own calendar server on my own LAN.
It didn't work — not on the hosted cloud, and not on Terminus, the official
BYOS server. So I read the stack, and the "self-hosted lightweight display"
story didn't hold up:

| Terminus (official BYOS) | blazonry |
|---|---|
| 3 containers: app (649 MB), Postgres (120 MB), Valkey (19 MB) — **~790 MB pulled** | **one 13 MB binary** |
| Ruby + Hanami + Node + esbuild + Puma + Sidekiq + Overmind + Rodauth | Rust, no runtime deps |
| Postgres + Valkey/Redis to run, migrate, back up | SQLite file. Copy it, you're backed up |
| Full **Chromium** install in the image to rasterize screens | native renderer (tiny-skia + fontdue), ~5 ms per 800×480 screen |
| Docker Compose, multiple processes, `overmind start` | `blazonry serve` |

Chromium. For a **4-level grayscale, 800×480, 1-bit-class image**. That is
most of the image weight: a ~500 MB browser doing what a few thousand lines
of software rasterizer do in milliseconds. blazonry renders natively: Liquid
template → JSON screen tree → tiny-skia → Floyd–Steinberg dither → 2-bit BMP,
verified against the real firmware's parser (see the README's Testing
section).

If you came here from "why can't I point my display at my own calendar":
that's the whole reason. blazonry is a 13 MB binary that can.

## The hardware is Seeed's. The software is the product.

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
  **$47.99**. The OG's ~$90 premium buys the logo, the injection-molded
  case, and the cloud you're about to stop using.
- **blazonry is the software half of the deal, and it's the better half for a
  self-hoster.** No cloud round-trip, no plugin marketplace to live inside,
  no 790 MB container stack (see above): one 13 MB binary serving your
  screens from your LAN. The wire contract is identical, so the Seeed kit
  can't tell the difference.

**Seeed DIY kit + blazonry ≈ $48 all-in. TRMNL OG + cloud ≈ $139 plus a
subscription.** Same panel, faster MCU, no vendor in the middle, and
software that fits in one binary instead of three containers.

And the "plugin marketplace" objection is smaller than it looks: TRMNL
plugins are HTML/CSS/Liquid against HTTP data sources — a content format,
not a runtime. blazonry speaks that format out of the box (Liquid templates +
your own endpoints), so any non-JS extension works here the day you write
it, with no marketplace, review process, or vendor in between — and a
paste-a-TRMNL-recipe-link importer (fetch the recipe, transpile its Liquid
to blazonry's screen vocabulary) is a small, planned step.

What the OG + cloud still genuinely buys: mobile apps, someone else's
uptime, and a curated catalog if you want to browse instead of build. If
that's the product you want, the OG is fine hardware and the firmware is
open. But if you're a self-hoster who bought an e-ink display to put
*your* dashboards on *your* server, buy the panel from Seeed and run
blazonry. Own the whole stack for less.

Other good Seeed options that speak blazonry's protocol:
[reTerminal E1001/E1002](https://www.seeedstudio.com/reTerminal-E1002-p-6533.html)
(~$99–109, assembled, ESP32-S3, ESPHome/Home-Assistant-native — the E1002
is **full-color** E Ink Spectra 6 at that price) and
[XIAO ePaper kits](https://www.hackster.io/news/seeed-studio-s-epaper-offerings-shrink-in-size-not-in-quantity-meet-the-new-xiao-epaper-diy-kits-df9a3a29b638)
from **$16** for 2.13" panels.

## Relationship to Terminus

Respect to the Terminus team — the wire contract they published is what
blazonry conforms to, and the vendored spec in `tests/spec/` is their work.
The container-stack direction is a product choice, not a bug, and my
constraint (a dashboard that talks to my LAN in one binary on the box that
already runs everything) isn't compatible with it. Different tool for a
different constraint.
