# tairix-geometry

The single shared **integer screen geometry** for the TAIRiX desktop
(`AGENTS.md` §6, §17.4 — `PLAN.md` Stage 7): the `Point` and `Rect` types
used by the compositing window manager (`userland/gui/wm`), the taskbar
(`userland/gui/taskbar`), and the default graphical apps.

- `Point` — a signed (`i32`) screen coordinate; a window may sit partly off
  the top or left edge.
- `Rect` — an axis-aligned rectangle (a `Point` origin plus unsigned `u32`
  size) with checked `intersection`, `union`, and half-open `contains`. A
  zero-width or zero-height rectangle is *empty*, the canonical "covers
  nothing" value used by damage tracking and clipping. A layout carves bands
  off a rectangle with `take_top` / `take_bottom` / `take_left` /
  `take_right` and pulls one in with `inset` (`EMPTY` where that leaves
  nothing); `surface_origin` is the corner as the unsigned coordinates of a
  surface pixel, or `None` for a rectangle a surface cannot address.
- `Scale` — the desktop DPI / UI scale factor (`AGENTS.md` §10): the ratio of
  physical to logical pixels as a percentage of `REFERENCE_DPI` (96).
  `Scale::ONE` is 1:1; `from_percent`/`from_dpi` build a scale and fail closed
  outside `MIN_PERCENT..=MAX_PERCENT`; `scale_length` is the single
  logical→physical conversion every GUI consumer shares (§2.2), so a desktop
  authored in logical pixels stays a comfortable physical size across panel
  densities.
- `GridRun` and `GridFill` — the one arithmetic for a run of equal cells
  along one axis: how many whole cells an extent holds at a gap, where each
  sits, which one a coordinate falls in, and — `GridFill::Spread` against
  `FixedPitch` — whether the leftover room widens the gaps or stays at the
  far end. The file manager's and the desktop's grids and the picture choice
  all lay their tiles out through it, and `within` narrows each cell to one
  part of it — a tile's picture — over the same count and stride.
- `Region` — a set of screen pixels held as disjoint rectangles in a canonical
  band form, exact or, `with_budget`, degrading to its bounding box past a
  rectangle count: the one damage and clip set every consumer shares. A
  rectangle past the end of the band form is appended in place, and
  `add_region`/`subtract_region` merge a whole region in one walk over both,
  so folding one region into another costs their sizes, not their product.
- `Tiles` — which tiles of a picture a change touches, a bit a tile, and the
  cover of them a compositor is told to repaint: the marked tiles in band
  form, each tile row's runs joined with the rows beneath that mark the same
  tiles, the tiles doubled until the rectangles fit a budget, so a region
  holds exactly as many as were counted. Marking costs a tile's bit however many changes
  land on it, so a scattered change (a screensaver's cells, a ray tracer's
  passes) is named as a few rectangles rather than thousands.
- `desktop_modes` — the sizes a graphical desktop may take on an output: its
  native size first, then each standard VESA/CEA size smaller on both axes,
  no smaller than `DESKTOP_MIN`, within 1% of the output's shape. The session
  that applies a size and every surface that offers one read this one list.

All edge arithmetic widens through `i64`/`u32` so a pathological coordinate
saturates rather than wrapping — it fails closed (`AGENTS.md` §2.9). `Scale`
widens through `u64` and saturates the same way. `to_i32` (an extent) and
`saturate_i32` (a wide coordinate) are the one way back into an `i32`
coordinate, for this crate and its consumers alike.

## Why it lives in `lib/`

The GUI crates may not depend on one another (`AGENTS.md` §17.4), and code
shared by more than one crate lives in `lib/*` (§6). These coordinate types
are needed by the window manager, the taskbar, and every graphical app, so
they belong here rather than being defined once in the window manager and
duplicated elsewhere (§2.2). The crate has no dependencies and sits at the
bottom of the §17.4 layering: it is depended on, never depends. The window
manager re-exports `Point` and `Rect` from this crate, so there is exactly
one definition.

There is no rendering or compositing arithmetic here — that is the window
manager's job — keeping this crate a pure, dependency-free coordinate
vocabulary.

## Stability tier

`experimental` — the Stage 7 desktop geometry seam. It is `no_std` with
`alloc` and has no dependencies: `Region` and `Tiles` hold their rectangles
and bits on the heap (a `Tiles` the heap refuses is `None`), and the rest
allocates nothing. No `unsafe`, and no
`unwrap`/`expect`/`panic!` in production paths (`AGENTS.md` §2.9).
