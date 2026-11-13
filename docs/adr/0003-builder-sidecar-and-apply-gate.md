# ADR-0003: A builder sidecar compiles plugins, and apply requires a green check

**Date**: 2026-11-13
**Status**: accepted, partly implemented
**Deciders**: Daniel Buckner with pi

## Context

ADR-0002 puts the plugin load in a sidecar. Two questions remain. Where does the
Rust build happen, and what protects a live display from a bad plugin?

The runtime image has no compiler, and adding one contradicts ADR-0002. The
release image is multi-arch, amd64 and arm64, built by buildx with QEMU in
`.github/workflows/container.yml`. A shared library only loads on the
architecture that built it. The authoring loop is driven by an MCP, so a failed
build must not be able to take a screen down.

## Decision

Add a **builder sidecar** for dev mode. It pins rustc to the version the server
image uses, keeps a warm cargo cache, and mounts `/data/extensions` read-write.
It builds only the architecture it runs on. CI builds and publishes both
architectures for blessed plugins.

The MCP drives five tools over the existing admin API:

| Tool | Action |
|---|---|
| `ext_write` | Write source, manifest, template, and fixture into `/data/extensions/<kind>/` |
| `ext_build` | Ask the builder for this architecture, and return compiler diagnostics |
| `ext_check` | Probe run in a throwaway process: load, `vars()` against `vars.json`, render BMP and PNG |
| `ext_apply` | Swap in the new generation, then rebuild the screens that consume that shape |
| `ext_rollback` | Point the registry back at the previous generation |

`ext_apply` refuses unless the newest build and the newest check for that folder
are both green, and the recorded metadata matches the running server. The loader
validates four values before it loads: `blazonry-ext-api` version, rustc
version, target triple, and artifact sha256. The server's own target triple is
the authoritative value for what will load, because the server is what loads it.

The host never unloads a generation. It loads a new one, swaps the registry
pointer, and leaves the old mapping in place. `ext_rollback` swaps back to a
generation that is still loaded.

The whole path is off by default. It needs an explicit opt-in such as
`BLAZONRY_EXT_DEV=1`, and every write requires the admin session.

## Alternatives Considered

### Alternative 1: One-shot build container run on demand

- **Pros**: No always-on builder. Smaller steady-state stack.
- **Cons**: The MCP cannot start a container by itself, so each build needs an
  operator or a host-level helper.
- **Why not**: The point of the loop is that the MCP runs it unattended.

### Alternative 2: Build off-box and copy the artifact in

- **Pros**: No toolchain in the deployment at all.
- **Cons**: The artifact must match the target architecture, and the MCP cannot
  do that step.
- **Why not**: Kept as a supported path for published plugins, but it cannot
  carry the authoring loop.

### Alternative 3: Compile inside the server container

- **Pros**: One container.
- **Cons**: Ends the scratch base and the small image. See ADR-0002.
- **Why not**: Rejected there.

### Alternative 4: Apply immediately, or apply with human approval

- **Pros**: Immediate is faster. Human approval is safer.
- **Cons**: Immediate lets a bad plugin blank a wall. Human approval makes the
  MCP loop need a person on every iteration.
- **Why not**: The green-check gate gets the speed without the blank wall, and
  the check is already a machine-readable command with named failing stages.

## Consequences

### Positive

- A failed build or a failing check cannot reach a live screen. The previous
  generation stays up.
- The MCP loop runs unattended: write, build, check, apply.
- The builder is dev-mode only. A normal deployment never sees a compiler.
- The metadata check turns an architecture mismatch or a stale plugin into one
  readable error at load time.

### Negative

- One more image to build and publish, and one more container in dev mode.
- The builder must track the server's rustc version. A drift is a load-time
  rejection until the builder is rebuilt.
- Generations accumulate in the host process until it restarts. Reload is rare
  in production, and a restart clears it.
- `ext_check` renders a BMP and a PNG per run, so it needs the renderer and a
  writable scratch path in the host container.

### Risks

- **Code execution from the volume.** Rust built from `/data` runs as the server
  user with the database and every stored provider credential in reach. The
  opt-in flag and the admin gate are the only barrier. This needs a prose
  warning in the README, not only a code comment.
- **A plugin that passes its fixture and fails in production.** The check runs
  against `vars.json`, not live snapshots. Mitigation: `ext_apply` rebuilds the
  bound screen immediately, so a live failure shows in the admin UI at apply
  time rather than at the next refresh.
- **Silent staleness after an image upgrade.** A plugin built for the old rustc
  stops loading. Mitigation: the loader reports the mismatch as a named,
  operator-visible error instead of skipping the folder quietly.

## Implementation notes

Built: `ext_host build`, `check`, `apply`, `rollback`, `list`, the artifact
metadata the loader checks, and the admin endpoints that drive them. Not built:
the builder image, and the MCP tools.

| Tool | Endpoint |
|---|---|
| `ext_write` | `POST /admin/extensions/write` |
| `ext_build` | `POST /admin/extensions/build` |
| `ext_check` | `POST /admin/extensions/check` |
| `ext_apply` | `POST /admin/extensions/apply` |
| `ext_rollback` | `POST /admin/extensions/rollback` |

Each one carries a `kind`, never a path. The host owns the volume, so the server
cannot ask for a directory outside the one it was given.

Three details the design did not settle in advance:

- A build lands in `build/<arch>/staging/` and touches nothing live. `apply`
  promotes it and moves what it displaces into `build/<arch>/previous/`. Two
  generations deep is the whole history, and that is enough for one rollback.
- A build record and a check record each store the SHA-256 of the artifact they
  describe. That is what makes a green record expire: edit the source, and apply
  refuses with "the source changed after the last build" instead of promoting
  something nobody looked at.
- `check` returns the PNG it painted, base64'd. The author is a model looking
  through an API, and a path on the builder tells it nothing. This is the admin
  plane; `/api/display` still sends an image URL, because the panel firmware
  cannot parse a string that size.

- An extension declares `blazonry-ext-api` by version, not by path. The folder has
  to build wherever it lands in the volume. The builder supplies the source with
  `--config patch.crates-io.blazonry-ext-api.path=<dir>`, taken from `--ext-api`
  or `BLAZONRY_EXT_API`. A folder that names no ext-api location fails the build
  with a readable error.
- The builder runs cargo from the extension folder, so cargo reads that folder's
  config and not the server's.
- The builder sets `RUSTC` to the rustc beside its own cargo. Otherwise rustup
  resolves a toolchain per directory, the artifact records a different compiler,
  and the loader rejects it. This is the "builder must track the server's rustc"
  consequence, and it is enforced in code rather than by convention.
- `ext_host probe` is the check step: it loads one built artifact in a throwaway
  process, runs it against `request.json`, and prints the vars or the error. A bad
  extension dies there, not in the host.
