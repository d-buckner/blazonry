# ADR-0004: one container runs the server and the extension host as two processes

**Date**: 2026-11-14
**Status**: accepted, implemented
**Deciders**: Daniel Buckner with pi

## Context

ADR-0002 put plugin loading in a separate process so a plugin that segfaults
cannot take the server down. ADR-0003 put the compiler in a sidecar container,
because the runtime image was `FROM scratch` with a static musl binary that
cannot `dlopen`. That gave the deployment two containers sharing a volume, and CI
published only the small one.

Two things made that shape expensive.

The published image could not run the authoring loop at all. Six of the seven MCP
tools forward to the extension host, and each answers `no extension host: start
this server with --ext-host <socket>` until one is running. The builder image
existed, but CI did not publish it, so the loop was unreachable from any install
that pulled the release.

The second container also left a hole ADR-0003 recorded: "Nothing in blazonry
starts the host, and nothing brings it back." The sidecar's `--restart
unless-stopped` was the only recovery, and it cannot tell a plugin that crashed
once from a plugin that crashes every time it loads.

A deployment platform asked for one container per app. Bloud, which installs
blazonry, models each container as a graph node, and a second standing container
costs a node, an edge, and a wiring story for a capability most installs never use.

## Decision

Build **one image definition** that publishes **two tags**, and run **one
container** with **two processes**: the server is PID 1 and supervises the
extension host.

- `:latest` stays `FROM scratch` at about 6 MB. It carries the server and the MCP
  server and has no compiler, so it cannot author.
- `:toolchain` carries the server, the extension host, and the Rust toolchain, at
  about 950 MB. Its default command names the socket and the program to spawn, so
  the tag is the opt-in.

The process boundary from ADR-0002 stays. What goes away is the container boundary.

- The server starts the host with `--ext-host-spawn <program>`, alongside the
  existing `--ext-host <socket>` it already dials.
- The supervisor restarts the host with backoff, resets the backoff after the host
  stays up 60 seconds, and warns at three consecutive fast restarts.
- The server takes the host down on SIGINT and SIGTERM. A container signals PID 1
  and PID 1's children get nothing, so an unhandled host would outlive the server
  and the next start would find a stale socket behind it.
- The toolchain stage is glibc. `ext_host` dlopens, and a plugin's libc must match
  the host that loads it. Its build is native per platform, not a musl
  cross-compile, which is why the two tags need two builder stages.

## Consequences

**Accepted costs.**

- The `:toolchain` tag is about 950 MB against the 6 MB default. That is the
  toolchain. `rust:1.99.1-slim-bookworm` measures 916 MB and `rust:1.99.1-alpine`
  measures 1.04 GB, so a smaller base image buys nothing; the compiler is the cost,
  and the full authoring loop needs a compiler in the running container.
- CI builds the crate twice per platform: once cross-compiled to musl for the
  scratch tag, once natively against glibc for the toolchain tag. A native arm64
  build under QEMU is slower than the musl cross-compile. Not measured in CI at
  the time of writing.
- A host restart clears the staged artifact, so `ext_apply` after a restart
  refuses and asks for a build. Verified on the built image: build and check green,
  host killed, apply answered `nothing is staged; run a build first`. That is
  pre-existing and it is the safe direction, because an artifact a crashed host
  left half-written is not one to promote onto a live panel. The supervisor makes
  it more likely to be met, so the authoring docs say to build again after a
  restart.

**Gains.**

- A published tag can run the whole authoring loop. Six tools that refused become
  real tools.
- A crashed host comes back on its own, and a host that will not stay up announces
  itself instead of looping quietly.
- One container per deployment, so a platform that models an app as one container
  needs no second node.
- The default tag keeps its size, so an install that only shows a calendar still
  pulls 6 MB.

**Rejected for this decision.**

Nothing about the JSON capability boundary in ADR-0002 changes. Nothing about the
apply gate in ADR-0003 changes. Only the deployment topology and the image
contents change.

## Alternatives Considered

### Alternative 1: two containers, as built

- **Pros**: The server image stays 22 MB. Only an install that opts into authoring
  carries the toolchain.
- **Cons**: Costs a second graph node and a shared-volume socket in every platform
  that models an app as one container. The restart hole stays.
- **Why not**: The deployment that asked for this models one container per app, and
  the total toolchain cost is the same once authoring is wanted.

### Alternative 2: one published image, fat

- **Pros**: One tag, no choice to make, and the loop works everywhere.
- **Cons**: Every install pays about 950 MB for a compiler it may never run, and
  the project's published size stops being its differentiator: the comparison the
  README opens with is 6.3 MB against Terminus at 746 MB, and a single fat image
  makes blazonry the larger pull.
- **Why not**: Two tags from one build costs a second build step and keeps both
  claims true.

### Alternative 3: compile inside the server process

- **Pros**: One process, and no supervisor.
- **Cons**: Ends the isolation ADR-0002 exists for. A plugin that segfaults takes
  the server down, and the server holds the database and every provider token.
- **Why not**: Rejected in ADR-0002 for the same reason, and that reason did not
  change. Only the container boundary did.

### Alternative 4: build off-box and copy the artifact in

- **Pros**: No toolchain in the deployment at all.
- **Cons**: The artifact must match the target architecture, the exact `rustc`, and
  the libc. The MCP cannot do that step unattended.
- **Why not**: Kept as a supported path for published plugins, as in ADR-0003. It
  cannot carry an authoring loop.

### Alternative 5: an Alpine base

- **Pros**: Expected to be smaller.
- **Cons**: Measured 1.04 GB against 916 MB for the Debian slim base, so it is
  larger. Worse, musl `dlopen` of a musl `cdylib` puts two copies of libc in one
  process.
- **Why not**: Larger and wrong for the load path.
