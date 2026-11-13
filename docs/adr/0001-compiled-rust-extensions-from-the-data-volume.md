# ADR-0001: Runtime extensions are compiled Rust loaded from the data volume

**Date**: 2026-11-13
**Status**: accepted, implemented for the load path
**Deciders**: Daniel Buckner with pi

## Context

Every extension is compiled in today. `extensions::registry()` returns
`&'static [Plugin]`, and `Extension::template()` returns `include_str!` data. A
new extension means a Rust change, a rebuild, and a new image.

We want an MCP server to author extensions inside a running deployment. It
writes source, builds it, tests the render, applies it, and repeats. That loop
cannot work if every iteration needs an image build.

Rust is the opinionated extension language here. The existing contract is a Rust
trait with two methods, `template()` and `vars()`, and the framework is already
generic over kinds: nothing outside an extension folder names a kind.

## Decision

Add `/data/extensions/<kind>/` as a runtime extension folder. It holds the same
files a built-in folder holds, plus Rust source and a build output directory:

```text
/data/extensions/<kind>/
  src/               Rust source
  manifest.json      unchanged contract: kind, label, consumes, settings
  template.liquid    unchanged contract
  vars.json          fixture for the offline check
  build/<arch>/      lib<kind>.so plus a metadata file
```

The server loads built artifacts from that tree at boot and on reload. Bundled
extensions stay compiled in. A runtime extension and a bundled extension produce
the same `Plugin` entry, so the admin UI, the settings validation, and the
rebuild dispatch do not change.

## Alternatives Considered

### Alternative 1: Declarative vars mapping

Add a mapping spec so a folder turns snapshot rows into Liquid vars with no
Rust. A folder on disk would then be a complete extension.

- **Pros**: No compiler in the deployment. No code execution from the volume.
  The same mapping would let a provider be declared as data.
- **Cons**: It is a new language to design, document, and bound. The built-in
  extensions are the hard cases, not the easy ones. Calendar groups events by
  LA-local date, drops past timed events, caps the list, and formats 12-hour or
  24-hour times. Home Assistant flattens Lovelace containers into a tile grid.
- **Why not**: The user's stated opinion is that Rust is the extension language.
  A mapping language replaces one authoring surface with a smaller one and
  leaves the hard cases unsolved.

### Alternative 2: Embedded scripting with Rhai, QuickJS, or Lua

- **Pros**: Small runtime. Fast interpreted loop. No compiler needed.
- **Cons**: A second language in the plugin path, with its own types and its own
  standard library to learn and to keep current.
- **Why not**: Same objection as Alternative 1. It contradicts Rust as the
  extension language.

### Alternative 3: WASM plugins

- **Pros**: One artifact runs on every architecture. Strong sandboxing by default.
- **Cons**: A wasm runtime becomes a core dependency. Authoring needs a wasm
  target and a toolchain that lives outside the container.
- **Why not**: Deferred, not rejected. The boundary in ADR-0002 is JSON, so a
  wasm host can replace the dylib host later without changing the contract.

### Alternative 4: Keep extensions compiled in only

- **Pros**: No change at all.
- **Cons**: The authoring loop needs an image build per iteration.
- **Why not**: It does not serve the goal.

## Consequences

### Positive

- An MCP can iterate on an extension without an image build.
- The extension contract does not change. `manifest.json`, `template.liquid`, and
  `vars.json` keep their current meaning, and `blazonry ext check` already
  validates that folder shape from disk.
- Bundled extensions keep their current guarantees. A broken volume mount cannot
  remove the core set, because the core set stays in the image.
- A runtime extension gets the same admin form, the same settings validation,
  and the same rebuild dispatch for free.

### Negative

- The deployment needs a Rust toolchain somewhere. See ADR-0003.
- A folder in `/data` is arbitrary code running as the server user. It can read
  the database and every stored provider credential, including the Home
  Assistant long-lived access token. This must be off by default and gated
  behind the admin session. The README must say so in prose, not only in a
  comment.
- A plugin cannot add a provider shape. Shapes are the `providers::Snapshot`
  enum, and that stays a framework change.
- `Extension` needs a rework before any of this lands: `&'static dyn Extension`
  becomes `Arc<dyn Extension>`, and `template()` returns owned text.

### Risks

- **Version skew between the image and a folder in `/data`.** `/data` outlives
  image upgrades. The loader must reject an artifact whose recorded API version
  or compiler version does not match the running server. See ADR-0003.
- **A bad plugin blanks a wall display.** Mitigated by the apply gate in
  ADR-0002 and ADR-0003, and by the fact that the BMP cache keeps serving the
  last good image.
- **Scope creep into a plugin marketplace.** Out of scope. This record covers
  loading a folder that an operator or an MCP placed on the volume.

## Implementation notes

Built in `src/ext_host.rs` and `src/bin/ext_host.rs`.

- A folder holds `src/`, `Cargo.toml`, `manifest.json`, `template.liquid`,
  `vars.json` (the check fixture), and `request.json` (the probe fixture).
- The build output goes to `<folder>/build/<arch>/lib<kind>.so`, beside
  `lib<kind>.json`. That sidecar records the SHA-256 of the artifact, the rustc
  that built it, the architecture, and the build time.
- The folder name must equal the crate name. The loader looks for
  `lib<folder-name>.so`, so a folder called `quote` cannot ship a crate called
  `quotes`.
- A dot folder is scratch space (`.cargo`, `.git`) and the scan skips it.
- A runtime kind may not take a name a bundled extension already owns. The
  bundled set is the one the tests and the docs describe, and it wins.
- Built-in extensions stay compiled in, as decided above. Nothing seeds
  `/data/extensions` on first run.
