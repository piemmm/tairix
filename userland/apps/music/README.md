# `music` — the desktop music player

`music.app` plays a list of tracks in one window (`plans/SOUND.md` SND14). It
is named `music` because `play` is the command-line player and the build
refuses two bundles claiming one name.

Stability tier: **experimental**.

## What it plays

FLAC (native and in Ogg), WAV and AU — every encoding `lib/sound` reads, each
completely. MPEG audio, Vorbis and Opus are claimed with the decoders that
read them.

## Capabilities

`CAP_CONSOLE_WRITE`, `CAP_SHM`, `CAP_SANDBOX_SPAWN`, `CAP_LOG_EMIT` — and **no
filesystem capability**. A file reaches the player only as the user's act: a
one-shot delegation from the session's trusted picker (a file, or the files of
a chosen folder the player opens) or from the file manager. Each file's sound
is decoded in the audio sandbox and its album art in the image sandbox; the
player never parses a byte of either.

## How it is built

* **Engine** (`src/lib.rs`), host-tested, no window and no I/O: the playlist
  (`playlist`), the one layout (`layout`), the player's state with its input
  entry point (`view`), and the renderers (`paint`).
* **Playback** is `tairix-player`'s engine on a thread of its own, told what
  the listener asked and read back through the status it posts.
* **A worker** reads each track's tags and decodes its cover, lists the
  output devices and saves the listener's settings, so the window never waits.

The seek slider seeks once, where it settles; the volume slider sets the
stream's level as it moves and saves it where it settles; the meters repaint
their own rectangle and nothing else.

See `docs/src/userland/music.md`.
