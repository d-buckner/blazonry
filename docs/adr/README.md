# Architecture Decision Records

These records explain why blazonry is shaped the way it is. Each one states the
situation, the decision, the options we rejected, and the trade-offs we accepted.

| ADR | Title | Status | Date |
|-----|-------|--------|------|
| [0001](0001-compiled-rust-extensions-from-the-data-volume.md) | Runtime extensions are compiled Rust loaded from the data volume | accepted, implemented for the load path | 2026-11-13 |
| [0002](0002-sidecar-extension-host-with-a-json-boundary.md) | Plugins run in a sidecar host and speak JSON capabilities | accepted, implemented | 2026-11-13 |
| [0003](0003-builder-sidecar-and-apply-gate.md) | A builder sidecar compiles plugins, and apply requires a green check | accepted, implemented | 2026-11-13 |

Status is `proposed` while a decision is under discussion, `accepted` once the
team follows it, and `superseded by ADR-NNNN` when a later record replaces it.
A decision that is agreed but not yet built says so in its own line.
