# Screen recorder (`recorder.app`)

`userland/gui/recorder` is the component the desktop session starts to record
the screen ([Screen capture](../desktop/capture.md), `plans/CAPTURE.md`). Only
the session reaches it: its standard input and output are two pipes the
session holds, it writes one file, the empty one the session made and
delegated it, and it reads its frames from two slots of the session's memory,
granted to its process alone. Its one operand is the session's instance in
lowercase hexadecimal, named by whoever starts it — the session itself, or the
login broker naming the session it serves — and the recorder maps the slots
only as that instance's grants, since its pipes attest no peer. Without that
operand it ends at once, saying so. Its manifest asks for `CAP_SHM`,
`CAP_AUDIO_CAPTURE` and `CAP_LOG_EMIT`, and for no file, display, input or
spawn authority.

## The conversation

The protocol is `tairix_abi::recorder_ipc`, in one order:

1. The recorder says `Hello`.
2. The session's `Start` names the area, the sound, the output's delegation,
   the grants of the two slots it made and the instant the recording starts.
   The recorder maps each slot, makes what the recording needs and answers
   `Ready`, or `Unstarted` with why it cannot begin. A slot is mapped only at
   its own size, to its last page.
3. Each `Frame` names the slot the session composed into, when it is shown
   and the rectangles that changed. The slot is `Released` once its frame is
   coded.
4. `Stop` is answered `Finished`, with the file's length, once the file is
   finished and durable.

| `Unstarted` | Why |
|---|---|
| `OutOfMemory` | What the recording needs at once passes the process's address-space limit. |
| `AlreadyExists` | The delegated output is not empty. |
| `OutOfRange` | The output is not a regular file, or the area is a size VP8 cannot carry. |
| `LengthOutOfRange` | A slot the session granted is not the slot's size. |
| `PermissionDenied` | The audio service refused the sound: the account lacks `CAP_AUDIO_CAPTURE`, or its session does not hold the seat. |
| `BadMagic` | The session spoke before it sent `Start`. |
| `AbiVersionUnsupported` | The `Start` is of another version of the protocol. |
| any other | The refusal passed on: of the output's delegation, a slot's mapping, or the encoding thread. |

A recording that does not begin leaves the file as it found it.

Once recording, a frame naming a rectangle that reaches past its slot, a slot
still being encoded, or an instant before the frame before it ends the
recording, as do a second `Start`, a frame after `Stop` and a message that
does not decode. The file is finished as far as it got, the reason is logged
and stated on standard error, and no `Finished` follows, so the session hears
the recorder fail. A frame that arrives once the recording has ended of
itself is released uncoded, so the session never waits on a slot.

## How a recording ends

| Ending | The session hears | Exit status |
|---|---|---|
| `Stop` | `Finished`, `Stopped` | 0 |
| A write the volume, or the delegation's extent, refuses | `Finished`, `DiskFull` | 0, the reason stated |
| The session's pipe closes | nothing; the file is finished | 1 |
| A refusal, or a failure of the encoder, the file or the encoding thread | nothing; the file is finished as far as it got | 1 |

## Threads

- The service thread parks in one wait-set on the session's pipes, the sound
  streams' mailboxes and the encoding thread's wake. It checks each frame,
  reads the capture rings as they fill, and mixes and codes the sound. It
  never writes the file.
- The encoding thread writes the file's header with its first job, codes
  each frame as VP8 from its slot, writes it and the sound's packets through
  the WebM muxer, and finishes the file. Where no thread can be had, its work
  runs on the service thread (`NO_ENCODING_THREAD`).
- Each frame is coded across a worker pool made for the recording: one
  participant per online CPU the System Information API reports, the thread
  that codes the frame among them, so a single-core machine starts no further
  thread. A worker the kernel refuses is simply not made, and the frame is the
  same bytes however many there are.

## Video

The first frame is a keyframe, and so is the first after each ten seconds of
recording. A frame's damage tells the encoder which regions changed. A frame
the encoder or the muxer finds no memory for is dropped and the next coded
whole as a keyframe, so no change is lost; the first drop is logged
(`FRAME_DROPPED`).

## Sound

| Choice | Heard | Coded |
|---|---|---|
| Microphone | the microphone | Opus, mono, 64 kbit/s |
| Desktop | the audio service's monitor of the output mix | Opus, stereo, 128 kbit/s |
| Both | the two, mixed | Opus, stereo, 128 kbit/s |

Every source is an F32 capture stream at 48 kHz. Each 20 ms is one Opus
packet, the first timed at the moment the sound started, and frames the
service lost are heard as silence, so the timeline holds.

For both, the monitor is the clock. The microphone runs on another device's
clock, so `lib/audio`'s one resampler carries it onto the monitor's at the
ratio of the two devices' measured rates over their nominal ones, and its one
mixer sums them. The microphone joins once both have run half a second and
their rates are measured, or at two seconds on the nominal rates where they
are not; a measured ratio more than 5% from one is not believed. Beside the
monitor, a microphone that fails is heard as silence from then on; the source
the sound keeps time by failing ends the sound, and the picture goes on
(`SOUND_LOST`).

Packets wait in a batch of up to 500, ten seconds, while a picture codes. A
packet that finds the batch full is lost, and the loss logged once
(`SOUND_LOST`).

## The file

One WebM file, written with positional writes to the delegation, its video
and sound interleaved by timestamp. Room for a seek index naming 360
keyframes, an hour of them, is reserved at its start. Finishing writes what
is held, the index, the duration and the segment's size, and makes the file
durable before `Finished` is said. A refused write cuts the file back to the
last block written whole, to nothing where even the header was refused, and
the index goes into the reservation, which needs none of the room the disk
lacks.

## Memory

Before `Ready` a recording commits what it needs at once: the encoder at its
largest for the pool's participants, the room for a coded frame, and the
sound's queues, mixer, coder and batches. Its two slots are the session's memory, charged to the session. A
recording whose total passes the process's address-space soft limit is
answered `Unstarted(OutOfMemory)` before any of it is made.
Every later allocation is fallible: a frame the muxer cannot hold is dropped.

## Events

| Id | Name | Logged when |
|---|---|---|
| 36000 | `RECORDER_UNSTARTED` | A recording cannot begin. |
| 36001 | `RECORDER_REFUSED` | The recorder refuses what the session sent. |
| 36002 | `RECORDER_ENDED` | A recording ends other than as the session asked. |
| 36003 | `FRAME_DROPPED` | A frame is dropped for want of memory. |
| 36004 | `SOUND_LOST` | Some or all of the sound is lost. |
| 36005 | `NO_ENCODING_THREAD` | The encoding thread's work runs on the service thread. |
| 36006 | `FEW_ENCODING_CORES` | The kernel granted the encoding pool fewer threads than there are cores; frames are coded on fewer. |

## Tests

`cargo test -p tairix-recorder` runs the core over fakes of the pipes, the
slots, the sound, the clock and the file, and reads each file back through
`tairix_video::webm::Demuxer`.
