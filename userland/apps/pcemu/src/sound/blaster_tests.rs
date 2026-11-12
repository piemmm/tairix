use super::*;
use crate::chips::dma::testing::{program, Flat};
use alloc::vec::Vec;

const US: Nanos = 1_000;
/// 45 µs a sample.
const TIME_CONSTANT: u8 = 0xD3;
const PERIOD: Nanos = 45 * US;
/// Where the tests' samples lie.
const BUFFER: u32 = 0x1_0000;

struct Rig {
    dsp: Dsp,
    dma: Dma,
    memory: Flat,
    mixer: Mixer,
    now: Nanos,
}

impl Rig {
    fn new() -> Self {
        let mut rig = Self {
            dsp: Dsp::new(),
            dma: Dma::new(),
            memory: Flat::new(),
            mixer: Mixer::new(48_000).unwrap(),
            now: 0,
        };
        rig.reset();
        rig.command(&[0x40, TIME_CONSTANT]);
        rig
    }

    fn wait(&mut self, span: Nanos) {
        self.now += span;
        self.dsp
            .advance(self.now, &mut self.dma, &mut self.memory, &mut self.mixer);
    }

    /// Writes a byte as a program does: waiting while the DSP is busy.
    fn write(&mut self, byte: u8) {
        for _ in 0..100_000 {
            if self.dsp.input(0x0C) & 0x80 == 0 {
                self.dsp.output(0x0C, byte, self.now);
                self.wait(10 * US);
                return;
            }
            self.wait(US);
        }
        panic!("the DSP stayed busy");
    }

    fn command(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write(byte);
        }
    }

    /// Reads a byte as a program does: waiting for one to be there.
    fn read(&mut self) -> u8 {
        for _ in 0..100_000 {
            if self.dsp.out.front().is_some() {
                return self.dsp.input(0x0A);
            }
            self.wait(US);
        }
        panic!("no byte came");
    }

    fn reset(&mut self) {
        self.dsp.output(0x06, 1, self.now);
        self.wait(3 * US);
        self.dsp.output(0x06, 0, self.now);
        while let Some(&byte) = self.dsp.out.front() {
            if byte == READY && self.dsp.booting.is_none() {
                break;
            }
            self.read();
        }
        assert_eq!(self.read(), READY);
    }

    fn samples(&mut self, bytes: &[u8]) {
        for (at, &byte) in (BUFFER..).zip(bytes) {
            self.memory.0[at as usize] = byte;
        }
    }

    /// The DAC's codes over the next `ticks` samples.
    fn dac_over(&mut self, ticks: usize) -> Vec<u8> {
        (0..ticks)
            .map(|_| {
                self.wait(PERIOD);
                self.dsp.dac
            })
            .collect()
    }
}

#[test]
fn a_reset_ends_with_aah_behind_any_byte_unread() {
    let mut rig = Rig::new();
    rig.command(&[0xE1]);
    rig.wait(100 * US);
    rig.dsp.output(0x06, 1, rig.now);
    rig.wait(3 * US);
    rig.dsp.output(0x06, 0, rig.now);
    rig.wait(100 * US);
    assert_eq!([rig.read(), rig.read(), rig.read()], [0x02, 0x02, READY]);
}

#[test]
fn resets_no_one_reads_hold_no_more_than_the_output_does() {
    let mut rig = Rig::new();
    for _ in 0..100 {
        rig.dsp.output(0x06, 1, rig.now);
        rig.wait(3 * US);
        rig.dsp.output(0x06, 0, rig.now);
        rig.wait(BOOT + US);
    }
    assert_eq!(rig.dsp.out.len(), OUT_DEPTH);
    assert_eq!(rig.dsp.out.back(), Some(&READY));
}

#[test]
fn the_version_identification_and_test_register_answer() {
    let mut rig = Rig::new();
    rig.command(&[0xE1]);
    assert_eq!([rig.read(), rig.read()], VERSION);
    rig.command(&[0xE0, 0x5A]);
    assert_eq!(rig.read(), 0xA5);
    rig.command(&[0xE4, 0x33, 0xE8]);
    assert_eq!(rig.read(), 0x33);
    rig.command(&[0xD8]);
    assert_eq!(rig.read(), 0x00, "the speaker off after a cold reset");
}

#[test]
fn an_8_bit_block_plays_through_dma_and_interrupts_at_its_end() {
    let mut rig = Rig::new();
    let bytes: Vec<u8> = (0..100).collect();
    rig.samples(&bytes);
    program(&mut rig.dma, 1, 0x48, BUFFER, 99);
    rig.command(&[0x14, 99, 0]);
    rig.dma.take_transfers();
    rig.wait(99 * PERIOD);
    assert!(!rig.dsp.irq(), "not before its last byte");
    rig.wait(2 * PERIOD);
    assert!(rig.dsp.irq());
    assert_eq!(rig.dma.take_transfers(), 100, "a length of N moves N + 1");
    assert_eq!(rig.dsp.dac, 99);
    rig.dsp.input(0x0E);
    assert!(!rig.dsp.irq(), "reading 2xEh acknowledges it");
    rig.wait(10 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 0, "and the transfer is over");
}

#[test]
fn auto_init_repeats_its_block_until_told_to_exit() {
    let mut rig = Rig::new();
    rig.samples(&[0x40; 10]);
    program(&mut rig.dma, 1, 0x58, BUFFER, 9);
    rig.command(&[0x48, 9, 0, 0x1C]);
    for block in 0..3 {
        rig.dma.take_transfers();
        rig.wait(10 * PERIOD);
        assert!(rig.dsp.irq(), "block {block}");
        assert_eq!(rig.dma.take_transfers(), 10);
        rig.dsp.input(0x0E);
    }
    rig.command(&[0xDA]);
    rig.wait(20 * PERIOD);
    rig.dma.take_transfers();
    rig.wait(20 * PERIOD);
    assert_eq!(
        rig.dma.take_transfers(),
        0,
        "DAh ends it at its block's end"
    );
}

#[test]
fn four_bit_adpcm_decodes_as_the_firmware_does() {
    let mut rig = Rig::new();
    rig.samples(&[0x80, 0x77, 0x0F]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 2);
    rig.command(&[0x75, 2, 0]);
    assert_eq!(rig.dsp.dac, 0x80, "the reference, straight to the DAC");
    assert_eq!(rig.dac_over(4), [0x87, 0x96, 0x98, 0x89]);
    rig.wait(2 * PERIOD);
    assert!(rig.dsp.irq());
    // Without a reference the decoder carries on: its first byte plays
    // its high code, its low and its high again.
    rig.samples(&[0x10]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 0);
    rig.command(&[0x74, 0, 0]);
    assert_eq!(rig.dac_over(3), [0x8F, 0x91, 0x94]);
}

#[test]
fn the_2_6_bit_format_ends_each_byte_on_a_two_bit_code() {
    let mut rig = Rig::new();
    rig.samples(&[0x80, 0b0110_1101]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 1);
    rig.command(&[0x77, 1, 0]);
    assert_eq!(rig.dac_over(3), [0x83, 0x8A, 0x90]);
}

#[test]
fn two_bit_adpcm_doubles_its_step_on_each_one() {
    let mut rig = Rig::new();
    rig.samples(&[0x80, 0b0101_0100]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 1);
    rig.command(&[0x17, 1, 0]);
    assert_eq!(rig.dac_over(4), [0x81, 0x84, 0x8A, 0x8E]);
}

#[test]
fn silence_interrupts_after_its_length_with_no_dma() {
    let mut rig = Rig::new();
    rig.dma.take_transfers();
    rig.command(&[0x80, 9, 0]);
    rig.wait(9 * PERIOD);
    assert!(!rig.dsp.irq());
    rig.wait(2 * PERIOD);
    assert!(rig.dsp.irq(), "after N + 1 ticks");
    assert_eq!(rig.dma.take_transfers(), 0);
}

#[test]
fn a_host_byte_waiting_makes_a_normal_tick_play_nothing() {
    let mut rig = Rig::new();
    rig.samples(&[0x10; 200]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 199);
    rig.command(&[0x14, 199, 0]);
    rig.dma.take_transfers();
    let tick = rig.dsp.next_tick;
    rig.now = tick - 2 * US;
    // D4h is taken after the tick it waits through, and resumes the
    // stream the command stopped.
    rig.dsp.output(0x0C, 0xD4, rig.now);
    rig.wait(20 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 19, "one of twenty ticks skipped");
}

#[test]
fn the_speaker_ramps_on_and_off() {
    let mut rig = Rig::new();
    rig.command(&[0xD1]);
    assert_eq!(rig.dsp.dac, 0, "from 00h");
    rig.command(&[0xD8]);
    let answered = rig.read();
    assert_eq!(answered, 0xFF);
    assert!(rig.now >= 128 * RAMP_STEP, "the query waited out the ramp");
    assert_eq!(rig.dsp.dac, 0x80);
    rig.command(&[0xD3, 0xD8]);
    assert_eq!(rig.read(), 0x00);
    assert!(rig.dsp.muted);
}

#[test]
fn high_speed_plays_a_host_byte_and_takes_no_commands() {
    let mut rig = Rig::new();
    rig.samples(&[0x10; 20]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 19);
    rig.command(&[0x48, 19, 0, 0x91]);
    rig.wait(3 * PERIOD);
    assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "busy throughout");
    rig.dsp.output(0x0C, 0x42, rig.now);
    rig.wait(PERIOD);
    assert_eq!(rig.dsp.dac, 0x42, "the host's byte, as a sample");
    rig.wait(20 * PERIOD);
    assert!(rig.dsp.irq());
    assert_eq!(
        rig.dsp.input(0x0C) & 0x80,
        0,
        "commands again after the block"
    );
    rig.command(&[0xE1]);
    assert_eq!(rig.read(), 0x02);
}

#[test]
fn e2h_answers_by_dma_with_its_rolling_key() {
    let mut rig = Rig::new();
    program(&mut rig.dma, 1, 0x44, BUFFER, 0);
    rig.command(&[0xE2, 0x12]);
    assert_eq!(rig.memory.0[BUFFER as usize], 0x2E, "(12h ^ 96h) + AAh");
    program(&mut rig.dma, 1, 0x44, BUFFER, 0);
    rig.command(&[0xE2, 0x12]);
    assert_eq!(rig.memory.0[BUFFER as usize], 0xE5, "(12h ^ A5h) + 2Eh");
}

#[test]
fn an_identification_waits_for_its_dma() {
    let mut rig = Rig::new();
    rig.command(&[0xE2]);
    rig.dsp.output(0x0C, 0x12, rig.now);
    rig.wait(100 * US);
    assert!(rig.dsp.waiting_for_dma());
    assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "busy while it waits");
    program(&mut rig.dma, 1, 0x44, BUFFER, 0);
    rig.wait(US);
    assert_eq!(rig.memory.0[BUFFER as usize], 0x2E);
    assert!(!rig.dsp.waiting_for_dma());
}

#[test]
fn midi_uart_mode_swallows_everything_until_a_reset() {
    let mut rig = Rig::new();
    rig.command(&[0x34, 0xE1]);
    rig.wait(100 * US);
    assert!(rig.dsp.out.is_empty(), "E1h went out as MIDI");
    rig.reset();
    rig.command(&[0xE1]);
    assert_eq!(rig.read(), 0x02);
}

#[test]
fn a_halted_transfer_continues_where_it_stopped() {
    let mut rig = Rig::new();
    rig.samples(&[0x20; 100]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 99);
    rig.command(&[0x14, 99, 0]);
    rig.wait(10 * PERIOD);
    rig.command(&[0xD0]);
    rig.dma.take_transfers();
    rig.wait(50 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 0, "halted");
    rig.command(&[0xD4]);
    rig.wait(50 * PERIOD);
    assert!(rig.dma.take_transfers() >= 49, "continued");
}

#[test]
fn the_status_command_reports_the_mode_bits() {
    let mut rig = Rig::new();
    rig.samples(&[0x20; 100]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 99);
    rig.command(&[0x14, 99, 0, 0x04]);
    assert_eq!(rig.read(), 0x80, "the 8-bit DAC's bit");
}

#[test]
fn a_transfer_the_controller_has_not_been_given_waits_for_it() {
    let mut rig = Rig::new();
    rig.samples(&[0x55; 10]);
    rig.command(&[0x14, 9, 0]);
    rig.wait(20 * PERIOD);
    assert!(rig.dsp.waiting_for_dma());
    assert_ne!(rig.dsp.dac, 0x55);
    program(&mut rig.dma, 1, 0x48, BUFFER, 9);
    // The board brings a waiting device up to the controller's write.
    rig.dma.take_transfers();
    rig.wait(0);
    rig.wait(20 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 10, "it plays on");
    assert_eq!(rig.dsp.dac, 0x55);
}

#[test]
fn the_rate_cannot_change_while_a_transfer_runs() {
    let mut rig = Rig::new();
    rig.samples(&[0x20; 100]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 99);
    rig.command(&[0x14, 99, 0]);
    // 40h is skipped, so its would-be parameter is the next command.
    rig.command(&[0x40, 0xD0]);
    rig.dma.take_transfers();
    rig.wait(20 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 0, "D0h halted it");
    assert_eq!(rig.dsp.time_constant, TIME_CONSTANT);
}

#[test]
fn replies_the_host_does_not_read_hold_the_firmware_back() {
    let mut rig = Rig::new();
    for _ in 0..1_000 {
        rig.dsp.output(0x0C, 0xE1, rig.now);
        rig.wait(10 * US);
    }
    assert!(rig.dsp.out.len() <= 3, "{} bytes held", rig.dsp.out.len());
    assert_eq!([rig.read(), rig.read()], VERSION);
    rig.wait(20 * US);
    assert_eq!(
        rig.read(),
        VERSION[0],
        "the command waiting in the latch runs once there is room"
    );
}
