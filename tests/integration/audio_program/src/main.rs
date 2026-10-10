//! `audiotone` — the guest half of the end-to-end audio verticals
//! (`plans/SOUND.md` SND4).
//!
//! It plays the shared deterministic signal
//! ([`tairix_test_audio_wire`]) through the whole production stack — the
//! `audio-v1` client in `lib/audio`, the `audiod` mixer, the `audiochan-v1`
//! device channel, the autoloaded `virtio_snd` driver process, and QEMU's
//! emulated sound card — and QEMU's `wav` backend writes what the card
//! received to a file the harness then checks sample for sample.
//!
//! # Why it queues everything before starting
//!
//! The ring is sized to hold the whole signal, so every frame is written
//! *before* the device is clocked. The device therefore cannot run dry
//! however slowly the emulated machine runs: the capture is sample-exact
//! rather than "sample-exact modulo inserted silence", and the driver's
//! reported lost-frame tally is exactly zero rather than merely small. A
//! guest that raced the device would turn a real defect and a slow host into
//! the same observation.
//!
//! # The seat run
//!
//! `audiotone seat` proves the stream follows the seat's lease
//! (`plans/SOUND.md` SND13). Holding the seat itself, it schedules a pause at
//! [`HOLD_FRAME`](tairix_test_audio_wire::HOLD_FRAME) before the device moves,
//! so the segment's end is a named frame rather than a race. It then hands the
//! seat over, is told the stream is held, plays on — which waits for the room
//! — and takes the seat back, and is told the stream resumed on that very
//! frame. Each step waits for the service's own notification, so no step races
//! the one before it.
//!
//! It never self-exits on a failure path other than by saying why: a
//! shortfall prints its reason on `stderr` and exits non-zero, so the run
//! fails loud rather than quietly passing.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::vec;
    use core::fmt::Write as _;

    use tairix_abi::audio::{AudioNotify, OpenParams, StreamRole, StreamState};
    use tairix_abi::driver::audio::{ChannelMap, Frames, Rate, SampleFormat, StreamDirection};
    use tairix_abi::seat::{ReleaseSurface, SEAT_PRIMARY};
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::Errno;
    use tairix_audio::live::{LiveStream, RtAudio};
    use tairix_audio::stream::OpenFailure;
    use tairix_rt::io::{write_stderr_line, Stdout, Write};
    use tairix_test_audio_wire as wire;

    /// Exit code when the audio service refused, or was not there.
    const NO_SERVICE: i32 = 70;
    /// Exit code when the shared ring could not be created or granted.
    const NO_REGION: i32 = 71;
    /// Exit code when the signal did not reach the device intact.
    const NOT_PLAYED: i32 = 72;
    /// Exit code for an argument the fixture does not take.
    const USAGE: i32 = 64;
    /// Exit code when the seat could not be taken or given back.
    const NO_SEAT: i32 = 73;
    /// Exit code when the stream did not follow the seat as it must.
    const NOT_HELD: i32 = 74;

    /// Notifies to take before giving up on the drain completing.
    ///
    /// A bound rather than a timeout: each wake is a real service event, and
    /// a quarter-second signal at this device's period cannot need more than
    /// a few hundred of them. Exceeding it means the stack stopped making
    /// progress, which is a failure to report rather than to wait out.
    const MAX_WAKES: usize = 4_096;

    /// Wait-set token for the stream's notify mailbox.
    const NOTIFY_TOKEN: u64 = 1;

    /// How long one park may wait for the next drain event before the run is
    /// declared stalled. Generous: an emulated machine clocks a period far
    /// slower than the hardware would, and this bounds a wedged run rather
    /// than pacing a healthy one.
    const DRAIN_WAIT_NS: u64 = 10_000_000_000;

    /// Say why the run failed on `stderr`, so an abnormal exit is never
    /// silent, and answer the exit code.
    fn fail(reason: &str, err: Errno, code: i32) -> i32 {
        let mut line = [0u8; 128];
        let mut cursor = Cursor {
            buf: &mut line,
            len: 0,
        };
        // Bounded, well-formed input — a short fixed reason plus a small
        // integer — so an overflow cannot occur; a refused write leaves the
        // prefix written, which still names the failure.
        let _ = write!(cursor, "audiotone: {reason} (errno {})", err.as_i32());
        let len = cursor.len;
        write_stderr_line(core::str::from_utf8(&line[..len]).unwrap_or("audiotone: failed"));
        code
    }

    /// A bounded `core::fmt::Write` sink over a fixed buffer; a write past
    /// the end is refused rather than truncating mid-character.
    struct Cursor<'a> {
        buf: &'a mut [u8; 128],
        len: usize,
    }

    impl core::fmt::Write for Cursor<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            let end = self.len.checked_add(bytes.len()).ok_or(core::fmt::Error)?;
            if end > self.buf.len() {
                return Err(core::fmt::Error);
            }
            self.buf[self.len..end].copy_from_slice(bytes);
            self.len = end;
            Ok(())
        }
    }

    /// Open the stream on the default sink with a ring the whole signal fits
    /// in.
    fn arm(transport: &mut RtAudio) -> Result<LiveStream, i32> {
        let Ok(rate) = Rate::new(wire::RATE_HZ) else {
            return Err(NO_SERVICE);
        };
        let params = OpenParams {
            // Zero names the machine's default sink; the vertical's guest has
            // exactly one.
            device_id: 0,
            direction: StreamDirection::Playback,
            format: SampleFormat::S16,
            rate,
            channel_map: ChannelMap::STEREO,
            role: StreamRole::Media,
            latency_target_frames: wire::RING_FRAMES,
        };
        LiveStream::open(transport, &params).map_err(|failure| match failure {
            OpenFailure::Refused(err) => {
                fail("the audio service refused the stream", err, NO_SERVICE)
            }
            OpenFailure::Notify(err) => {
                fail("the stream notify port would not bind", err, NO_SERVICE)
            }
            OpenFailure::Ring(err) => fail("the service refused the ring", err, NO_REGION),
        })
    }

    /// Wait on the stream's own mailbox until the service tells it `want`,
    /// answering the frame the change took effect at.
    ///
    /// Each park is bounded; running out of wakes or a stalled park is the
    /// stack having stopped making progress, which is reported, not waited
    /// out.
    fn told(
        transport: &mut RtAudio,
        stream: &mut LiveStream,
        set: u64,
        want: StreamState,
    ) -> Result<Frames, i32> {
        for _ in 0..MAX_WAKES {
            while let Ok(Some(notify)) = stream.take_notify(transport) {
                if let AudioNotify::StateChanged { state, at, .. } = notify {
                    if state == want {
                        return Ok(at);
                    }
                }
            }
            let mut token = 0u64;
            if tairix_rt::waitset_wait(set, DRAIN_WAIT_NS, &mut token) != 0 {
                break;
            }
        }
        Err(fail(
            "the service never reported the state awaited",
            Errno::TimedOut,
            NOT_HELD,
        ))
    }

    /// Take the boot seat, or give it up leaving its screen to `next`.
    fn seat(take: Option<ReleaseSurface>) -> Result<(), i32> {
        let ret = match take {
            None => tairix_rt::display_acquire(SEAT_PRIMARY),
            Some(next) => tairix_rt::display_release(SEAT_PRIMARY, next),
        };
        if ret < 0 {
            return Err(fail(
                "the seat would not change hands",
                Errno::from_syscall(ret),
                NO_SEAT,
            ));
        }
        Ok(())
    }

    /// Fail with `reason` unless the service named `at` as [`wire::HOLD_FRAME`].
    fn at_hold(at: Frames, reason: &str) -> Result<(), i32> {
        if at.get() == wire::HOLD_FRAME as u64 {
            return Ok(());
        }
        write_stderr_line(reason);
        Err(NOT_HELD)
    }

    /// The seat run, from a stream whose whole signal is queued to one held
    /// at [`wire::HOLD_FRAME`] and resumed on it.
    fn held_and_resumed(
        transport: &mut RtAudio,
        stream: &mut LiveStream,
        set: u64,
    ) -> Result<(), i32> {
        let hold_frame = Frames::new(wire::HOLD_FRAME as u64);
        seat(None)?;
        stream
            .client_mut()
            .stop(transport, hold_frame)
            .map_err(|err| fail("the pause would not be scheduled", err, NOT_PLAYED))?;
        stream
            .client_mut()
            .start(transport, Frames::ZERO)
            .map_err(|err| fail("the stream would not start", err, NOT_PLAYED))?;
        let paused_at = told(transport, stream, set, StreamState::Paused)?;
        at_hold(
            paused_at,
            "audiotone: the segment did not end on the frame named",
        )?;
        seat(Some(ReleaseSurface::Handover))?;
        let held_at = told(transport, stream, set, StreamState::SeatInactive)?;
        at_hold(
            held_at,
            "audiotone: the stream was not held where it paused",
        )?;
        // Played on outside the room, it waits for the room.
        stream
            .client_mut()
            .start(transport, hold_frame)
            .map_err(|err| fail("the stream would not play on", err, NOT_PLAYED))?;
        seat(None)?;
        let resumed_at = told(transport, stream, set, StreamState::Running)?;
        at_hold(
            resumed_at,
            "audiotone: the stream did not resume where it was held",
        )?;
        stream
            .client_mut()
            .drain(transport)
            .map_err(|err| fail("the stream would not drain", err, NOT_PLAYED))?;
        told(transport, stream, set, StreamState::Idle)?;
        seat(Some(ReleaseSurface::Text))
    }

    /// The plain run: start the queued stream and drain it.
    fn played(transport: &mut RtAudio, stream: &mut LiveStream, set: u64) -> Result<(), i32> {
        stream
            .client_mut()
            .start(transport, Frames::ZERO)
            .map_err(|err| fail("the stream would not start", err, NOT_PLAYED))?;
        stream
            .client_mut()
            .drain(transport)
            .map_err(|err| fail("the stream would not drain", err, NOT_PLAYED))?;
        told(transport, stream, set, StreamState::Idle).map(|_| ())
    }

    fn main() -> i32 {
        let seat_run = match tairix_rt::args().as_deref() {
            Some([]) => false,
            Some([arg]) if *arg == wire::SEAT_ARG => true,
            _ => {
                write_stderr_line("audiotone: usage: audiotone [seat]");
                return USAGE;
            }
        };
        let mut transport = RtAudio::new();
        let mut stream = match arm(&mut transport) {
            Ok(stream) => stream,
            Err(code) => return code,
        };
        // Queue the whole stream *before* the device is clocked: a device
        // that cannot run dry makes the capture exact. The signal is
        // followed by its silent tail, so the signal has left the host
        // backend's buffer before the stream ends.
        let mut samples = vec![0u8; wire::STREAM_FRAMES * wire::FRAME_BYTES];
        if wire::fill_signal(&mut samples) != wire::SIGNAL_FRAMES * wire::FRAME_BYTES {
            return fail(
                "the signal did not fit its buffer",
                Errno::BufferTooSmall,
                NOT_PLAYED,
            );
        }
        match stream.write_at(Frames::ZERO, &samples) {
            Ok(written)
                if written.sample_frames as usize == wire::STREAM_FRAMES
                    && written.silence_frames == 0 => {}
            Ok(_) => {
                write_stderr_line("audiotone: the ring took only part of the signal");
                return NOT_PLAYED;
            }
            Err(err) => return fail("the ring refused the signal", err, NOT_PLAYED),
        }

        // Park on the stream's own mailbox, each park bounded, for every
        // change the run waits on.
        let Ok(set) = u64::try_from(tairix_rt::waitset_create()) else {
            return fail(
                "no wait set for the drain",
                Errno::NotImplemented,
                NOT_PLAYED,
            );
        };
        let joined = transport.notify_port().is_some_and(|port| {
            tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Port,
                port,
                NOTIFY_TOKEN,
            ) == 0
        });
        if !joined {
            return fail(
                "the notify port would not join the wait set",
                Errno::NotImplemented,
                NOT_PLAYED,
            );
        }
        let run = if seat_run {
            held_and_resumed(&mut transport, &mut stream, set)
        } else {
            played(&mut transport, &mut stream, set)
        };
        if let Err(code) = run {
            return code;
        }
        let report = match stream.client_mut().report(&mut transport) {
            Ok(report) => report,
            Err(err) => return fail("the stream state could not be read", err, NOT_PLAYED),
        };
        if report.state != StreamState::Idle {
            write_stderr_line("audiotone: the drain never completed");
            return NOT_PLAYED;
        }
        if report.xrun_frames != 0 {
            write_stderr_line("audiotone: frames were lost on a ring sized to hold them all");
            return NOT_PLAYED;
        }
        // Close what was opened, before claiming success: the endpoint is
        // released only when its last stream goes, and a device told to
        // release is a device that has finished with the frames rather than
        // one still holding some.
        if let Err(err) = stream.close(&mut transport) {
            return fail("the stream would not close", err, NOT_PLAYED);
        }
        let mut marker = [0u8; 128];
        let mut cursor = Cursor {
            buf: &mut marker,
            len: 0,
        };
        let marker_line = if seat_run {
            wire::SEAT_PASS_MARKER
        } else {
            wire::PASS_MARKER
        };
        let _ = writeln!(cursor, "{marker_line}");
        let len = cursor.len;
        if Stdout.write_all(&marker[..len]).is_err() {
            write_stderr_line("audiotone: the pass report could not be written");
            return NOT_PLAYED;
        }
        0
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
#[cfg(not(freestanding))]
fn main() {}
