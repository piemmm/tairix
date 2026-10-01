# Pointer cursors

TAIRiX pointer cursors are **vectorised, colourful, scalable, and
replaceable** — richer than a one-bit fill mask (`AGENTS.md` §6 / §10,
`PLAN.md` Stage 7). They live in the shared `lib/cursor` crate
(`tairix-cursor`) so the window manager and the default apps use them without
depending on one another (`AGENTS.md` §17.4). The crate is `no_std`,
`#![forbid(unsafe_code)]`, and owns no colour arithmetic of its own.

## The vector representation

A cursor is a `VectorCursor`: filled `Shape`s over a square design grid, plus
a hotspot and an optional `Outline`. A shape is what it is painted with, which
points it encloses (its fill rule), and the contours that bound them — lists
of `(x, y)` design-grid coordinates. It is the shared `lib/raster` artwork layer, so a built-in cursor
and one decoded from SVG are the same thing to the rasteriser; a decoded one
may also carry groups, where a clip, a mask, or a group opacity composites
part of the artwork as a unit. Because the artwork is geometry rather than a
fixed bitmap:

- **Scaling** is exact: `VectorCursor::rasterise(side)` renders the artwork
  into a `side`x`side` pixel image, whatever design grid it was authored on.
  The side is asked for in **pixels**, not as a factor of that grid, because
  the grid is an authoring detail that differs between a built-in cursor (64
  units to the reference pixel) and one decoded from SVG
  (`tairix_svg::DESIGN_GRID`): a caller
  naming a factor would get a different pointer size from each, so swapping
  cursor sets would resize the pointer. `tairix_cursor::CURSOR_BASE_SIDE_PX`
  is the *logical* side the desktop draws a pointer at before density and
  the user's pointer size, and the one logical-to-physical conversion
  (`tairix_geometry::Scale::scale_length`) turns it into pixels.
- **Crisp at every size**: stretching a grid across an arbitrary side puts an
  upright or level edge part-way across a pixel at almost every ratio, where it
  smears into a grey column — which is why a pointer drawn that way is sharp
  only at the one size its set was drawn for. `rasterise` therefore *fits* the
  artwork to the side's pixel grid first. Every upright or level edge at least
  a pixel long moves to the nearest pixel boundary, measured out from the
  hotspot, and every other coordinate is carried along between the edges on
  either side of it, so diagonals and curves keep their sub-pixel placement and
  no coordinate passes another. Measuring from the hotspot makes the hotspot a
  pixel corner at every side and keeps artwork symmetric about it symmetric; an
  edge at least half a pixel past its neighbour never lands on it, so no stem
  vanishes. The fit bends only at those edges' lines, and each edge is split
  where it crosses one, so artwork built of overlapping pieces — every stroke
  is — stays unbroken. A gradient or pattern paint is carried across by the
  plain stretch. The fitted artwork is capped at a fixed number of points, so
  artwork built to cross every such line draws no cursor rather than costing
  the square of its size.
- **The outline is declared, not drawn**: an `Outline` is a colour and a width.
  The renderer strokes the fitted silhouette — every contour a fill draws,
  outside any masked group — twice that width in whole pixels (never under
  one), with joins mitred up to a right angle and round past it
  (`lib/svg`'s `LineJoin::MiterOrRound`), and lays it beneath the artwork. The
  part outside the silhouette is therefore exactly the outline's width from it
  on every edge; corners of a right angle or wider stay square, as the body's
  are, and sharper ones are round rather than a spike several rims long. A
  drawn rim would be stretched with the artwork and come out a different
  weight on every edge. The band is capped at a fixed number of points too;
  artwork whose band would pass it draws no cursor rather than one without
  its rim.
- **Anti-aliasing** is exact: each output pixel takes the true fraction of its
  own area the shape covers, so an edge lands where the geometry puts it
  instead of on the nearest of a handful of sample points.
- **Colour** is real: every layer carries its own colour and alpha and is
  composited through `lib/raster`'s single premultiplied-alpha path, so the
  cursor library duplicates no colour arithmetic (`AGENTS.md` §2.2).
- **Shapes meet cleanly**: the stack is composed through
  `Surface::layered`, which paints a multi-shape cursor larger and averages it
  down, so a light body over its outline shows no pale seam where the two
  anti-aliased edges meet.

Both the scan conversion and the blend live in one place — `lib/raster`'s
`Surface::draw_artwork`. The cursor library fits each `Shape` onto the side's
pixel grid and hands the artwork to that shared path rather than carrying its
own scan converter, so the desktop has exactly one polygon rasteriser, shared
with the icon library (`AGENTS.md` §2.2 / §10).

Rasterising yields a `CursorImage`: a `lib/raster` `Surface` (transparent
outside the artwork) plus the hotspot in that image's pixel coordinates.
Degenerate cursors and scales fail closed with `None` rather than panicking
(`AGENTS.md` §2.9).

## Cursor sets

A `CursorTheme` binds one `VectorCursor` to each `tairix_theme::CursorKind`
(`Arrow`, `Text`, `Pointer`, `Move`, `Busy`, `Crosshair` — four arms that stop
short of a clear centre, the hotspot, so the pixel under it is never hidden —
and the four resize double arrows
`ResizeHorizontal`, `ResizeVertical`, `ResizeDiagonalRising`,
`ResizeDiagonalFalling`). `tairix_theme::CURSOR_KINDS` is that closed
vocabulary as a table, so a loader, a cache, or a test iterates every kind
without restating the list. The fields are fixed and `CursorTheme::from_cursors`
asks for the artwork *by kind* rather than by argument position, so a set can
neither omit a cursor nor mis-order two (`AGENTS.md` §2.11). The built-in set
(`CursorTheme::builtin`) draws each cursor as a light body inside a one-pixel
dark outline, so it stays legible on any background, and its busy ring carries
a coloured arc. It is authored in logical pixels of the reference side, on a
grid 64 units to the pixel, so curves and 45-degree edges land where they are
drawn; its curves are SVG path data flattened by `lib/svg`'s one flattener. The arrow's hotspot is the corner its rounded tip is drawn into; the
move cursor's heads stay short beside its arms so the gaps between them stay
open rather than closing the cross into a diamond.

The four resize cursors are one arrow at four angles — a head at either end
joined by a thin shaft, centred on the design grid with the hotspot at its
middle; a diagonal's heads are right-angled corners whose sides are level and
upright, so they land on whole pixels as crisply as a straight arrow's. Each is unchanged by a half turn about that hotspot, because a resize
edge can be dragged either way and a one-headed arrow would say otherwise; the
vertical arrow is the horizontal one transposed and the two diagonals are
mirror images, so a window's two corners get opposite slopes. The unit tests
assert all three relations on the rasterised coverage rather than trusting the
authored coordinate tables, at every side from half the reference size to
four times it.

Because a `CursorTheme` is plain data, an entirely different look is just a
different theme. The `CursorRegistry` holds the available sets and the active
one, keyed by a `CursorSetId` — an owned, bounded, `Copy` name held inline
(`lib/inline`'s `ArrayString`) rather than on the heap, because the window
manager compares its cursor-cache epoch on every pointer refresh and an
owned heap name would put an allocation on that path. The id is the set's
**directory name in the store, which is also the label a chooser draws**,
exactly as a wallpaper category's is, so no second spelling of it can drift.
`CursorSetId::new` fails closed on anything that is not a plain leaf name
within `tairix_abi::desktop::CURSOR_SET_NAME_MAX` bytes: a name with a
separator could widen the store path it is spliced into. The built-in set
(`CursorSetId::builtin()`, named `Standard`) is always present, so there is
always an active set to return; `register` and `set_active` fail closed on a
duplicate or unknown id rather than panicking (`AGENTS.md` §5.4 / §2.9).
Swapping the active set replaces the whole pointer look at runtime — no
window-manager change.

## The store

On-disk cursor sets follow the desktop's **SVG-first** asset rule
(`AGENTS.md` §10). `tairix_cursor::store` is the store's one definition:

- `/System/Graphics/Cursors/<set>/<asset-id>.svg` — one directory per set,
  one asset per `CursorKind` inside it. The `<set>` level is the user's
  choice (the `cursor.set` setting); the `<asset-id>` level is what the
  *active theme* names that kind (`CursorSet::asset`), so a theme may point
  at artwork of its own inside whichever set the user chose. The shipped
  sets are authored against `CursorSet::canonical()`, which is every kind
  under its own `CursorKind::asset_id`.
- `catalog_sets` builds the choice space from a directory listing: it
  performs no I/O, drops a name no set could carry, sorts by name, and
  leaves the built-in set the slot a reply frame reserves for it. A
  directory claiming the built-in name is dropped, so a store cannot shadow
  it.
- `MAX_CURSOR_ASSET_BYTES` is the fixed validation bound on one untrusted
  asset. `tools/syshelp` refuses to plant an asset over it, or one whose
  name no kind asks for, or a set directory no chooser could offer — so
  unreachable or over-large artwork fails the *build*, never the desktop.

The sets are discovered from `lib/cursor/assets/` at build time by the same
`GRAPHICS_FAMILIES` walk that discovers the icon masters and the wallpapers
(`GraphicsFamilyKind::Cursor`), never from a hand-maintained list. One set
ships today: **High Visibility**, a dark pointer inside a two-pixel white
outline declared on each asset, with bolder heads, which is what makes the
`cursor.set` row a real choice rather than a control of one value. A shipped
asset is its cursor's body alone, and declares its rim with
`data-outline-color` / `data-outline-width` beside its hotspot. The body is
authored as filled shapes: a rim traces every contour the fills draw, and a
stroke decodes to a union of a piece per segment and join, so a stroked ring
costs its rim a trace of each of its two hundred pieces — five times the
drawing of the same ring as two filled circles.

`tairix_cursor::decode_svg(bytes)` (built on `tairix_svg::decode` and
`VectorCursor::from_svg`) performs the conversion — through the curated
§16.4 image-decoding library — preserving the asset's
`data-hotspot-x`/`data-hotspot-y` hotspot and its declared outline; a malformed or undecodable asset
fails closed **per kind**, so the desktop keeps the built-in cursor for that
kind rather than crashing (`AGENTS.md` §2.9). See [SVG asset
decoding](./svg-assets.md). The built-in set remains the always-present
fallback.

**A cursor asset is decoded in the session, not in a parser sandbox, and
that is deliberate.** §19.5 sandboxes parsers of *untrusted* input, which is
why a wallpaper is decoded in one (`WallpaperChoice::Image` names any
absolute path) and why every icon is (the one artwork resolver serves each
bundle's own icon as well as the shipped masters). A cursor set can
only ever come from `/System/Graphics/Cursors/`, on the read-only,
system-signed `/System` volume that nothing but the installer and the
updater may write (`AGENTS.md` §16.2) — it is first-party shipped artwork
reached by a name the store itself supplied. The decode is nonetheless
bounded on every axis a hostile file would push: `MAX_CURSOR_ASSET_BYTES`
before a byte is parsed, and `lib/svg`'s own layer, total-vertex and
`<use>`-depth bounds during, with a decoder that is total and returns a
typed error rather than panicking for *any* input. An asset id the theme
supplies is validated as a plain leaf name at the path splice
(`cursor_asset_path`), so a theme can never make the session read outside
the set directory.

**Every set is loaded at bring-up, not when one is first chosen.** The
desktop session walks the store once — `/System` is read-only, so the choice
space is fixed for the life of the boot — reads each set's assets, and
registers them all. Activating a set afterwards is pure memory, which is the
point: the choice arrives on the loop that owes the user a frame, and
reading a directory there is exactly what an interactive surface must never
do (`AGENTS.md` §28.1).

## Placing one on screen

A `CursorImage` is artwork; where it goes is a `PlacedCursor`. It stores the
image's top-left corner as the pointer position minus the hotspot, so the
hotspot lands exactly on the pointer, and it answers the two questions a
screen has about a drawn cursor: `bounds()` — the rectangle it covers, for
damage — and how to get its pixels: sampled a row at a time (`local_row` /
`sample_row`), or read whole rows of its `image` by a screen that blends a run
at once, as the compositor does. Either way the cursor is blended over what
lies behind rather than painted into it: a screen that painted it in would
have to rebuild those pixels before it could move.

This lives in `lib/cursor` rather than in the window manager because it has
two consumers that may not depend on one another (`AGENTS.md` §17.3 / §2.2):
the compositor, and the graphical login screen
(`userland/session/greeter`), which is a `userland/session/*` crate and so is
forbidden a `userland/gui/*` edge.

## Shadow and size, after rasterising

A `CursorImage` can be transformed once drawn (`lib/cursor`'s `image`
module), each through `lib/raster`'s one blur and one resampler:

- `shadowed` lays the image over the soft shadow it casts: its own coverage,
  dropped down and to the right and softened by `soften_coverage` — the same
  three-pass recipe a text shadow uses — in proportion to the image's side. The
  image grows to hold it and the hotspot moves with the artwork, so the pointer
  lands exactly where it did.
- `resampled_to(side, recycled, scratch)` redraws the image at another size
  with the hotspot scaled to the nearest pixel, into a recycled image's buffer
  where one is given and filtering in a `ResampleScratch` the caller keeps, so
  a pointer shown at a new size every frame allocates only while it outgrows
  the buffers it cycles through.

## In the compositor

The window manager owns the active `CursorRegistry`. It resolves a
`CursorKind` to a `VectorCursor`, rasterises it at the display scale once, and
composites the resulting `PlacedCursor` as the top-most overlay so the hotspot
tracks the pointer. Moving the pointer marks the cursor's old and new
rectangles dirty, so only those pixels are recomposited (the same damage model
the window stack uses), and hiding the cursor restores the pixels beneath it.

The overlay (`userland/gui/wm`'s `pointer` module) also draws, beneath the
cursor, the two aids the session asks for: a **trail** of `Ghost`s — the
cursor's own current image at positions the pointer has just left, each at its
own opacity, so a trail costs no image of its own — and a **halo** of up to
four `HaloRing`s centred on the pointer, drawn through `lib/raster`'s one ring
rasteriser into a buffer the overlay keeps while the halo shrinks. Every part's
damage is derived at composite time by diffing its footprint against the one
the last composite drew, so the cursor still costs exactly two rectangles per
frame however many samples moved it, and a ring's damage is its band cut into
slabs — never the square around it, whose inside the ring leaves untouched.
Hiding the cursor hides its aids with it. The accelerated present hands the
engine one layer per sprite in the order the software composite blends them.

## Helping find the pointer

The Accessibility pane offers four aids (`cursor.*` in the desktop settings
document). Shaking to find is on for everyone, since it costs nothing until
the pointer is shaken; the other three are asked for.

- **Pointer shadow** (`cursor.shadow`) is part of the artwork: the controller
  draws each kind `shadowed`, and the shadow is part of its cache epoch
  (`CursorEpoch { side, set, shadow }`).
- **Shake to find** (`cursor.shake`): three quick strokes across, each
  reversing the last, at least 40 logical pixels wide, done within 220 ms and
  more across than down, grow the pointer to `ENLARGED_SIDE_PX` (four times the
  reference pointer, or half again the user's own where that is larger). It
  stays grown while the shaking goes on and 450 ms after, then settles back,
  over the theme's `PointerEnlarge` and `PointerRestore` timings — at once
  under reduced motion. The controller rasterises the shown kind once at its
  enlarged size and resamples every step between from that, outside its cache,
  into the two buffers it trades with the compositor and one resample scratch,
  so a step allocates nothing once they have grown and growing never evicts
  the images the pointer returns to. At rest the pointer is its own crisp
  cached image again, and the enlarged buffers are let go.
- **Pointer trails** (`cursor.trail` = `off` | `short` | `medium` | `long`):
  three, five or eight copies, each where the pointer was a fixed interval
  ago, read from its path rather than from the samples the device reported, so
  they sit evenly along it and draw back into the pointer once it stops.
- **Find with Ctrl** (`cursor.locate`): Ctrl pressed and released on its own
  — down from no modifier, up again within a second, with no key, no other
  modifier and no pointer button between — sends two rings closing in on the
  pointer, the second a beat behind the first, each in the accent over a thin
  dark rim so it reads on any ground. Under reduced motion one ring stands
  around the pointer for the same 720 ms instead. The keyboard source is where
  the tap is recognised, since every record passes it in order; the pointer
  source answers whether a button went with it. A key the keyboard never
  reports (Caps Lock, an unmapped key) cannot spoil a tap, because nothing on
  the desktop could have acted on it either.

The session steps all of them once a frame, against the one clock reading the
frame is shown at, and each asks for a frame only while it is changing, so an
idle desktop with every aid on still parks indefinitely. Under the screensaver
nothing is drawn and whatever was in flight is dropped. The login screen draws
its own pointer and offers none of them: it runs before any user's settings.

## On the login screen

The greeter has no compositor, and it runs before any user's own settings,
so it takes the built-in `Arrow` at the reference pointer side, rasterises it
once at start-up for the active
`tairix_geometry::Scale`, and samples the `PlacedCursor` over the painted
surface as each frame is composed — the pointer is therefore always on top of
everything the authentication surface drew, and the surface itself never
holds it. That is what lets the login screen keep its rendered surface
between frames and rebuild it only when its own content changes, so a moving
mouse re-composes a cursor-sized patch of pixels that already exist instead
of repainting the screen. Motion damages the union of the cursor's old and
new rectangles clipped to the screen, so a mouse move never costs a
whole-screen present and never leaves a cursor painted where it no longer is.
An arrow that will not rasterise costs the *drawing* only: the pointer still
moves and still hit-tests, the event is logged, and the screen stays usable
(`AGENTS.md` §2.9).

## Choosing the shape from interaction state

Which `CursorKind` to show is decided from what the user is doing, not hard
coded per window action. The window manager's `select` module
(`userland/gui/wm`) holds that policy:

- `desired_cursor(at, router, compositor)` is a pure function of state. `at`
  is the pointer position, which the desktop's input seat owns: a router holds
  a position only for as long as it holds the pointer, and the shape has to be
  right wherever the pointer is — including over the desktop's own bar, which
  the window manager's router never holds. An
  in-flight grab outranks everything: a window move-grab yields `Move`, and a
  resize-grab keeps the double arrow of the edge it is dragging for the whole
  gesture — the pointer routinely runs past that edge, and re-deriving the
  shape from where it now is would flicker mid-drag. Otherwise a point on a
  decorated window's resize edge yields the double arrow of the axis that edge
  moves along (the two sides share the horizontal arrow, the two corners take
  opposite diagonals), so a grabbable edge announces itself before it is
  pressed. Over a decorated window's title bar, controls or rim it is the
  `Arrow`: the frame is the window manager's, not the application's to
  restyle. Over a window's content the pointer takes the **cursor hint** of
  the top-most window under it; over the desktop background it is the plain
  `Arrow`.
  The resize zone is the frame's own hit map, so it reaches into the client's
  outermost pixels exactly as far as a press on them does — the pointer never
  changes shape somewhere a press would not start a resize, and an undecorated
  window has no resize edges to point at.
- Each window carries a `cursor_hint` (default `Arrow`). An application sets
  its own window's with the `SetCursor { window_id, shape }` window-channel
  request (`WindowClient::set_cursor`), naming one of the content shapes
  `CursorShape` allows — `Arrow`, `Text`, `Pointer`, `Busy`, `Crosshair`;
  the resize and move shapes are the frame's and cannot be asked for. The session checks the
  window is the caller's, sets the hint through
  `Compositor::set_window_cursor`, and refreshes the pointer at once. One
  shape per window: content with regions of different kinds restates it as the
  pointer crosses them. A hint is window state, not pixels, so it marks no
  damage.
- `CursorController` ties the policy to the artwork. It owns the active
  `CursorRegistry` and remembers the kind on screen and the density it was
  rasterised at, but it does **not** own the scale: the desktop density belongs
  to the output, so the controller reads it from `Compositor::scale` when it
  installs a cursor (`AGENTS.md` §10 / §2.2). `refresh(at, router, compositor)`
  runs the policy and
  re-renders only when the chosen kind, the active cursor set, **or** the
  pixel side the output scale and the logical side resolve to changed,
  installing the result in place — at `at`, the seat's pointer
  position, which is also where the hotspot is placed. A runtime cursor-set
  swap is `set_active_set(id, at, compositor)` (or
  `set_registry(registry, at, compositor)` to replace the sets outright),
  neither of which needs a router at all; a DPI change is
  `Compositor::set_scale` followed by one `refresh`, a pointer-size
  change is `set_logical_side(side, at, compositor)`, a shadow is
  `set_shadow`, and a shaken pointer grows through `set_enlargement`.
  Rasterisation can fail
  for a degenerate cursor or side; the
  controller then fails closed, leaving the current pointer untouched rather
  than blanking it (`AGENTS.md` §2.9).
- The controller owns the pointer's **logical** side — the user's own
  accessibility choice — but not the scale, which belongs to the output.
- The controller rasterises each kind at most once per epoch: a
  `tairix_reclaim::ReclaimCache` keyed by `CursorKind` within a
  `CursorEpoch { side, set, shadow }` keeps the converted
  `CursorImage`, so toggling back to a previously-shown kind reuses its image
  and only a change to the side, the set or the shadow re-rasterises (the
  SVG-first "convert once, re-render only on a scale or theme change" rule,
  `AGENTS.md` §10). The epoch carries the *side* rather than the scale and
  the pointer size separately, because an image depends on how many pixels
  across it is, which set it came from and what it casts, and nothing else —
  so two different (scale, size) pairs resolving to one side correctly share
  one cached image. The
  cache is built by `cursor_cache` from the shared
  `tairix_reclaim::desktop::disposable_ui_cache` policy: owned by the seat,
  bounded by a budget derived from the real framebuffer byte size, dropped
  under memory pressure, and wiped on release rather than left to linger in
  reusable heap. It is the same policy the taskbar uses for its notification
  glyphs — one mechanism, not one per asset kind (`AGENTS.md` §2.2). See
  [SVG asset decoding](./svg-assets.md).
