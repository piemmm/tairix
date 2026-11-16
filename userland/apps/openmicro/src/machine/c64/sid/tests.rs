use alloc::vec::Vec;

use super::*;

const CLOCK: u64 = 985_248;
const CAPACITY: usize = 1 << 20;

struct Chip {
    sid: Sid,
    audio: Vec<f32>,
    now: u64,
}

impl Chip {
    fn new(model: Model) -> Self {
        Self {
            sid: Sid::new(model, CLOCK),
            audio: Vec::new(),
            now: 0,
        }
    }

    fn write(&mut self, register: u8, value: u8) {
        self.now += 1;
        self.sid
            .write(self.now, register, value, &mut self.audio, CAPACITY);
    }

    fn read(&mut self, register: u8) -> u8 {
        self.now += 1;
        self.sid.read(self.now, register, &mut self.audio, CAPACITY)
    }

    fn run(&mut self, cycles: u64) {
        self.now += cycles;
        self.sid.advance(self.now, &mut self.audio, CAPACITY);
    }

    /// The largest swing among the samples since `from`.
    fn swing(&self, from: usize) -> f32 {
        let samples = &self.audio[from..];
        let high = samples.iter().copied().fold(f32::MIN, f32::max);
        let low = samples.iter().copied().fold(f32::MAX, f32::min);
        high - low
    }
}

#[test]
fn a_write_only_register_reads_the_bus_until_it_fades() {
    for model in [Model::Mos6581, Model::Mos8580] {
        let mut chip = Chip::new(model);
        chip.write(reg::FREQUENCY_LO, 0x5A);
        assert_eq!(chip.read(reg::CUTOFF_HI), 0x5A);
        chip.run(u64::from(model.bus_life()) + 10);
        assert_eq!(chip.read(reg::CUTOFF_HI), 0, "{model:?}");
    }
}

#[test]
fn osc3_reads_voice_3_s_waveform_and_env3_its_envelope() {
    let mut chip = Chip::new(Model::Mos8580);
    let voice3 = 2 * reg::VOICE;
    chip.write(voice3 + reg::FREQUENCY_HI, 0x10);
    chip.write(voice3 + reg::ATTACK_DECAY, 0x00);
    chip.write(voice3 + reg::SUSTAIN_RELEASE, 0xF0);
    chip.write(voice3 + reg::CONTROL, 0x21);
    chip.run(0x100);
    let first = chip.read(reg::OSC3);
    chip.run(0x100);
    let second = chip.read(reg::OSC3);
    assert!(second > first, "{first:02X} then {second:02X}");
    chip.run(10_000);
    assert_eq!(chip.read(reg::ENV3), 0xFF, "the attack reached its top");
}

#[test]
fn a_gated_voice_sounds_and_volume_zero_silences_it() {
    let mut chip = Chip::new(Model::Mos8580);
    chip.write(reg::FREQUENCY_HI, 0x1C);
    chip.write(reg::ATTACK_DECAY, 0x00);
    chip.write(reg::SUSTAIN_RELEASE, 0xF0);
    chip.write(reg::MODE_VOLUME, 0x0F);
    chip.write(reg::CONTROL, 0x21);
    chip.run(CLOCK / 10);
    let from = chip.audio.len();
    chip.run(CLOCK / 10);
    let loud = chip.swing(from);
    assert!(loud > 0.1, "{loud}");
    chip.write(reg::MODE_VOLUME, 0x00);
    chip.run(CLOCK / 10);
    let from = chip.audio.len();
    chip.run(CLOCK / 10);
    assert!(chip.swing(from) < 0.001);
}

#[test]
fn the_6581_s_volume_register_sounds_on_its_own() {
    for (model, heard) in [(Model::Mos6581, true), (Model::Mos8580, false)] {
        let mut chip = Chip::new(model);
        chip.run(CLOCK / 10);
        let from = chip.audio.len();
        for step in 0..200u8 {
            chip.write(reg::MODE_VOLUME, if step % 2 == 0 { 0x0F } else { 0x00 });
            chip.run(100);
        }
        let swing = chip.swing(from);
        assert_eq!(swing > 0.1, heard, "{model:?}: {swing}");
    }
}

#[test]
fn sync_restarts_voice_1_as_voice_3_s_top_bit_rises() {
    let mut chip = Chip::new(Model::Mos8580);
    chip.write(reg::FREQUENCY_HI, 0x01);
    chip.write(reg::CONTROL, 0x22);
    chip.write(2 * reg::VOICE + reg::FREQUENCY_HI, 0x80);
    chip.run(0x100);
    let wave = &chip.sid.state().waves[0];
    assert!(
        wave.accumulator() < 0x100 * 0x100,
        "{:06X}",
        wave.accumulator()
    );
}

#[test]
fn the_paddles_are_read_every_512_cycles() {
    let mut chip = Chip::new(Model::Mos6581);
    chip.run(POT_PERIOD - 10);
    chip.sid.set_pots(0x40, 0x80);
    assert_eq!(chip.read(reg::POT_X), 0xFF, "not yet read");
    chip.run(POT_PERIOD);
    assert_eq!((chip.read(reg::POT_X), chip.read(reg::POT_Y)), (0x40, 0x80));
}

#[test]
fn a_snapshot_restores_the_chip_exactly() {
    let mut chip = Chip::new(Model::Mos6581);
    chip.write(reg::FREQUENCY_HI, 0x1C);
    chip.write(reg::SUSTAIN_RELEASE, 0xF0);
    chip.write(reg::MODE_VOLUME, 0x1F);
    chip.write(reg::RESONANCE, 0xF1);
    chip.write(reg::CONTROL, 0x41);
    chip.run(12_345);
    let mut w = snapshot::Writer::new();
    w.put(chip.sid.state());
    let body = snapshot::open(&w.finish().expect("fits")).expect("opens");
    let mut r = snapshot::Reader::new(&body);
    r.set_clock(chip.now).expect("in range");
    let state: SidState = r.get().expect("restores");
    let mut restored = Sid::new(Model::Mos6581, CLOCK);
    restored.restore(state).expect("this chip's state");
    assert_eq!(
        restored.state(),
        chip.sid.state(),
        "the filter tuned again from its registers"
    );
    let mut other = Sid::new(Model::Mos8580, CLOCK);
    assert!(other.restore(state).is_err(), "the other chip's state");
}
