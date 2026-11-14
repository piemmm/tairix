//! The Plus 1 ROM on the emulated Electron with BASIC: the analogue port and
//! its fire buttons, the parallel printer, and *FX163.

use alloc::vec::Vec;

use super::build;
use crate::machine::electron::keyboard::Key;
use crate::machine::electron::Machine;
use crate::rom::harness::{
    electron_with_plus1, last_number, press, screen, shows, type_text, wait,
};
use crate::rom::mos::ws::var::POLL_SEMAPHORE;
use crate::rom::os::page2;

fn printed(machine: &mut Machine) -> Vec<u8> {
    machine.take_printer_output().collect()
}

/// BREAK, with CTRL for a hard one, and the time the machine takes to start.
fn break_key(machine: &mut Machine, hard: bool) {
    if hard {
        machine.set_key(Key::CTRL, true);
    }
    machine.set_break(true);
    wait(machine, 2);
    machine.set_break(false);
    wait(machine, 300);
    machine.set_key(Key::CTRL, false);
}

#[test]
fn the_header_names_a_service_rom_of_its_own() {
    let image = build().expect("the Plus 1 ROM assembles");
    let bytes = &image.bytes;
    assert_eq!(bytes.len(), 0x4000);
    assert_eq!(&bytes[..3], &[0, 0, 0], "no language entry");
    assert_eq!(bytes[6], 0x82, "a service ROM of 6502 code");
    assert_eq!(&bytes[9..23], b"OpenMOS Plus 1");
    let copyright = usize::from(bytes[7]);
    assert_eq!(&bytes[copyright..copyright + 4], b"\0(C)");
}

#[test]
fn help_names_the_rom_before_the_os() {
    let (mut machine, os) = electron_with_plus1();
    type_text(&mut machine, "*HELP\r");
    wait(&mut machine, 20);
    let lines = screen(&machine, &os);
    let plus1 = lines.iter().position(|line| line == "OpenMOS Plus 1 1.00");
    let os_line = lines.iter().position(|line| line == "OS 1.00");
    assert!(
        matches!((plus1, os_line), (Some(p), Some(o)) if p < o),
        "{lines:?}"
    );
}

/// A reading is the input's top eight bits, high in sixteen.
#[test]
fn adval_reads_each_channel_of_the_analogue_port() {
    let (mut machine, os) = electron_with_plus1();
    for (channel, level) in [(0, 0x1234), (1, 0x8000), (2, 0xFFFF), (3, 0x0100)] {
        machine.set_analogue(channel, level);
    }
    wait(&mut machine, 10);
    type_text(
        &mut machine,
        "PRINT ADVAL(1),ADVAL(2)\rPRINT ADVAL(3),ADVAL(4)\r",
    );
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "      4608     32768"),
        "{:?}",
        screen(&machine, &os)
    );
    assert!(
        shows(&machine, &os, "     65280       256"),
        "{:?}",
        screen(&machine, &os)
    );
}

#[test]
fn adval_0_holds_the_fire_buttons_and_the_channel_last_converted() {
    let (mut machine, os) = electron_with_plus1();
    type_text(&mut machine, "*FX16,1\r");
    machine.set_fire_buttons(0b10);
    wait(&mut machine, 10);
    type_text(&mut machine, "PRINT ADVAL(0) AND 3,ADVAL(0) DIV 256\r");
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "         2         1"),
        "{:?}",
        screen(&machine, &os)
    );
    machine.set_fire_buttons(0b01);
    type_text(&mut machine, "PRINT ADVAL(0) AND 3\r");
    wait(&mut machine, 20);
    assert_eq!(
        last_number(&machine, &os),
        Some(1),
        "{:?}",
        screen(&machine, &os)
    );
}

#[test]
fn fx16_converts_only_the_channels_it_selects() {
    let (mut machine, os) = electron_with_plus1();
    for channel in 0..4 {
        machine.set_analogue(channel, 0x1000);
    }
    wait(&mut machine, 10);
    type_text(&mut machine, "*FX16,2\r");
    for (channel, level) in [(0, 0x2000), (1, 0x3000), (2, 0x4000), (3, 0x5000)] {
        machine.set_analogue(channel, level);
    }
    wait(&mut machine, 10);
    type_text(
        &mut machine,
        "PRINT ADVAL(1),ADVAL(2)\rPRINT ADVAL(3),ADVAL(4)\r",
    );
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "      8192     12288"),
        "{:?}",
        screen(&machine, &os)
    );
    assert!(
        shows(&machine, &os, "      4096      4096"),
        "{:?}",
        screen(&machine, &os)
    );
}

/// With no channels selected, a forced conversion reads its channel, and the
/// channels below it as the turn runs down, and no other.
#[test]
fn fx17_converts_its_channel_with_none_selected() {
    let (mut machine, os) = electron_with_plus1();
    type_text(&mut machine, "*FX16,0\r");
    machine.set_analogue(2, 0x6000);
    machine.set_analogue(3, 0x7000);
    wait(&mut machine, 10);
    type_text(&mut machine, "PRINT ADVAL(3),ADVAL(4)\r");
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "         0         0"),
        "{:?}",
        screen(&machine, &os)
    );
    type_text(&mut machine, "*FX17,3\r");
    wait(&mut machine, 10);
    type_text(&mut machine, "PRINT ADVAL(3),ADVAL(4)\r");
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "     24576         0"),
        "{:?}",
        screen(&machine, &os)
    );
}

/// A setting past the four channels is the fourth, and nothing past page 2's
/// tables is written.
#[test]
fn a_channel_setting_past_four_converts_the_four() {
    let (mut machine, os) = electron_with_plus1();
    let events = machine.peek(page2::ADC_LAST + 1);
    type_text(&mut machine, "*FX189,200\r*FX188,200\r");
    machine.set_analogue(3, 0x9000);
    wait(&mut machine, 10);
    type_text(&mut machine, "PRINT ADVAL(4)\r");
    wait(&mut machine, 20);
    assert_eq!(
        last_number(&machine, &os),
        Some(0x9000),
        "{:?}",
        screen(&machine, &os)
    );
    assert!((1..=4).contains(&machine.peek(page2::ADC_LAST)));
    assert_eq!(machine.peek(page2::ADC_LAST + 1), events);
}

/// An event handler counting event 3 at &70 and keeping its channel at &71.
#[test]
fn each_conversion_raises_event_3_with_its_channel() {
    let (mut machine, os) = electron_with_plus1();
    type_text(
        &mut machine,
        "DIM C% 11:!C%=&D003C908:C%!4=&8470E604:C%!8=&602871\r\
         ?&220=C% MOD 256:?&221=C% DIV 256:?&70=0\r*FX14,3\r",
    );
    wait(&mut machine, 50);
    type_text(&mut machine, "PRINT ?&70>0,?&71\r");
    wait(&mut machine, 20);
    let lines = screen(&machine, &os);
    let answer = lines
        .iter()
        .rev()
        .find(|line| line.starts_with("        -1"))
        .expect("an answer");
    let channel: u8 = answer[10..].trim().parse().expect("a channel");
    assert!((1..=4).contains(&channel), "{lines:?}");
}

#[test]
fn the_parallel_printer_takes_what_vdu_2_prints() {
    let (mut machine, _) = electron_with_plus1();
    type_text(&mut machine, "VDU2:PRINT \"HELLO\":VDU3\r");
    wait(&mut machine, 30);
    assert_eq!(printed(&mut machine), b"HELLO\r");
}

/// More than the buffer holds and a poll sends, in order, each wake from
/// dormant and each return to it on the way.
#[test]
fn a_long_print_arrives_whole_and_in_order() {
    let (mut machine, _) = electron_with_plus1();
    type_text(
        &mut machine,
        "VDU2:FOR I=1 TO 40:PRINT \"LINE \";I:NEXT:VDU3\r",
    );
    wait(&mut machine, 300);
    let expected: Vec<u8> = (1..=40)
        .flat_map(|i| alloc::format!("LINE {i}\r").into_bytes())
        .collect();
    assert_eq!(printed(&mut machine), expected);
}

#[test]
fn fx163_128_1_turns_both_ports_off_and_0_on_again() {
    let (mut machine, os) = electron_with_plus1();
    assert_eq!(machine.peek(POLL_SEMAPHORE), 1, "polled from the reset");
    type_text(&mut machine, "*FX163,128,1\r");
    assert_eq!(machine.peek(POLL_SEMAPHORE), 0, "and no longer");
    machine.set_analogue(0, 0x4000);
    wait(&mut machine, 10);
    type_text(&mut machine, "VDU2:PRINT \"OFF\":VDU3\rPRINT ADVAL(1)\r");
    wait(&mut machine, 30);
    assert_eq!(printed(&mut machine), b"", "nothing printed");
    assert_eq!(last_number(&machine, &os), Some(0), "nothing converted");
    type_text(&mut machine, "*FX163,128,0\r");
    assert_eq!(machine.peek(POLL_SEMAPHORE), 1);
    wait(&mut machine, 10);
    type_text(&mut machine, "VDU2:PRINT \"ON\":VDU3\rPRINT ADVAL(1)\r");
    wait(&mut machine, 30);
    assert_eq!(printed(&mut machine), b"ON\r");
    assert_eq!(
        last_number(&machine, &os),
        Some(0x4000),
        "{:?}",
        screen(&machine, &os)
    );
}

/// The OSBYTE calls *FX163 makes inside itself leave the registers it
/// returns as it was given them.
#[test]
fn fx163_returns_the_registers_it_was_called_with() {
    let (mut machine, os) = electron_with_plus1();
    type_text(
        &mut machine,
        "A%=&A3:X%=128:Y%=1:PRINT ~USR(&FFF4) AND &FFFFFF\r",
    );
    wait(&mut machine, 20);
    assert!(
        shows(&machine, &os, "     180A3"),
        "{:?}",
        screen(&machine, &os)
    );
    assert_eq!(machine.peek(POLL_SEMAPHORE), 0);
}

#[test]
fn a_break_keeps_the_ports_off_and_a_hard_break_turns_them_on() {
    let (mut machine, _) = electron_with_plus1();
    type_text(&mut machine, "*FX163,128,1\r");
    break_key(&mut machine, false);
    assert_eq!(machine.peek(POLL_SEMAPHORE), 0, "off through BREAK");
    break_key(&mut machine, true);
    assert_eq!(machine.peek(POLL_SEMAPHORE), 1, "on after CTRL-BREAK");
    press(&mut machine, &[Key::RETURN]);
    type_text(&mut machine, "VDU2:PRINT \"BACK\":VDU3\r");
    wait(&mut machine, 30);
    assert_eq!(printed(&mut machine), b"BACK\r");
}
