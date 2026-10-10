# Sound player (`play`)

`play FILE...` plays sound files on a sink (`plans/SOUND.md` SND10). Its
options and keys are its own Help document; this page is how it is built.

## No file is parsed in `play`

Each file is decoded in the parser sandbox: `play` re-enters its own binary as
a capability-empty session worker (`lib/sandbox::audiodecode`) and hands it the
file's length, never the file. The worker asks for the bytes it reads, and
`play` reads them and supplies them; nothing the worker answers is believed past
the client's checks. A worker that fails is replaced after a paced delay and
brought back to where the stream was, so a hostile file can end its own decode
and nothing else: it is left out with the reason stated, and the list plays on.

## One stream while the shape holds

Playback is `tairix-player`'s engine (`docs/src/lib/player.md`), over a fixed
`List` of the files named, each played over the start and duration asked for,
for the passes asked for. It writes what the decoder answers into one
`audio-v1` stream while consecutive files share a rate, sample format and
channel layout, so a list plays gapless. A file of another shape waits for the
stream to drain — every frame already queued is heard — and opens its own. The
ring is half a second deep: pausing, seeking and changing level act on the
stream rather than on what it has queued, so a deep ring costs no
responsiveness and rides out a slow disk.

The ring counts frames from zero per stream and each file counts its own; where
the two stop moving together — a new file, a seek, a new pass — a segment
records which file frame a ring frame carries, so the position the audio
service reports reading is shown as the file and time being heard.

A stream's level is attenuation (`AudioGain`, never above unity), as the audio
ABI requires; raising a whole sink is its owner's business. A sink named by its
location is looked up among the sinks listed when the session starts, so a
script can name the same speakers on every boot; `--list-devices` prints each
sink's id and location.

## Playback is not in the interface loop

The playback loop runs on the main thread, parked on one wait-set: the
decoder's pipes, the stream's mailbox, the interface's commands, the signal
intake, and the decoder's paced restart as its only timer. The full-screen
interface is a thread of its own, parked on the keyboard relay
(`tairix_rt::keys`), a wake the playback loop nudges when the status changes,
and the terminal's foreground edge. It paints from the status alone, at most
once every 50 ms however fast the status changes, and only while the process
holds its terminal. Sent to the background, `play` gives the terminal back and
keeps playing; brought back, it draws again. Ctrl-Z, which reaches it as a byte
while its input is raw, pauses the stream, gives the terminal back and stops
the process; continued, it plays on from the frame it paused on.

Without the interface — no terminal, `--no-ui`, or `-q` — it plays on the same
loop, and unless `-q` reports a one-line progress on a terminal's standard
error while it holds the terminal.

When its session's seat is elsewhere the service holds the stream on its
frame, and `play` shows *Held* until the room is its own again — then playing,
or paused if the user paused it, from that frame. A stream the service found
corrupt ends playback with that reason.

## What it reports

On fd 3 it writes a `schema` record for each file it plays, an `omission`
record for each file left out or cut short with the reason's stable token, and
a `summary` record of what was heard and how often the device ran short — the
audio service's own count for each stream, taken as it closes, since its
notifications may have fallen short of it. A file left out is also stated on
standard error — after the interface has given the
terminal back, when there is one — and makes the exit status `1`.

## Testing

The engine is host-tested in `tairix-player`; `play`'s own tests cover its
command line, its interface and its reports.
The QEMU `play` verticals plant the shared signal as a FLAC file, written by
`lib/sound`'s encoder at image build, and play it on all three architectures;
the host-side capture must hold exactly the signal, so the frame decoder, the
sandboxed worker and the whole output path are held bit-exact together.
