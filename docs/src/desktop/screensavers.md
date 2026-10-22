# Screensavers

Every screensaver but the desktop's own two, Black and the dimmed desktop, is
a bundle of the `screensaver` kind, and the desktop runs each one sandboxed
(`plans/SCREENSAVERS.md`). This page is what a saver is, how one is installed,
what it is given, and the savers that ship. How the desktop hosts one is
[the session's](./session.md#idleness-the-screensaver-the-display-and-the-idle-lock);
the saver side is [`lib/saver`](../lib/saver.md).

## A saver is a bundle

A saver is `<Name>.app` in one of two stores:

- `/System/Screensavers`, the system's, signed by the system's key;
- `/Users/<name>/Library/Screensavers`, the user's own, which every account's
  home is given, filed directly or in plain folders of it. A bundle there is
  self-signed: its signature proves the bundle is whole and grants nothing,
  and it may claim its threads and memory but no feed. A home outside `/Users`
  has no saver store.

To install one, put its bundle in the user's store. The desktop watches the
store and reads both stores again when it changes; Settings lists every saver
found, and says why any it cannot offer cannot be chosen, which the desktop
records in the system log too. A bundle still being copied in when the stores
are read may be listed as damaged until they are read again, which Settings
asks for each time it lists them to the end.

A saver runs only in the **drawing sandbox**
([`docs/src/security/sandbox.md`](../security/sandbox.md#the-drawing-sandbox)):
no capability, no file, no IPC endpoint, no wall clock and no randomness of
its own — two pipes to the desktop, one shared region it presents through,
the threads and memory the desktop grants, and the monotonic clock. The load
gate refuses a saver anywhere else, and a drawing spawn anything else.

## What a saver declares

Its signed `AppInfo` declares, beside the fields every bundle has:

- **options** (`option.<key> = { … }`), at most eight: a switch, a choice of
  two to twelve values, or a whole-number range of at most 1024 values with a
  unit. Each has a label and, optionally, a note saying what choosing it does.
  Settings draws a row for each; the desktop keeps a value under
  `saver.<bundle-id>.<key>` only where it differs from the default, and hands
  every option to the saver at its value or its default, so a saver holds no
  defaults of its own.
- **feeds** (`feeds = [...]`), the privileged inputs only a system saver is
  given — a user's own saver declaring one cannot be chosen:
  - `machine`, the machine's readings, relayed from the Switchboard monitor;
    whose busiest tasks are named is the desktop's
    `screensaver.machine.tasks` (`none`, `own`, the default, or `all`);
  - `keep`, handing a finished picture to the desktop to keep as a PNG in
    `Home/Pictures/<saver's title>/`, named for what the saver called it and
    when, never written over another;
  - `slides`, the shipped pictures, one at a time, prepared at the screen's
    size; which they are is the desktop's `screensaver.slides.source` (every
    picture, or one category's — a category the catalogue no longer holds
    offers every picture).
- **threads** (`threads = "cores"`) for a saver that runs a thread per online
  core, where any other runs one, and **memory** (`memory = "large"`) for one
  that needs the large ceiling. Every saver is given twice its region and
  128 MiB of address space, the large ceiling 2 GiB more, and never more than
  half the machine.

A saver also ships `Resources/Preview.png`, the picture Settings shows for
it: 16:9, at least as large as the largest picture the chooser draws, and
within 1 MiB. The image build refuses a system saver without one.

## What every saver is given

Its start carries the screen's size and scale, the display's refresh rate,
the desktop's look — appearance, text, contrast, density and reduced motion
— a seed from the CSPRNG, the account and machine names, the memory
pressure band (moved as it moves), its ceilings, and its options. Each frame
it is asked for carries its serial, its monotonic time and the wall clock as
the icon bar reads it. Text is drawn through the font service, the desktop
relaying each call.

## The savers that ship

- **Slideshow** (`Slideshow.app`): the shipped pictures, in the catalogue's
  order or shuffled — every picture once a pass, never the same one twice
  running from one pass into the next — one each interval. Each is prepared
  by the desktop through the sandboxed decode its backdrop uses and laid in
  the saver's feed area; a desktop with no preparer answers that the slide is
  unavailable rather than decoding on its loop.
- **Clock** (`Clock.app`): the icon bar's own reading and spelling of the
  time, so the two never disagree, over the date as the bar writes it and
  *account · machine*, either of which its options may leave out. Each minute
  the block fades out, moves somewhere new so no pixel stays lit, and fades
  back in over the theme's stage transition; under reduced motion it simply
  moves.
- **Minimal Clock** (`Ribbon.app`): the same reading set in a hairline weight
  with tabular figures, over the local date spelled out (`Mon 28 Sep 2026`),
  still, with a ribbon of orange light roaming the screen beneath it — five
  soft strands, each one smooth Bézier curve, kept clear of the text — from
  [`lib/ribbon`](../lib/ribbon.md), which the login screen stands over too.
  Under reduced motion the ribbon holds still and only the minute turning
  redraws.
- **Starfield** (`Starfield.app`): stars flown through, projected with
  perspective and drawn as the path each travelled over the frame — a dot
  while cruising, a streak in warp — the flight cruising, surging into warp,
  holding and settling back as the field turns slowly about the line of
  flight. Its options set the density and whether it warps; under reduced
  motion it only cruises and does not turn.
- **Game of Life** (`Life.app`): Conway's B3/S23 on a torus, bit-packed and
  stepped a word at a time, a newborn taking the colour of the colony most of
  its parents belong to and cells shading as they age; a world that settles
  into stillness, a cycle or near-emptiness is reseeded after a grace. Its
  options set the cell size and the speed; while no cell is changing it asks
  for no frame until the next generation.
- **Ray Tracer** (`Raytrace.app`, over [`lib/raytrace`](../lib/raytrace.md)):
  a scene composed at random in one of the tracer's settings, prepared a
  bounded unit at a time and revealed coarse to fine (`plans/RAYTRACE.md`):
  soft within moments, sharpening pass by pass, every pixel traced once at the
  tracer's best quality. A readout in the lower right says how far the scene
  is prepared and then traced. A whole picture is held a minute, faded out
  and followed by a scene set elsewhere. Its options set whether it traces on
  one thread of its own or on every core the desktop grants, whether each
  whole picture is kept through the keeping feed, and the detail its scenes
  are set out at — a scene whose peak the memory the desktop granted and the
  pressure band cannot spare is set out simply.
- **Retro Games** (`RetroGames.app`): a flight over a glowing grid towards a
  banded sun between two wireframe ranges, its reflection rippling on the
  floor, at the chosen speed, where wireframe craft now and then play out
  retro arcade games — a starfighter, a flying saucer, a tank battle, and
  riders walling each other in. Each act is a closed course drawn from a
  random stream of its own, so it plays the same however the frames fall;
  the craft are drawn a band of rows at a time across the saver's threads.
  Under reduced motion the scene holds still and nothing comes on.
- **System Monitor** (`SystemMonitor.app`, the machine feed): the machine's
  own readings, set out to be read from across a room — a verdict naming the
  worst concern first, then panels for the processors, memory, the tasks,
  storage and the network. Readings are never shown as live once they stop:
  a feed that goes quiet past three of the monitor's periods dims the panels
  and says when it stopped, and one whose monitor ended says so. Each minute
  the board steps round a small orbit against burn-in.

Each animated saver draws on whole frames of the display's rate, at most
thirty a second (`tairix_theme::motion::SceneClock`), presents only what
changed, and asks for its next frame no sooner than it needs one; the
desktop asks for none faster than the display shows them.

## Tests

Beside each saver's own host tests and the host's, the QEMU vertical
`screensaver_qemu_aarch64` boots the desktop with two savers in the seeded
account's own store, each the Clock's program under a manifest of its own
signed by a key of the user's own: one asks for no feed, one for the machine
feed. The script opens Settings, chooses the first in the chooser once the
catalogue has reached it, and presses Preview; a key typed once its first
frame is on the screen takes it down. It passes on the desktop's records: the
second saver refused for asking for a feed, the first loaded from the
account's store, its first frame shown, and its dismissal. The second is never
asked for, so the run cannot show the load gate refusing it; that refusal
(`APP_FEED_REFUSED`) is the kernel's host tests' to pin.
