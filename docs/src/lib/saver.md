# `tairix-saver`

The saver side of a screensaver (`plans/SCREENSAVERS.md`): everything between
the drawing sandbox a saver bundle runs in and the `Scene` it draws. A saver's
`Run` is its scene and one call to `run`, which builds the scene from the
`Setup` the host's `Start` makes and serves it until the host dismisses it.
The hosting side is [the session's](../desktop/session.md#idleness-the-screensaver-the-display-and-the-idle-lock);
what a saver is and what ships is [Screensavers](../desktop/screensavers.md).

## What it does

- **Answers `Start`** (`lib/abi` `saver_ipc`): maps the spawn share as the
  region it presents through (`tairix_rt::shm::SpawnShare`, mapped for the
  rest of the saver's life, since a drawing sandbox may not unmap), refusing
  one smaller than the layout `Start` states, and builds the scene; a region
  or a scene it cannot have is said to the host as `Unstarted`, with why. The
  frame area holds the whole picture from before `Ready`, an opaque black
  screen.
- **Draws each `Frame`** the host asks for in place: the scene's surface is
  lent over the region's frame area (`tairix_raster::PixelMemory`), keeps
  every pixel between frames, and is written only from a `Frame` to its
  `Presented`, while the host reads it only from a `Presented` to the next
  `Frame`. `Presented` names the rectangles the scene says changed and when it
  next wants a frame: soon, at a time, or only when a feed brings something.
- **Relays text.** Its font transport sends each font request as a
  `FontCall` and waits for the `FontReply`, keeping any other message that
  arrives meanwhile, so `tairix_font` draws as it does anywhere else. Its
  caches are sized from the machine's installed memory, as every desktop
  program's are, each never past a sixteenth of the saver's memory. Text is
  drawn on the saver's main thread alone, which is the one reading the pipe.
- **Hands the scene its feeds**: machine readings and their loss, a slide
  laid in the feed area, the answer to a picture it asked to keep, and the
  pressure band, which its caches answer to; a band that moved empties the
  font caches at once. Every host message is read into one buffer the loop
  keeps.

A scene reads its options through `Setup` by kind — `choice`, `switch`,
`range`, and `pace`, the slow, normal and fast every speed offers, in percent
— and a face that tells the time compares its reading as a `Told`, with the
date it shows (`DateShown`), allocating nothing to find a minute unchanged. An
option the host did not send, or a value no declaration
admits, refuses the scene rather than falling back: the host always sends
every declared option at its value or its default, so a scene holds no
defaults of its own. `Setup` also gives the start itself — its seed, the
time it set off at, the refresh rate a scene's clock is built from — the
theme the desktop is drawn in, and the threads it was granted as a pool.

## Bounds

The protocol's messages and the region's layout are fixed-size and checked
on both sides; a message out of turn ends the saver. A frame names at most
`SAVER_DAMAGE_RECTS` rectangles, and damage past that is kept as its bounding
box, so a scene with a scattered change marks it in `DAMAGE_TILE` tiles
(`tairix_geometry::Tiles`) and names their cover, which fits.

## Tests

The `test-support` feature gives a scene's own tests a `Screen`: a start, a
setup and a frame's host with no pipe, region or process behind it, so a
scene's frames, feeds and keeping are tested on the host.
