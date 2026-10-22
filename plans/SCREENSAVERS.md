# SCREENSAVERS — every screensaver a sandboxed bundle

Binding under `AGENTS.md`. How a screensaver is packaged, admitted, run,
configured and shown: each one a self-contained, signed bundle that draws into
a surface the desktop hands it, inside a sandbox that can reach nothing else.

Read first: `plans/PINBOARD.md` (the settings document and previews),
`plans/NEW-DESKTOP-SETTINGS.md` DS12, DS18, DS19, DS21, DS23, DS24 (the
Screensaver pane and the scenes' rules), `plans/RAYTRACE.md` (the ray-tracer
screensaver), `plans/APPS.md` (bundles), `plans/RUST.md` decision 12 (trust by
store), `docs/src/security/sandbox.md`.

## Ledger

| Id | Item | Status |
|---|---|---|
| SV1 | The `screensaver` program kind and its two stores, `/System/Screensavers` and `<home>/Library/Screensavers` | done |
| SV2 | A saver's manifest declares its options, the privileged feeds it needs and its resource needs | done |
| SV3 | The drawing sandbox: a sandboxed spawn handed one shared region, and `share_map` to map it | done |
| SV4 | The load gate knows the spawn's mode: a saver runs only in the drawing sandbox, and the drawing sandbox nothing else; a user's own saver is admitted self-signed | done |
| SV5 | The saver protocol (`lib/abi` `saver_ipc`) and the region a saver presents through | done |
| SV6 | `lib/saver`: the saver side — the run loop, the font relay, options and the wall reading | done |
| SV7 | The session hosts a saver: start, pace, present, feeds, deadlines, dismissal | done |
| SV8 | The catalogue and the Settings pane: discovered savers, previews, declared options, feed settings | done |
| SV9 | Every built-in scene is its own bundle under `userland/gui/screensavers/`; the session draws none of them | done |
| SV10 | Image build, docs, charter amendments and the QEMU vertical | done |

## 0. The binding decisions

- **A screensaver is a bundle, and only a bundle.** Clock, Ribbon, Starfield,
  Life, the ray tracer, Retro Games, System Monitor and Slideshow each ship as
  `<Name>.app` of kind `screensaver`. Blank and Dim stay the session's own
  display states: they draw nothing a bundle could.
- **Every saver is sandboxed, system ones too.** A saver runs capability-empty
  in the drawing sandbox (SV3): its two pipes, its one shared region, its own
  threads and memory, and the monotonic clock to pace its own work, nothing
  else — no file, no IPC endpoint, no wall clock, no RNG. The desktop gives it
  everything it draws from.
- **A user installs a saver by dropping it in `<home>/Library/Screensavers`.**
  Such a bundle is self-signed: its signature proves the bundle is whole, and
  grants nothing. It is admitted only into the drawing sandbox and only when it
  declares no privileged feed (SV4). This is the one exception to
  `plans/RUST.md` decision 12; developer signing (R7) is not a prerequisite.
- **A saver draws over the lock, and sees and hears nothing of it.** The
  session composites the saver's frames over a locked screen, as macOS does.
  The saver receives no input; the first input dismisses it to the real lock
  and is drained. `plans/NEW-SWITCHBOARD.md` S14 states the exception.
- **Feeds are narrow and declared.** Every saver gets the basic feeds: its
  size and scale, frame times, the wall clock as the bar reads it, the theme,
  reduced motion, a seed, the account and machine names, fonts, and its own
  options. The privileged feeds — machine readings, picture keeping, slides —
  are declared in the signed manifest and given only to a system-signed saver.
- **A feed's settings are the desktop's, not the saver's.** Which pictures the
  slides feed offers and whose task names the machine feed carries are host
  settings Settings shows beside a saver that declares the feed. The host
  applies them before a byte reaches the saver, so no saver is trusted to hide
  what it was given: another account's task names reach a saver only when the
  user chose `all` (closes D496).
- **Frames travel like any window's.** The saver draws into its own surface and
  writes the damaged rectangles into the shared region, laid out as an app's
  window frame; the session decodes them with the same fail-closed decoder
  (`winframe::decode_list`), every pixel laid opaque so no saver can show what
  it covers. A frame costs a saver what a frame costs an app.
- **Nothing waits on a saver.** The session drives it through the duplex
  sandbox session, never blocking; a saver late past its deadline, or crashed,
  or speaking out of turn, is ended, the screen goes Blank, and the end is
  logged.

## 1. SV1 — the kind and its stores

`ProgramKind::Screensaver` (`screensaver`). Its stores are
`SYSTEM_SCREENSAVER_STORE` (`/System/Screensavers`, written by the installer
and updater only) and, under each home's `Library/`,
`HOME_SCREENSAVER_STORE_DIR` (`Screensavers`). A saver is never searched for by
a typed word, never listed by the program library, never given an icon-bar
slot, and is launchable only from one of those stores. Every home is given its
`Library/Screensavers` with the rest of its shape (`lib/users`). A saver ships
no `Help/`; its preview is `Resources/Preview.png` (16:9, the preview family's
contract), which Settings shows and the image build requires of a system
saver. A saver's sources live under `userland/gui/screensavers/`, so a
headless image ships none. The file manager types a bundle in either store as
a screensaver, never a program to launch.

## 2. SV2 — what a saver declares

The manifest header's `option_count` byte (at most `SAVER_MAX_OPTIONS`)
counts the options, and the body carries one fixed `SAVER_OPTION_ENTRY_LEN`
entry per option after the browsed services (`lib/abi/src/saver.rs`): a key
(a word, `[a-z][a-z0-9_]*`), a label, and its kind — a switch with its default,
a choice of two to `SAVER_MAX_CHOICES` spelled values with their labels and a
default, or a whole-number range whose step divides its span, of at most
`SAVER_RANGE_VALUES_MAX` values (a slider sets each), with a unit and a
default. Keys are unique within a manifest.

Flag bits: the privileged feeds `APPINFO_FLAG_FEED_MACHINE`,
`APPINFO_FLAG_FEED_KEEP`, `APPINFO_FLAG_FEED_SLIDES`, and the resource needs
`APPINFO_FLAG_SAVER_PARALLEL` (threads up to one per core) and
`APPINFO_FLAG_SAVER_LARGE_MEMORY`. Decode refuses options or saver flags on a
manifest of another kind, and refuses a screensaver that requests a
capability, claims a file type, browses a service, sets another flag or is
listed by the program library.

The values live in the session's settings document. `screensaver.saver` names
the chosen saver — a bundle id, `blank` or `dim`. An option's value is
`saver.<bundle-id>.<key>`, held only when it differs from the default, read
leniently (a value its option no longer accepts reads as the default, so a
saver update never resets the desktop) and written strictly against the
declaration Settings was listed. A saver's values outlive its bundle, so one
reinstalled keeps them; `SAVER_VALUES_MAX` bounds what a document holds. The
host sends every declared option at its value or its default, so a scene holds
no defaults and refuses an option missing or outside its declaration. The
feed settings are `screensaver.slides.source` (every wallpaper, or one
category; one the catalogue no longer holds offers every picture) and
`screensaver.machine.tasks` (`none`, `own` — the default — or `all`).

## 3. SV3 — the drawing sandbox

`SpawnAttach` gains `share`: the id of a shared region the spawner holds a
grant for, zero for none, valid only on a sandbox spawn. The kernel checks the
spawner's grant covers the region before admission (delegation never widens),
mints the child its own grant to it with the spawner as grantor, and records it
as the child's spawn share. The child maps it once, read-write, with
`share_map`, needing no capability because the spawn grant is the authority; a
second call answers `AlreadyExists`. The sandbox allow-list also admits a
sandbox's own threads and futex words and the monotonic clock, each naming
nothing outside the task (`docs/src/security/sandbox.md`).
`SpawnAttach::drawing(wires, memory, threads, share)` is the canonical block.

The host sizes the bounds from the machine and the saver's declared needs: one
thread, or one per online core for `PARALLEL`; an address space of twice the
region and 128 MiB, 2 GiB more for `LARGE_MEMORY`, never more than half the
machine. The ray tracer's *Performance* takes its cores and its *Maximum*
detail its memory within those bounds.

## 4. SV4 — admission

The bundle load gate (`lib/appload`) is told the spawn's mode. A `screensaver`
loads only into a drawing spawn and a drawing spawn loads nothing else,
re-checked on a launch-cache hit, so a saver can never run with its spawner's
authority. A bundle in a user's saver store
loads only as a screensaver into a drawing spawn: its manifest signature is
verified against its own signer key rather than the system anchor, its
publisher binding and content digest as any bundle's, and it is refused if it
declares a privileged feed. A system saver is verified against the app-signing
anchor. Every refusal is typed and logged with its own event.

## 5. SV5 — the protocol

`lib/abi/src/saver_ipc.rs`, over `lib/sandbox`'s length-prefixed frames on the
saver's two pipes, every message bounded and decoded fail-closed on both sides.
One frame is outstanding at a time.

- **Host to saver:** `Start` (size, scale, refresh, the monotonic time it set
  off at, reduced motion, contrast, density, the desktop's text, appearance,
  seed, pressure band, cores, the granted threads and memory, the account and
  machine names, option values, granted feeds, the region's layout, the slide
  count); `Frame` (serial, monotonic time, the wall reading as the bar spells
  it); `Machine` (a filtered machine reading); `MachineLost` (its source
  stopped); `Slide` (a picture written into the feed area, or why not);
  `FontReply` (the font service's reply); `Kept` (the feed area is the
  saver's again); `Pressure` (the band moved).
- **Saver to host:** `Ready`; `Presented` (serial, at most
  `MAX_DAMAGE_RECTS` damage rectangles, and when the next frame is wanted);
  `FontCall` (one font request in the font service's own wire form); `Keep` (a
  finished picture in the feed area, and its title); `WantSlide`; `Unstarted`
  (why it cannot start, before `Ready` alone).
- **The region** is one window frame (stride × height, the app frame format)
  and, for a saver given slides or keeping, a feed area of one screen-sized
  picture after it, owned by whichever side the protocol last handed it to.
  The frame area holds the whole picture from before `Ready`.

## 6. SV6 — `lib/saver`

The saver side, `no_std` + `alloc`: the `Scene` trait (frame, machine
reading, machine lost, slide, kept; the scene itself is built by the closure
`run` is given), and `run`, which maps the region, answers `Start` (or says
`Unstarted` with the reason the region or the scene could not be had), draws each
`Frame` into the scene's own surface, writes the damage into the region and
replies `Presented`. The font relay is a `tairix_font::FontTransport` that
sends `FontCall` and blocks for its `FontReply`, keeping any other message that
arrives meanwhile, so `BitmapFont` works unchanged. Options are read by key
and kind, strictly; the wall reading spells the time and date as the bar
does. A frame's damage past what one `Presented` carries is folded into a
covering few.

## 7. SV7 — the host

The session spawns the chosen saver with a fresh region sized to the screen
(`saver::Hosted` over the pure conversation in `saver/talk.rs`), asks for each
frame on its own frame step at the time each `Presented` asks for and never
faster than the display, after every feed message, decodes each frame into
the screensaver's window opaque, and serves its feeds off the loop, each
answer carrying the saver instance that asked so none reaches a later one:

- **fonts** relayed to `fontd` on a thread of their own, one call
  outstanding and at most `SAVER_FONT_CALLS_MAX` while starting or from one
  frame asked for to the next, the relay's time held against no deadline;
- **machine readings** from the Switchboard watch the session already holds,
  each task carrying whether it is the session user's own; names are cleared
  by `screensaver.machine.tasks` before the reading is sent;
- **picture keeping** — the feed area read out opaque and encoded as PNG on
  the saver file worker into `Home/Pictures/<saver's title>/`, written to a
  hidden scratch file and renamed without replacing onto the first free name,
  named in the user's own zone (`plans/RAYTRACE.md` RT3); a keep sooner than
  `SAVER_KEEP_SPACING_NS` after the last, or past `SAVER_KEEPS_MAX` in one
  run, is answered as not kept;
- **slides** decoded by the session's slide preparers into the feed area;
- **the pressure band**, relayed as it moves.

Readings and the pressure band are held while anything is queued for a saver,
so one slow to read is told the latest, never a backlog, and the queue holds
every message the host can have outstanding at once. A saver must be `Ready`
within `SAVER_START_DEADLINE_NS` and answer each frame within
`SAVER_FRAME_DEADLINE_NS`, naming damage on the screen alone. A resize or an
output change ends it and the idle deadline starts the screensaver again; a
missed deadline, a malformed or out-of-turn message, a pipe the session cannot
watch, its `Unstarted`, or its exit ends it and the screen goes black, the end
said on standard error and recorded (`SAVER_ENDED`). A saver ended while it
still runs is killed and reaped when its exit is reported, never waited for on
the loop. Its first frame laid on the screen (`SAVER_SHOWN`) and a dismissal or
a display switched off (`SAVER_DISMISSED`) are recorded too.

## 8. SV8 — catalogue and Settings

The session scans both stores on its saver file worker and keeps the
catalogue (`saver::SaverCatalog`): Black and Dim, then each saver's id, title,
purpose, origin, declared options, declared feeds and standing, the reason it
cannot be chosen said on standard error and recorded (`SAVER_REFUSED`) when
the catalogue changes. It scans
at bring-up, when a watch on the user's store reports, and when Settings reads
the list to its end, so the stores move under the next listing rather than
this one. Settings, which reads no file, pages the catalogue entry by entry
over the window channel (`QueryScreensaver`, answered for Settings alone),
each entry with its options, starting over if the list moves under it; it
lists it when the pane comes on show and whenever its window is focused while
it shows, and asks for a preview by catalogue place
(`PreviewSubject::Screensaver`); the first such request after a listing
reached its end is recorded (`SAVER_CATALOGUE_SHOWN`), since a chooser can
make none before it holds the list. The pane builds each saver's rows from its
declared options and shows the feed settings beside a saver that declares the
feed; it says why each saver it cannot offer cannot be chosen, and that a
chosen saver is not installed; **Preview** shows the chosen saver with the
pane's values.

## 9. SV9 — the scenes as bundles

Each scene moves to `userland/gui/screensavers/<name>/` over `lib/saver`,
keeping its behaviour, options and tests; the session's `saver/` scene code and
its `lib/raytrace` and `lib/ribbon` dependencies go. Clock, Ribbon,
System Monitor and the ray tracer's readout draw their text through the font
relay; the ray tracer keeps its pictures through the keeping feed and its
threads through its own pool.

## 10. SV10 — image, docs, charter

The image plants `/System/Screensavers/`; `AGENTS.md` §3, §16.2, §16.3, §16.5
and §16.8 name the source tree, the store, the user store and the kind; the
sandbox, desktop and pinboard docs describe the drawing sandbox and the host.
The QEMU vertical `screensaver_qemu_aarch64` plants two savers in the seeded
account's own store, each the Clock's program under a manifest of its own
signed by a key of the user's own, one asking for no feed and one for the
machine feed. Its script opens Settings, chooses the first once the catalogue
has reached the chooser, and previews it; a key typed on its first frame
takes it down. It passes on the second refused and never loaded, the first
loaded from the account's store, shown, and dismissed.
