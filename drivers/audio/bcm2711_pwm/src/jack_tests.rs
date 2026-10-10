//! The jack's own part over the shared engine and its modelled channel: the
//! facts, the bring-up ramp, and frames shaped in the FIFO's order. The
//! stream's behaviour is the engine's, tested with it.

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{
    Audio, ChannelMap, Frames, JackState, Rate, RateSupport, SampleFormat, StreamDirection,
};
use tairix_abi::driver::audio_channel::ConfigureParams;
use tairix_abi::driver::audio_ring::{PcmGeometry, PcmRing};
use tairix_abi::DriverError;
use tairix_audiochan::cyclic::mock::Channel;
use tairix_audiochan::cyclic::PERIODS;

use super::{Jack, MIN_PERIOD_FRAMES, RAMP_FRAMES};

const LEVELS: u32 = 250;
const SILENCE: u32 = LEVELS / 2;
const PERIOD: u32 = MIN_PERIOD_FRAMES;

fn rate() -> Rate {
    Rate::new(375_000).expect("a rate")
}

fn jack() -> Jack<Channel> {
    Jack::new(Channel::default(), rate(), LEVELS, 7).expect("a jack")
}

fn configure(jack: &mut Jack<Channel>) {
    jack.configure(
        0,
        &ConfigureParams {
            endpoint: 0,
            rate: rate(),
            format: SampleFormat::S32,
            channel_map: ChannelMap::STEREO,
            period_frames: PERIOD,
        },
    )
    .expect("configured");
}

#[test]
fn one_stereo_playback_endpoint_is_offered_at_the_jacks_rate_alone() {
    let jack = jack();
    assert_eq!(jack.device_facts().expect("facts").endpoints, 1);
    let facts = jack.endpoint_facts(0).expect("facts");
    assert_eq!(facts.direction, StreamDirection::Playback);
    assert_eq!(facts.jack, JackState::Unknown);
    assert_eq!(facts.channel_map, ChannelMap::STEREO);
    assert!(facts.formats.contains(SampleFormat::S32));
    assert!(matches!(facts.rates, RateSupport::Discrete(set) if set.contains(rate())));
    assert!(facts.gain.is_none(), "the mixer applies the gain");
    assert!(facts.validate().is_ok());
    assert!(matches!(jack.endpoint_facts(1), Err(DriverError::NotFound)));
}

#[test]
fn bring_up_ramps_from_low_to_silence_and_leaves_the_channel_stopped() {
    let mut jack = jack();
    jack.bring_up().expect("brought up");
    let channel = jack.playback.dma_mut();
    let ramp = &channel.sent[..(RAMP_FRAMES * 2) as usize];
    assert_eq!(ramp[0], 0);
    assert!(ramp.windows(2).all(|pair| pair[1] >= pair[0]), "monotone");
    let last = ramp[ramp.len() - 1];
    assert!(
        last + 1 >= SILENCE && last <= SILENCE,
        "ends at silence: {last}"
    );
    assert!(!channel.running);
}

#[test]
fn configuring_parks_the_buffer_on_silence() {
    let mut jack = jack();
    configure(&mut jack);
    let channel = jack.playback.dma_mut();
    assert_eq!(channel.periods, PERIODS);
    assert!(channel
        .buffer
        .as_chunks::<4>()
        .0
        .iter()
        .all(|word| u32::from_le_bytes(*word) == SILENCE));
}

#[test]
fn staged_frames_play_shaped_with_the_right_side_first() {
    let mut jack = jack();
    configure(&mut jack);
    let geometry = PcmGeometry::new(PERIOD * 8, SampleFormat::S32, 2).expect("geometry");
    let mut bytes = vec![0; geometry.region_len()];
    let mut ring = PcmRing::bind(&mut bytes, geometry).expect("ring");
    // Left high, right low: the right side's words come first.
    let mut frames = Vec::new();
    for _ in 0..PERIOD * PERIODS {
        frames.extend_from_slice(&(i32::MAX / 2).to_le_bytes());
        frames.extend_from_slice(&(i32::MIN / 2).to_le_bytes());
    }
    ring.write(&frames).expect("written");
    let staged = jack.service(0, &mut ring).expect("serviced");
    assert_eq!(staged.transferred, PERIOD * PERIODS);
    jack.set_event_interrupts(true).expect("unmasked");
    jack.start(0, Frames::new(1000)).expect("started");
    let channel = jack.playback.dma_mut();
    channel.advance(PERIODS);
    let (mut right, mut left) = (0u32, 0u32);
    for [first, second] in channel.sent.as_chunks::<2>().0 {
        right += first;
        left += second;
    }
    let count = f64::from(u32::try_from(channel.sent.len() / 2).expect("a count"));
    let (right, left) = (f64::from(right) / count, f64::from(left) / count);
    assert!(
        (left - (f64::from(SILENCE) + 56.5)).abs() < 0.5,
        "left {left}"
    );
    assert!(
        (right - (f64::from(SILENCE) - 56.5)).abs() < 0.5,
        "right {right}"
    );
}
