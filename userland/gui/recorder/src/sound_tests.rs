//! Host tests of a recording's sound: its timeline from the first frame
//! heard, lost frames heard as silence, a full batch, the tail, and the
//! microphone carried onto the desktop's clock and mixed.

use alloc::vec::Vec;

use tairix_abi::audio::CaptureSource;
use tairix_abi::driver::audio::ChannelMap;
use tairix_abi::recorder_ipc::RecordSound;
use tairix_audio::{ChannelMatrix, Ratio};

use super::{drift_between, Sound, BATCH_PACKETS, JOIN_AFTER_FRAMES, JOIN_BY_FRAMES, PERIOD};
use crate::testing::{clock_report, FakeSound};

const MS: u64 = 1_000_000;

/// The sound `choice` hears over fake streams, and each stream with what it
/// hears.
fn opened(choice: RecordSound) -> (Sound<FakeSound>, Vec<(CaptureSource, FakeSound)>) {
    let mut streams = Vec::new();
    let sound = Sound::open(choice, |source, layout| {
        let stream = FakeSound::open(layout);
        streams.push((source, stream.clone()));
        Ok(stream)
    })
    .expect("the sound opens")
    .expect("it hears something");
    (sound, streams)
}

/// The time of every packet waiting for the encoding thread.
fn times(sound: &mut Sound<FakeSound>) -> Vec<u64> {
    let batch = sound.take_batch().expect("packets wait");
    let times = batch.packets().map(|(at_ns, _)| at_ns).collect();
    sound.give_back(batch);
    times
}

#[test]
fn packets_are_timed_from_the_first_frame_heard() {
    let (mut sound, streams) = opened(RecordSound::Microphone);
    sound.start(0, 7 * MS).expect("starts");
    streams[0].1.capture(3 * PERIOD, 0.25);
    sound.hear().expect("hears");
    assert_eq!(times(&mut sound), [7 * MS, 27 * MS, 47 * MS]);
}

#[test]
fn lost_frames_are_heard_as_silence_so_the_timeline_holds() {
    let (mut sound, streams) = opened(RecordSound::Desktop);
    sound.start(0, 0).expect("starts");
    let desktop = &streams[0].1;
    desktop.capture(PERIOD, 0.25);
    sound.hear().expect("hears");
    desktop.0.borrow_mut().lost = PERIOD as u64;
    desktop.capture(PERIOD, 0.25);
    sound.hear().expect("hears");
    assert_eq!(times(&mut sound), [0, 20 * MS, 40 * MS]);
}

#[test]
fn a_full_batch_loses_packets_and_says_so_once() {
    let (mut sound, streams) = opened(RecordSound::Microphone);
    sound.start(0, 0).expect("starts");
    for _ in 0..=BATCH_PACKETS {
        streams[0].1.capture(PERIOD, 0.0);
        sound.hear().expect("hears");
    }
    assert_eq!(sound.take_lost(), 1);
    assert_eq!(sound.take_lost(), 0, "a loss is counted once");
    assert_eq!(times(&mut sound).len(), BATCH_PACKETS);
}

#[test]
fn the_tail_is_padded_and_the_coders_delay_played_out() {
    let (mut sound, streams) = opened(RecordSound::Microphone);
    sound.start(0, 0).expect("starts");
    streams[0].1.capture(PERIOD + PERIOD / 2, 0.25);
    sound.finish().expect("finishes");
    assert_eq!(times(&mut sound), [0, 20 * MS, 40 * MS]);
}

#[test]
fn both_mixes_the_microphone_in_once_both_clocks_are_measured() {
    let (mut sound, streams) = opened(RecordSound::Both);
    let sources: Vec<CaptureSource> = streams.iter().map(|(source, _)| *source).collect();
    assert_eq!(
        sources,
        [CaptureSource::Monitor(0), CaptureSource::Input(0)]
    );
    let (desktop, microphone) = (&streams[0].1, &streams[1].1);
    desktop.clocked(48_000_000);
    microphone.clocked(48_000_000);
    sound.start(0, 0).expect("starts");
    let weight = ChannelMatrix::derive(&ChannelMap::MONO, &ChannelMap::STEREO)
        .expect("mono reaches stereo")
        .weight(0, 0);
    let joins = usize::try_from(JOIN_AFTER_FRAMES).expect("small") / PERIOD;
    for period in 1..=joins {
        desktop.capture(PERIOD, 0.5);
        microphone.capture(PERIOD, 0.25);
        sound.hear().expect("hears");
        let expected = if period < joins {
            0.5_f32
        } else {
            0.5 + 0.25 * weight
        };
        assert!(
            sound
                .last_mix()
                .iter()
                .all(|sample| sample.to_bits() == expected.to_bits()),
            "period {period}"
        );
    }
}

#[test]
fn the_microphone_joins_on_the_nominal_rates_where_none_is_measured() {
    let (mut sound, streams) = opened(RecordSound::Both);
    sound.start(0, 0).expect("starts");
    let joins = usize::try_from(JOIN_BY_FRAMES).expect("small") / PERIOD;
    for period in 1..=joins {
        streams[0].1.capture(PERIOD, 0.5);
        streams[1].1.capture(PERIOD, 0.25);
        sound.hear().expect("hears");
        let joined = sound
            .last_mix()
            .iter()
            .any(|sample| sample.to_bits() != 0.5_f32.to_bits());
        assert_eq!(joined, period == joins, "period {period}");
    }
}

#[test]
fn a_drift_between_two_clocks_is_their_measured_ratio() {
    let output = clock_report(48_000_000);
    assert_eq!(drift_between(&output, &output), Ratio::new(1, 1));
    let ahead = Ratio::new(1_000_010, 1_000_000);
    assert_eq!(
        drift_between(&clock_report(44_100_441), &output),
        ahead,
        "a microphone running fast against its own standard rate"
    );
    assert_eq!(
        drift_between(&output, &clock_report(47_999_520)),
        ahead,
        "an output running slow"
    );
    assert_eq!(
        drift_between(&clock_report(40_000_000), &output),
        None,
        "no clock drifts nine per cent"
    );
}
