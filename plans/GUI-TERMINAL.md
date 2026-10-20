# GUI-TERMINAL — the first-class graphical terminal

Binding under `AGENTS.md`. This plan owns everything about `terminal.app` as a
*graphical* program: how large it opens, that it is **one process with many
windows** and what that costs, its icon-bar presence, the user profile behind
it, the window menu it asks the desktop for, the settings sheet, the colour
schemes, and the
screen effects. The emulator itself — the VT parser, the grid, the pty seam —
belongs to `plans/APPWIN.md` AW4, `plans/PTY.md`, and `plans/CURSES.md`;
nothing here restates them.

| # | Item | Status |
|---|---|---|
| T1 | The size the terminal opens at (§1) | done |
| T2 | The user profile (§2) | done |
| T3 | Colour schemes (§3) | done |
| T4 | Screen effects: the strengths and the passes they resolve to (§4) | done |
| T4.1 | The effect pipeline: retained stages redrawn only over what changed, the phosphor stepped by the clock and settling, a divide-free fuzz, rows spread across a pool started on first use (§4) | done |
| T4.2 | Scan lines dim a row's colour and leave its coverage (§4) | done |
| T5 | The window menu (§5) | done |
| T6 | The settings sheet (§6) | done |
| T7 | Compositor backdrop blur (§7) | done |
| T8 | The window fills its frame (§8) | done |
| T9 | The settings sheet is its own surface (§9) | done |
| T10 | Command output reaches the screen (§10) | done |
| T11 | Screen semantics (§11) | done |
| T12 | A repaint costs what changed (§12) | done |
| T13 | A closed window's shell is reaped (§13) | done |
| T14 | One process, many windows (§14) | done |
| T15 | Its icon-bar presence (§15) | done |

Read alongside: `plans/GUI-CONTROLS-DESIGN.md` (the Reactive Alloy control
language every surface here is built from), `plans/APPWIN.md` (the window
channel), `plans/GPU.md` (the programmable path the effect passes would need to
leave the CPU).

---

## 1. Why the terminal opens at the size it does

A terminal's natural size is a character count, not a pixel count. The window
is therefore whatever the conventional 80×25 screen measures in the face
actually being drawn with, and the *text size* is what gives when a display
cannot hold it — never the grid. A terminal that quietly dropped to 60 columns
would break every program that lays itself out for 80.

The rules live in `userland/apps/terminal/src/layout.rs`:

| Rule | Definition |
|---|---|
| The grid it opens on | `COLS` × `ROWS` = 80 × 25 |
| What a grid measures | `grid_size` — the face's own advance and line height |
| What a client holds | `grid_dims` — floored, at least 1×1, capped at `MAX_DIMENSION` |
| What the furniture costs | `chrome_extent` — the shared `WindowFrame::insets`, never a second copy of the frame arithmetic |
| The size that fits | `fit_font_size` — the largest logical size ≤ the profile's whose grid plus furniture fits the screen, down to `MIN_FONT_SIZE_PX` |
| The window it asks for | `window_size` — the grid, clamped to the screen less the furniture |

The default text size is **14 logical pixels** (`profile::DEFAULT_FONT_SIZE_PX`).
On a 640×480 display at 100% the shared monospace face advances 7 physical
pixels per column at that height, so the grid is 560×350 and the framed window
562×380 — inside the screen with room left for the taskbar. A denser display
multiplies through the desktop scale; a smaller one steps the size down.

There is deliberately **no compile-time window size**, and nothing may
reintroduce one: the face's metrics come from the font service at runtime and
can differ from the compiled-in console-atlas fallback, so only a runtime
measurement is honest. Anything that needs to know where a terminal's client
sits inside its decorated window — the desktop's own pointer vertical among
them — reads `chrome_insets` rather than predicting an extent.

---

## 2. The user profile

One [`Profile`](../userland/apps/terminal/src/profile.rs) holds everything a
user can change: the colour scheme, their own custom scheme's colours, the
text size, and the strength of every screen effect. It lives in the OS
app-data store, reached through `lib/appdata`, so it is **private to this
application**: the store is gated on the bundle identity the kernel attests
and no other app the user launches can read or rewrite it
(`plans/APPDATA.md`). It was a file this app wrote under the user's home until
AD5 migrated it.

- Closed key registry (`ProfileKey`) of dotted keys in the store's shared
  `key = value` format. A save writes only what the store's layers do not
  already imply, and *Restore defaults* removes the user's opinions rather
  than freezing today's values.
- A key no layer sets means its documented default, silently. A stored value
  the registry refuses costs only itself — that one field stays at its default
  and the key is named on `stderr` — and a store the service cannot serve
  leaves the bundle's shipped defaults standing, also said on `stderr`.
- Reached through the app-data service under this bundle's kernel-attested
  identity. The app writes no file of its own and holds no new capability;
  `CAP_APPDATA_ADMIN` is the service's, never an app's.
- Colours are bare `rrggbb`, never `#rrggbb`: the grammar's comment marker
  would cut the line at the `#`.
- **The sheet's controls are not wired to the store** (`AGENTS.md` §28). The
  profile the windows *render* and the profile the store *holds* are separate
  (`tairix_appdata::Publication`): a slider drag previews every sample and writes
  nothing, and only the settle — the released drag, the chosen scheme, the key
  step, the sheet closing — asks for a write. Wiring the value to the write
  cost one store round trip and one disk commit per pointer-motion sample, and
  the window was frozen for each of them.
- **The write happens on a worker, and its answer is what applies** — to
  every setting the user has not changed since that write was asked for. A
  settled edit is submitted to the settings worker, which publishes and answers
  with the profile the store then implies, so a machine policy or a shipped
  default the user's document does not override still wins over what the
  widget asked for, and *Restore defaults* needs no second mechanism. A setting
  edited since the write was asked for keeps its value when the answer lands,
  so an answer arriving mid-drag never moves the slider under the pointer, and
  that setting's own settle writes it. A refused write reverts the settings it
  carried and says why, so no window keeps showing a look the next start would
  not restore.
- **One write is outstanding at a time** (`Publication`). A settle or a restore
  asked for while one is outstanding is owed until it answers, restore first,
  so every answer describes the only write in flight, a restore is never
  displaced by a later save, and no save writes back values a restore is about
  to remove. Any number of settled edits during one write cost one further
  write. With no worker granted, the write happens on the loop exactly as it
  used to — slower under load, never wrong, and stated once.

Defaults: system scheme, 14 px text, 80% opacity, the backdrop blurred at half
strength, and the four pass effects off. Translucency is free (it is the alpha
the background is filled at, and it is not a pass), so the shipped look keeps
the cell-diff present; the blur is what makes the window read as frosted glass
rather than as a hole. A screenful of frosted terminals is affordable because
the compositor rations *retention*, never the blur: it retains frosts from the
front of the stack until the budget runs out, and a terminal buried beneath
that is still frosted, recomputing only what each frame uncovers of its frost
(`plans/FIX-DESKTOP-SPEEDUP.md` D.13). A window piled on by others therefore
keeps its look; what it gives up is only the retained copy that made
repainting it cheap.

---

## 3. Colour schemes

`userland/apps/terminal/src/scheme.rs` is the one place a terminal colour comes
from. A `ColorScheme` is the sixteen ANSI slots plus background, foreground,
cursor, and cursor text. `Painted` resolves the scheme in force once per
repaint (never per cell) and answers every cell's foreground/background.

Shipped: **System** (follows the desktop theme, the default), **Midnight**,
**Phosphor**, **Amber**, **Ember**, **Contrast**, **Paper**, and **Custom** —
the user's own, editable in the settings sheet and persisted with the rest of
the profile. Adding a scheme is a variant plus a palette; the compiler forces
every consumer to state what it means.

The hard-coded xterm ANSI table that used to live in the renderer is now the
default scheme's palette — one definition.

---

## 4. Screen effects

`userland/apps/terminal/src/effects.rs` carries one strength per effect, each a
permille `0..=1000` a slider sets directly, and turns them into the ordered
`Pass` list a frame goes through. A zero-strength effect contributes no pass,
so a terminal with the effects off pays nothing.

| Effect | Where it happens | How |
|---|---|---|
| Translucency | While the cells are painted | The default background is filled at `background_alpha`, so the compositor's own premultiplied blend does the work and a glyph stays opaque over it. Never below `MIN_OPACITY`. |
| Backdrop blur | The compositor | Only the compositor can see behind a window, so the strength becomes a logical radius (`blur_radius_px`, capped at `MAX_BLUR_RADIUS_PX`) and is handed to the window channel's `set_backdrop_blur`. Zero when the window is opaque — a blur nobody can see is wasted work. The compositor rations how many stacked windows it frosts, so a deeply buried terminal shows the translucency without the frost. |
| Scan lines | `Pass::ScanLines` | Dims the colour of alternate physical rows and leaves their coverage, so an opaque terminal stays opaque. Static. |
| Glow | `Pass::Glow` | Halation: the light of brightly-driven pixels spread into their neighbourhood and added back. Static. |
| Fuzz | `Pass::Fuzz` | Per-pixel luminance jitter, one 64-bit draw per eight pixels scaled by a multiply and a shift, moving each animation step. |
| Phosphor | `Pass::Phosphor` | A trail of what was lit recently, decaying once per animation step and added back in the pixel's own hue. |
| Wobble | `Pass::Wobble` | Per-row horizontal displacement along a travelling integer sine. |

**The glow is the spatial half of a tube's light, phosphor the temporal
half.** It runs *after* wobble, phosphor, and the scan lines, because it
spreads the light those settled on — so its halo reaches into the scan lines'
dark rows as a real tube's does — and before fuzz, so the noise floor reads as
noise rather than being smeared into the halo. Three decisions carry it:

- **Drive is the peak channel, never a luma weighting.** A phosphor driven to
  full emits as much light whatever its colour; weighted, saturated red would
  not clear the knee at all where green sailed over it, and red text would
  carry no halo.
- **`GLOW_KNEE` (half scale) is what makes it light off the text** rather than
  a flat wash: an unlit or mid-grey background emits nothing. The reach is a
  density-scaled constant (`GLOW_REACH_PX`), because halation is a fixed
  distance through the glass — the slider sets how bright the halo is, not how
  wide.
- **`MAX_GLOW_INTENSITY` is a gain above unity, and still capped for the
  reason `MIN_OPACITY` is:** at full strength the halo is brighter than the
  light it was spread from, so a lit glyph blooms instead of merely softening
  — the intensity is applied as a saturating per-channel multiply (`gain`),
  because scaling through an 8-bit alpha would silently clamp every intensity
  above unity at exactly the light spread. The cap remains because a slider
  must not be able to make text unreadable: a light scheme's background
  clears the knee on its own, so at full strength the whole field glows, and
  the cap is what keeps dark glyphs standing off it.

The spread is the workspace's one separable box blur (`tairix_raster::box_blur`)
run twice, at radii summing to the reach, so the falloff is a tent rather than
a box with a visible edge — there is no second blur implementation. Both
working buffers are held at the window's size while the glow is on, so a frame
allocates nothing; a window that cannot afford them is drawn without effects.

**The passes are drawn by a retained pipeline, redrawn over what changed.**
`pipeline::Pipeline` holds three stages by what an output pixel depends on:
`pre` (wobble, phosphor, scan lines — one screen pixel and its row), `lit`
(glow — the neighbourhood of `pre` its reach spans) and `out` (fuzz — one pixel
of `lit` and the step). A stage exists only while one of its passes is on, and
a render redraws each only where its input changed: the screen's damage carried
along by each row's wobble, the rows a wobble step moved, the pixels whose
trail moved, then the glow's reach around all of that. A keystroke under glow
costs its cells and the glow's reach round them (five pixels at the reference
density), never two whole-window blurs and a whole-window present; an
unchanging screen costs nothing. The retained screen
stays clean, so the next cell diff still describes the text.

- **Exact, and proven so.** A pixel's glow depends on `pre` within the reach of
  it and nowhere else, so a rectangle blurred from its reach-grown
  neighbourhood — clipped where the whole-screen blur also replicates its edge —
  writes exactly what the whole-screen blur would. The tests drive a scripted
  session through one pipeline told only what changed and one told everything
  did, and require the pictures byte-identical after every step and every
  changed pixel reported.
- **The phosphor decays in time, not per present, and settles.** The trail
  steps once per animation step; a present between steps shows the screen's
  new light against the trail as it stands. A pixel whose trail is the level
  its own light holds it at is settled, and once every pixel is, the pipeline
  stops asking for steps: a static screen with phosphor on parks.
- **Moving passes move; the rest do not.** Wobble and fuzz draw differently at
  every step whatever the screen shows; the window asks for a timed step only
  while one is on or a trail has not settled (`Pipeline::animated`).
- **Turning the last pass off presents the plain screen whole**
  (`Pipeline::retire`): the effected picture stands on screen until it is
  replaced, and the cell diff alone would leave it there.
- **Rows spread across a pool started on first use.** The `pre` and `out`
  stages split their rows across `lib/parallel` bands; the pool is created the
  first time a pass draws, so a terminal with the effects off holds no worker
  threads, and the CPU count it is sized from is asked once at start-up, never
  from the loop that owes the windows their frames.
- **One `clear()` forgets everything** held under a screen that has stopped
  existing — a resize, new colours or a new face, memory pressure — which is
  what releases the stages' buffers.

**Animation is a clock, never a spin.** Every moving pass is a pure function
of a monotonically increasing `Phase`. The program's wait-set park carries a
one-shot frame deadline (`frame_interval_ns`: whole frames of the desktop's
rate, at most 20 a second) only while some window
animates; otherwise it parks indefinitely.

The settings sheet is drawn **after** the effects: a
settings sheet that wobbled with the screen behind it would be unusable, and
its controls must read exactly as they do everywhere else on the desktop.

---

## 5. The window menu

The terminal is a client of the desktop's one menu service
(`plans/NEW-MENUS.md` M3.1); it keeps no menu shell of its own.

`userland/apps/terminal/src/menu.rs` builds the **row model** and reads a
chosen row back. Six typed commands from one ordered `Command::ALL` list, read
back through that same list by `Command::from_item`, so a reordering cannot
re-map a row:

| Row | Shortcut |
|---|---|
| Settings… | `Ctrl ,` |
| Larger text | `Ctrl +` |
| Smaller text | `Ctrl -` |
| Actual size | `Ctrl 0` |
| Clear screen | `Ctrl Shift K` |
| Close | `Ctrl Shift W` |

Every advertised shortcut is really honoured by `Command::accelerator`; a row
never shows a key combination that does nothing — checked by parsing each row's
own caption out of the built model rather than restating it beside the matcher.
Only combinations a shell would not otherwise receive as a control byte are
claimed, so intercepting one never swallows input a program was waiting for.

**The desktop owns every pixel.** A secondary press sends `OpenMenu` with the
model and the window-local point the press was reported at — the only space the
terminal can speak truthfully, since it is never told where its window sits —
and the session titles, places, draws, grabs, routes and dismisses. The answer
is one `MenuClosed` naming the session-minted open id, matched against the id
this window is waiting on so an answer to a settled gesture can never run a
stale command. The terminal never learns a pointer position inside the plate
and cannot hold one open.

**A refused menu is an answer, not a death.** The refusal is reported on
`stderr` and the terminal carries on with no menu; it never falls back to
drawing its own.

Because the plate is the desktop's, it draws in the desktop's interface face
and cannot turn monospace or grow with the user's terminal text-size setting.
The settings sheet below keeps that property through the shared controls, which
accept no typeface from the app.

---

## 6. The settings sheet

`userland/apps/terminal/src/settings.rs` — a modal sheet composed from the
shared Reactive Alloy controls (`Panel`, `Tabs`, `Slider`, `Radio`, `Button`,
`SwatchGrid`, `ColourPicker`), on its own popup surface.

- **Appearance**: the scheme chooser, the text-size slider, and the custom
  scheme's editor — a `SwatchGrid` of the twenty editable colours and a
  `ColourPicker` on the selected well, captioned with its name. The picker's
  samples restyle the windows live and its settle is the write, so a drag is
  one write; changing the well settles what the picker held first.
- **Effects**: one labelled slider per effect — opacity, backdrop blur, scan
  lines, glow, fuzz, phosphor, wobble — built from and read back through the
  one ordered `EffectKey::ALL` list, so a reordering cannot redirect a slider
  onto another effect's field.
- Footer: *Restore defaults* and *Done*.
- **The body scrolls in pixels** through the shared `ScrollView`: rows are
  laid out whole from the body's own top, a row the body's edge crosses is
  drawn cut by that edge, and only the part that shows takes the pointer — a
  pointer over the footer reaches the footer, never the hidden part of the row
  above it. The wheel over the body or its bar scrolls it by `WHEEL_STEP` a
  detent, carrying what a turn leaves short of a pixel, and moves no keyboard
  focus; a press on the bar is what focuses it. A key on a row scrolls the
  least that shows that row. Rows that move or change beneath a resting
  pointer are handed it again, so the hover follows the row now under it.

Every edit clamps through `Profile::clamp`, so the sheet can never produce an
invalid profile. The program applies only the settings an edit changed
(`Publication::edit`, over the profile's `Live` per-key bridge), so
a sheet whose copy has fallen behind — another window's, or one open while the
menu changed the size — cannot put stale values back, and every open sheet
follows the live profile (`Settings::adopt`), reporting the rows that moved and
leaving a drag, the selected well and the focus where they are. A change
re-derives the colours and the face, re-applies the backdrop blur, reshapes the
grid (the pty follows), and writes the document once it settles.

---

## 7. Compositor backdrop blur

Translucency alone composites a window straight over whatever is behind it.
Backdrop blur makes a translucent terminal read as frosted glass, and only the
compositor can do it.

- `WindowRequest::SetBackdropBlur { window_id, radius_px }` in `lib/abi`, the
  radius in logical pixels, `0` disabling, bounded by
  `WINDOW_BACKDROP_BLUR_MAX_PX`. Validated and owner-checked server-side;
  fails closed.
- `WindowClient::set_backdrop_blur` on the app side.
- The compositor blurs the back buffer inside the window's rectangle — which
  already holds everything behind it, compositing being back-to-front —
  weighted by the window's rounded-corner coverage, then blends the window
  over it. A separable two-pass box blur with running sums, so the cost is
  O(area) and not O(area × radius).
- Damage touching a blurred window widens to that window's whole rectangle, to
  a fixed point, so a change *behind* a blurred window cannot leave stale
  pixels.
- The hardware layer path cannot express a backdrop blur, so a frame
  containing a blurred window falls back to the software composite rather than
  presenting a wrong frame.
- Retention is rationed from the front of the stack, so a pile of blurred
  terminals retains at most the machine's frost ceiling rather than one frost
  per window, however deep, and a terminal it does not reach is frosted all the
  same, recomputing only what each frame needs of its frost
  (`plans/FIX-DESKTOP-SPEEDUP.md` D.13). That is what lets the blur ship on by
  default: a cell repaint in the front of sixteen cascaded terminals blurs
  nothing.

---

## 8. The window fills its frame

Two separate things used to leave dead space around the screen, and both are
gone.

- **The window manager's grab band.** A resizable window used to reserve the
  theme's `resize_grabber_extent` — 16 physical pixels at 100% — on its left,
  right and bottom, and inset the client inside it, so every resizable app
  showed a visible border of frame plate. The band is now the thin frame rim,
  the same as a fixed-size window, and the resize *hit* zone overlaps the
  client's outer `hit_slop` pixels instead: an invisible resize edge, as every
  mainstream desktop uses. Being resizable now costs an app no screen space.
  The trade-off is deliberate and documented: a press in that outer sliver
  resizes rather than reaching the app.
- **The partial cell.** A character grid can only show whole cells, so a
  freely-dragged client left up to one cell of background along the right and
  bottom that the terminal could never draw in. On a settled resize the
  terminal now snaps its client to a whole number of cells
  (`layout::snap_to_cells`) and re-maps at that size, so the frame shrinks to
  fit the grid exactly. Snapping is idempotent, so the re-map converges in one
  step and cannot oscillate.

---

## 9. The settings sheet is its own surface

The settings sheet is a **popup window**, not pixels drawn inside the
terminal's own surface, so shrinking the terminal no longer clips it: it opens
at its own preferred size. The window menu is not a popup at all — it is the
desktop's chain (§5).

`WindowRequest::CreatePopup` is the protocol addition — an undecorated,
parent-anchored, app-positioned surface any app may open (offsets are relative
to the parent's client origin, because an app is never told its own window's
screen position; the session resolves and clamps the absolute placement).
A popup counts against the same per-client window budget as any other window,
is never listed on the taskbar, is kept directly above its parent every frame,
and is torn down with its parent. A refused popup is reported on `stderr` and
simply not shown — never a crash, and never a fallback to a clipped in-window
draw.

---

## 10. Why command output used to be invisible

Worth recording because the symptom pointed away from the cause: the terminal
opened, the prompt appeared and typing echoed, but a command's output never
arrived. Nothing was wrong with the terminal, the pty readiness, or the wake
path — the shell's *children* were being handed a closed stdout.

`apply_attach_wires` resolved an inherited standard descriptor only from the
parent's console descriptor table, never from the parent's open entries. A
pty-hosted shell's fd 0/1/2 are pty-slave *entries* and its console slots are
closed, so a spawned command inherited three closed slots: `ls` ran and every
write failed. The prompt and the echo still worked because those are the
shell's own, correctly-wired streams. An inherited base now clones the
parent's open entry behind the slot; an explicitly selected console still
cannot reach the parent's pty, so no authority is widened.

---

## 11. Screen semantics

Two screens in the tree apply the one `tairix_vt::Op` vocabulary to a
character grid — the emulator's `Grid` and the framebuffer boot console
(`lib/fbcon`) — and a program must land in the same cells on both. The
agreement is written once, as the shared `tairix_vt::conformance` script, and
each screen implements its `ScreenModel` in its own tests and runs `check`, so
altering one screen's semantics fails the other's test too. What it guarantees:

- **The wrap is owed, not taken.** Filling the last column leaves the cursor
  resting on that column with the wrap owed; the next printable glyph pays it,
  and anything that moves the cursor or erases first cancels it. Taking the
  wrap eagerly would line-feed — and on the bottom margin scroll the whole
  screen — the moment a full-screen program painted a full-width status bar,
  after which its incremental repaints land a row out. The cursor column
  therefore always addresses a real cell.
- **`DECSTBM` homes into the region**, not to the screen's top-left, so a
  program that reserves a header above its scrolling body starts inside the
  body.
- The script also pins the rubout at the right edge, erase-to-end-of-line from
  the owed wrap, tab stops, wide-glyph wrap-whole, scroll-region confinement,
  cursor clamping, the alternate screen, and save/restore.

---

## 12. A repaint costs what changed, not a window

`render::Screen` owns the window-sized premultiplied surface the grid is
drawn into and **keeps it, and the cells it was painted from, between
frames**. `Screen::paint` compares the live `Grid` against that snapshot,
redraws only the block of cells that differs, and returns it as the surface
rectangle to present. One `write_frame` — shared with the popup path —
copies just that rectangle into the shared frame region, and that rectangle
is the `DamageRect` the window channel carries, so the session converts and
the compositor recomposites the same few cells.

Before this, every present allocated a window-sized surface, re-drew every
cell in it, un-premultiplied every pixel into the frame, and declared
whole-window damage — for one keystroke. On a Pi 4B that is the ~30 ms
between pressing a key and seeing it, and it grew to ~80 ms with a
translucent background, because an alpha below 255 takes `unpremultiply`'s
three-divide path for every pixel and the compositor's blend path for every
row. The cliff is invisible: an opacity a hair under full looks opaque yet
costs the same as a fully see-through window, which is why turning
translucency "off" appeared to leave the terminal permanently slower. Scoping
the work to the damage removes the cliff rather than hiding it by snapping
the slider.

**What the diff may assume, and what it may not.** Two equal cells paint
identically *only* under the same colours and the same face, so a theme or
scale change calls `Screen::invalidate`, and so does a session redraw request.
A *profile* change invalidates only when it moved one of those two — new
colours or a new size. A backdrop blur the compositor draws behind the window,
or a post-processing pass over the finished frame, leaves every retained cell
still true, and discarding them for it would make each sample of those sliders
cost a whole screen (`plans/FIX-DESKTOP.md` DESK-13). A resize needs no such call: `present_frame` reconciles the
picture to the `DisplayMode` describing the frame region, so a surface and a
region of different shapes cannot arise however a resize half-fails, and
reshaping invalidates implicitly. The cursor block is tracked beside the
cells, so a cursor that moves damages the cell it left and the one it
entered. A damaged block is widened to whole glyphs, so clobbering a wide
glyph's continuation cell repaints its lead cell.

**Both directions are tested, not audited.** A scripted walk — typing,
cursor moves, scrolling, rendition changes, erases, cursor hide/show, and
wide-glyph clobbering — asserts after every step that every pixel the
repaint changed lies inside the rectangle it reported, *and* that the
retained surface is byte-identical to a fresh whole-window paint of the same
grid. The tight direction is pinned separately, or the suite would pass by
reporting everything: a keystroke reports exactly two cells, a cursor reveal
exactly one, and an unchanged grid reports nothing and presents nothing.

**The sheet's own picture is retained the same way** (`sheet::SheetScreen`).
The popup was re-derived from nothing per pointer sample: a sheet-sized
surface allocated afresh, every tab, row, label and swatch re-rendered, and the
whole popup presented. So a drag of the transparency or blur slider cost the
whole sheet several dozen times a second, on top of the whole-window
recomposite those two fields legitimately ask the compositor for, and the knob
lagged the pointer. `SheetScreen::paint` clips the render to what was reported
and answers that rectangle for `write_frame` and the present; `invalidate`
covers the sheet for a change no control could have reported — a re-theme, a
new scale, or a frame region the session took back, which holds none of the
pixels a partial present would leave standing. A profile adopted from the store
or another window reports the rows it moved. A scoped paint is asserted
byte-identical to the same band of a whole one, and the effect sliders' own
reports are asserted to be a small part of the sheet, so neither half can pass
by covering everything.

**A control's own report is not the whole scope, and the sheet reports the
rest.** The sheet composes state *above* its controls, and a scoped paint shows
the old state for anything it does not name — the tab strip reports only the
two plates whose selection changed, so a sheet that painted just that left
every Appearance row standing under the Effects tab until an unrelated hover
happened to redraw it. `settings::Layout` resolves where each part is drawn
**once** per routing pass and every report reads that one resolution, so a
reported rectangle is always the one the control was hit-tested and drawn in.
Four kinds of change are the sheet's rather than a control's:

| The sheet's change | Its scope |
|---|---|
| A switched tab replaces every row and re-clamps the bar | the body and scrollbar bands |
| A value written back into a control is also spelled out in the label beside it (`Text size 14px`, `Blur 40%`) | that whole row |
| A scroll moves every row the body shows | the body band |
| A mark of the sheet's own moves — keyboard focus, the scheme dot, the well the picker edits | the elements it moves between, through `damage::move_mark` |

A mark a *container* draws on its own children stays the container's to report,
because only it can name them: the strip's keyboard cursor (`Tabs::set_current`)
and the colour grid's selected well (`SwatchGrid::on_pointer`/`on_key`). The
grid's non-reporting `adopt_selected` is for a caller rebuilding it and
presenting whole, never for an interactive move.

Both halves are tested. Each report is pinned by a test that fails without it,
and a press-drag-release at every node of a lattice over the whole sheet
asserts the retained picture is byte-identical to a whole repaint of the sheet
that gesture left — the one property that catches a change nothing named.

---

## 13. What remains

Nothing in the other sections. Recognised later work, none of it blocking:

- **Effects off the CPU.** Every pass is a per-pixel program, which no
  scan-out engine runs: hardware layers place and blend finished pictures.
  Moving them off the CPU is a compute pipeline over `lib/gpu`
  (`plans/GPU.md` GP3/GP4), with the passes here as its conformance oracle.
- **Scrollback.** The emulator keeps none and reports no mouse input to the
  program, so the wheel over the screen has nothing to move — a correct,
  complete answer today, but a scrollback buffer would give the wheel and a
  scrollbar something to do.
- **Selection and clipboard.** There is no system clipboard yet; when one
  exists the terminal gains select/copy/paste and the model gains its rows.
- **A profile per window.** Today one document serves every terminal window,
  and a change in one window's sheet reaches them all — which is right for a
  *user's* profile. Named profiles a user could switch a single window to
  would be a registry of documents under the same store directory.

**A closed window's shell is reaped.** Closing a window drops the pty master,
so the shell sees end-of-file and exits; `close` hands back the running shell's
`(pid, child_token)` as an `EndingShell`, the loop keeps it in `EndingShells`,
and the child wake that follows reaps it and deletes the member — event-driven,
with no poll and no wait on the close path, so opening and closing windows
leaves no zombie behind.

---

## 14. One process, many windows

A terminal window is not an application: the application is the emulator, and
each window is one hosted shell. `terminal.app` is therefore **one process
with a `Vec` of windows**, each carrying its own pseudo-terminal, shell child,
screen model, retained picture, `Look`, and overlay, over **one** wait-set
that carries one event mailbox for the whole process plus that window's own
shell-output and child members. Opening another window costs a pty, a spawn, a
frame region, and two wait-set members; it costs no second process, no second
event mailbox, and no second icon-bar slot. What a window costs is bounded by
the session's per-client frame budget and by this process's own stream,
process, and address-space limits — no count of its own; the last window
closing ends it.

Three things make it work:

- **Every event is demuxed on the window id it carries.** The one mailbox
  serves every window and every window's popup, so the drain resolves which
  window (or which window's overlay) an event belongs to before routing it.
  An id neither names is a window that has just closed and the event has
  nowhere to land — dropped, never guessed at.
- **The mailbox is read through the shared stream, not a drain of its own.**
  The loop dispatches its own wakes — a shell stream and a child per window, a
  settings worker, a pressure wake, an animation deadline — so it owns the
  park and implements `tairix_window::EventDrain` alone (over `EventMailbox`,
  which authenticates each frame against the session's attested identity)
  rather than `EventSource`. It still reads through `WindowEvents`, which is
  what folds a resize-grab's run of client extents onto the newest: a window
  slower than the pointer lags a frame, never a queue of re-maps for sizes the
  drag has already left behind.
- **Each window's wait-set tokens are minted from a monotonic slot**, not an
  index into a list that shifts, so a token names the same window for as long
  as that window lives and is never reused after it goes. A window's members
  are removed with the window.

The user's profile is the *user's*, not a window's, so a setting changed in
one window's sheet re-derives every window's look and reshapes every grid; the
sheet that made the change is the only surface re-presented on top of that.

## 15. Its icon-bar presence

The terminal declares one presence on the desktop's icon bar
(`tairix_terminal::appbar`, `plans/NEW-TASKBAR.md` T7) whose slot stands for
the emulator, not for any one window:

- **`AppBarClick::Open`** (`appbar::SLOT_CLICK`) — every primary click on the
  slot is the terminal's to handle, and means *New window*. A terminal's
  windows are interchangeable, so raising one of them is less use than making
  another, and a terminal left on the bar with none is opened by the same
  click.
- **Closing the last window does not end it.** The terminal keeps its slot
  with no window open; only *Quit* closes them all and ends the process. That
  is the desktop-wide rule for every application on the bar, which is why the
  slot's click has to be able to produce a window.
- **It is declared before the first window is opened.** A declared presence
  belongs to the *process*, so the declaration goes out first and the slot
  carries this menu and this click behaviour from the moment it appears.
  Declared after a window, the session meanwhile derives a slot from that
  window alone — one that opens no menu — so for as long as the gap lasts the
  bar shows a slot that answers less. Every application that declares a
  presence does it in this order.
- **The menu** follows the desktop's one icon-bar convention
  (`tairix_window::declaration`): the session-drawn *Info* row, then the
  terminal's own *New window* row, then a separator and *Quit*. The terminal
  states only its own row — it cannot place the two ends, so it cannot get the
  order wrong. Its row id lives in the `appbar` module rather than in the
  program body, because two independent readers need it: the running program,
  which matches a chosen row back to a command, and the desktop QEMU vertical,
  which reconstructs the menu to know where to click. It is derived from the
  convention's own `QUIT_ROW` so the two ids can never collide.
- **A refused declaration is an answer, not a death.** A terminal whose
  declaration the desktop refuses says so on `stderr` and carries on with no
  slot of its own; its windows are still reachable through the slot the
  session derives from them.
