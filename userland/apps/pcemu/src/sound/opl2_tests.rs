use super::*;
use alloc::vec::Vec;

/// The mix's rate, at which the chip makes its samples.
const RATE: f64 = 315_000_000.0 / 6_336.0;

fn write(chip: &mut Opl2, address: u8, value: u8) {
    chip.select(address);
    chip.write(value);
}

/// The F-number's two bits in register B0h.
fn high(f_number: u16) -> u8 {
    f_number.to_le_bytes()[1]
}

fn run(chip: &mut Opl2, samples: u64) -> Vec<i16> {
    let mut out = Vec::new();
    let until = chip.samples() + samples;
    chip.run_to(until, &mut |_, sample| out.push(sample));
    out
}

/// Channel 0 as a sine at full level: the modulator never attacks, the
/// carrier attacks at once and holds.
fn sine(chip: &mut Opl2, f_number: u16, block: u8) {
    write(chip, 0x20, 0x01);
    write(chip, 0x23, 0x21);
    write(chip, 0x40, 0x3F);
    write(chip, 0x43, 0x00);
    write(chip, 0x60, 0x00);
    write(chip, 0x63, 0xF0);
    write(chip, 0x80, 0x00);
    write(chip, 0x83, 0x00);
    let [low, high] = f_number.to_le_bytes();
    write(chip, 0xA0, low);
    write(chip, 0xB0, 0x20 | (block << 2) | high);
}

#[test]
fn the_tables_follow_their_formulas() {
    let tables = Tables::new();
    assert_eq!(tables.log_sin[0], 2137);
    assert_eq!(tables.log_sin[255], 0);
    assert_eq!((tables.exp[0], tables.exp[255]), (0, 1018));
    assert!(tables.log_sin.windows(2).all(|pair| pair[0] >= pair[1]));
    assert_eq!(tables.wave(0x100, 0, 0), 4084, "the peak");
    assert_eq!(tables.wave(0x300, 0, 0), -4084, "the trough");
    assert_eq!(tables.wave(0x100, 256, 0), 2042, "6 dB down halves it");
    assert_eq!(tables.wave(0x300, 0, 1), 0, "the half sine's second half");
    assert_eq!(tables.wave(0x300, 0, 2), 4084, "the absolute sine");
    assert_eq!(
        tables.wave(0x180, 0, 3),
        0,
        "the quarter sine's second quarter"
    );
}

#[test]
fn a_note_sounds_at_its_frequency() {
    let mut chip = Opl2::new();
    sine(&mut chip, 577, 4);
    let out = run(&mut chip, 49_716);
    let rises = out
        .windows(2)
        .filter(|pair| pair[0] < 0 && pair[1] >= 0)
        .count();
    let expected = 577.0 * 16.0 * RATE / f64::from(1u32 << 20);
    assert!(
        (f64::from(u32::try_from(rises).unwrap()) - expected).abs() <= 1.0,
        "{rises} cycles a second against {expected}"
    );
    let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap();
    // The DAC truncates toward minus infinity, so the trough reaches 4088.
    assert!((4_000..=4_088).contains(&peak), "full level: {peak}");
}

#[test]
fn the_envelope_takes_the_times_the_manual_gives() {
    let mut chip = Opl2::new();
    sine(&mut chip, 577, 0);
    // Attack rate 1 and decay rate 2, no key scaling: rates 4 and 8.
    write(&mut chip, 0x63, 0x12);
    write(&mut chip, 0x83, 0xF0);
    write(&mut chip, 0xB0, 0x20 | high(577));
    let mut attack = 0u64;
    while chip.operators[3].stage == Stage::Attack {
        run(&mut chip, 1);
        attack += 1;
    }
    let seconds = |samples: u64| f64::from(u32::try_from(samples).unwrap()) / RATE;
    // The manual's 2826.24 ms at 3.6 MHz, at the bus's 3.58 MHz.
    let manual = 2.826_24 * 3.6 / 3.579_545;
    assert!(
        (seconds(attack) - manual).abs() < manual * 0.03,
        "attack in {} s",
        seconds(attack)
    );
    let start = chip.samples();
    while chip.operators[3].envelope < 480 {
        run(&mut chip, 1);
    }
    let manual = 19.640_32 * 3.6 / 3.579_545;
    let decay = seconds(chip.samples() - start);
    assert!(
        (decay - manual).abs() < manual * 0.01,
        "decay to 90 dB in {decay} s"
    );
}

#[test]
fn a_key_off_releases_at_the_release_rate() {
    let mut chip = Opl2::new();
    sine(&mut chip, 577, 4);
    write(&mut chip, 0x83, 0x0F);
    run(&mut chip, 1_000);
    write(&mut chip, 0xB0, (4 << 2) | high(577));
    let out = run(&mut chip, 2_000);
    assert!(
        out[1_000..].iter().all(|&s| s == 0),
        "silent within a release"
    );
    assert_eq!(chip.operators[3].stage, Stage::Release);
}

#[test]
fn timers_flag_their_overflows_as_the_adlib_test_expects() {
    let mut chip = Opl2::new();
    write(&mut chip, 0x04, 0x60);
    write(&mut chip, 0x04, 0x80);
    assert_eq!(chip.status() & 0xE0, 0, "both clear");
    assert_eq!(chip.status() & 0x1F, 0x06, "an OPL2, not an OPL3");
    write(&mut chip, 0x02, 0xFF);
    write(&mut chip, 0x04, 0x21);
    run(&mut chip, 4);
    assert_eq!(chip.status() & 0xE0, 0xC0, "timer 1 overflowed in 80 µs");
    write(&mut chip, 0x04, 0x60);
    write(&mut chip, 0x04, 0x80);
    assert_eq!(chip.status(), 0x06);
    write(&mut chip, 0x03, 0x00);
    write(&mut chip, 0x04, 0x02);
    let mut samples = 0;
    while chip.status() & 0x20 == 0 {
        run(&mut chip, 1);
        samples += 1;
    }
    assert!(
        samples > 255 * 16 && samples <= 256 * 16,
        "timer 2: {samples} samples"
    );
}

#[test]
fn a_masked_timer_runs_but_raises_nothing() {
    let mut chip = Opl2::new();
    write(&mut chip, 0x02, 0xFF);
    write(&mut chip, 0x04, 0x41);
    run(&mut chip, 64);
    assert_eq!(chip.status() & 0xE0, 0);
}

#[test]
fn the_waveforms_need_their_enable() {
    let mut chip = Opl2::new();
    sine(&mut chip, 577, 4);
    write(&mut chip, 0xE3, 0x01);
    assert!(
        run(&mut chip, 500).iter().any(|&s| s < 0),
        "a sine until enabled"
    );
    write(&mut chip, 0x01, 0x20);
    let out = run(&mut chip, 500);
    assert!(
        out.iter().all(|&s| s >= 0) && out.iter().any(|&s| s > 0),
        "the half sine"
    );
}

#[test]
fn the_additive_connection_sounds_both_operators() {
    let mut chip = Opl2::new();
    sine(&mut chip, 577, 4);
    write(&mut chip, 0x60, 0xF0);
    write(&mut chip, 0x40, 0x00);
    write(&mut chip, 0xC0, 0x01);
    write(&mut chip, 0xB0, 0x20 | (4 << 2) | high(577));
    let peak = run(&mut chip, 500)
        .iter()
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 8_000, "two in phase: {peak}");
}

#[test]
fn rhythm_mode_sounds_the_drums_it_is_keyed_for() {
    let mut chip = Opl2::new();
    for op in [12u8, 13, 14, 15, 16, 17] {
        let offset = [0x10u8, 0x11, 0x12, 0x13, 0x14, 0x15][usize::from(op - 12)];
        write(&mut chip, 0x60 + offset, 0xF0);
        write(&mut chip, 0x80 + offset, 0x0F);
    }
    for channel in [6u8, 7, 8] {
        write(&mut chip, 0xA0 + channel, 0x80);
        write(&mut chip, 0xB0 + channel, 0x09);
    }
    write(&mut chip, 0xBD, 0x20);
    assert!(
        run(&mut chip, 2_000).iter().all(|&s| s == 0),
        "nothing keyed"
    );
    write(&mut chip, 0xBD, 0x21);
    let hats = run(&mut chip, 2_000);
    assert!(hats.iter().any(|&s| s != 0), "the hi-hat");
    write(&mut chip, 0xBD, 0x20);
    run(&mut chip, 4_000);
    write(&mut chip, 0xBD, 0x30);
    assert!(
        run(&mut chip, 2_000).iter().any(|&s| s != 0),
        "the bass drum"
    );
}

#[test]
fn composite_sine_mode_keys_every_channel_on_timer_1() {
    let mut chip = Opl2::new();
    sine(&mut chip, 577, 4);
    write(&mut chip, 0x83, 0x0F);
    write(&mut chip, 0xB0, (4 << 2) | high(577));
    run(&mut chip, 1_000);
    assert_eq!(chip.operators[3].envelope, SILENT);
    write(&mut chip, 0x08, 0x80);
    write(&mut chip, 0x02, 0xF0);
    write(&mut chip, 0x04, 0x01);
    run(&mut chip, 16 * 4 + 2);
    assert!(
        chip.operators[3].envelope < SILENT,
        "struck by the overflow"
    );
    assert_eq!(
        chip.operators[3].stage,
        Stage::Release,
        "and at once released"
    );
}

#[test]
fn the_dac_keeps_ten_bits_of_a_loud_sample() {
    assert_eq!(dac(511), 511);
    assert_eq!(dac(4_095), 4_088);
    assert_eq!(dac(-600), -600);
    assert_eq!(dac(-601), -602);
    assert_eq!(dac(40_000), 32_704);
    assert_eq!(dac(-40_000), -32_768);
}
