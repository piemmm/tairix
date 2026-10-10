# play

Stability tier: **experimental**.

The command-line sound player (`plans/SOUND.md` SND10). Each file is decoded in
the parser sandbox — `play` re-enters its own binary as the capability-empty
worker and never parses a byte of a sound file itself — and written into one
`audio-v1` stream while the files' rate, sample format and channel layout
hold, so a list plays gapless. A file of another shape waits for the stream to
play out and opens its own.

Playback is not in the interface loop. The full-screen interface is a thread
that draws the engine's status while this process holds its terminal, so
`play` keeps playing when it is sent to the background and draws itself again
when it is brought back. Without a terminal, or with `--no-ui`, it plays
silently on standard output and reports a one-line progress on a terminal's
standard error unless `-q`.

| Module | What it is |
|---|---|
| `command` | The command line. |
| `engine` | The playback engine; files, the stream and the decoder's worker are seams, so it is host-tested over the real decoder in-process. |
| `report` | The `stdinfo` records on fd 3 and the lines on standard error. |
| `view` | The interface, drawn from the status alone. |
| `run` | The `Run` binary: the playback loop, the interface thread, the live seams. |

Capabilities, and why each, are in `AppInfo.toml`; the page is
`docs/src/userland/play.md`.
