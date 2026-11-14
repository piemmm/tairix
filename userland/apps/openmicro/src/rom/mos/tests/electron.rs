//! The Electron's OS on the emulated Electron with BASIC, typed at as a
//! user types and read back off its screen.

use alloc::vec::Vec;

use super::{var, vdu};
use crate::machine::canvas::encode;
use crate::machine::electron::keyboard::Key;
use crate::machine::electron::Machine;
use crate::machine::electron::{control, sheila as ula};
use crate::media::tape::Tape;
use crate::rom::harness::{electron, last_number, press, screen, shows, type_text, wait};

#[test]
fn the_electron_starts_with_its_own_name_then_basic() {
    let (machine, os) = electron();
    let lines = screen(&machine, &os);
    assert_eq!(&lines[..6], ["", "OpenMOS Electron", "", "BASIC", "", ">_"]);
    assert_eq!(machine.peek(vdu::MODE), 6, "it starts in MODE 6");
    assert!(machine.caps_lock_led(), "with CAPS LOCK on");
}

#[test]
fn a_line_typed_at_basic_runs() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "PRINT 6*7\r");
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "        42"),
        "{:?}",
        screen(&machine, &os)
    );
}

#[test]
fn shift_with_func_turns_caps_lock_over_and_its_light_with_it() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "A$=GET$\r");
    press(&mut machine, &[Key::SHIFT, Key::FUNC]);
    assert!(!machine.caps_lock_led());
    press(&mut machine, &[Key::A]);
    press(&mut machine, &[Key::SHIFT, Key::FUNC]);
    assert!(machine.caps_lock_led());
    type_text(&mut machine, "PRINT A$;\"@\"\r");
    wait(&mut machine, 20);
    assert!(shows(&machine, &os, "a@"), "{:?}", screen(&machine, &os));
}

#[test]
fn func_with_a_letter_types_its_keyword_and_with_a_digit_its_soft_key() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "*KEY0 PRINT 6*7|M\r");
    press(&mut machine, &[Key::FUNC, Key::DIGIT_0]);
    wait(&mut machine, 30);
    assert!(
        shows(&machine, &os, "        42"),
        "{:?}",
        screen(&machine, &os)
    );
    type_text(&mut machine, "10 ");
    press(&mut machine, &[Key::FUNC, Key::L]);
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, ">10 LIST"),
        "{:?}",
        screen(&machine, &os)
    );
    type_text(&mut machine, "\r");
    press(&mut machine, &[Key::FUNC, Key::R]);
    wait(&mut machine, 30);
    assert!(
        screen(&machine, &os)
            .iter()
            .any(|line| line.starts_with(">RUN")),
        "RUN types its RETURN: {:?}",
        screen(&machine, &os)
    );
}

#[test]
fn the_cursor_keys_and_copy_type_their_characters_with_shift_and_ctrl() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "PRINT \"^~|\\`{_}[]\"\r");
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "^~|\\`{_}[]"),
        "{:?}",
        screen(&machine, &os)
    );
}

#[test]
fn the_os_names_itself_as_electron_os_1_00() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "PRINT INKEY(-256)\r*HELP\r");
    wait(&mut machine, 30);
    let lines = screen(&machine, &os);
    assert!(shows(&machine, &os, "         1"), "{lines:?}");
    assert!(shows(&machine, &os, "OS 1.00"), "{lines:?}");
}

#[test]
fn mode_7_is_mode_6_and_mode_4_its_own() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "MODE 7\r");
    wait(&mut machine, 30);
    assert_eq!(machine.peek(vdu::MODE), 6);
    type_text(&mut machine, "MODE 4\rPRINT \"FOUR\"\r");
    wait(&mut machine, 30);
    assert_eq!(machine.peek(vdu::MODE), 4);
    assert!(shows(&machine, &os, "FOUR"), "{:?}", screen(&machine, &os));
}

#[test]
fn a_colour_shows_through_the_palette_vdu_19_sets() {
    let (mut machine, _) = electron();
    let red = encode(1, 0, 3);
    let blue = encode(4, 0, 3);
    let has = |machine: &Machine, slot: u8| machine.video().shown().contains(&slot);
    type_text(&mut machine, "MODE 1:COLOUR 1:PRINT \"XX\"\r");
    wait(&mut machine, 30);
    assert!(has(&machine, red), "logical 1 is red in MODE 1");
    type_text(&mut machine, "VDU 19,1,4,0,0,0\r");
    wait(&mut machine, 30);
    assert!(
        has(&machine, blue) && !has(&machine, red),
        "and blue once set so"
    );
}

/// The regression: a queued note fell into the queue's buffer routine rather
/// than starting, and the counter was then given to the cassette's output.
#[test]
fn a_note_sounds_on_the_ulas_counter_for_its_duration() {
    let (mut machine, _) = electron();
    let comms = |machine: &Machine| machine.peek(var::ULA_CONTROL_COPY) & control::COMMS;
    type_text(&mut machine, "SOUND 1,-15,89,10\r");
    machine.take_audio();
    wait(&mut machine, 30);
    let sounding: Vec<f32> = machine.take_audio().collect();
    assert!(sounding.iter().any(|&level| level > 0.0), "the note sounds");
    assert_eq!(comms(&machine), control::SOUND);
    wait(&mut machine, 100);
    machine.take_audio();
    wait(&mut machine, 20);
    assert!(machine.take_audio().all(|level| level == 0.0), "and stops");
    assert_eq!(
        comms(&machine),
        control::CASSETTE_INPUT,
        "the counter given back"
    );
}

#[test]
fn time_counts_centiseconds_from_the_two_ticks_a_frame() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "TIME=0\r");
    wait(&mut machine, 200);
    type_text(&mut machine, "PRINT TIME DIV 10\r");
    wait(&mut machine, 20);
    let reading = last_number(&machine, &os).expect("TIME printed");
    // Two seconds, and the time the second line took to type.
    assert!(
        (20..=40).contains(&reading),
        "{reading} tenths: {:?}",
        screen(&machine, &os)
    );
}

#[test]
fn a_program_saved_to_tape_loads_back() {
    let (mut machine, os) = electron();
    machine.insert_tape(Tape::default());
    type_text(&mut machine, "10 PRINT \"TAPE\"\rSAVE \"P\"\r");
    wait(&mut machine, 50);
    assert!(
        shows(&machine, &os, "RECORD then RETURN"),
        "{:?}",
        screen(&machine, &os)
    );
    machine.set_tape_record(true);
    type_text(&mut machine, "\r");
    wait(&mut machine, 2000);
    machine.set_tape_record(false);
    machine.wind_tape(0);
    type_text(&mut machine, "NEW\rLOAD \"P\"\r");
    wait(&mut machine, 2000);
    type_text(&mut machine, "RUN\r");
    wait(&mut machine, 30);
    assert!(shows(&machine, &os, "TAPE"), "{:?}", screen(&machine, &os));
}

/// The screen start the ULA was last given, as `&FE02` and `&FE03` hold it.
fn ula_start(machine: &mut Machine) -> Option<u16> {
    let (mut low, mut high) = (None, None);
    for access in machine.take_io_trace().filter(|access| access.write) {
        match access.addr {
            ula::START_LOW => low = Some(access.value),
            ula::START_HIGH => high = Some(access.value),
            _ => {}
        }
    }
    Some((u16::from(high? & 0x3F) << 9) | (u16::from(low? & 0xE0) << 1))
}

#[test]
fn the_screen_scrolls_by_moving_the_ulas_start() {
    let (mut machine, os) = electron();
    type_text(&mut machine, "FOR I=1 TO 30:PRINT I:NEXT");
    machine.monitor_io(true);
    type_text(&mut machine, "\r");
    wait(&mut machine, 100);
    let start = u16::from_le_bytes([
        machine.peek(vdu::SCREEN_START),
        machine.peek(vdu::SCREEN_START + 1),
    ]);
    assert_ne!(start, 0x6000, "scrolled");
    assert_eq!(
        ula_start(&mut machine),
        Some(start),
        "the ULA shows where the OS draws"
    );
    let lines = screen(&machine, &os);
    assert_eq!(lines[22], "        29", "{lines:?}");
    assert_eq!(lines[23], "        30", "{lines:?}");
}

#[test]
fn a_flashing_colour_alternates_at_the_flash_rate() {
    let (mut machine, _) = electron();
    type_text(&mut machine, "MODE 2:COLOUR 9:PRINT \"XXXX\"\r");
    wait(&mut machine, 20);
    let (red, cyan) = (encode(1, 0, 3), encode(6, 0, 3));
    let mut seen = Vec::new();
    for _ in 0..24 {
        wait(&mut machine, 5);
        let shown = machine.video().shown();
        seen.push((shown.contains(&red), shown.contains(&cyan)));
    }
    assert!(seen.contains(&(true, false)), "red alone: {seen:?}");
    assert!(seen.contains(&(false, true)), "cyan alone: {seen:?}");
    // Half a second each, as *FX9 and *FX10 start.
    let changes = seen.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert!((1..=3).contains(&changes), "{seen:?}");
}

#[test]
fn the_cursor_flashes_every_sixteen_centiseconds() {
    let (mut machine, os) = electron();
    let mut shown = Vec::new();
    for _ in 0..32 {
        wait(&mut machine, 2);
        shown.push(screen(&machine, &os)[5] == ">_");
        assert!(screen(&machine, &os)[5].starts_with('>'));
    }
    let changes = shown.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert!((3..=5).contains(&changes), "on and off: {shown:?}");
}
