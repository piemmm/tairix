//! The DOS on the true drive, this tree's KERNAL and BASIC driving it
//! through BASIC programs typed at the prompt; each screen is the one the
//! original DOS leaves in the same drive.

use alloc::string::String;
use alloc::vec::Vec;

use crate::machine::c64::drive::{bus, via1, Drive, Lines, Unit, CLOCK};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::{Machine, Standard};
use crate::machine::via::reg;
use crate::media::c1541::{d64, Surface};
use crate::rom::c64::system;
use crate::rom::c64::testing::{machine, press, run_for, screen_rows, type_text, JIFFY};

use super::errors::number;

mod blocks;
mod controller;
mod disk;
mod files;
mod format;
mod rel;
mod writing;

/// A machine with this DOS's drive as device 8 holding `disk`, settled.
fn with_drive(disk: Surface) -> Machine {
    let system = system().expect("the BASIC and KERNAL ROMs assemble");
    let dos = super::build().expect("the DOS assembles");
    let mut machine = machine(Standard::Pal, system.basic(), system.kernal());
    machine
        .fit_drive(Unit::Eight, &dos.bytes)
        .expect("a 16 KiB DOS");
    if let Some(drive) = machine.drives().unit_mut(Unit::Eight) {
        drive.insert(disk);
    }
    run_for(&mut machine, 4 * Standard::Pal.clock());
    machine
}

fn surface(image: &[u8]) -> Surface {
    d64::decode(image).expect("a D64")
}

/// The jiffies a `~` lets pass.
const PAUSE: u64 = 300;

/// The machine after CLR and `keys` typed with `disk` in the drive: `|` is
/// RETURN, `~` lets [`PAUSE`] jiffies pass, and `{` swaps `second` in for
/// the disk.
fn played(disk: Surface, second: Option<&[u8]>, keys: &str) -> Machine {
    let mut machine = with_drive(disk);
    press(&mut machine, &[Key::LEFT_SHIFT], Key::HOME);
    for (index, part) in keys.split(['~', '{']).enumerate() {
        if index > 0 {
            let at = keys
                .match_indices(['~', '{'])
                .nth(index - 1)
                .map(|(at, _)| at);
            match at.and_then(|at| keys[at..].chars().next()) {
                Some('{') => {
                    machine
                        .drives()
                        .unit_mut(Unit::Eight)
                        .expect("drive 8 is fitted")
                        .eject();
                    run_for(&mut machine, 100 * JIFFY);
                    if let (Some(drive), Some(second)) =
                        (machine.drives().unit_mut(Unit::Eight), second)
                    {
                        drive.insert(surface(second));
                    }
                    run_for(&mut machine, 100 * JIFFY);
                }
                _ => run_for(&mut machine, PAUSE * JIFFY),
            }
        }
        type_text(&mut machine, &part.replace('|', "\n"));
    }
    run_for(&mut machine, 90 * JIFFY);
    machine
}

/// The screen after CLR, `keys` typed (`|` is RETURN) and `jiffies` let
/// pass, with `disk` in the drive.
fn screen_after(disk: Surface, keys: &str, jiffies: u64) -> Vec<String> {
    let mut machine = with_drive(disk);
    press(&mut machine, &[Key::LEFT_SHIFT], Key::HOME);
    type_text(&mut machine, &keys.replace('|', "\n"));
    run_for(&mut machine, jiffies * JIFFY);
    screen_rows(&machine)
}

#[test]
fn the_error_channel_reads_the_power_on_message() {
    assert_eq!(
        screen_after(
            Surface::blank(),
            "10 OPEN 15,8,15:INPUT#15,A,B$,C,D:PRINT A;B$;C;D|RUN|",
            300
        ),
        [
            "10 OPEN 15,8,15:INPUT#15,A,B$,C,D:PRINT",
            "A;B$;C;D",
            "RUN",
            " 73 OPENMICRO DOS 1541 0  0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn an_unknown_command_is_a_syntax_error_and_the_message_then_clears() {
    assert_eq!(
        screen_after(
            Surface::blank(),
            "10 OPEN 15,8,15:PRINT#15,\"X\"|20 FOR I=1 TO 2:INPUT#15,A,B$,C,D:PRINT A;B$;C;D:NEXT|RUN|",
            400
        ),
        [
            "10 OPEN 15,8,15:PRINT#15,\"X\"",
            "20 FOR I=1 TO 2:INPUT#15,A,B$,C,D:PRINT",
            "A;B$;C;D:NEXT",
            "RUN",
            " 31 SYNTAX ERROR 0  0",
            " 0 OK 0  0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn the_talker_holds_each_bit_long_enough_for_a_stalled_controller() {
    let mut machine = with_drive(Surface::blank());
    press(&mut machine, &[Key::LEFT_SHIFT], Key::HOME);
    // The screen blanked, so no bad line stalls the controller and every
    // sample falls within a few cycles of the last.
    type_text(
        &mut machine,
        "10 POKE 53265,11:OPEN 15,8,15:INPUT#15,A$\nRUN\n",
    );
    // CLOCK's spans while the drive talks, in the drive's cycles, until the
    // message's RETURN is taken and its error cleared.
    // The span under way when sampling starts is not counted whole.
    let mut spans: Vec<(bool, u64)> = Vec::new();
    let drive_state = |machine: &mut Machine| {
        machine
            .drives()
            .unit(Unit::Eight)
            .map(|drive| (drive.holds().clock, drive.cycles()))
    };
    let (mut held, _) = drive_state(&mut machine).unwrap_or_default();
    let mut since = None;
    let error = |machine: &mut Machine| {
        machine
            .drives()
            .unit(Unit::Eight)
            .map(|drive| drive.ram(super::ws::zp::ERROR))
    };
    let end = machine.cycles() + 120 * JIFFY;
    while error(&mut machine) != Some(number::OK) {
        assert!(machine.cycles() < end, "the message was read");
        run_for(&mut machine, 4);
        let (now, at) = drive_state(&mut machine).unwrap_or_default();
        if now != held {
            spans.extend(since.map(|since| (held, at - since)));
            (held, since) = (now, Some(at));
        }
    }
    let shortest = |state: bool| {
        spans
            .iter()
            .filter(|&&(held, _)| held == state)
            .map(|&(_, length)| length)
            .min()
    };
    assert!(
        spans.len() > 16 * 8,
        "the message was sent: {} spans",
        spans.len()
    );
    assert!(
        shortest(true).is_some_and(|length| length >= 90),
        "CLOCK held at least 90 cycles a bit: {:?}",
        shortest(true)
    );
}

#[test]
fn the_talker_holds_its_acknowledge_long_enough_for_a_stalled_controller() {
    let mut machine = with_drive(Surface::blank());
    press(&mut machine, &[Key::LEFT_SHIFT], Key::HOME);
    if let Some(drive) = machine.drives().unit_mut(Unit::Eight) {
        drive.monitor_io(true);
    }
    // The error channel, then a channel with nothing to send.
    type_text(
        &mut machine,
        "10 OPEN 15,8,15:GET#15,A$:OPEN 2,8,2:GET#2,A$\nRUN\n",
    );
    // Taken often, as the controller's polls of VIA2 would fill the trace.
    let mut port: Vec<(u64, u8)> = Vec::new();
    for _ in 0..4000 {
        run_for(&mut machine, 500);
        if let Some(drive) = machine.drives().unit_mut(Unit::Eight) {
            let writes = drive
                .take_io_trace()
                .filter(|access| access.write && access.addr & 0x1C0F == via1(reg::ORB));
            port.extend(writes.map(|access| (access.cycle, access.value)));
        }
    }
    // An acknowledge is DATA let go and CLOCK taken, held until CLOCK is
    // released as ready to send.
    let mut holds = Vec::new();
    for (index, writes) in port.windows(3).enumerate() {
        let [(_, held), (_, let_go), (taken, took)] = [writes[0], writes[1], writes[2]];
        if held & bus::DATA_OUT != 0
            && let_go & (bus::DATA_OUT | bus::CLOCK_OUT) == 0
            && took & bus::CLOCK_OUT != 0
        {
            let released = port[index + 3..]
                .iter()
                .find(|&&(_, value)| value & bus::CLOCK_OUT == 0);
            holds.extend(released.map(|&(at, _)| at - taken));
        }
    }
    assert!(holds.len() >= 2, "both channels talked: {holds:?}");
    assert!(
        holds.iter().all(|&hold| hold >= 80),
        "each acknowledge held 80 microseconds: {holds:?}"
    );
}

#[test]
fn an_idle_drive_holds_data_until_the_controller_takes_clock_after_atn() {
    let dos = super::build().expect("the DOS assembles");
    let mut drive = Drive::new(&dos.bytes, Unit::Eight).expect("a 16 KiB DOS");
    drive.run_until(CLOCK / 2);
    // ATN asserted, CLOCK taken a little after, as the KERNAL's controller
    // does; the drive is in its ATN service before CLOCK is taken.
    drive.set_others(Lines {
        atn: true,
        clock: false,
        data: false,
    });
    let taken = drive.cycles() + 200;
    drive.run_until(taken);
    drive.set_others(Lines {
        atn: true,
        clock: true,
        data: false,
    });
    // The controller looks for a device holding DATA after a millisecond.
    while drive.cycles() < taken + 1000 {
        assert!(
            drive.holds().data,
            "DATA held at cycle {}",
            drive.cycles() - taken
        );
        drive.run_until(drive.cycles() + 4);
    }
}

#[test]
fn a_drive_restarting_under_atn_answers_it() {
    let dos = super::build().expect("the DOS assembles");
    let mut drive = Drive::new(&dos.bytes, Unit::Eight).expect("a 16 KiB DOS");
    // ATN asserted while the drive starts, its edge cleared with the rest
    // as VIA1 is set up.
    drive.set_others(Lines {
        atn: true,
        clock: true,
        data: false,
    });
    drive.run_until(CLOCK / 2);
    drive.set_others(Lines {
        atn: true,
        clock: false,
        data: false,
    });
    let ready = drive.cycles();
    while drive.holds().data {
        assert!(drive.cycles() < ready + 3000, "DATA let go for the command");
        drive.run_until(drive.cycles() + 4);
    }
}

#[test]
fn a_drive_late_to_atn_answers_a_controller_already_ready_to_send() {
    let dos = super::build().expect("the DOS assembles");
    let mut drive = Drive::new(&dos.bytes, Unit::Eight).expect("a 16 KiB DOS");
    drive.run_until(CLOCK / 2);
    // CLOCK taken and let go before the drive looks, as a drive busy with a
    // job finds it: the controller waits for DATA to be let go.
    drive.set_others(Lines {
        atn: true,
        clock: true,
        data: false,
    });
    drive.run_until(drive.cycles() + 4);
    drive.set_others(Lines {
        atn: true,
        clock: false,
        data: false,
    });
    let ready = drive.cycles();
    while drive.holds().data {
        assert!(drive.cycles() < ready + 3000, "DATA let go for the command");
        drive.run_until(drive.cycles() + 4);
    }
}

/// Run `drive` on until `held` is what it does to DATA, within `cycles`.
fn wait_data(drive: &mut Drive, held: bool, cycles: u64) -> bool {
    let end = drive.cycles() + cycles;
    while drive.cycles() < end {
        if drive.holds().data == held {
            return true;
        }
        drive.run_until(drive.cycles() + 4);
    }
    false
}

/// `bits` of `byte` sent as a controller sends them, low bit first, CLOCK
/// held between them, ATN as `atn` says.
fn send_bits(drive: &mut Drive, byte: u8, bits: u8, atn: bool) {
    for bit in 0..bits {
        let one = byte >> bit & 1 != 0;
        drive.set_others(Lines {
            atn,
            clock: true,
            data: !one,
        });
        drive.run_until(drive.cycles() + 60);
        drive.set_others(Lines {
            atn,
            clock: false,
            data: !one,
        });
        drive.run_until(drive.cycles() + 60);
    }
}

/// A whole byte sent, the controller's ready to send taken up by the drive
/// first and its acknowledgement after.
fn send_byte(drive: &mut Drive, byte: u8, atn: bool) {
    drive.set_others(Lines {
        atn,
        clock: false,
        data: false,
    });
    assert!(wait_data(drive, false, 3000), "ready for data");
    send_bits(drive, byte, 8, atn);
    drive.set_others(Lines {
        atn,
        clock: true,
        data: false,
    });
    assert!(wait_data(drive, true, 3000), "the byte acknowledged");
}

#[test]
fn a_controller_restarted_mid_byte_finds_the_drive_answering_its_atn() {
    let dos = super::build().expect("the DOS assembles");
    let mut drive = Drive::new(&dos.bytes, Unit::Eight).expect("a 16 KiB DOS");
    drive.run_until(CLOCK / 2);
    // LISTEN 8 and the command channel, then a byte of a command begun.
    drive.set_others(Lines {
        atn: true,
        clock: true,
        data: false,
    });
    assert!(wait_data(&mut drive, true, 3000), "ATN acknowledged");
    send_byte(&mut drive, 0x28, true);
    send_byte(&mut drive, 0x6F, true);
    drive.set_others(Lines {
        atn: false,
        clock: true,
        data: false,
    });
    drive.run_until(drive.cycles() + 200);
    drive.set_others(Lines {
        atn: false,
        clock: false,
        data: false,
    });
    assert!(wait_data(&mut drive, false, 3000), "ready for the command");
    send_bits(&mut drive, b'I', 3, false);
    // The controller restarts: CLOCK held, then ATN for a new LISTEN.
    drive.set_others(Lines {
        atn: false,
        clock: true,
        data: false,
    });
    drive.run_until(drive.cycles() + 100);
    drive.set_others(Lines {
        atn: true,
        clock: true,
        data: false,
    });
    drive.run_until(drive.cycles() + 1000);
    drive.set_others(Lines {
        atn: true,
        clock: false,
        data: false,
    });
    assert!(
        wait_data(&mut drive, false, 3000),
        "DATA let go for the new command, not held waiting out the byte"
    );
}
