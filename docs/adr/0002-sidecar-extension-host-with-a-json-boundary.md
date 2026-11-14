# ADR-0002: Plugins run in a sidecar host and speak JSON capabilities

**Date**: 2026-11-13
**Status**: accepted, implemented
**Deciders**: Daniel Buckner with pi

## Context

ADR-0001 loads compiled Rust plugins from `/data/extensions`. Four constraints
from the current build decide how that loading works.

1. The runtime image is `FROM scratch` with a static musl binary. It has no
   dynamic loader, so it cannot `dlopen` anything without giving up the scratch
   base and the roughly 7 MB multi-arch image.
2. `extensions::Ctx` carries `sqlx::SqlitePool` and `serde_json::Value`. Rust
   types across a shared-library boundary require the same compiler, the same
   profile, and the same allocator. That is a coincidence, not a contract.
3. `[profile.release]` sets `panic = "abort"`, `lto = "fat"`, and
   `codegen-units = 1`. A plugin panic would take the process down, and a plugin
   that links the `blazonry` crate rebuilds axum, sqlx, taffy, and tiny-skia.
4. `dlclose` on Rust code with thread-locals, statics, or live tokio tasks
   segfaults. A loaded generation cannot be safely unloaded.

## Decision

Load plugins in a separate **extension host** process. The server talks to it
over a unix socket at `/data/run/exthost.sock`. The host holds the shared
libraries, answers `vars` requests, and restarts on crash.

Nothing but JSON crosses the boundary. The host fetches nothing and the plugin
fetches nothing. The server resolves the snapshots and passes them in, because
every built-in `vars()` reads snapshots only, through `providers::load_snapshot`
or `providers::merged_calendar_rows`.

```jsonc
// request
{
  "kind": "calendar",
  "settings": { "days": 3, "time_format": "24h" },
  "local_date": "2026-11-13",
  "snapshots": [
    { "provider_id": 4, "name": "Home", "shape": "calendar",
      "fetched_at": "2026-11-13T07:00:00Z", "payload": {} }
  ]
}
// response
{ "vars": {} }
// or
{ "error": "plain text shown in the admin UI" }
```

`local_date` is LA-local today, supplied by the server, so a plugin needs no
timezone database. The capability set is closed on purpose. A plugin that needs
to fetch live data needs a new capability, and that is a separate decision.

Plugins link only `blazonry-ext-api`, a small crate with serde, serde_json, and
chrono. It never links the `blazonry` crate.

## Alternatives Considered

### Alternative 1: In-process `dlopen`

- **Pros**: One container. No socket. Lower latency per rebuild.
- **Cons**: The image can no longer be scratch, because a static musl binary
  needs a shared loader at runtime. A plugin segfault or panic ends the server,
  which also ends the device plane that the panels poll.
- **Why not**: The cost lands on the thing this project optimizes for, a small
  single-binary deployment on a Raspberry Pi, and it puts a wall display one
  bad plugin away from going blank.

### Alternative 2: Pass `Ctx` and trait objects across the FFI

- **Pros**: No serialization. Existing extension code is unchanged.
- **Cons**: Requires the plugin and the server to be built by the same compiler
  build with the same profile. Any drift is undefined behaviour at runtime.
- **Why not**: Unsupportable across a published image and a plugin built later.

### Alternative 3: HTTP between the server and the host

- **Pros**: Works across hosts. Easy to inspect with a browser.
- **Cons**: A second network listener and a second auth surface, for a channel
  that never leaves the container group.
- **Why not**: A unix socket needs no credentials of its own.

### Alternative 4: Reload by unloading the old generation

- **Pros**: Memory stays flat across many reloads.
- **Cons**: `dlclose` on Rust code segfaults if any thread still holds a pointer
  into it.
- **Why not**: Correctness beats the memory. See ADR-0003 for the generation
  rule.

## Consequences

### Positive

- The core image stays `FROM scratch`, static, and small.
- A plugin crash ends the host process. The server restarts it, and the BMP
  cache keeps serving the last good image, so the panel does not go blank.
- The boundary is transport-independent. A later WASM host or an in-process host
  can replace the sidecar without changing what a plugin sees.
- `blazonry-ext-api` stays tiny, so a plugin build takes seconds against a warm
  cache instead of minutes.
- The capability set is explicit and small, so a plugin cannot reach the database
  or the network by accident.

### Negative

- A second process in the stack, plus a socket to supervise.
- Snapshot payloads serialize on every rebuild. For a Home Assistant tab this is
  a display-ready tile list, which is small, but it is a real cost per rebuild.
- The `Extension` trait changes shape before any plugin exists. That is the first
  implementation step and the largest existing-code diff.

### Risks

- **A slow plugin blocks its host.** Mitigation: a per-request timeout in the
  client, and a host that restarts on timeout.
- **A plugin panics on every request.** Mitigation: the apply gate in ADR-0003
  runs the plugin against its fixture before it goes live.
- **Protocol drift between the server and the host.** Both are built from one
  workspace and carry a protocol version in every request. A mismatch is a
  startup error, not a silent failure.

## Implementation notes

Built in `ext-api/`, `src/ext_host.rs`, `src/ext_link.rs`, and
`src/bin/ext_host.rs`.

- The four entry points compile as `#[unsafe(no_mangle)]`. Edition 2024 rejects
  the bare attribute.
- The server opens one connection per request and closes it. A rebuild is seconds
  apart, and a kept-alive socket would have to notice a restarted host for no
  gain.
- A host that is down leaves the last sync in the registry. The sync loop logs and
  retries, and the screens keep their last good paint.
- A rebuild writes the extension's template next to its vars
  (`screens::set_paint`). Bundled templates never change, so the stored copy was
  harmless. A runtime template changes on every apply, and a stale copy paints the
  previous screen.
- The client now bounds every call with a per-op limit (`ext_link::Client`): 5s
  for `ping`, 30s for `vars`, 900s for `build`. One limit for everything was
  wrong, because `vars` should answer in a blink and `build` runs cargo.
- What the limit does **not** do: rescue the host. A plugin that spins forever
  keeps the blocking thread it was spawned on forever, because a blocking task
  cannot be cancelled. The limit stops the server from waiting on it, so the
  rebuild loop and every other screen stay alive. Killing a wedged plugin means
  restarting the host process.
- Still not built: requests carry no protocol version. The ABI version is checked
  once, at load, so an old server against a new host fails at load rather than
  misparsing a request.
- **Text metrics do not cross the boundary.** `render::text_width`, `fit`, and
  `fit_size` measure with the embedded DejaVu fonts, and they live in the core. A
  plugin links only `blazonry-ext-api`, so it cannot measure, and the renderer
  never wraps. `examples/extensions/quote` wraps on a 0.62 em estimate instead.
  A follow-up must choose: move the helpers and the fonts into `blazonry-ext-api`
  (about 1.4 MB per plugin), or add a fit call to the host protocol.
