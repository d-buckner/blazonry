# Bloud OS

Your friendly personal cloud.

Bloud is self-hosting without the sysadmin homework. The house voice here is
plainspoken, first-person, dry, and honest to the point of self-incrimination
("Please don't expose bloud to the public internet yet"). We never hype, never
inflate a number, never promise what we can't verify. Our enemy is not a
competitor — it's the boring misery of wiring YAML at 11pm.

## The products

| Repo | What it is |
|---|---|
| [bloud](https://github.com/bloud-os/bloud) | The cloud itself: add an app and the proxy, SSL, SSO, and databases get wired for you. |
| [blazonry](https://github.com/bloud-os/blazonry) | A TRMNL BYOS server in Rust — one 13 MB binary, your LAN, your screens. |

Products are siblings, not re-skins. Each has its own brand contract
(`BRAND.md` in each repo) and its own palette. They share a house, not a
template.

## House voice rules

1. **Plain words, short sentences.** Contractions on. No marketing adjectives
   ("seamless", "powerful", "cutting-edge" are banned in every repo).
2. **First person singular is allowed.** The maintainer is a person, not a
   logo. "I built this because it didn't work" beats "we deliver solutions".
3. **Punch at absurdity, never at people.** Attack architectures, never users.
4. **Measured claims only.** Every number must be reproducible from the repo.
   Honesty about status (alpha, license TBD) is a feature of the brand.
5. **Own the trade-offs.** Every product says what it is *for* and *not for*.

## Shared token spec (kinship, not convergence)

The house palette core — products keep their own values and never merge them:

| Role | Convention | bloud | blazonry |
|---|---|---|---|
| paper / bg | warm off-white, never pure white | `#FAF9F6` | `#F4F2ED` |
| ink / text | near-black, never pure black | `#1C1917` | `#1C1A16` |
| data type | monospace stack (`ui-monospace`) for numbers, labels, specs | same | same |
| body type | system-ui stack; no cloud font CDNs, ever | same | same |
| semantic accent | **one per product, semantic-only, never decorative, never the primary action** | amber `#B45309` | red `#B23A2E` |

**Mark grammar (shared):** flat, two-tone, monochrome, rounded geometry,
negative space as the primary device. No gradients, no shadows in marks.

**The one-accent rule:** amber means Bloud; red means blazonry. They are not a
conflict and will not be merged. A product's accent only ever carries meaning
(needs-attention, destructive, live) — never decoration.

## Licensing stance

Everything in this house is AGPL-3.0: the engine, the catalog, the products.
Copyleft keeps modified network services open and keeps community catalog
contributions compounding in one repo instead of fragmenting into forks. A
commercial dual-license keeps white-label embedding possible without anyone
forking. The paid part is the Mesh subscription — multi-host coordination —
never a feature of the free single host. We say it plainly when asked.
