# The WinterSun client shell

`userland/games/wintersun/app` (`tairix-wintersun-app`) is the window a player
looks through and the frame they see in it: the perspective camera, the
software renderer that draws the land in depth, the light composite over it,
the figures standing on the ground, the fixed-tick pacing the render interpolates over, the input
drain, the three window size states, the player's graphics choice and the
settings window it is made in, the degradation ladder `auto` turns when frames
run late, where a session's first body stands, and the one reference scene
every check of the picture draws. It is `plans/WINTERSUN.md` WS5, WS6, WS18's
detail control, WS23, WS25's client half and WS32, and a crate of the
`userland/games/` leaf subtree. Stability tier: **experimental**.

Everything with behaviour is the crate's `[lib]`; the `[[bin]]` is the on-disk
`wintersun.app` bundle's `Run` entry point and only composes it over
`lib/window`'s client half. A freestanding binary is reachable by no host
test, which is how a companion that walked on the spot once survived three
green pipelines.

The graphics store, the settings window and the icon-bar declaration sit
behind the `settings` feature, on by default, so a client-frame vertical links
none of them. The shared controls draw text through the font client, which
links the userland runtime where the vertical brings its own allocator and
panic handler. The store's wipe links `alloc`, and each target's verticals are
built in one invocation whose features unify, so it would impose an allocator
on every allocator-free guest beside them.

## Where floating point is

The library denies `clippy::float_arithmetic` but in four places a
perspective needs reals: the camera's basis (`Camera::perspective`), the
light's per-frame tables (`Atmosphere::new`), and two per-triangle setups in
the terrain pass. Those, `lib/raster`'s perspective and scan converter, and
the figure engine take `f64` basic operations in a fixed order and
`tairix_util::mathf`, never a fused multiply-add, so a frame is the same bits
on every target. The splat and the per-pixel light are integer.

The frame is where four crates meet, and each part agreeing separately does
not say the composition does. `digest::REFERENCE_DIGEST` is folded over two
whole frames with figures standing in them, the art crate's own digest after
the pixels, and asserted by a vertical on each Tier-1 target.

## The camera

A perspective onto the land from a distance and a tilt. The eye looks at a
point a cell above the focus's ground, `Zoom::distance` away: five stops, each
a doubling, from six cells to ninety-six. `Tilt` runs from twelve degrees
below level to straight down, authored at fifty; the heading is north until
WS33 lets the player turn it. The focal length is the render height times
1.072, a fifty-degree field up the screen, so a figure at the focus is drawn
at the zoom's distance over the focal length (`focus_step`) whatever the
window's width.

The focus is clamped into the realm, and the view past its edge is sky. The
eye stands at least two cells above the ground beneath it and, raised, looks
back down at the focus, so the character stays where it was on the screen.
Ground rising *between* eye and character can still hide it until WS33's
camera draws in.

## The frame

Described once, then drawn. `Renderer::describe` resolves everything a frame
draws into a `Frame` — the camera, the meshes nearest first with the skirts
each hangs, the material tiles, the light's tables, every figure posed and
placed — and `execute` draws it reading nothing else, so one description drawn
twice is one picture. The stages are a GPU's: the vertex stage carries each
mesh into view space across `lib/parallel`; each band of rows sets up the
triangles reaching it and scan-converts them depth-tested into the geometry
buffer; the light composite lights each pixel once; the figures are drawn last,
hidden wherever the ground is nearer.

A band is a full-width run of rows, cut finer than the runner is wide. A target
a clip window cuts is refused rather than drawn from the wrong column. Mesh
builds and tile synthesis are timed apart from the passes
(`FrameTimes::warm`).

### The ground

The client holds two kinds of ground (`terrain::HeldGround`): chunks solved in
full within two of the focus's, and far patches — level 0's reading of a chunk
— over every chunk the camera sees within `VIEW_RANGE`, where the fog has all
but closed, solved chunks included, so one given back leaves its far reading
in place. Both are asked of one worker through the shared deferral desk,
nearest first, far patches sixteen to an ask; an ask made while one is in
flight or its answer waits is declined rather than allowed to discard it
(`terrain::ChunkDesk`). What to ask for is `terrain::Holding`'s: a chunk the
generator refuses is never asked for again, ground the heap has no room for is
held back, each kind on its own, until some is given back or the pressure band
moves, and a scan that found nothing is not made again until the view or the
ground held moves. A frame draws only what is held, and the sky shows where
ground has not arrived. Ground beyond reach is given back, and the worn ground
chunks are solved from is one `lib/reclaim` cache the landing search and the
worker share.

Each drawn chunk is a `mesh::TerrainMesh`: vertices at cell centres a stride
apart, the stride chosen so a quad spans at most sixteen pixels at its
distance; each quad cut north-west to south-east, as the relief is read
(`terrain::triangle_height`), and carrying the plan its fragments splat from.
A vertex's normal is the fine surface's at every stride, and the material warp
is a vertex attribute. A frame builds at most `frame::MESH_BUILDS` meshes and
draws one of each other chunk's built meshes meanwhile, from its own source
first and the fewest halvings from the stride wanted. Skirts hang only where a
neighbour drawn from another source or at another stride meets an edge, or
none does, as deep as the mesh's spacing and facing out from the edge; a seam
on ground steeper than one in one can still show a crack until WS38.

Work is culled before it is set up: chunks against the view, vertex rows
wholly behind the eye, and per band every quad beside the band or wholly
behind ground already drawn there, against a coarse depth of each 8-pixel row
segment — whether it is drawn whole, and the farthest depth drawn in it.
Meshes are drawn nearest first and each one's quads outward from the eye, so
the nearest surface takes a pixel before what it hides is shaded. Roads are
decals stamped into the weight field, once per realm.

A fragment interpolates, perspective-correct, its place in the quad, its
height, normal and warp; the splat reads each ground at the mip its footprint
asks for. A steep face reads its materials in the vertical plane it most
nearly faces, the planes dithered into one another about forty-five degrees,
so a cliff's texture keeps its scale rather than streaking down it. What a
fragment leaves in the geometry buffer is its depth (a reversed reciprocal),
albedo, normal, height, and how near its triangle's edge lies.

### The light

One low directional sun, folded each frame into tables (`light::Atmosphere`):
the tint of each normal, the fog of each octave of reciprocal depth, the sky
up each row. A pixel takes its albedo under its normal's tint — relative to
the palette, so a slope facing neither way draws the material's own colour —
then the mist pooling below `MIST_CEILING` and the fog of its distance, which
closes all but a third of the view over 900 cells; the sky shows where nothing
was drawn. The shading saturates at the steepest rise a body may step up.

Silhouettes are resolved here. A pixel whose triangle's edge lies within half a
pixel of a different surface — a depth an eighth apart, or the sky — takes the
neighbour's light for the share of it the neighbour covers.

### The figures

`figures::Cast` is everyone in the scene. A frame poses and places every
figure the camera can see, one to a core, through the scene's own camera, so a
figure grows as it nears; each foot is planted on the surface the frame draws
(`mesh::Surfaces`). Figures are drawn far to near, the entity id breaking a tie,
each strip hidden wherever the ground's depth is nearer than its own plane, so
a hill hides the figure behind it and water the legs of a figure wading. The
contact shadow lies on the slope it darkens, depth-tested by it, and thins as
the water deepens; the mist and fog at a figure's feet veil its every stroke.
A band draws only the figures whose rows reach it.

## The command line

`wintersun` takes no operands. `-h`, `-?` and `--help` print the bundle's own
short help, and win wherever they are reached; `--reference-scene` holds the
reference scene still in place of a new world (below); `--` ends the options.
`--seed SEED` (or `--seed=SEED`) opens the world a decimal seed names, the
last one given standing. Without one a session draws a seed from the random
API and, once its world is generated, leaves a `context` record,
`world.seed_drawn`, on `stdinfo`, carrying the command that reopens the same
world. The reference scene is one fixed
realm, so a seed beside it is refused rather than ignored. Anything else,
read left to right before a help switch, is a usage error with exit status
`2`. A reference scene that cannot be drawn exits `87` with its
reason, since a window holding some other picture would defeat the mode.

## The reference scene

`reference` is one realm, one cast and one moment, drawn by three consumers
that must agree to the bit: the cross-target digest folds it at two fixed
sizes, `Run --reference-scene` holds it in a live window, and the client
vertical draws it on the host at that window's size to check what the guest
scanned out. Everything that decides a pixel is fixed there — the seed, the
cast, the moment, the material cache's size — except the pressure the cache is
taken under, which is the caller's: the digest and the host take it at
normal pressure whatever the machine says, and a live window takes it under
the process's own gauge so it gives memory back like any other cache. A frame
whose cache refused a tile is drawn with that material flat and is not the
reference, so the renderer counts refusals, the digest fails rather than fold
one, and a live window says so on standard error.

The window holding it draws only when its extent changes, on a worker so a
resize never holds its events, and presents its last frame again when the
session asks for the window's picture, so a still scene costs no frames.

## The player

The player walks as a preset record the bundle ships in its own `Resources/`,
read once before the window opens; where it cannot be read the client says
why and walks as the reference figure instead.

A session starts on dry ground with room to walk, at the point of it nearest
the realm's centre (`landfall`). The centre itself may be sea, a lake, a river
bed between banks too steep to climb, or a hollow among cliffs, and a body
placed in any of them wades from its first frame or never gets anywhere. So
the chunks holding the dry coarse samples nearest the centre are solved in
turn, up to sixty-four of them, and each chunk's cells are joined into
walkable stretches by the rules' own step test, taken both ways — a drop is
legal and the climb back is not, so a stretch joined through one would hold
ground a body could reach and never leave. The start is in a stretch of at
least a quarter of its chunk, at its dry cell nearest the centre, with no
water anywhere under the body's footprint. Where no chunk tried holds that
much room, the start is the roomiest found; a realm with no dry ground at all
near its centre is refused with that reason.

The body the rules collide is
as wide as the figure is drawn, so two bodies the simulation lets touch are
drawn touching and never through one another. Each frame the figure is moved
to where the frame shows the body — the same interpolated point the camera
follows — over the real time since it was last posed, and a paused game poses
its figure at the moment it paused.

## Input

Every event already queued is applied before a frame is produced from the
state those events left, so a burst of pointer or key input is one paint
rather than one per event, and a run of resize samples folds to the newest in
the reader.

## Pacing

The simulation steps at a fixed rate and the display refreshes at whatever
rate it has; tying one to the other would make the sim rate a visual property.
The pacer converts elapsed real time into whole ticks, and what is left over
is the fraction a frame reads *between* the last two authoritative states.

The accumulator counts in nanosecond-ticks rather than nanoseconds divided by
a tick, so a rate that does not divide a second evenly — thirty does not —
loses nothing over an hour. Time beyond a quarter of a second is dropped
rather than replayed, so a client descheduled for a second resumes instead of
spending the next second simulating the last one.

A minimized window stops the clock and draws nothing until the compositor
gives it focus or a size again; the frame on screen when it went is the frame
that comes back. The shell models losing the seat the same way, but no seat
notice reaches a window application yet (`plans/WINTERSUN.md` WS6).

Frames fall due on a fixed beat of one display frame (`pacing::Cadence`), at
the rate the desktop composes at (`Desktop::refresh`), counted from when each
was due rather than when the last one finished. A wake between frames — input,
a worker's answer, the settings window — draws nothing, and the time a frame
takes is not added to the wait for the next. The frame budget stays the
baseline's, so a faster display is given more frames where the machine has
them, never less detail.

## Detail

Three knobs decide how a frame looks and what it costs: the shadows, the
ground's texture, and the render scale. A `Detail` is one setting of all
three. The player's choice (`graphics`) is one of four modes:

- **Ultra**, the default on a new install: every knob at its finest, whatever
  the frame rate.
- **Basic**: every knob at its plainest, drawn at the window's own resolution.
- **Custom**: the player's own setting of each knob.
- **Auto**: the governor decides, walking the ladder below.

The choice is the application's own per-app data, reached only through the
app-data service. It is read once before the window opens, and a store that
cannot be read leaves every detail at its finest and says why. It is written
through a worker, never on the frame loop, and only where an interaction
settles; what the store then holds is adopted unless the player has moved on
since, and a refused write is reported and puts the stored choice back. The
store keeps what is in force and nothing else: the mode always, and the three
knobs only while the choice is custom.

## The settings window

The icon-bar slot's menu reads *Info*, *Settings…*, a rule, then *Quit*.
*Settings…* opens a second window on the same channel and event mailbox, with
a category strip down its side — graphics is the category there is — and every
control a shared one. The quality chooser picks the mode; one detented slider
per knob sets it, plainest at the left, and moving any of them makes the
choice custom, starting from the detail on screen, so a player can watch what
`auto` settled on and pin it by touching it. On `auto` the sliders follow the
governor. A render scale coarser than the one that keeps figures readable in
the game's window at its zoom is allowed and says so on its row.

A drag previews on the next frame and writes nothing; the one write is where it
settles, and a chosen mode is one write. The window's picture is retained and
only the rectangles its controls report are repainted and presented. Its size
is fixed when it opens, measured for every row at the longest it can be put, so
nothing the player does pushes a row out of it. A window cannot raise itself,
so while it is open the *Settings…* row is declared disabled with that reason,
and the window is reached through the slot's picker.

## The degradation ladder

`auto` walks a ladder with a fixed and total order, one notch at a time, of
the knobs that cost frame time:

1. shadow softness: each figure's contact shadow hardens to one ellipse, then
   the ground is drawn in its materials' own colours, unlit. A contact shadow
   never goes: it is what says where a figure stands and whether it has left
   the ground.
2. render scale, upscaled to the window

Each rung sheds through its own notches before the next is touched, so two
machines at the same step are drawing the same picture and the step is the one
number a diagnostic has to report. The ground's texture is not on it: its
octaves are spent synthesising a tile once, so shedding one frees no frame
time, and — the octave count being the tile cache's generation token — costs a
re-synthesis of every tile held.

The ladder has a **floor**: the deepest step whose frame still draws the
smallest figure a record describes at the art harness's own readability floor,
for the window's size and the zoom the player chose. Only the render scale
makes a figure smaller, so the floor falls in its rung; where the zoom already
draws figures below that size, the player's choice has made the call and the
render scale does not move. The governor holds the floor before every frame,
so a zoom out or a smaller window takes the ladder back at once, and it never
sheds past it. Overrunning at the floor — or at the ladder's end — is
reported once as it happens: the frame rate is what gives way.

## The governor

The governor reads the machine over seconds, not frames:

- **Cost is per render pixel**, so a window that grows or shrinks moves the
  frame's predicted cost at once, and a larger window does not read as a
  slower machine.
- **Each step remembers its cheapest frame**: what the machine draws there
  when nothing else wants it. Frames now against that best is how busy the
  machine is, smoothed over six seconds. A step entered for the first time
  starts from the best of the step above it, so one first drawn while the
  machine is busy does not take the busy frames for its best.
- **It sheds one notch at a time, on a frame that itself overran**, once frames
  smoothed over a second and a half have overrun for a second where even the
  step's best frame would not fit, and for six where it would — the machine
  has shown it can draw this, and something else is slowing it for now.
- **It gives one back on a prediction**: the finer step's best, scaled by how
  busy the machine is and by its pixels, must fit in four fifths of the
  budget for four seconds. The notch is then on trial for three, and one that
  overruns in its trial goes again after one. A step's best is believed less
  the longer ago it was seen, so one found too dear is tried again only once
  the evidence against it has aged.
- **Frames after a move or a resize are not counted** for half a second and
  four frames: they pay for buffers and textures once. Nor is the time a frame
  spent building meshes and synthesising tiles, which `FrameTimes::warm`
  records apart from the passes. Every dwell is counted in time frames were drawn in, not time that
  passed, so a paused window was neither overrunning nor comfortable.

## The window

Three size states over the window channel: restored, maximised, and exclusive
fullscreen. The client *asks* and the compositor *answers*; nothing assumes a
request took effect. The state rides on the resize event alongside the extent
precisely so the two cannot be believed separately — an app that learnt it was
fullscreen before its extent would lay out edge-to-edge at the old size.

Leaving fullscreen returns to the state it was entered from, so a maximised
window comes back maximised.

The window opens at the baseline resolution at the desktop's scale, capped to
the screen by the one rule every application's window size goes through, so
on a screen smaller than the baseline the window still fits and its title
bar's controls stay reachable. A frame the renderer refuses is withheld, so the
window keeps showing the last frame drawn rather than a half-painted one, and a
run of refusals is reported once.

## The frame budget

The baseline is 1280×720 on a four-core machine, with the per-pass allocation
`plans/WINTERSUN.md` states — which, from the 3D view on, the GPU path holds;
the software path is held to being as fast as the CPU allows. `tests/budget.rs`
measures it rather than asserting about it: a long view at the lowest tilt
across the most broken ground of a 64-chunk realm, the plan's sixty-four rigged
figures standing in it, on one thread and on four, printing what each pass cost
and what placing one figure costs a single core. The plan records those numbers
as the baseline a regression is judged against. No elapsed time is asserted: a
wall-clock bound is a claim about the machine, so what the test gates is that
real threads draw the picture one thread does.

The material cache is a share of the machine's memory, asked of the System
Information API once before the window opens, and obeys the band the kernel
publishes to the process. A system that will not say how much memory it has
admits no tiles, and the ground is drawn in its materials' flat tones.

The headroom is *derived* from the frame and the named passes rather than
stated beside them: two numbers that must add up are one number and a
subtraction.

## Tests

The host suite covers the camera, the detail knobs, the ladder and its floor,
the governor against simulated machines — a moment of other work, a busy
machine that has shown it can keep up, detail too dear for the machine, a
larger window, a pause, and a restore that does not hold — the stored choice
over the shared fake app-data service, the settings window's drag, detents,
chooser, layout and scoped repaint, the icon-bar declaration, the start search
over synthetic hollows and river beds, the render target and its bands, the
held ground and what a view asks for, the meshes and their seams, the terrain
pass — a ridge hiding the ground behind it, the eye's own ground clipped
rather than dropped, the same picture however the rows are cut — the light
model and its silhouettes, the cast and the figure pass, a frame drawn twice
from one description, the pacing, the input mapping and the size-state model.
The start search over the realms the client itself opens runs with the scene
tests (`--features scene-tests`), as each generates a realm. `tests/bands.rs`
asserts that cutting a peopled frame for any number of threads produces the
identical picture. `tests/proptest_model.rs` is the invariant model over
generated window, input, camera and frame-cost programs — it is what found the
camera's resize hole and the governor counting a pause as overrun time, and it
holds the ladder above its floor and to one notch a frame. `tests/budget.rs` is
the measurement.

The cross-target rendering claim is four verticals —
`client_frame_qemu_{aarch64,riscv64,x86_64}` and `client_frame_wasm32` — each
folding two composited frames into `digest::REFERENCE_DIGEST`. They are also
what first builds the ground art and the figures for each Tier-1 target.

`wintersun_client_qemu_aarch64` is the end-to-end claim: the installed bundle
is launched by name from a terminal on the reference scene, and its window is
read back as it opened, fullscreen, restored and maximised. Each dump waits on
the session's witness that the window is on screen at that state's extent, and
is compared pixel for pixel with the scene drawn on the host at the extent the
window manager gives it, the cursor and the window's rounded corners aside.
The witness also names how the frame reached the display, which on this
software-composited board must be `composited`.
