# CAPTURE.md — Screen capture

## Ledger

| ID | Item | Status |
|---|---|---|
| CP1 | `lib/video`: VP8 moved out of `lib/image`, decoding keyframes and inter frames; the encoder behind `encode` | done |
| CP2 | `lib/video`: WebM (EBML) mux and demux, with a seek index written into reserved space | done |
| CP3 | `lib/sound`: an Opus encoder sharing the decoder's model; `encode` split into `encode-flac` and `encode-opus` | done |
| CP4 | The physical key in every key record, filled in by `lib/hid` and every keyboard producer | done |
| CP5 | The compositor's capture tap: `CaptureTap`, `Target::Capture`, `compose_capture` | done |
| CP6 | The session's capture controller: chords, the capture bar, area and window selection, options, deadlines, the indicator | done |
| CP7 | Stills: PNG to the chosen folder or the clipboard (`ClipboardKind::Png`), through the shared keep rule on the capture file worker | done |
| CP8 | `recorder.app`: the recording component — its ring, pacing, VP8 and Opus encoding, WebM output, and every way a recording ends | done |
| CP9 | audiod monitor streams: the desktop's own output, gated by the seat lease and `CAP_AUDIO_CAPTURE` | done |
| CP10 | view.app plays WebM recordings through a streaming sandboxed decode, with sound through `lib/player` | done |
| CP11 | Docs, Help in every locale, and a QEMU vertical for stills and recordings | done |
| CP12 | The VP8 encoder spread over the recorder's cores: its first pass a wavefront of macroblock rows, its tokens written as parallel partitions | done |

Binding under `AGENTS.md`. The desktop session is the only component that can
capture the screen: no window, display or other protocol request can start or
read a capture, and ordinary applications never see another window's pixels.

## Formats

- **Still:** PNG, recording the pixel density as 96 dpi × the UI scale.
- **Recording:** WebM, VP8 video and Opus audio. Both codecs are royalty-free,
  and it is the format browsers themselves write when they record a screen.
  Video is lossy 4:2:0; stills stay lossless.
- **Rejected.**
  - APNG: no sound, no seek index, and motion beyond an SD card's write rate.
  - MP4 with H.264/HEVC/AAC: patent-encumbered.
  - AV1: several times VP8's encoder, and unverifiable until its decoder
    exists. `lib/video` is shaped so it can join later.
  - Matroska with FLAC: browsers do not play it.
  - AVI: no variable frame rate.
  - GIF and animated WebP: no sound.
  - FFV1: intra-only.

## The VP8 encoder

`lib/video`'s encoder, behind its `encode` feature, is what the recorder codes
with. Keyframes take the four whole-macroblock modes, or subblock modes where
they cost less; inter frames predict from the last frame alone, the golden and
alternate frames never refreshed or read. A macroblock no changed rectangle
touches is skipped with no motion and never examined, and an inter frame's
no-motion mode delta takes its loop-filter level to 0; a changed one takes
whichever of no motion with a residual, a whole-pixel vector searched from its
neighbours', and the whole-macroblock intra modes costs least — never the
subblock modes, which cost an inter frame most of its time for a few per cent
of its size. Quality is the quantiser index:
Compact 48, Standard 28, Fine 12. Every frame is reconstructed by the
decoder's own code, so the encoder's reference is every decoder's to the bit.
`Encoder::peak_bytes` states all an encoder holds; a frame allocates only into
the caller's buffer, and a refused one leaves the encoder as it was.

The recorder codes across a `tairix_parallel::Pool` of one participant per
online CPU, the encoding thread among them, and the encoder's bytes are the
same on any runner. Its first pass is a wavefront: each lane codes a
macroblock row in a band of its own, two macroblocks behind the row above and
filtering one behind itself, from what the row above publishes through
`Published` slots — its unfiltered bottom samples, decisions and contexts, then
its filtered bottom samples for the top edges below. A row an inter frame's
change leaves alone is copied from the last frame whole. A frame coding at
least 1024 macroblocks writes its tokens as eight partitions at once. At 1080p
a whole-screen change costs about 70 ms on one core, 24 ms on four and 13 ms on
eight, a keyframe 165, 60 and 35 ms; the pacing above merges what a busy
encoder has not yet coded, so a recording loses frame rate, never content.

## One capture path, in the compositor

- No memory holds the screen as seen: layered windows blend during scan-out,
  and the pointer rides the cursor plane. Nothing reads back scan-out.
- `CaptureTap { area, only, exclude, blank, pointer }`, armed by the session,
  receives every change: the `mark_from` funnel, the layer router, and the
  pointer's footprint. `only` shows one window and its transients, so a
  window capture is armed once, and windows opening, closing or restacking
  around it cost only what they change.
- `compose_capture` runs `compose_span` against `Target::Capture`, in bands
  across the job runner.
  - It composes at full strength, ignoring any fade, at desktop resolution.
  - It skips excluded windows and fills blanked ones opaque.
  - It touches none of the live frame's damage or counters.
  - It gives the same picture whether the frame was composited, layered or
    scaled.

## The session's controller

- **Chords.**
  - Matched by physical key where the secure attention key is already
    intercepted, before any seat holder sees them, and never while the lock,
    the authorisation sheet or the screensaver holds the keys.
  - Meta stands for Command: Meta+Shift+3 takes the screen; Meta+Shift+4 opens
    the area overlay, where Space picks a window and Alt-click leaves out its
    shadow; Meta+Shift+5 opens the capture bar.
  - Adding Ctrl copies to the clipboard.
  - Any capture chord during a recording stops it. Once it is stopped the
    chords capture again while its file is finished, and a recording asked
    for meanwhile is refused and said.
- **The capture bar.**
  - It sits bottom-centre of the work area, as floating chrome from
    `lib/controls`.
  - It offers Screen, Window and Area stills; Record Screen, Window and Area;
    Options; Capture or Record; and close. Its tips come through the seat's
    one tooltip.
- **Options.**
  - Save to: Desktop, Pictures (stills) or Videos (recordings), or the
    Clipboard.
  - Timer: none, 5 s or 10 s.
  - Duration: unlimited, 10 s, 30 s, 1, 5 or 10 minutes.
  - Sound: none, microphone, desktop or both.
  - Show the pointer, show clicks, and the floating thumbnail.
  - Choices are kept in the user's desktop settings, each written once it
    settles.
- **Timing.** Countdowns and durations are park deadlines. Escape while
  choosing or counting down discards everything.
- **Indicator.** A screen-recording signal in the notification area; clicking
  it stops the recording.
- **Holding the seat.** The bar, the overlay and the countdown hold the seat
  as the menu chain does; a chord pressed while one holds it cancels it. The
  bar's captures run the timer, the chords take at once.
- **Where it lives.** `userland/gui/session/src/capture.rs` and `capture/`;
  the recorder protocol is `lib/abi/src/recorder_ipc.rs`, and the session's
  side of it, tested over a fake recorder, is `capture/recording.rs`.

## Stills

- The capture is composed into a fallibly allocated surface and handed to
  the session's capture file worker, which writes
  `Screenshot YYYY-MM-DD at HH.MM.SS.png` off the loop. That worker's queue
  holds each job's answer, so no answer waits on an allocation; it also makes
  and names recordings' files, in the order they were asked for, and each
  recording's file is answered with the recorder it was made for, so one
  landing late for an ended recording is never another's.
- One naming rule serves captures and the screensaver's kept pictures:
  numbered " 2" to " 64" on collision, with the date omitted while the clock
  is unset, and a name too long for the filesystem with its date and number
  refused before anything is written. Each file is written to a hidden scratch
  file of its own — `.keeping.png`, `.keeping 2.png`, … made afresh, so no two
  captures, and no capture and a file an earlier session left, ever share one
  — and renamed without replacing. It is `userland/gui/session/src/keep.rs`,
  which the screensaver's file worker runs too. A scratch file is created
  afresh, never over one already there. Every scratch file left in a capture
  folder is named for the moment it last changed at the next start, and one
  left in a saver's folder when its next picture is kept, named for its saver;
  an empty one, a recorder that died before it wrote, is removed: none holding
  a still, a recording or a picture is discarded unread.
- A PNG records 96 dpi at the desktop's UI scale. A client's put naming
  `ClipboardKind::Png` whose bytes do not begin with the PNG signature is
  refused, as text that is not UTF-8 is. Nothing more of it is checked:
  decoding a picture is the parser sandbox's work, never the session's.
- The clipboard variant posts a `ClipboardKind::Png`. A capture larger than
  the clipboard's 16 MiB bound is saved to the chosen folder instead, and a
  notice says so.
- A floating thumbnail follows, from pixels the session already holds.

## Recordings: `recorder.app`

- **A separate component**, so the desktop can lower its priority, the
  microphone authority and the encoder's memory are charged to it, and an
  encoder fault cannot take the desktop down.
- **Manifest:** `CAP_SHM`, `CAP_AUDIO_CAPTURE`, `CAP_LOG_EMIT`, and no file,
  display, input or spawn authority.
- **Plumbing.** It has control pipes each way. The session starts it through
  `RtSessionChannel::launch_component`, which reads the child's
  kernel-attested process as it starts, and reaps it off the loop should it
  outlive its recording. One an administrator allowed is the login broker's
  child, which the broker attests and reaps, and reaches the session as the
  pipe ends the broker handed over (`RtSessionChannel::redeemed`). Its output
  — a hidden scratch file of its own the session made, written with
  positional writes — and its slots go to that process alone; its `Hello`
  names none, and it speaks the one protocol however it was started.
- **The ring.** Two capture-sized slots of the session's own memory, charged
  to it. The session makes them once the recorder says `Hello`, grants each to
  the recorder's attested process alone (`shm_grant_to`) and carries the
  grants in `Start`; a slot it cannot make or grant ends the attempt with its
  reason, and the slots are released however the recording ends. The recorder
  maps a slot only at its own size, to its last page, and only as a grant of
  the session its spawner named as its one operand — the session itself, or
  the broker's attested requester — since its pipes attest no peer. Each
  slot's outstanding
  damage is tracked by `tairix_geometry::StaleFrames`. What changed is kept in
  16-pixel tiles, so scattered changes cost a fixed set of bits; the session
  composes their cover, plus what the free slot missed, into it and posts
  `Frame { slot, t, rects }`, at most 64 rectangles merged at the least cost.
- **Pacing.** At most 30 frames a second, and only when something changed.
  While both slots are busy or the last frame is still queued, damage merges
  into the next frame, so nothing queues and the desktop never waits; a picture
  that cannot be composed is tried again a frame later.
- **Encoding.**
  - Unchanged macroblocks are coded as skip, with zero motion and loop-filter
    level 0, so static content never smears.
  - A keyframe comes every 10 s.
  - Video and audio are interleaved by timestamp, with the seek index written
    at the end into space reserved at the start.
  - The service thread serves the pipes, reads the capture rings and codes
    the sound; the encoding thread writes the file's header with its first
    job, codes each frame from its slot, writes every block and finishes the
    file. A slot is released once its frame is coded, and the sound waits in a
    batch of up to ten seconds while a picture codes.
  - A frame the encoder or the muxer finds no memory for is dropped, and the
    next is coded as a keyframe, so no change is lost.
- **What the recorder checks.** The protocol's decoders check a message's
  shape, not its sense. Before it reads a pixel the recorder bounds every
  rectangle a `Frame` names to its slot — `x + width <= slot.width` and
  `y + height <= slot.height` — and refuses a frame naming any other, a slot
  it holds that is still being encoded, or an instant before the frame before
  it. That refusal, like a message out of turn or one that does not decode,
  ends the recording: the file is finished as far as it got, the reason is
  stated, and no `Finished` follows.
- **Sound.**
  - The microphone is an ordinary capture stream.
  - Desktop sound is an audiod monitor stream of the output mix, taken before
    the speaker's volume, and needing `CAP_AUDIO_CAPTURE` and the seat lease.
  - "Both" mixes the two through `lib/audio`'s one mixer and resampler, on
    the output's clock. The microphone is carried onto the monitor stream's
    clock at the ratio of the two devices' measured rates over their nominal
    ones, joining once both have run half a second, or at two seconds on the
    nominal rates where no plausible measurement came.
  - Every source is F32 at 48 kHz, and each 20 ms is one Opus packet, mono at
    64 kbit/s for the microphone alone and stereo at 128 kbit/s otherwise.
    Frames the service lost are heard as silence, so the timeline holds.
  - `CAP_AUDIO_CAPTURE` stays out of every account's baseline. Whether the
    account may allow the recorder itself is asked of the broker off the
    loop, as the sheet asks who may allow a program (`Authorisers`). Where an
    account lacks it, starting a recording with sound raises the system's
    authorisation sheet: an administrator's answer runs the recorder as that
    administrator for that one recording — the login broker creates its
    control pipes, starts it in the requester's own login session (so the
    seat the desktop's sound is checked against is the requester's) and
    hands the session its ends of the pipes; a cancel starts nothing and says
    so. The sound rows carry no Authority Mark, which marks a choice that
    cannot be taken: this one can, with an administrator. While the sheet
    holds the keys the capture chords are not matched.
- **How a recording ends.**
  - **Stop** (a chord, the indicator, or the duration): what changed since
    the last frame goes as a last frame, and it is finalised, renamed to
    `Screen Recording YYYY-MM-DD at HH.MM.SS.webm`, and shown as a thumbnail.
  - **Lock, user switch, logout, seat loss or a resolution change:** it is
    finalised first. A logout waits for the recorder to finish its file, within
    the finish deadline, and a session in the background still wakes for it.
  - **Screensaver up:** no frames are taken.
  - **Recorder death:** the file is kept as far as it got (WebM without a
    seek index still plays), and the loss is stated: a recorder whose program
    never loaded says why, its exit waited for up to a second where its pipes
    close first, and one that failed before it recorded says nothing was kept.
  - **Session death:** the recorder finalises when its pipe closes.
  - **Disk full:** a write the volume or the delegation's extent refuses cuts
    the file back to the last block written whole — to nothing where even its
    header was refused — and its index goes into the reservation; `Finished`
    says `DiskFull`, and the reason is stated.
  - Every scratch file left in a capture folder is swept at the next session
    start.
- **Memory.** A recording commits what it needs at once before `Ready` — the
  encoder at its largest, the room for a coded frame, the sound's queues,
  mixer and batches — and one whose total passes the recorder's address-space
  limit is answered `Unstarted(OutOfMemory)` before any of it is made; the
  slots are the session's. What the muxer holds later is allocated fallibly: a
  frame it cannot hold is dropped, and the next coded as a keyframe.
- **Where it lives.** `userland/gui/recorder`: `recorder.rs` the service
  thread and the conversation, `encode.rs` the encoding thread's work,
  `sound.rs` the mix and the coder, and `run.rs` the binding to the runtime,
  host-tested over fakes of the pipes, the slots, the sound and the file.

## Security

- **Who can capture.** Only the user's chord or a click on session chrome
  starts a capture.
- **What a capture shows.**
  - Capture chrome — the bar, the overlay, the countdown and the floating
    thumbnail — is always excluded, and the thumbnail is taken down as a
    recording starts. Applications' own tooltips are part of the screen and
    are shown.
  - The lock and the authorisation sheet are blanked, and locking ends a
    recording.
  - The login screen and other seats are unreachable by construction, since
    each session composes only its own scene.
- **Afterwards.** The composed picture, the PNG written from it and the
  clipboard's copy are wiped once let go; the floating thumbnail's small copy
  and the PNG encoder's working memory are not. Every capture is logged with a
  stable event id.

## Playback on TAIRiX

view.app plays WebM recordings (`plans/VIEW.md`, *Movies*). A supervised
`moviedecode` session pages the file through the page cache `audiodecode`
shares, walks the pictures and the sound apart, and draws VP8 pictures into a
ring of three screen-sized slots shared with the viewer, naming each over the
pipe by slot and head alone, beside Opus sound over the pipe. A playback
thread per window plays the sound through `lib/player`'s stream seam and
paces the pictures by its clock, or by the monotonic clock when there is no
sound; the window copies each picture out of its slot and scales it to the
canvas. A recording its writer never finished plays from its
start, unseekable.

## Tests

- **VP8.**
  - Every encoded frame decodes bit-exact to the encoder's own
    reconstruction.
  - Image-quality floors per setting.
  - A thousand static frames stay identical, as the smearing regression test.
  - Odd sizes and keyframes.
  - A refused frame leaves the encoder as it was; an encoder holds no more
    than it states.
  - A fuzzed inter-frame decoder.
- **WebM.**
  - Mux and demux round trips: variable timestamps, the Opus header, the seek
    index.
  - A truncated file still demuxes, and a disk-full finalise produces a valid
    file.
  - A fuzzed demuxer.
- **Opus.** A round trip through `lib/sound`'s RFC 8251-verified decoder, with
  signal-to-noise floors.
- **Compositor.**
  - A capture equals the back buffer when composited, and the software
    composite under a layered display.
  - Excluded windows never appear, and blanked ones are filled.
  - The tap sees layered moves and cursor-plane pointer moves.
  - The live frame is untouched.
- **Session.**
  - Chords are taken before any seat holder and ignored while locked.
  - The bar, the overlay, deadlines, naming and collisions.
  - The recorder protocol over a fake: back-pressure, death, a recorder that
    never loaded, disk full, lock, a `Ready` and refusals crossing a stop, the
    slots granted to the recorder's own process and released however the
    recording ends, and a refused grant that starts nothing.
  - Blanking during authorisation, and a recording with sound over a fake
    broker: the account's own leave, an administrator's recorder taken up
    and spoken to, a cancel that starts nothing, and an answer no longer
    waited on let go.
- **Recorder.** Over fakes: the conversation in its one order; each refusal —
  a rectangle past its slot or whose far edge overflows, a slot past the ring
  or still being encoded, an instant before the last, a second `Start`; a
  closed pipe; a full volume, and one too full for the header; every start
  that cannot begin, a slot that does not map or not at its own size among
  them, leaving the file as it was; a stop before the first frame; each sound
  choice's sources; packets on one timeline, with lost frames heard as
  silence; a full batch; the microphone joining on measured and on nominal
  rates; keyframes every 10 s; each file read back through the demuxer.
- **audiod.** A monitor stream is refused without the capability or the lease,
  counted, audited, and its mix is bit-exact.
- **Playback.** The movie decode over recordings muxed from fixture VP8 frames
  and encoded Opus packets: units in file order with their times, a seek to
  the latest keyframe, an unfinished and a cut recording, a stream too large
  refused, a replaced worker resumed, and no dishonest answer believed, also
  fuzzed. The player over a fake audio service on a fake clock: pictures at
  their times on the sound's clock, drift followed, a late picture dropped,
  pause, seek, the end, a dying decoder, a refused budget, a silent movie, and
  sound that cannot play. The viewer's orders, seek bar and status over a fake
  playback.
- **QEMU** (aarch64, four cores; a ramfb board, and a virtio-GPU board whose
  pointer rides the cursor plane): the start's sweep keeps a scratch file an
  earlier session left; Meta+Shift+3 keeps a still that decodes, read back
  from the guest's disk, equal to the screen dump off the capture bar — and,
  where the dump carries no pointer, off the pointer too, the still drawing
  one there; a recording started from the bar and stopped by the chord ends
  with nothing lost, its last picture at least 34 dB PSNR from the dump.
