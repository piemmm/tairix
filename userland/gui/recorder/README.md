# tairix-recorder — `recorder.app`, the screen recorder

Stability: experimental.

## What this crate is

The component the desktop session starts to record the screen
(`plans/CAPTURE.md` CP8), and that nothing else can reach. It speaks the
recorder protocol (`lib/abi/src/recorder_ipc.rs`) over its two pipes to the
session, codes the frames the session composes into the two slots it grants
the recorder as VP8 and the sound the session asked for as Opus, and writes
both as one WebM file through positional writes to the empty file the
session delegated it. The page is `docs/src/userland/recorder.md`.

## Capabilities

`CAP_SHM`, `CAP_AUDIO_CAPTURE` and `CAP_LOG_EMIT`, and no file, display,
input or spawn authority. Its one file is the session's delegation, written
under the session's authority. Its slots are the session's memory, granted to
the recorder's process alone and mapped by the handles the session's `Start`
carries, each only as a grant of the session its one operand names: the
session itself, or the login broker's attested requester.

## Threads

The service thread parks in one wait-set on the session's pipes, the sound
streams' mailboxes and the encoding thread's wake, and never writes the file.
The encoding thread writes the file's header with its first job, so a
recording that never begins leaves the file empty, then codes each frame as
VP8 and writes it, writes the sound's packets, and finishes the file. It codes
each frame across a `tairix_parallel::Pool` made for the recording: one
participant per online CPU the System Information API reports, the encoding
thread among them, so a single-core machine starts no further thread and a
four-core one codes a full-screen change in about a third of the time. The
service thread codes the sound itself, a packet costing a fraction of its
20 ms, and holds up to ten seconds of packets while a picture codes.

## Layout

| File | What it holds |
|---|---|
| `src/recorder.rs` | The service thread: the protocol, the slots' state, every refusal and ending. |
| `src/encode.rs` | The encoding thread's work: VP8, the muxer, the file's finish. |
| `src/sound.rs` | The sources, the mix on the output's clock, the Opus packets. |
| `src/run.rs` | The `Run` binary: the runtime behind every seam. |

## Tests

`cargo test -p tairix-recorder` drives the core over fakes of the pipes, the
slots, the sound, the clock and the file: the conversation in its one order,
every refusal before and after `Ready`, the session's pipe closing, a full
disk, the keyframe cadence, the sources each choice opens and the mix of
both, each file read back through `tairix_video::webm::Demuxer`.
