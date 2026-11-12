use super::*;
use crate::sound::testing::cycles;
use alloc::vec;
use alloc::vec::Vec;

/// Timer 2 in `mode` with `count`, its gate high.
fn timer(mode: u8, count: u16) -> Pit {
    let mut pit = Pit::new();
    pit.write(3, 0x80 | 0x30 | (mode << 1), 0);
    let [low, high] = count.to_le_bytes();
    pit.write(2, low, 0);
    pit.write(2, high, 0);
    pit.set_gate(2, true, 0);
    pit
}

fn heard(speaker: &mut Speaker, pit: &Pit, ticks: u64, mixer: &mut Mixer) -> Vec<f32> {
    speaker.advance(pit, ticks, mixer);
    mixer.render(ticks * PIT_TICKS);
    let mut out = vec![0.0f32; 2 * 100_000];
    let frames = mixer.take(&mut out);
    out[..2 * frames]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|frame| frame[0])
        .collect()
}

#[test]
fn timer_2s_square_wave_sounds_while_the_data_bit_is_on() {
    let pit = timer(3, 1_193);
    let mut mixer = Mixer::new(48_000).unwrap();
    let mut speaker = Speaker::new();
    let second = 1_193_182;
    let silent = heard(&mut speaker, &pit, second / 10, &mut mixer);
    assert!(silent.iter().all(|&x| x.abs() < 1e-6), "the data bit off");
    speaker.set_data(true, &pit, second / 10, &mut mixer);
    let tone = heard(&mut speaker, &pit, second / 10 + second, &mut mixer);
    let seconds = tairix_util::mathf::real(tone.len()) / 48_000.0;
    let rate = tairix_util::mathf::real(cycles(&tone[4_800..])) / (seconds - 0.1);
    assert!((rate - 1_000.15).abs() < 3.0, "{rate} Hz");
}

#[test]
fn an_ultrasonic_carrier_is_heard_as_its_duty() {
    let pit = timer(3, 10);
    let mut mixer = Mixer::new(48_000).unwrap();
    let mut speaker = Speaker::new();
    speaker.set_data(true, &pit, 1_000, &mut mixer);
    let out = heard(&mut speaker, &pit, 1_193_182, &mut mixer);
    assert_eq!(speaker.heard, (5, 10), "the cone at half");
    assert!(out[24_000..].iter().all(|&x| x.abs() < 1e-3), "and no tone");
}

#[test]
fn the_data_bit_alone_moves_the_cone() {
    // Mode 3 with its gate low holds the output high, so toggling the data
    // bit alone is the speaker's sample playback.
    let mut pit = timer(3, 1_193);
    pit.set_gate(2, false, 0);
    let mut mixer = Mixer::new(48_000).unwrap();
    let mut speaker = Speaker::new();
    for n in 0..2_000u64 {
        speaker.set_data(n % 2 == 0, &pit, 1_000 + n * 600, &mut mixer);
    }
    let out = heard(&mut speaker, &pit, 1_000 + 2_000 * 600, &mut mixer);
    assert!(out.iter().any(|&x| x.abs() > 0.1), "a click train");
}
