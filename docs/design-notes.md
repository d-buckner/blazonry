# Design notes

*The rendering-stack decisions behind blazonry, moved out of the README.*

## Why not GPUI?

The first attempt used GPUI 0.2.2 as a headless rasterizer. It can't be:
`Scene`, `Quad`, and paint output are all `pub(crate)`, the test platform's
`draw()` is a no-op sink, and the Linux headless platform bails on
`open_window`.

## Why taffy?

The GPUI post-mortem left a hand-rolled measure/paint pass (tiny-skia +
fontdue) in place, but hand-rolling CSS-ish layout is a bug factory: leaf
`align`, padding overflow, and ignored `width`/`height` all shipped silently
before PR #3 chased them down. Geometry now comes from
[`taffy`](https://github.com/DioxusLabs/taffy) (MIT, the CSS flexbox/grid
engine several UI frameworks use): it computes boxes, we still paint the
pixels ourselves — text leaves answer taffy's measure calls with fontdue
advances, and the raster backend is unchanged. A future GPUI with a software
backend could replace the paint side without touching layout.

## Why not just fix Terminus?

Respect to the Terminus team — the wire contract they published is what
blazonry conforms to. But the container-stack direction is a product choice,
not a bug, and my constraint (a dashboard that talks to my LAN in one binary
on the box that already runs everything) isn't compatible with it. Different
tool for a different constraint.
