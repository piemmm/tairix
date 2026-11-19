//! The Electron's four sound queues on its one generator, held to what
//! Electron OS 1.00 writes to the ULA's counter.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::vec::Vec;

use super::{page2, var};
use crate::machine::electron::{control, sheila as ula, Machine};
use crate::rom::harness::{electron, type_text, wait};
use crate::rom::os::buffer::SOUND;

/// The four sound queues' busy flags.
pub(super) fn sound_busy(machine: &Machine) -> [u8; 4] {
    [0, 1, 2, 3]
        .map(|channel: u16| machine.peek(page2::ELECTRON.buffer_busy + u16::from(SOUND) + channel))
}

/// What the OS wrote to the counter since the last look.
fn counters(machine: &mut Machine) -> Vec<u8> {
    machine
        .take_io_trace()
        .filter(|access| access.write && access.addr == ula::COUNTER)
        .map(|access| access.value)
        .collect()
}

fn comms(machine: &Machine) -> u8 {
    machine.peek(var::ULA_CONTROL_COPY) & control::COMMS
}

/// A sound for another channel flushes the one last addressed, its note and
/// its queue, where the Model B would play both: from the reset, the boot's
/// bell's channel 3, then channel 1.
#[test]
fn a_sound_for_another_channel_flushes_the_one_last_addressed() {
    let (mut machine, _) = electron();
    let p2 = &page2::ELECTRON;
    let one = u16::from(SOUND) + 1;
    machine.monitor_io(true);
    type_text(&mut machine, "SOUND1,-15,100,100:SOUND1,-15,150,100\r");
    wait(&mut machine, 20);
    assert_eq!(sound_busy(&machine), [0x80, 0, 0, 0x80]);
    assert_ne!(
        machine.peek(p2.buffer_start + one),
        machine.peek(p2.buffer_end + one),
        "the second note waits in channel 1's queue"
    );
    counters(&mut machine);
    type_text(&mut machine, "SOUND2,-15,200,10\r");
    wait(&mut machine, 20);
    assert_eq!(sound_busy(&machine), [0x80, 0x80, 0, 0x80]);
    assert_eq!(
        machine.peek(p2.buffer_start + one),
        machine.peek(p2.buffer_end + one),
        "channel 1's queue emptied where it stood"
    );
    assert_eq!(counters(&mut machine).last(), Some(&0x0D));
    assert_eq!(comms(&machine), control::SOUND);
    wait(&mut machine, 60);
    assert_eq!(
        comms(&machine),
        control::CASSETTE_INPUT,
        "silent once channel 2's note ends, though channel 1's would run on"
    );
}

#[test]
fn a_second_sound_for_the_same_channel_waits_for_the_first() {
    let (mut machine, _) = electron();
    machine.monitor_io(true);
    type_text(&mut machine, "SOUND1,-15,100,20:SOUND1,-15,150,20\r");
    wait(&mut machine, 50);
    assert_eq!(counters(&mut machine).last(), Some(&0x3A), "the first");
    wait(&mut machine, 80);
    assert_eq!(
        counters(&mut machine).last(),
        Some(&0x1B),
        "then the second"
    );
}

/// The counter is a quarter of the SN76489's period for the pitch, less
/// one, so the note is in tune at the counter's 1 MHz / 32 (S + 1); these
/// pitches' counters are Electron OS 1.00's.
#[test]
fn a_pitch_sets_the_counter_electron_os_1_00_sets() {
    let (mut machine, _) = electron();
    machine.monitor_io(true);
    for (pitch, counter) in [
        (5, 0xE9),
        (53, 0x74),
        (101, 0x39),
        (149, 0x1C),
        (197, 0x0D),
        (255, 0x05),
    ] {
        counters(&mut machine);
        type_text(&mut machine, &format!("SOUND1,-15,{pitch},1\r"));
        wait(&mut machine, 20);
        assert_eq!(
            counters(&mut machine).last(),
            Some(&counter),
            "pitch {pitch}"
        );
    }
}

/// Channel 0's notes are noise: kinds 0–3 a fixed tone, kinds 4–7 a counter
/// drawn in the kind's range.
#[test]
fn noise_is_a_fixed_tone_or_drawn_in_its_kinds_range() {
    let (mut machine, _) = electron();
    machine.monitor_io(true);
    for (kind, lowest, highest) in [
        (0, 59, 59),
        (1, 119, 119),
        (2, 239, 239),
        (3, 119, 119),
        (4, 0x40, 0x7F),
        (5, 0x80, 0xFF),
        (6, 0xC0, 0xFF),
        (7, 0x80, 0xFF),
    ] {
        type_text(&mut machine, &format!("SOUND0,-15,{kind},4\r"));
        counters(&mut machine);
        wait(&mut machine, 10);
        let drawn = counters(&mut machine);
        assert!(
            !drawn.is_empty()
                && drawn
                    .iter()
                    .all(|counter| (lowest..=highest).contains(counter)),
            "kind {kind}: {drawn:?}"
        );
        if lowest != highest {
            assert!(
                drawn.iter().collect::<BTreeSet<_>>().len() > 1,
                "kind {kind}: {drawn:?}"
            );
        }
        wait(&mut machine, 20);
    }
}

/// Noise drawn at random is drawn afresh at each pass while OSRDCH waits, as
/// Electron OS 1.00 draws it, but once a tick while a program runs.
#[test]
fn random_noise_is_drawn_at_each_pass_while_osrdch_waits() {
    let (mut machine, _) = electron();
    machine.monitor_io(true);
    type_text(&mut machine, "SOUND0,-15,4,40\r");
    wait(&mut machine, 5);
    counters(&mut machine);
    wait(&mut machine, 20);
    let waiting = counters(&mut machine).len();
    wait(&mut machine, 300);
    type_text(&mut machine, "SOUND0,-15,4,40:FOR I%=1 TO 3000:NEXT\r");
    wait(&mut machine, 5);
    counters(&mut machine);
    wait(&mut machine, 20);
    let running = counters(&mut machine).len();
    assert!((15..=21).contains(&running), "a draw a tick: {running}");
    assert!(
        waiting > 10 * running,
        "{waiting} at the prompt, {running} running"
    );
}
