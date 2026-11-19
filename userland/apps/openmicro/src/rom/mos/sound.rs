//! SOUND, ENVELOPE and the bell: the four channel queues, the envelopes,
//! and the centisecond tick that plays them on the SN76489.
//!
//! A queued note is three bytes: a control byte holding the amplitude or
//! envelope in bits 3–7 (amplitude `a` as `(a − 1) & 31`, envelope `e` as
//! `e − 1`, so bit 7 marks an amplitude), H in bit 2 and S in bits 0–1, then
//! the pitch and the duration in twentieths of a second, 255 lasting until
//! flushed. Each channel counts twentieths of its own, from the note that
//! breaks its silence, whose first twentieth is a centisecond short. An
//! envelope steps every T centiseconds from the note's start through its
//! three pitch sections, a section a step at most and repeating them unless
//! T's top bit is set, and through attack, decay, sustain and, once the
//! duration is over, release; attack and decay end once the level passes
//! their targets. A note waiting when the
//! duration ends starts at once; one with H set leaves the release running
//! for its own duration instead. A note with S set waits at the head of its
//! queue until S other channels have one with the same S, and all start
//! together.
//!
//! The Electron keeps the four queues but has one generator, its ULA's
//! counter, so a SOUND or bell for a channel other than the one last
//! addressed first flushes that one's queue, as Electron OS 1.00 does: one
//! channel plays at a time. A note's amplitude only sounds it or not, an
//! envelope's pitch still steps, and S has nothing to synchronise with.
//! Channel 0's notes are noise, by their pitch's low three bits: a fixed
//! tone for 0–3, and for 4–7 a counter drawn at random each tick and, while
//! OSRDCH waits, at each pass. With the external sound flag set, BEL and a
//! flush are service calls &16 and &17 instead.

use super::buffers::number;
use super::ws::{buffer, os, page2, var};
use super::Model;
use super::{latch, svia};
use crate::machine::bbc::latch as machine_latch;
use crate::machine::electron::{control, sheila as ula};
use crate::machine::via::reg;
use crate::rom::asm::{abs, byte, imm, lo, zp, Acc, Asm, RTS};
use crate::rom::os::service;

/// Each channel's state, four bytes a field, indexed by channel.
mod field {
    use super::buffer;

    const fn at(n: u16) -> u16 {
        buffer::SOUND_WORKSPACE + 4 * n
    }

    pub const PHASE: u16 = at(0);
    /// The amplitude, 0 to 127; an eighth of it is the volume.
    pub const LEVEL: u16 = at(1);
    /// The envelope's offset from the first, or &FF for an amplitude.
    pub const ENVELOPE: u16 = at(2);
    pub const DURATION: u16 = at(3);
    /// Centiseconds to the envelope's next step.
    pub const STEP: u16 = at(4);
    /// The pitch section, 3 once all three are done.
    pub const SECTION: u16 = at(5);
    pub const SECTION_STEPS: u16 = at(6);
    pub const PITCH: u16 = at(7);
    /// What the chip was last given: the attenuation, and the pitch.
    pub const ATTENUATION: u16 = at(8);
    pub const CHIP_PITCH: u16 = at(9);
    /// The S of the note waiting at the head of the queue, or 0.
    pub const WAITING: u16 = at(10);
    /// On the Electron, which has no S to wait on: the counter `CHIP_PITCH`
    /// gives.
    pub const COUNTER: u16 = WAITING;
    /// Centiseconds left in the current twentieth of a second.
    pub const TICKS: u16 = at(11);
    /// The note's own pitch, which repeating pitch sections start again from.
    pub const BASE: u16 = at(12);
    /// The fields there are.
    pub const COUNT: u16 = 13;
}

/// The tick's own workspace, after the channels'.
mod tick {
    use super::{buffer, field};

    const fn at(n: u16) -> u16 {
        buffer::SOUND_WORKSPACE + 4 * field::COUNT + n
    }

    /// The channel being worked on, kept across buffer calls.
    pub const CHANNEL: u16 = at(0);
    /// A note being taken from its queue, and the period being built.
    pub const CONTROL: u16 = at(1);
    pub const NOTE_PITCH: u16 = at(2);
    pub const NOTE_DURATION: u16 = at(3);
    pub const PERIOD_LOW: u16 = at(4);
    pub const PERIOD_HIGH: u16 = at(5);
    pub const OCTAVE: u16 = at(6);
    pub const QUARTERS: u16 = at(7);
    /// A loop's count, or an envelope step's amplitude change.
    pub const COUNT: u16 = at(8);
    /// The synchronising pass's channel; on the Electron, the noise's shift
    /// register.
    pub const SYNCHRONISING: u16 = at(9);
    pub const NOISE_SEED: u16 = SYNCHRONISING;
    /// The S whose notes the synchronising pass starts, apart from `COUNT`,
    /// which starting an envelope's note overwrites.
    pub const STARTING: u16 = at(10);
    /// On the Electron, which synchronises nothing: the channel SOUND or the
    /// bell last addressed.
    pub const ADDRESSED: u16 = STARTING;
    /// On the Electron: the kind of channel 0's noise while noise drawn at
    /// random sounds, 0 otherwise.
    pub const NOISE_KIND: u16 = at(11);
    /// The last byte.
    pub const END: u16 = NOISE_KIND;
}

// The sound workspace is &800–&83F.
const _: () = assert!(tick::END < buffer::SOUND);

mod phase {
    pub const IDLE: u8 = 0;
    pub const ATTACK: u8 = 1;
    pub const DECAY: u8 = 2;
    pub const SUSTAIN: u8 = 3;
    pub const RELEASE: u8 = 4;
}

// A phase advances by one, and indexes the envelope's bytes for it; idle is
// also a silent channel's level.
const _: () = assert!(
    phase::IDLE == 0
        && phase::DECAY == phase::ATTACK + 1
        && phase::SUSTAIN == phase::DECAY + 1
        && phase::RELEASE == phase::SUSTAIN + 1
);

/// A queued note's control bits.
mod note {
    pub const LEVEL: u8 = 0xF8;
    pub const HOLD: u8 = 0x04;
    pub const SYNC: u8 = 0x03;
}

/// An envelope's bytes, as OSWORD 8 gives them after its number.
mod envelope {
    use super::buffer;
    pub const STEP: u16 = buffer::ENVELOPES;
    pub const PITCH_CHANGE: u16 = buffer::ENVELOPES + 1;
    pub const PITCH_STEPS: u16 = buffer::ENVELOPES + 4;
    /// Attack, decay, sustain and release changes, in phase order.
    pub const AMPLITUDE_CHANGE: u16 = buffer::ENVELOPES + 7 - 1;
    /// The attack's and decay's target levels, in phase order.
    pub const TARGET: u16 = buffer::ENVELOPES + 11 - 1;
    pub const LENGTH: u8 = 13;
    /// Each envelope's slot.
    pub const SIZE: u8 = 16;
    /// T's bit that stops the pitch sections repeating.
    pub const NO_REPEAT: u8 = 0x80;
}

const HIGHEST_LEVEL: u8 = 127;
const SILENT: u8 = 15;
/// Centiseconds in a twentieth of a second.
const TWENTIETH: u8 = 5;
/// A duration that lasts until flushed.
const FOREVER: u8 = 0xFF;
/// Quarter semitones in an octave.
const OCTAVE: u8 = 48;
/// Each semitone's period in the lowest octave, and the period a quarter
/// semitone above it takes off.
const SEMITONES: [(u16, u8); 12] = [
    (1008, 14),
    (951, 13),
    (898, 12),
    (847, 12),
    (800, 11),
    (755, 10),
    (712, 10),
    (672, 9),
    (635, 9),
    (599, 8),
    (565, 8),
    (534, 7),
];

// The Electron's counter takes a quarter of the longest period.
const _: () = assert!(SEMITONES[0].0 / 4 <= 0x100);

/// The SN76489's command bytes.
mod chip {
    pub const LATCH: u8 = 0x80;
    pub const VOLUME: u8 = 0x10;
    pub const NOISE: u8 = 0xE0;
    pub const NOISE_CONTROL: u8 = 0x07;
    pub const LOW_PERIOD: u8 = 0x0F;
    pub const HIGH_PERIOD: u8 = 0x3F;
    /// SOUND channel `n` is the chip's channel `3 − n`, in bits 5–6.
    pub const fn channel(n: u8) -> u8 {
        (3 - n) << 5
    }
}

/// The addressable latch writes that drop and raise the chip's write enable.
const WRITE_ENABLE_LOW: u8 = latch(machine_latch::SOUND_WE, false);
const WRITE_ENABLE_HIGH: u8 = latch(machine_latch::SOUND_WE, true);

/// On the Electron, whose one generator is the first channel's: whether it
/// sounds, 0, or is `SILENT`.
const GENERATOR: u16 = field::ATTENUATION;
/// The bits of a noise note's pitch that give its kind.
const NOISE_KIND_BITS: u8 = 0x07;
/// The first of the kinds drawn at random.
const RANDOM_NOISE: u8 = 4;
/// The counter of the SN76489's periodic noise at shift rate `rate`, its
/// 4 MHz / 512 · 2^rate over the pattern's 15 shifts, on the counter's
/// 1 MHz / 32 (S + 1).
const fn periodic_noise(rate: u32) -> u8 {
    byte(((15 * 512 / (4 * 32)) << rate) - 1)
}
/// Each kind of noise's counter: the byte drawn, masked, with the bits set in
/// it. Kinds 0–3 draw nothing, sounding the three rates, and for 3, which
/// follows tone channel 1 on the Model B, the middle one; kinds 4–7 draw
/// from the ranges Electron OS 1.00 draws from.
const NOISE_MASKS: [u8; 8] = [0, 0, 0, 0, 0x3F, 0x7F, 0x3F, 0x7F];
const NOISE_BITS: [u8; 8] = [
    periodic_noise(0),
    periodic_noise(1),
    periodic_noise(2),
    periodic_noise(1),
    0x40,
    0x80,
    0xC0,
    0x80,
];
// The kinds drawn at random are those with a mask.
const _: () =
    assert!(NOISE_MASKS[RANDOM_NOISE as usize - 1] == 0 && NOISE_MASKS[RANDOM_NOISE as usize] != 0);

pub fn emit(a: &mut Asm, model: Model) {
    commands(a, model);
    tick_routine(a, model);
    if model == Model::ModelB {
        synchronising(a);
    }
    starting(a, model);
    stepping(a);
    match model {
        Model::ModelB => output(a),
        Model::Electron => {
            electron_output(a);
            noise_tables(a);
        }
    }
    sound_period(a, model);
    if model == Model::ModelB {
        a.later(|a| {
            a.here("sound_chip_channel");
            for n in 0..4 {
                a.byte(chip::channel(n));
            }
        });
    }
    semitone_tables(a);
}

fn commands(a: &mut Asm, model: Model) {
    // OSWORD 7: the block at (&F0) holds the channel word, amplitude,
    // pitch and duration. The note waits for room in its queue, or until
    // an Escape; any of F's bits flushes the queue first, as MOS 1.20's do.
    a.here("sound_command");
    a.ldy(imm(0));
    a.lda(zp(os::CALL_X).ind_y());
    if model == Model::Electron {
        a.jsr("sound_address");
        a.ldy(imm(0));
        a.lda(zp(os::CALL_X).ind_y());
    }
    a.and(imm(0xF0));
    a.beq("sound_no_flush");
    a.jsr("sound_buffer");
    a.jsr("buffer_flush");
    a.here("sound_no_flush");
    // Room for the note first: an interrupt routine's own SOUND may run
    // while this waits.
    a.bit(zp(os::ESCAPE));
    a.bmi(RTS);
    a.jsr("sound_buffer");
    a.sec();
    a.jsr("buffer_count");
    a.cpx(imm(3));
    a.bcs("sound_command_room");
    a.jsr("interrupt_window");
    a.bcc("sound_no_flush");
    a.here("sound_command_room");
    // The control byte: amplitude or envelope, then H and S.
    a.ldy(imm(2));
    a.lda(zp(os::CALL_X).ind_y());
    a.sec();
    a.sbc(imm(1));
    for _ in 0..3 {
        a.asl(Acc);
    }
    a.sta(zp(os::TEMP));
    a.ldy(imm(1));
    a.lda(zp(os::CALL_X).ind_y());
    a.pha();
    a.and(imm(note::SYNC));
    a.ora(zp(os::TEMP));
    a.sta(zp(os::TEMP));
    a.pla();
    a.and(imm(0x10));
    a.lsr(Acc);
    a.lsr(Acc);
    a.ora(zp(os::TEMP));
    a.sta(zp(os::TEMP));
    a.jsr("sound_buffer");
    a.lda(zp(os::TEMP));
    a.jsr("buffer_insert");
    a.ldy(imm(4));
    a.lda(zp(os::CALL_X).ind_y());
    a.jsr("buffer_insert");
    a.ldy(imm(6));
    a.lda(zp(os::CALL_X).ind_y());
    a.jmp("buffer_insert");

    // X = the queue of the channel the block names.
    a.here("sound_buffer");
    a.ldy(imm(0));
    a.lda(zp(os::CALL_X).ind_y());
    a.here("sound_channel_buffer");
    a.and(imm(3));
    a.ora(imm(number::SOUND));
    a.tax();
    a.rts();

    // OSWORD 8: envelope N's thirteen bytes.
    a.here("envelope_command");
    a.ldy(imm(0));
    a.lda(zp(os::CALL_X).ind_y());
    a.sec();
    a.sbc(imm(1));
    a.and(imm(0x0F));
    for _ in 0..envelope::SIZE.trailing_zeros() {
        a.asl(Acc);
    }
    a.tax();
    a.here("envelope_copy");
    a.iny();
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(abs(envelope::STEP).x());
    a.inx();
    a.cpy(imm(envelope::LENGTH));
    a.bne("envelope_copy");
    a.rts();
    bell(a, model);
    flushing(a, model);
    init(a, model);
}

/// The sound's state at a reset.
fn init(a: &mut Asm, model: Model) {
    let p2 = page2::layout(model);
    // Every channel idle and silent, its period pitch 0's, the noise's
    // control 0; on the Electron each busy flag clear and channel 0 the one
    // addressed.
    a.here("sound_init");
    a.ldx(imm(lo(tick::END - buffer::SOUND_WORKSPACE)));
    a.lda(imm(0));
    a.here("sound_init_clear");
    a.sta(abs(buffer::SOUND_WORKSPACE).x());
    a.dex();
    a.bpl("sound_init_clear");
    a.ldx(imm(3));
    a.here("sound_init_channel");
    a.lda(imm(0xFF));
    a.sta(abs(field::ATTENUATION).x());
    a.sta(abs(field::CHIP_PITCH).x());
    if model == Model::Electron {
        // The counter the cached pitch gives, so the cache holds from the
        // first note.
        a.jsr("sound_period");
        a.sta(abs(field::COUNTER).x());
        a.lda(imm(0));
        a.sta(abs(p2.buffer_busy + u16::from(number::SOUND)).x());
    }
    a.jsr("sound_output");
    a.dex();
    a.bpl("sound_init_channel");
    if model == Model::Electron {
        // Any seed but zero keeps the shift register turning.
        a.lda(imm(1));
        a.sta(abs(tick::NOISE_SEED));
    }
    a.rts();
}

/// A sound queue flushed, and on the Electron a channel addressed.
fn flushing(a: &mut Asm, model: Model) {
    let p2 = page2::layout(model);
    // A sound queue flushed: its note stops, the chip silenced at the next
    // tick. X is kept.
    a.here("sound_purge");
    a.txa();
    a.pha();
    if model == Model::Electron {
        // A flushed queue's busy flag, as Electron OS 1.00 leaves it.
        a.lda(imm(FLUSHED));
        a.sta(abs(p2.buffer_busy).x());
        a.lda(abs(var::EXTERNAL_SOUND));
        a.beq("sound_purge_own");
        a.lda(imm(service::SOUND_FLUSH));
        a.jsr("service_call");
        a.pla();
        a.tax();
        a.rts();
        a.here("sound_purge_own");
        a.txa();
    }
    a.and(imm(3));
    a.tax();
    a.lda(imm(phase::IDLE));
    a.sta(abs(field::PHASE).x());
    a.sta(abs(field::LEVEL).x());
    if model == Model::ModelB {
        a.sta(abs(field::WAITING).x());
    }
    // A note sounding stops with its queue.
    a.jsr("sound_output");
    a.pla();
    a.tax();
    a.rts();

    if model == Model::Electron {
        // A, a channel addressed: the channel last addressed flushed if it
        // is another, and this one's busy flag cleared. A and Y are lost.
        a.here("sound_address");
        a.and(imm(3));
        a.cmp(abs(tick::ADDRESSED));
        a.beq("sound_addressed");
        a.pha();
        a.lda(abs(tick::ADDRESSED));
        a.jsr("sound_channel_buffer");
        a.jsr("buffer_flush");
        a.pla();
        a.sta(abs(tick::ADDRESSED));
        a.here("sound_addressed");
        a.jsr("sound_channel_buffer");
        a.lda(imm(0));
        a.sta(abs(p2.buffer_busy).x());
        a.rts();
    }
}

/// VDU 7: OSBYTE 211–214's note, dropped if its queue has no room.
fn bell(a: &mut Asm, model: Model) {
    a.here("bell");
    if model == Model::Electron {
        a.lda(abs(var::EXTERNAL_SOUND));
        a.beq("bell_own");
        a.lda(imm(service::BELL));
        a.jmp("service_call");
        a.here("bell_own");
    }
    // Counted and filled at once, so an interrupt routine's SOUND cannot
    // take the room between.
    a.php();
    a.sei();
    if model == Model::Electron {
        a.lda(abs(var::BELL_CHANNEL));
        a.jsr("sound_address");
    }
    a.jsr("bell_buffer");
    a.sec();
    a.jsr("buffer_count");
    a.cpx(imm(3));
    a.bcc("bell_done");
    a.jsr("bell_buffer");
    for setting in [var::BELL_AMPLITUDE, var::BELL_PITCH, var::BELL_DURATION] {
        a.lda(abs(setting));
        a.jsr("buffer_insert");
    }
    a.here("bell_done");
    a.plp();
    a.rts();
    a.here("bell_buffer");
    a.lda(abs(var::BELL_CHANNEL));
    a.jmp("sound_channel_buffer");
}

/// A flushed sound queue's busy flag on the Electron.
const FLUSHED: u8 = 0x80;

/// The centisecond tick, from the 100 Hz interrupt.
fn tick_routine(a: &mut Asm, model: Model) {
    let p2 = page2::layout(model);
    a.here("sound_tick");
    a.ldx(imm(3));
    a.here("sound_tick_next");
    a.jsr("sound_channel");
    a.dex();
    a.bpl("sound_tick_next");
    match model {
        Model::ModelB => a.jmp("sound_synchronise"),
        Model::Electron => a.rts(),
    }

    a.here("sound_channel");
    if model == Model::ModelB {
        a.lda(imm(0));
        a.sta(abs(field::WAITING).x());
    }
    a.lda(abs(field::PHASE).x());
    a.bne("sound_channel_playing");
    // A silent channel waits on its queue alone, examined only once a note
    // is in it.
    a.lda(abs(p2.buffer_start + u16::from(number::SOUND)).x());
    a.cmp(abs(p2.buffer_end + u16::from(number::SOUND)).x());
    a.beq(RTS);
    a.jsr("sound_next");
    a.jmp("sound_output");
    a.here("sound_channel_playing");
    a.dec(abs(field::TICKS).x());
    a.bne("sound_channel_running");
    a.lda(imm(TWENTIETH));
    a.sta(abs(field::TICKS).x());
    a.lda(abs(field::DURATION).x());
    a.beq("sound_channel_over");
    a.cmp(imm(FOREVER));
    a.beq("sound_channel_running");
    a.dec(abs(field::DURATION).x());
    a.bne("sound_channel_running");
    // The duration is over: the next note, or the release.
    a.here("sound_channel_over");
    a.jsr("sound_next");
    a.bcs("sound_channel_ended");
    // A held note carries the release on, which steps this tick as ever.
    a.here("sound_channel_started");
    a.lda(abs(field::PHASE).x());
    a.cmp(imm(phase::RELEASE));
    a.beq("sound_channel_steps");
    a.bne("sound_output_jump");
    a.here("sound_channel_ended");
    a.lda(abs(field::ENVELOPE).x());
    a.bpl("sound_channel_release");
    // Silent, a channel's level is nothing.
    a.lda(imm(phase::IDLE));
    a.sta(abs(field::PHASE).x());
    a.sta(abs(field::LEVEL).x());
    a.here("sound_output_jump");
    a.jmp("sound_output");
    a.here("sound_channel_release");
    a.lda(imm(phase::RELEASE));
    a.sta(abs(field::PHASE).x());
    a.bne("sound_channel_steps");
    // A release past its duration gives way to a waiting note.
    a.here("sound_channel_running");
    a.lda(abs(field::PHASE).x());
    a.cmp(imm(phase::RELEASE));
    a.bne("sound_channel_steps");
    a.lda(abs(field::DURATION).x());
    a.bne("sound_channel_steps");
    // As after silence, its first twentieth is a centisecond short.
    a.lda(imm(TWENTIETH - 1));
    a.sta(abs(field::TICKS).x());
    a.jsr("sound_next");
    a.bcc("sound_channel_started");
    a.here("sound_channel_steps");
    a.lda(abs(field::ENVELOPE).x());
    a.bmi("sound_output_jump");
    a.dec(abs(field::STEP).x());
    a.bne("sound_output_jump");
    a.jsr("envelope_step");
    a.jmp("sound_output");
}

fn synchronising(a: &mut Asm) {
    // Notes waiting to synchronise start once enough channels wait with
    // the same S.
    a.here("sound_synchronise");
    a.ldx(imm(3));
    a.here("sound_synchronise_next");
    a.lda(abs(field::WAITING).x());
    a.beq("sound_synchronise_skip");
    a.ldy(imm(3));
    a.lda(imm(0));
    a.sta(abs(tick::COUNT));
    a.here("sound_synchronise_count");
    a.lda(abs(field::WAITING).y());
    a.cmp(abs(field::WAITING).x());
    a.bne("sound_synchronise_other");
    a.inc(abs(tick::COUNT));
    a.here("sound_synchronise_other");
    a.dey();
    a.bpl("sound_synchronise_count");
    a.lda(abs(field::WAITING).x());
    a.cmp(abs(tick::COUNT));
    a.bcs("sound_synchronise_skip");
    // Enough: every channel waiting with this S starts.
    a.sta(abs(tick::STARTING));
    a.stx(abs(tick::SYNCHRONISING));
    a.ldx(imm(3));
    a.here("sound_synchronise_start");
    a.lda(abs(field::WAITING).x());
    a.cmp(abs(tick::STARTING));
    a.bne("sound_synchronise_started");
    a.lda(imm(0));
    a.sta(abs(field::WAITING).x());
    a.jsr("sound_start");
    a.jsr("sound_output");
    a.here("sound_synchronise_started");
    a.dex();
    a.bpl("sound_synchronise_start");
    a.ldx(abs(tick::SYNCHRONISING));
    a.here("sound_synchronise_skip");
    a.dex();
    a.bpl("sound_synchronise_next");
    a.rts();
}

const _: () = assert!(phase::SUSTAIN != 0);

/// Taking a note from channel X's queue.
fn starting(a: &mut Asm, model: Model) {
    // Start the note at the head of channel X's queue, C clear, unless
    // there is none or it waits to synchronise. X is kept.
    a.here("sound_next");
    a.jsr("channel_buffer");
    a.jsr("buffer_examine");
    a.ldx(abs(tick::CHANNEL));
    a.bcs(RTS);
    match model {
        Model::ModelB => {
            a.and(imm(note::SYNC));
            a.beq("sound_start");
            a.sta(abs(field::WAITING).x());
            a.sec();
            a.rts();
        }
        Model::Electron => a.jmp("sound_start"),
    }

    // A: the next byte of channel X's queue, taken. X is kept.
    a.here("sound_take");
    a.jsr("channel_buffer");
    a.jsr("buffer_remove");
    a.ldx(abs(tick::CHANNEL));
    a.rts();

    // X: channel X's buffer, the channel kept in the tick's workspace.
    a.here("channel_buffer");
    a.stx(abs(tick::CHANNEL));
    a.txa();
    a.ora(imm(number::SOUND));
    a.tax();
    a.rts();

    a.here("sound_start");
    for slot in [tick::CONTROL, tick::NOTE_PITCH, tick::NOTE_DURATION] {
        a.jsr("sound_take");
        a.sta(abs(slot));
    }
    a.lda(abs(tick::NOTE_DURATION));
    a.sta(abs(field::DURATION).x());
    // From silence the first twentieth is a centisecond short.
    let playing = a.local();
    a.lda(abs(field::PHASE).x());
    a.bne(playing);
    a.lda(imm(TWENTIETH - 1));
    a.sta(abs(field::TICKS).x());
    a.bind(playing);
    a.lda(abs(tick::CONTROL));
    a.and(imm(note::HOLD));
    a.beq("sound_start_fresh");
    // Held: an envelope playing releases through this duration; anything
    // else is silence for it.
    a.lda(abs(field::PHASE).x());
    a.beq("sound_start_silence");
    a.lda(abs(field::ENVELOPE).x());
    a.bmi("sound_start_silence");
    a.lda(imm(phase::RELEASE));
    a.here("sound_start_phase");
    a.sta(abs(field::PHASE).x());
    a.clc();
    a.rts();
    a.here("sound_start_silence");
    a.lda(imm(0));
    a.sta(abs(field::LEVEL).x());
    a.beq("sound_start_amplitude_set");

    a.here("sound_start_fresh");
    a.lda(abs(tick::NOTE_PITCH));
    a.sta(abs(field::PITCH).x());
    a.sta(abs(field::BASE).x());
    a.lda(abs(tick::CONTROL));
    a.bpl("sound_start_envelope");
    // An amplitude: its level held for the duration, at the top of its
    // volume's eight.
    a.and(imm(note::LEVEL));
    a.eor(imm(note::LEVEL | 7));
    a.sta(abs(field::LEVEL).x());
    a.here("sound_start_amplitude_set");
    a.lda(imm(0xFF));
    a.sta(abs(field::ENVELOPE).x());
    a.lda(imm(phase::SUSTAIN));
    a.bne("sound_start_phase");

    a.here("sound_start_envelope");
    a.asl(Acc);
    a.and(imm(0xF0));
    a.sta(abs(field::ENVELOPE).x());
    a.tay();
    // The attack starts from the level the channel has.
    a.lda(imm(0));
    a.sta(abs(field::SECTION).x());
    a.lda(abs(envelope::PITCH_STEPS).y());
    a.sta(abs(field::SECTION_STEPS).x());
    a.lda(imm(phase::ATTACK));
    a.sta(abs(field::PHASE).x());
    // The start's pitch, then the first step, both at once.
    a.jsr("sound_output_pitch");
    a.jsr("envelope_step");
    a.clc();
    a.rts();
}

/// One step of channel X's envelope: the pitch, then the amplitude.
fn stepping(a: &mut Asm) {
    a.here("envelope_step");
    a.ldy(abs(field::ENVELOPE).x());
    a.lda(abs(envelope::STEP).y());
    a.and(imm(!envelope::NO_REPEAT));
    a.bne("envelope_step_length");
    a.lda(imm(1));
    a.here("envelope_step_length");
    a.sta(abs(field::STEP).x());

    // The pitch: the section's change while it has steps, the next section's
    // once it has none; a step moves on one section at most.
    a.lda(imm(1));
    a.sta(abs(tick::COUNT));
    a.here("envelope_pitch");
    a.lda(abs(field::SECTION).x());
    a.cmp(imm(3));
    a.bcs("envelope_pitch_done");
    a.lda(abs(field::SECTION_STEPS).x());
    a.beq("envelope_pitch_section");
    a.dec(abs(field::SECTION_STEPS).x());
    a.tya();
    a.clc();
    a.adc(abs(field::SECTION).x());
    a.tay();
    a.lda(abs(envelope::PITCH_CHANGE).y());
    a.clc();
    a.adc(abs(field::PITCH).x());
    a.sta(abs(field::PITCH).x());
    a.ldy(abs(field::ENVELOPE).x());
    a.jmp("envelope_pitch_done");
    a.here("envelope_pitch_section");
    a.dec(abs(tick::COUNT));
    a.bmi("envelope_pitch_done");
    a.inc(abs(field::SECTION).x());
    a.lda(abs(field::SECTION).x());
    a.cmp(imm(3));
    a.bcc("envelope_pitch_load");
    a.lda(abs(envelope::STEP).y());
    a.bmi("envelope_pitch_done");
    // Repeating, the sections start again from the note's own pitch.
    a.lda(imm(0));
    a.sta(abs(field::SECTION).x());
    a.lda(abs(field::BASE).x());
    a.sta(abs(field::PITCH).x());
    a.here("envelope_pitch_load");
    a.tya();
    a.clc();
    a.adc(abs(field::SECTION).x());
    a.tay();
    a.lda(abs(envelope::PITCH_STEPS).y());
    a.sta(abs(field::SECTION_STEPS).x());
    a.ldy(abs(field::ENVELOPE).x());
    a.jmp("envelope_pitch");
    a.here("envelope_pitch_done");

    // The amplitude: the phase's change, clamped to 0–127. Attack and decay
    // end once the level is past their targets, from the side it moves, the
    // level then one above the target; release ends on reaching nothing.
    a.tya();
    a.clc();
    a.adc(abs(field::PHASE).x());
    a.tay();
    a.lda(abs(envelope::AMPLITUDE_CHANGE).y());
    a.sta(abs(tick::COUNT));
    a.clc();
    a.adc(abs(field::LEVEL).x());
    a.bit(abs(tick::COUNT));
    a.bmi("envelope_level_falling");
    // Rising, the sum cannot carry.
    a.cmp(imm(HIGHEST_LEVEL + 1));
    a.bcc("envelope_level_set");
    a.lda(imm(HIGHEST_LEVEL));
    a.bne("envelope_level_set");
    a.here("envelope_level_falling");
    a.bcs("envelope_level_set");
    a.lda(imm(0));
    a.here("envelope_level_set");
    a.sta(abs(field::LEVEL).x());
    a.lda(abs(field::PHASE).x());
    a.cmp(imm(phase::SUSTAIN));
    a.beq(RTS);
    a.bcs("envelope_releasing");
    // A level that does not move counts as falling.
    a.dec(abs(tick::COUNT));
    a.lda(abs(envelope::TARGET).y());
    a.cmp(abs(field::LEVEL).x());
    a.beq(RTS);
    // Bit 7: the target above the level, inverted for a falling one.
    a.ror(Acc);
    a.eor(abs(tick::COUNT));
    a.bmi(RTS);
    a.lda(abs(envelope::TARGET).y());
    a.sta(abs(field::LEVEL).x());
    a.inc(abs(field::LEVEL).x());
    a.inc(abs(field::PHASE).x());
    a.rts();
    a.here("envelope_releasing");
    a.lda(abs(field::LEVEL).x());
    a.bne(RTS);
    a.lda(abs(field::DURATION).x());
    a.bne(RTS);
    a.lda(imm(phase::IDLE));
    a.sta(abs(field::PHASE).x());
    a.rts();
}

/// Channel X's volume and pitch onto the chip, each only when it changed.
fn output(a: &mut Asm) {
    a.here("sound_output");
    a.lda(imm(SILENT));
    a.ldy(abs(field::PHASE).x());
    a.beq("sound_output_volume");
    a.ldy(abs(var::SOUND_SUPPRESS));
    a.bne("sound_output_volume");
    a.lda(abs(field::LEVEL).x());
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.eor(imm(SILENT));
    a.here("sound_output_volume");
    a.cmp(abs(field::ATTENUATION).x());
    a.beq("sound_output_pitch");
    a.sta(abs(field::ATTENUATION).x());
    a.ora(abs("sound_chip_channel").x());
    a.ora(imm(chip::LATCH | chip::VOLUME));
    a.jsr("sound_write");
    a.here("sound_output_pitch");
    a.lda(abs(field::PITCH).x());
    a.cmp(abs(field::CHIP_PITCH).x());
    a.beq(RTS);
    a.sta(abs(field::CHIP_PITCH).x());
    a.cpx(imm(0));
    a.bne("sound_period");
    a.and(imm(chip::NOISE_CONTROL));
    a.ora(imm(chip::NOISE));
    a.jmp("sound_write");
}

/// The Electron's generator, driven by the channel last addressed alone:
/// the counter given to sound while the note sounds at any level and taken
/// back when it is silent, and while a note plays written each tick, as
/// Electron OS 1.00 writes it, but while the cassette has it: a tone's from
/// its pitch, channel 0's from its kind of noise.
fn electron_output(a: &mut Asm) {
    a.here("sound_output");
    a.cpx(abs(tick::ADDRESSED));
    a.bne(RTS);
    a.lda(imm(0));
    a.sta(abs(tick::NOISE_KIND));
    a.lda(imm(SILENT));
    a.ldy(abs(field::PHASE).x());
    a.beq("sound_output_volume");
    a.ldy(abs(var::SOUND_SUPPRESS));
    a.bne("sound_output_volume");
    a.ldy(abs(field::LEVEL).x());
    a.beq("sound_output_volume");
    a.lda(imm(0));
    a.here("sound_output_volume");
    a.cmp(abs(GENERATOR));
    a.beq("sound_output_counter");
    a.sta(abs(GENERATOR));
    a.bit(zp(os::CFS_CRITICAL));
    a.bmi(RTS);
    a.cmp(imm(SILENT));
    a.lda(imm(control::SOUND));
    a.bcc("sound_output_comms");
    a.lda(imm(control::CASSETTE_INPUT));
    a.here("sound_output_comms");
    a.stx(zp(os::SCRATCH));
    a.ldx(imm(!control::COMMS));
    a.jsr("ula_control");
    a.ldx(zp(os::SCRATCH));
    a.jmp("sound_output_counter");
    a.here("sound_output_pitch");
    a.cpx(abs(tick::ADDRESSED));
    a.bne(RTS);
    a.here("sound_output_counter");
    a.bit(zp(os::CFS_CRITICAL));
    a.bmi(RTS);
    a.lda(abs(field::PHASE).x());
    a.beq(RTS);
    a.txa();
    a.bne("sound_output_tone");
    a.lda(abs(field::PITCH));
    a.and(imm(NOISE_KIND_BITS));
    a.tay();
    a.cpy(imm(RANDOM_NOISE));
    a.bcc("sound_noise");
    // Noise drawn at random while it sounds is drawn at OSRDCH's waits too.
    a.lda(abs(GENERATOR));
    a.bne("sound_noise");
    a.sty(abs(tick::NOISE_KIND));
    // Y, a kind of noise: its counter, drawn from the shift register. X is
    // kept.
    a.here("sound_noise");
    a.lda(abs(tick::NOISE_SEED));
    a.asl(Acc);
    a.bcc("sound_noise_drawn");
    a.eor(imm(0x1D));
    a.here("sound_noise_drawn");
    a.sta(abs(tick::NOISE_SEED));
    a.and(abs("noise_mask").y());
    a.ora(abs("noise_bits").y());
    a.sta(abs(ula::COUNTER));
    a.rts();
    a.here("sound_output_tone");
    a.lda(abs(field::PITCH).x());
    a.cmp(abs(field::CHIP_PITCH).x());
    a.beq("sound_output_cached");
    a.sta(abs(field::CHIP_PITCH).x());
    a.jsr("sound_period");
    a.sta(abs(field::COUNTER).x());
    a.here("sound_output_cached");
    a.lda(abs(field::COUNTER).x());
    a.sta(abs(ula::COUNTER));
    a.rts();

    // While OSRDCH waits: a fresh counter for noise drawn at random.
    a.here("sound_noise_wait");
    a.php();
    a.sei();
    a.ldy(abs(tick::NOISE_KIND));
    a.beq("sound_noise_waited");
    a.bit(zp(os::CFS_CRITICAL));
    a.bmi("sound_noise_waited");
    a.jsr("sound_noise");
    a.here("sound_noise_waited");
    a.plp();
    a.rts();
}

fn noise_tables(a: &mut Asm) {
    a.later(|a| {
        a.here("noise_mask");
        for mask in NOISE_MASKS {
            a.byte(mask);
        }
    });
    a.later(|a| {
        a.here("noise_bits");
        for bits in NOISE_BITS {
            a.byte(bits);
        }
    });
}

/// The period of pitch A on channel X: the semitone's, less the quarter
/// semitones, halved for each octave, then on the Model B detuned by the
/// channel so channels sounding one pitch do not cancel. The Electron's
/// counter sounds 1 MHz / 32 (S + 1) where the SN76489 sounds 4 MHz / 32 N,
/// so its counter, returned in A, is a quarter of the period, less one.
fn sound_period(a: &mut Asm, model: Model) {
    a.here("sound_period");
    a.ldy(imm(0));
    a.here("sound_period_octave");
    a.cmp(imm(OCTAVE));
    a.bcc("sound_period_semitone");
    a.sbc(imm(OCTAVE));
    a.iny();
    a.bne("sound_period_octave");
    a.here("sound_period_semitone");
    a.sty(abs(tick::OCTAVE));
    a.pha();
    a.and(imm(3));
    a.sta(abs(tick::QUARTERS));
    a.pla();
    a.lsr(Acc);
    a.lsr(Acc);
    a.tay();
    a.lda(abs("sound_semitone_low").y());
    a.sta(abs(tick::PERIOD_LOW));
    a.lda(abs("sound_semitone_high").y());
    a.sta(abs(tick::PERIOD_HIGH));
    a.here("sound_period_quarter");
    a.dec(abs(tick::QUARTERS));
    a.bmi("sound_period_shift");
    a.lda(abs(tick::PERIOD_LOW));
    a.sec();
    a.sbc(abs("sound_quarter").y());
    a.sta(abs(tick::PERIOD_LOW));
    a.bcs("sound_period_quarter");
    a.dec(abs(tick::PERIOD_HIGH));
    a.bcc("sound_period_quarter");
    a.here("sound_period_shift");
    a.ldy(abs(tick::OCTAVE));
    if model == Model::Electron {
        a.iny();
        a.iny();
    }
    a.beq("sound_period_detune");
    a.here("sound_period_halve");
    a.lsr(abs(tick::PERIOD_HIGH));
    a.ror(abs(tick::PERIOD_LOW));
    a.dey();
    a.bne("sound_period_halve");
    a.here("sound_period_detune");
    if model == Model::Electron {
        a.ldy(abs(tick::PERIOD_LOW));
        a.dey();
        a.tya();
        a.rts();
        return;
    }
    a.txa();
    a.clc();
    a.adc(imm(0xFF));
    a.clc();
    a.adc(abs(tick::PERIOD_LOW));
    a.sta(abs(tick::PERIOD_LOW));
    a.bcc("sound_period_write");
    a.inc(abs(tick::PERIOD_HIGH));
    a.here("sound_period_write");
    a.and(imm(chip::LOW_PERIOD));
    a.ora(abs("sound_chip_channel").x());
    a.ora(imm(chip::LATCH));
    a.jsr("sound_write");
    a.lda(abs(tick::PERIOD_HIGH));
    a.ldy(imm(4));
    let shift = a.here_local();
    a.asl(abs(tick::PERIOD_LOW));
    a.rol(Acc);
    a.dey();
    a.bne(shift);
    a.and(imm(chip::HIGH_PERIOD));

    // A to the chip: on port A while its write enable is pulsed low long
    // enough for it to take the byte. X is kept.
    a.here("sound_write");
    a.php();
    a.sei();
    a.ldy(imm(0xFF));
    a.sty(abs(svia(reg::DDRA)));
    a.sta(abs(svia(reg::ORA_NH)));
    a.lda(imm(WRITE_ENABLE_LOW));
    a.sta(abs(svia(reg::ORB)));
    a.pha();
    a.pla();
    a.pha();
    a.pla();
    a.lda(imm(WRITE_ENABLE_HIGH));
    a.sta(abs(svia(reg::ORB)));
    a.plp();
    a.rts();
}

/// The lowest octave's periods, and the quarter semitones' steps.
fn semitone_tables(a: &mut Asm) {
    a.later(|a| {
        a.here("sound_semitone_low");
        for (period, _) in SEMITONES {
            a.byte(period.to_le_bytes()[0]);
        }
    });
    a.later(|a| {
        a.here("sound_semitone_high");
        for (period, _) in SEMITONES {
            a.byte(period.to_le_bytes()[1]);
        }
    });
    a.later(|a| {
        a.here("sound_quarter");
        for (_, quarter) in SEMITONES {
            a.byte(quarter);
        }
    });
}
