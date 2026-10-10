//! The engine driven over the modelled channel, with a codec that marks each
//! kind of frame so a test can tell encoded, silent and parked words apart.

extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{Frames, Rate, SampleFormat};
use tairix_abi::driver::audio_ring::{PcmGeometry, PcmRing};
use tairix_abi::DriverError;

use super::mock::Channel;
use super::{frame_bytes, CyclicPlayback, FrameCodec, MAX_PERIOD_FRAMES, PERIODS};

const PERIOD: u32 = 512;
const SILENT: u32 = 0x5151_5151;
const PARKED: u32 = 0xA5A5_A5A5;

/// Signed 32-bit frames, each sample encoded as itself plus one; silence and
/// parking are marked.
#[derive(Default)]
struct Marker {
    resets: u32,
}

fn mark(frame: &mut [u8], word: u32) {
    for slot in frame.as_chunks_mut::<4>().0 {
        *slot = word.to_le_bytes();
    }
}

impl FrameCodec for Marker {
    fn format(&self) -> SampleFormat {
        SampleFormat::S32
    }
    fn encode(&mut self, frame: &mut [u8]) {
        for word in frame.as_chunks_mut::<4>().0 {
            *word = i32::from_le_bytes(*word).wrapping_add(1).to_le_bytes();
        }
    }
    fn silence(&mut self, frame: &mut [u8]) {
        mark(frame, SILENT);
    }
    fn parked(&self, frame: &mut [u8]) {
        mark(frame, PARKED);
    }
    fn reset(&mut self) {
        self.resets += 1;
    }
}

type Playback = CyclicPlayback<Channel, Marker>;

fn rate() -> Rate {
    Rate::new(48_000).expect("a rate")
}

fn playback() -> Playback {
    let mut playback = CyclicPlayback::new(Channel::default(), Marker::default());
    playback.configure(rate(), PERIOD).expect("configured");
    playback
}

/// A ring of `frames`, and the region it lives in.
struct Ring {
    bytes: Vec<u8>,
    geometry: PcmGeometry,
}

impl Ring {
    fn new(frames: u32) -> Self {
        let geometry = PcmGeometry::new(frames, SampleFormat::S32, 2).expect("geometry");
        Self {
            bytes: vec![0; geometry.region_len()],
            geometry,
        }
    }

    fn ring(&mut self) -> PcmRing<'_> {
        PcmRing::bind(&mut self.bytes, self.geometry).expect("ring")
    }

    /// Write `count` frames whose samples are both `sample`.
    fn write(&mut self, sample: i32, count: u32) {
        let mut out = Vec::new();
        for _ in 0..count {
            out.extend_from_slice(&sample.to_le_bytes());
            out.extend_from_slice(&sample.to_le_bytes());
        }
        let written = self.ring().write(&out).expect("written");
        assert_eq!(written, count);
    }
}

fn started(playback: &mut Playback, ring: &mut Ring, staged: u32, at: u64) {
    ring.write(7, PERIOD * staged);
    playback.service(&mut ring.ring()).expect("staged");
    playback.set_events(true).expect("unmasked");
    playback.start(Frames::new(at)).expect("started");
}

#[test]
fn configuring_prepares_the_buffer_parked() {
    let mut playback = playback();
    let channel = playback.dma_mut();
    assert_eq!(channel.periods, PERIODS);
    assert_eq!(channel.period_bytes, PERIOD * 8);
    assert!(channel
        .buffer
        .as_chunks::<4>()
        .0
        .iter()
        .all(|word| u32::from_le_bytes(*word) == PARKED));
}

#[test]
fn staged_frames_play_encoded_in_their_order() {
    let mut playback = playback();
    let mut ring = Ring::new(PERIOD * 8);
    started(&mut playback, &mut ring, PERIODS, 0);
    let channel = playback.dma_mut();
    channel.advance(PERIODS);
    assert!(
        channel.sent.iter().all(|&word| word == 8),
        "each sample encoded"
    );
}

#[test]
fn each_boundary_refills_the_freed_period_and_reports_the_position_played() {
    let mut playback = playback();
    let mut ring = Ring::new(PERIOD * 8);
    started(&mut playback, &mut ring, PERIODS, 1_000);
    playback.dma_mut().advance(1);
    assert_eq!(
        playback
            .take_interrupt()
            .expect("a boundary")
            .period_elapsed,
        1
    );
    ring.write(9, PERIOD);
    let serviced = playback.service(&mut ring.ring()).expect("serviced");
    assert_eq!(serviced.transferred, PERIOD);
    assert!(serviced.running);
    assert_eq!(serviced.position, Frames::new(1_000 + u64::from(PERIOD)));
    assert_eq!(serviced.xrun_frames, 0);
    assert!(
        playback.dma_mut().is_posted(),
        "the next boundary is waited for"
    );
}

#[test]
fn a_dry_ring_at_a_boundary_writes_silence_counted_lost() {
    let mut playback = playback();
    let mut ring = Ring::new(PERIOD * 8);
    started(&mut playback, &mut ring, 2, 0);
    playback.dma_mut().advance(1);
    playback.take_interrupt().expect("a boundary");
    let serviced = playback.service(&mut ring.ring()).expect("serviced");
    assert_eq!(serviced.transferred, 0);
    assert_eq!(
        serviced.xrun_frames,
        u64::from(PERIOD),
        "the period due next"
    );
    let channel = playback.dma_mut();
    channel.advance(2);
    let due = &channel.sent[(PERIOD * 2 * 2) as usize..];
    assert!(
        due.iter().all(|&word| word == SILENT),
        "the silence, not a lap-old period"
    );
}

#[test]
fn starting_with_nothing_staged_plays_two_periods_of_silence_counted_lost() {
    let mut playback = playback();
    playback.set_events(true).expect("unmasked");
    playback.start(Frames::ZERO).expect("started");
    let mut ring = Ring::new(PERIOD * 8);
    let serviced = playback.service(&mut ring.ring()).expect("serviced");
    assert_eq!(serviced.xrun_frames, u64::from(PERIOD) * 2);
}

#[test]
fn stopping_parks_and_halts_the_channel_at_its_next_boundary() {
    let mut playback = playback();
    let mut ring = Ring::new(PERIOD * 8);
    started(&mut playback, &mut ring, PERIODS, 0);
    playback.stop(Frames::new(777)).expect("stopped");
    assert!(
        playback.dma_mut().running,
        "still clocking until its boundary"
    );
    playback.dma_mut().advance(1);
    assert_eq!(
        playback
            .take_interrupt()
            .expect("the boundary")
            .period_elapsed,
        0
    );
    let channel = playback.dma_mut();
    assert!(!channel.running, "halted at the boundary");
    assert_eq!(
        channel.sent.last(),
        Some(&PARKED),
        "left on the parked frame"
    );
    let mut empty = Ring::new(PERIOD * 8);
    let serviced = playback.service(&mut empty.ring()).expect("serviced");
    assert!(!serviced.running);
    assert_eq!(serviced.position, Frames::new(777), "the position kept");
}

#[test]
fn a_drain_ends_at_the_position_its_last_frame_played() {
    let mut playback = playback();
    let mut ring = Ring::new(PERIOD * 8);
    ring.write(7, PERIOD * 2 + 100);
    playback.service(&mut ring.ring()).expect("staged");
    playback.set_events(true).expect("unmasked");
    playback.start(Frames::new(5_000)).expect("started");
    playback.drain().expect("draining");
    let mut finished = None;
    for _ in 0..8 {
        playback.dma_mut().advance(1);
        if playback
            .take_interrupt()
            .expect("a boundary")
            .period_elapsed
            != 0
        {
            let serviced = playback.service(&mut ring.ring()).expect("serviced");
            if !serviced.running {
                finished = Some(serviced);
                break;
            }
        }
    }
    let serviced = finished.expect("the drain finished");
    assert_eq!(
        serviced.position,
        Frames::new(5_000 + u64::from(PERIOD) * 2 + 100)
    );
    assert_eq!(serviced.xrun_frames, 0, "a drain's tail is not lost");
}

#[test]
fn a_drain_with_nothing_queued_ends_at_once() {
    let mut playback = playback();
    playback.set_events(true).expect("unmasked");
    playback.start(Frames::ZERO).expect("started");
    playback.drain().expect("drained");
    assert!(!playback.is_running());
}

#[test]
fn masking_while_running_leaves_nothing_to_replay() {
    let mut playback = playback();
    let mut ring = Ring::new(PERIOD * 8);
    started(&mut playback, &mut ring, PERIODS, 0);
    playback.set_events(false).expect("masked");
    let channel = playback.dma_mut();
    channel.advance(PERIODS);
    assert!(channel.sent.iter().all(|&word| word == PARKED));
}

#[test]
fn a_faulted_channel_ends_the_stream_with_the_fault() {
    let mut playback = playback();
    playback.dma_mut().fault = true;
    playback.set_events(true).expect("unmasked");
    playback.start(Frames::ZERO).expect("started");
    playback.dma_mut().advance(1);
    assert!(matches!(
        playback.take_interrupt(),
        Err(DriverError::DeviceFault)
    ));
    assert!(!playback.is_running());
}

#[test]
fn a_ring_of_another_shape_is_refused() {
    let mut playback = playback();
    let geometry = PcmGeometry::new(PERIOD * 8, SampleFormat::S16, 2).expect("geometry");
    let mut bytes = vec![0; geometry.region_len()];
    let mut ring = PcmRing::bind(&mut bytes, geometry).expect("ring");
    assert!(matches!(
        playback.service(&mut ring),
        Err(DriverError::BadMagic)
    ));
}

#[test]
fn playing_once_sends_the_frames_then_parks_with_the_channel_stopped() {
    let mut playback = CyclicPlayback::new(Channel::default(), Marker::default());
    playback
        .play_once(64, |frame, words| mark(words, frame))
        .expect("played");
    let channel = playback.dma_mut();
    let played: Vec<u32> = channel.sent[..128].iter().copied().step_by(2).collect();
    assert_eq!(played, (0..64).collect::<Vec<u32>>());
    assert!(!channel.running);
    assert!(!channel.is_posted());
}

#[test]
fn a_stream_starting_afresh_resets_its_codec() {
    let mut playback = playback();
    assert_eq!(playback.codec.resets, 1, "configured");
    let mut ring = Ring::new(PERIOD * 8);
    started(&mut playback, &mut ring, PERIODS, 0);
    assert_eq!(playback.codec.resets, 1, "a start keeps what staging began");
    playback.stop(Frames::ZERO).expect("stopped");
    assert_eq!(playback.codec.resets, 2, "stopped");
}

#[test]
fn a_period_of_no_frames_or_past_the_buffers_bound_is_refused() {
    let mut playback = playback();
    for frames in [0, MAX_PERIOD_FRAMES + 1, u32::MAX] {
        assert_eq!(
            playback.configure(rate(), frames),
            Err(DriverError::OutOfRange)
        );
        assert_eq!(
            playback.play_once(frames, |_, _| {}),
            Err(DriverError::OutOfRange)
        );
    }
    assert_eq!(playback.configure(rate(), MAX_PERIOD_FRAMES), Ok(()));
}

#[test]
fn a_parking_channel_clocks_until_its_boundary_or_until_halted() {
    let mut playback = playback();
    let mut ring = Ring::new(PERIOD * 8);
    started(&mut playback, &mut ring, PERIODS, 0);
    assert_eq!(
        playback.halt_parking(),
        Err(DriverError::Busy),
        "a stream runs"
    );
    playback.stop(Frames::ZERO).expect("stopped");
    assert!(playback.is_clocking(), "parking");
    playback.dma_mut().advance(1);
    playback.take_interrupt().expect("the boundary");
    assert!(!playback.is_clocking(), "halted at its boundary");

    started(&mut playback, &mut ring, PERIODS, 0);
    playback.stop(Frames::ZERO).expect("stopped");
    playback.halt_parking().expect("halted");
    assert!(!playback.is_clocking());
    assert!(!playback.dma_mut().running, "at once");
    assert!(!playback.dma_mut().is_posted(), "its wait collected");
}

#[test]
fn a_frame_is_whole_fifo_words_of_two_channels() {
    assert_eq!(frame_bytes(SampleFormat::S32), Ok(8));
    assert_eq!(frame_bytes(SampleFormat::S24In32), Ok(8));
    assert_eq!(frame_bytes(SampleFormat::S16), Ok(4), "two samples a word");
    for format in [SampleFormat::U8, SampleFormat::S24] {
        assert_eq!(frame_bytes(format), Err(DriverError::Unsupported));
    }
}

/// Signed 16-bit frames, two to a FIFO word, copied as they are.
struct Packed;

impl FrameCodec for Packed {
    fn format(&self) -> SampleFormat {
        SampleFormat::S16
    }
    fn encode(&mut self, _frame: &mut [u8]) {}
    fn silence(&mut self, frame: &mut [u8]) {
        frame.fill(0);
    }
    fn parked(&self, frame: &mut [u8]) {
        frame.fill(0);
    }
    fn reset(&mut self) {}
}

#[test]
fn a_narrower_format_streams_frames_of_its_own_size_from_a_ring_of_it() {
    let mut playback = CyclicPlayback::new(Channel::default(), Packed);
    playback.configure(rate(), PERIOD).expect("configured");
    assert_eq!(playback.dma_mut().period_bytes, PERIOD * 4);
    let geometry = PcmGeometry::new(PERIOD * 8, SampleFormat::S16, 2).expect("geometry");
    let mut bytes = vec![0; geometry.region_len()];
    let mut ring = PcmRing::bind(&mut bytes, geometry).expect("ring");
    let mut frames = Vec::new();
    for _ in 0..PERIOD * PERIODS {
        frames.extend_from_slice(&0x1234_i16.to_le_bytes());
        frames.extend_from_slice(&(-2_i16).to_le_bytes());
    }
    ring.write(&frames).expect("written");
    playback.service(&mut ring).expect("staged");
    playback.set_events(true).expect("unmasked");
    playback.start(Frames::ZERO).expect("started");
    let channel = playback.dma_mut();
    channel.advance(PERIODS);
    assert!(
        channel.sent.iter().all(|&word| word == 0xFFFE_1234),
        "the left sample in the low half, as the ring holds it"
    );
    let s32 = PcmGeometry::new(PERIOD * 8, SampleFormat::S32, 2).expect("geometry");
    let mut wide = vec![0; s32.region_len()];
    let mut ring = PcmRing::bind(&mut wide, s32).expect("ring");
    assert!(matches!(
        playback.service(&mut ring),
        Err(DriverError::BadMagic)
    ));
}
