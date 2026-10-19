# tairix-wintersun-app

WinterSun's client shell — the window a player looks through and the frame they
see in it: the land drawn in depth through a perspective camera, with the
figures standing on it (`plans/WINTERSUN.md` WS5, WS6, WS32).

**Stability tier:** `experimental`.

The crate's `[lib]` holds everything with behaviour worth testing: the command
line and the record a drawn seed leaves (`cli`), the perspective camera and
its zoom stops and tilt (`camera`), the detail knobs, the degradation ladder
and its readability floor (`quality`), the player's graphics choice and how it
is kept (`graphics`), the settings window and the icon-bar declaration that
reaches it (`settings`, `appbar`; these three behind the default `settings`
feature), where a session's first body stands (`landfall`), the render target
and its bands (`view`), the ground the client holds and what a view asks for
(`terrain`), the chunk meshes and their skirts (`mesh`), the terrain pass into
the geometry buffer (`raster`), the sun, the air and the light composite
(`light`), the cast and the figure pass (`figures`), the frame described once
and then drawn (`frame`), the preset the player walks as (`presets`), the
fixed-tick clock and its interpolation (`pacing`), the input drain (`input`),
the window size states (`shell`), the per-pass budget and the governor that
turns the ladder from it (`budget`), and the cross-target frame digest
(`digest`).

The `[[bin]]` is the on-disk `wintersun.app` bundle's `Run` entry point,
composing the library over `lib/window`'s client half. It is a freestanding
pure-Rust program on the Tier-1 bare-metal targets and an inert host stub
elsewhere, so nothing with behaviour lives in it. It reads the player's preset
from the bundle's own `Resources/` once, before the window opens, which is
what its `CAP_FS_ACCESS` is for, and reads and writes the graphics choice in
its own per-app data through the app-data service, which asks no capability.

The library denies `clippy::float_arithmetic` but where a perspective needs
reals — the camera's basis, the light's per-frame tables, two per-triangle
setups — and those take `f64` basic operations in a fixed order and
`tairix_util::mathf`, so a frame is the same bits on every target.
`digest::REFERENCE_DIGEST` is the record of the whole composition — two frames
with figures standing in them — and folds in
`tairix_wintersun_art::digest::REFERENCE_DIGEST` so a change to the ground
moves it too.
