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

    /// Reads a byte as a program does: waiting for one to be there. The
    /// board brings the DSP up after the read, as a firmware held on the
    /// latch may go on.
    fn read(&mut self) -> u8 {
        for _ in 0..100_000 {
            if self.dsp.out.front().is_some() {
                let byte = self.dsp.input(0x0A);
                self.wait(0);
                return byte;
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
fn a_held_reset_stops_the_writes_the_firmware_was_to_make() {
    let mut rig = Rig::new();
    rig.command(&[0xE1]);
    rig.wait(100 * US);
    rig.dsp.output(0x06, 1, rig.now);
    rig.wait(3 * US);
    assert_eq!(
        rig.dsp.input(0x0A),
        VERSION[0],
        "the latch's byte, still there"
    );
    assert_eq!(rig.dsp.input(0x0E) & 0x80, 0, "its second never comes");
}

#[test]
fn a_reset_s_release_loses_the_byte_unread_though_the_latch_still_gives_it() {
    let mut rig = Rig::new();
    rig.command(&[0xE1]);
    rig.wait(100 * US);
    rig.dsp.output(0x06, 1, rig.now);
    rig.wait(3 * US);
    rig.dsp.output(0x06, 0, rig.now);
    assert_eq!(rig.dsp.input(0x0E) & 0x80, 0, "the release clears the flag");
    assert_eq!(rig.dsp.input(0x0A), VERSION[0]);
    rig.wait(BOOT + US);
    assert_eq!(rig.read(), READY);
    assert_eq!(rig.dsp.input(0x0E) & 0x80, 0, "nothing behind it");
}

/// The regression: a reset kept a byte unread and a reply queued behind it,
/// so the host found the reset's ready byte fourth.
#[test]
fn a_reset_leaves_its_ready_byte_alone_behind_a_reply_and_a_byte_unread() {
    let mut rig = Rig::new();
    rig.command(&[0xD8, 0xE1]);
    rig.wait(100 * US);
    assert_eq!(rig.dsp.out.len(), OUT_DEPTH);
    rig.dsp.output(0x06, 1, rig.now);
    rig.wait(3 * US);
    rig.dsp.output(0x06, 0, rig.now);
    rig.wait(BOOT + US);
    assert_eq!(rig.read(), READY);
    assert!(rig.dsp.out.is_empty());
}

#[test]
fn resets_no_one_reads_leave_one_ready_byte() {
    let mut rig = Rig::new();
    for _ in 0..100 {
        rig.dsp.output(0x06, 1, rig.now);
        rig.wait(3 * US);
        rig.dsp.output(0x06, 0, rig.now);
        rig.wait(BOOT + US);
    }
    assert_eq!(rig.dsp.out, [READY]);
}

#[test]
fn a_command_written_while_the_reset_is_held_is_lost_at_its_release() {
    let mut rig = Rig::new();
    rig.dsp.output(0x06, 1, rig.now);
    rig.dsp.output(0x0C, 0xE1, rig.now);
    rig.wait(3 * US);
    rig.dsp.output(0x06, 0, rig.now);
    rig.wait(BOOT + 100 * US);
    assert_eq!(rig.read(), READY);
    rig.wait(100 * US);
    assert!(rig.dsp.out.is_empty(), "E1h never ran");
}

#[test]
fn the_ram_test_writes_its_answer_over_a_byte_unread() {
    let mut rig = Rig::new();
    rig.command(&[0xE4, 0x5A, 0xE8, 0xF8]);
    rig.wait(100 * US);
    assert_eq!(rig.read(), 0x00, "the pass, over 5Ah");
    assert!(rig.dsp.out.is_empty());
}

#[test]
fn the_checksum_writes_its_first_byte_over_one_unread() {
    let mut rig = Rig::new();
    rig.command(&[0xE4, 0x5A, 0xE8, 0xF4]);
    rig.wait(100 * US);
    assert_eq!([rig.read(), rig.read()], [0x00, 0x00], "5Ah lost");
    assert!(rig.dsp.out.is_empty());
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
fn high_speed_takes_no_command_and_loses_a_host_byte_under_its_next() {
    let mut rig = Rig::new();
    rig.samples(&[0x10; 20]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 19);
    rig.command(&[0x48, 19, 0, 0x91]);
    rig.wait(3 * PERIOD);
    assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "busy throughout");
    rig.dsp.output(0x0C, 0x42, rig.now);
    rig.wait(PERIOD);
    assert_eq!(
        rig.dsp.dac, 0x10,
        "the controller's byte lands on the host's"
    );
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
fn a_handler_the_controller_leaves_waiting_takes_a_host_byte_as_its_sample() {
    for speed in [0x14, 0x91] {
        let mut rig = Rig::new();
        rig.samples(&[0x10; 20]);
        if speed == 0x14 {
            rig.command(&[0x14, 19, 0]);
        } else {
            rig.command(&[0x48, 19, 0, 0x91]);
        }
        rig.wait(3 * PERIOD);
        assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "{speed:02X}h: busy, held");
        rig.dsp.output(0x0C, 0x42, rig.now);
        rig.wait(0);
        assert_eq!(
            rig.dsp.dac, 0x42,
            "{speed:02X}h: the host's byte, as a sample"
        );
    }
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
fn an_identification_waits_for_its_reply_to_be_taken() {
    let mut rig = Rig::new();
    rig.command(&[0xE2, 0x12]);
    rig.wait(100 * US);
    assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "busy while it waits");
    assert_ne!(
        rig.dsp.input(0x0E) & 0x80,
        0,
        "its reply in the output latch"
    );
    program(&mut rig.dma, 1, 0x44, BUFFER, 0);
    rig.wait(US);
    assert_eq!(rig.memory.0[BUFFER as usize], 0x2E);
    assert_eq!(rig.dsp.input(0x0C) & 0x80, 0, "free once it was taken");
}

#[test]
fn an_identification_waits_for_room_in_the_output_latch() {
    let mut rig = Rig::new();
    program(&mut rig.dma, 1, 0x44, BUFFER, 0);
    rig.command(&[0xD8, 0xE2, 0x12]);
    rig.wait(100 * US);
    assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "held behind the byte unread");
    assert_eq!(rig.memory.0[BUFFER as usize], 0x00, "nothing sent yet");
    assert_eq!(rig.read(), 0x00);
    rig.wait(US);
    assert_eq!(rig.memory.0[BUFFER as usize], 0x2E);
    assert_eq!(rig.dsp.input(0x0C) & 0x80, 0);
}

#[test]
fn a_request_the_host_answered_is_served_when_dma_is_next_enabled() {
    let mut rig = Rig::new();
    rig.command(&[0xE2, 0x12]);
    rig.wait(100 * US);
    assert_eq!(
        rig.read(),
        0x2E,
        "the host took the reply, its request still latched"
    );
    rig.wait(100 * US);
    rig.samples(&[0x40, 0x41, 0x42, 0x43]);
    program(&mut rig.dma, 1, 0x48, BUFFER, 3);
    rig.command(&[0x14, 3, 0]);
    assert_eq!(rig.dma.take_transfers(), 1, "served as DMA came on");
    rig.wait(PERIOD);
    assert_eq!(
        rig.dsp.dac, 0x41,
        "its byte was a command in the latch, ignored while DMA runs"
    );
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
    assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "busy, held in its handler");
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

#[test]
fn a_recording_waits_for_the_host_to_read_a_byte_left_in_the_output_latch() {
    let mut rig = Rig::new();
    program(&mut rig.dma, 1, 0x44, BUFFER, 9);
    rig.command(&[0xD8, 0x24, 9, 0]);
    rig.dma.take_transfers();
    rig.wait(20 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 0, "held behind the byte unread");
    assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "busy while held");
    assert_eq!(rig.read(), 0x00);
    rig.wait(20 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 10, "on once it was read");
    assert!(rig.dsp.irq());
    assert_eq!(rig.dsp.dac, MID_SCALE, "the conversion's result on the DAC");
}

#[test]
fn a_sample_the_controller_does_not_take_is_the_host_s_to_read() {
    let mut rig = Rig::new();
    rig.command(&[0x24, 9, 0]);
    rig.wait(PERIOD + US);
    assert_ne!(
        rig.dsp.input(0x0E) & 0x80,
        0,
        "the sample in the output latch"
    );
    assert_eq!(rig.dsp.input(0x0A), MID_SCALE);
    rig.wait(2 * PERIOD);
    program(&mut rig.dma, 1, 0x44, BUFFER, 9);
    rig.dma.take_transfers();
    // The board brings a waiting device up to the controller's write.
    rig.wait(0);
    rig.wait(20 * PERIOD);
    assert_eq!(rig.dma.take_transfers(), 9, "the rest, the host having one");
    assert!(rig.dsp.irq());
}

#[test]
fn a_recording_told_to_exit_waits_for_its_last_sample_to_be_taken() {
    for answering in [true, false] {
        let mut rig = Rig::new();
        if answering {
            program(&mut rig.dma, 1, 0x54, BUFFER, 0);
        }
        rig.command(&[0x48, 0, 0, 0x2C, 0xDA]);
        rig.wait(PERIOD);
        assert_eq!(rig.dsp.irq(), answering, "taken: {answering}");
        if !answering {
            assert_ne!(rig.dsp.input(0x0C) & 0x80, 0, "held");
            assert_eq!(rig.read(), MID_SCALE);
            assert!(rig.dsp.irq(), "it interrupts once the sample is gone");
        }
    }
}
