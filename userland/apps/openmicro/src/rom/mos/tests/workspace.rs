//! Page 2 where each model's original keeps it: the Electron's a byte below
//! the Model B's from the clocks up, what a reset leaves there and what a
//! flush does to a buffer.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::vec::Vec;

use super::electron_sound::sound_busy;
use super::{page2, run, var};
use crate::machine::electron::keyboard::Key;
use crate::machine::electron::{control, Machine};
use crate::rom::harness::{electron, last_number, press, press_break, screen, type_text, wait};
use crate::rom::mos::vdu::status;
use crate::rom::mos::ws::vdu_zp;
use crate::rom::os::buffer::{COUNT, KEYBOARD, SOUND};

#[test]
fn osbyte_aa_gives_the_rom_table_where_electron_os_1_00_keeps_it() {
    let (mut machine, os) = electron();
    type_text(
        &mut machine,
        "A%=&AA:X%=0:Y%=&FF:PRINT (USR(&FFF4) AND &FFFF00) DIV 256\r",
    );
    wait(&mut machine, 10);
    assert_eq!(
        last_number(&machine, &os),
        Some(i64::from(page2::ELECTRON.rom_types)),
        "{:?}",
        screen(&machine, &os)
    );
    assert_ne!(machine.peek(page2::ELECTRON.rom_types + 11), 0, "BASIC");
}

/// OSWORD 0's limits and the event flags, an enabled event's flag its own
/// number, as both originals leave it.
#[test]
fn the_line_limits_and_event_flags_lie_where_electron_os_1_00_keeps_them() {
    let (mut machine, _) = electron();
    // Copied before BASIC's own line input sets its limits there.
    type_text(
        &mut machine,
        &format!(
            "?&70=0:?&71=&A:?&72=&40:?&73=&21:?&74=&7E:A%=0:X%=&70:Y%=0:CALL&FFF1:!&80=!&{:X}\r",
            page2::ELECTRON.line_limits
        ),
    );
    wait(&mut machine, 10);
    type_text(&mut machine, "Q\r");
    wait(&mut machine, 10);
    assert_eq!(
        [0x80, 0x81, 0x82].map(|at| machine.peek(at)),
        [0x40, 0x21, 0x7E]
    );
    type_text(&mut machine, "*FX14,9\r");
    wait(&mut machine, 5);
    assert_eq!(machine.peek(page2::ELECTRON.events + 9), 14);
    type_text(&mut machine, "*FX13,9\r");
    wait(&mut machine, 5);
    assert_eq!(machine.peek(page2::ELECTRON.events + 9), 0);
}

#[test]
fn the_model_b_marks_an_enabled_event_with_its_own_number() {
    let machine = run(&[], &["FX14,9"]);
    assert_eq!(machine.peek(page2::MODEL_B.events + 9), 14);
}

/// Every reset fills page 2 from the buffers' flags up with &FF but for the
/// mode's field and the last ADC channel, every buffer empty at its last
/// slot; a hard reset's bell, on channel 3, then flushes channel 0, the one
/// addressed before it, where a soft reset leaves every sound flag clear.
/// Either clears the first repeat's count.
#[test]
fn a_reset_leaves_page_2_as_electron_os_1_00_does() {
    let (mut machine, _) = electron();
    let p2 = &page2::ELECTRON;
    let starts = |machine: &Machine| {
        [0, 1, 2, 3].map(|channel: u16| machine.peek(p2.buffer_start + u16::from(SOUND) + channel))
    };
    assert_eq!(sound_busy(&machine), [0x80, 0, 0, 0]);
    assert_eq!(
        starts(&machine),
        [0xFF, 0xFF, 0xFF, 0xF2],
        "the bell's three bytes"
    );
    let mode = 6 << control::MODE.trailing_zeros();
    for at in p2.buffer_end + u16::from(COUNT)..0x300 {
        let set = match at {
            _ if at == page2::ULA_FIELD => mode,
            _ if at == p2.adc_last => 0,
            _ => 0xFF,
        };
        assert_eq!(machine.peek(at), set, "&{at:03X}");
    }
    type_text(&mut machine, "*FX11,20\r");
    machine.set_key(Key::A, true);
    wait(&mut machine, 100);
    machine.set_key(Key::A, false);
    type_text(&mut machine, "\r");
    assert_ne!(
        machine.peek(p2.first_repeat),
        0,
        "a held key's repeat counted"
    );
    press_break(&mut machine, &[]);
    wait(&mut machine, 100);
    assert_eq!(machine.peek(p2.first_repeat), 0);
    assert_eq!(sound_busy(&machine), [0; 4]);
    assert_eq!(starts(&machine), [0xFF; 4]);
}

/// A flush empties a buffer where it stands, dropping what was not yet read,
/// as Electron OS 1.00's does, and marks each sound queue it flushes.
#[test]
fn a_flush_empties_a_buffer_where_it_stands() {
    let (mut machine, _) = electron();
    let p2 = &page2::ELECTRON;
    let offsets = |machine: &Machine| {
        let keyboard = u16::from(KEYBOARD);
        [
            machine.peek(p2.buffer_start + keyboard),
            machine.peek(p2.buffer_end + keyboard),
        ]
    };
    type_text(&mut machine, "FOR I%=1 TO 3000:NEXT:OSCLI\"FX15,1\"\r");
    wait(&mut machine, 5);
    let [read, _] = offsets(&machine);
    type_text(&mut machine, "XYZ");
    let [_, typed] = offsets(&machine);
    assert_ne!(typed, read, "typed ahead");
    wait(&mut machine, 300);
    assert_eq!(offsets(&machine), [read; 2]);
    press(&mut machine, &[Key::ESCAPE]);
    wait(&mut machine, 10);
    assert_eq!(sound_busy(&machine), [0x80; 4]);
}

#[test]
fn mode_records_its_bits_in_the_ula_field() {
    let (mut machine, _) = electron();
    for mode in 0..=6u8 {
        type_text(&mut machine, &format!("MODE{mode}\r"));
        wait(&mut machine, 10);
        assert_eq!(
            machine.peek(page2::ULA_FIELD),
            mode << control::MODE.trailing_zeros()
        );
    }
}

/// The flash phase steps between 0 and 7, the EOR the flashing colours'
/// second colours take, as Electron OS 1.00 keeps it.
#[test]
fn the_flash_phase_steps_by_the_flashing_colours_eor() {
    let (mut machine, _) = electron();
    type_text(&mut machine, "*FX9,1\r*FX10,1\r");
    let mut phases = BTreeSet::new();
    for _ in 0..20 {
        wait(&mut machine, 2);
        phases.insert(machine.peek(var::ULA_CONTROL));
    }
    assert_eq!(phases.into_iter().collect::<Vec<_>>(), [0, 7]);
}

/// VDU 23,1,0 turns the cursor off in the VDU status, where Electron OS 1.00
/// records it, until MODE turns it back on.
#[test]
fn the_cursor_stays_off_in_the_status_until_mode() {
    let (mut machine, _) = electron();
    let off = |machine: &Machine| machine.peek(vdu_zp::STATUS) & status::CURSOR_OFF != 0;
    type_text(&mut machine, "VDU23,1,0;0;0;0;\r");
    wait(&mut machine, 5);
    assert!(off(&machine));
    type_text(&mut machine, "MODE6\r");
    wait(&mut machine, 5);
    assert!(!off(&machine));
}
