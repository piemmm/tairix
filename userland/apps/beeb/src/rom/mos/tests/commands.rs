//! The command line's dealings with the filing system and the ROMs.

use super::{claim, oscli, program, run_rom, var, DONE, DONE_MARK};
use crate::rom::asm::{abs, hi, imm, lo, zp};
use crate::rom::os::entry::OSASCI;
use crate::rom::os::vector::{BRK, FIND, FSC};
use crate::rom::os::{fsc, service, ERROR_POINTER};
use crate::scenario::mode7_text;

/// The regression: OSCLI ran a command without telling the filing system,
/// where MOS 1.20 calls FSCV with A=8 before each, as a DFS counts its
/// *ENABLE's life by.
#[test]
fn the_filing_system_hears_of_each_command_before_it_runs() {
    const OLD: u16 = 0x0076;
    const HEARD: u16 = 0x0074;
    // At each call, the *FX4 setting the commands are about to change.
    const SETTINGS: u16 = 0x0900;
    let rom = program(None, |a| {
        a.lda(imm(0));
        a.sta(zp(HEARD));
        claim(a, FSC, "fsc", OLD);
        oscli(a, "FX4,1");
        oscli(a, "FX4,2");
        a.jmp("ran");
        a.here("fsc");
        a.cmp(imm(fsc::COMMAND_LINE));
        let other = a.local();
        a.bne(other);
        a.pha();
        a.txa();
        a.pha();
        a.ldx(zp(HEARD));
        a.lda(abs(var::CURSOR_EDITING));
        a.sta(abs(SETTINGS).x());
        a.inc(zp(HEARD));
        a.pla();
        a.tax();
        a.pla();
        a.bind(other);
        a.jmp(abs(OLD).ind());
        a.here("ran");
    });
    let machine = run_rom(&rom);
    assert_eq!(machine.peek(HEARD), 2);
    assert_eq!((machine.peek(SETTINGS), machine.peek(SETTINGS + 1)), (0, 1));
    assert_eq!(machine.peek(var::CURSOR_EDITING), 2);
}

/// The regression: *HELP showed the MOS's version before offering the ROMs
/// the call, where MOS 1.20 shows each ROM's help first.
#[test]
fn help_shows_the_roms_help_before_the_mos_version() {
    let rom = program(Some("service"), |a| {
        oscli(a, "HELP");
        a.lda(imm(DONE_MARK));
        a.sta(abs(DONE));
        a.here("idle");
        a.jmp("idle");
        a.here("service");
        a.cmp(imm(service::HELP));
        a.bne("unclaimed");
        a.pha();
        a.txa();
        a.pha();
        a.ldx(imm(0));
        a.here("help_next");
        a.lda(abs("help_text").x());
        a.beq("help_done");
        a.jsr(OSASCI);
        a.inx();
        a.bne("help_next");
        a.here("help_done");
        a.pla();
        a.tax();
        a.pla();
        a.here("unclaimed");
        a.rts();
        a.here("help_text");
        a.text("ROM HELP\r");
        a.byte(0);
    });
    let screen = mode7_text(&run_rom(&rom));
    let at = |line: &str| screen.iter().position(|row| row == line);
    let (rom, os) = (at("ROM HELP"), at("OS 1.20"));
    assert!(rom.is_some() && os.is_some() && rom < os, "{screen:?}");
}

/// The regression: *EXEC of a file the filing system could not open raised
/// nothing, where MOS 1.20 reports File not found.
#[test]
fn exec_of_a_file_that_cannot_be_opened_is_file_not_found() {
    const FILE_NOT_FOUND: u8 = 214;
    const ERROR: u16 = 0x0074;
    let rom = program(None, |a| {
        a.lda(imm(0));
        a.sta(zp(ERROR));
        for (vector, handler) in [(FIND, "no_file"), (BRK, "caught")] {
            a.lda(imm(lo(handler)));
            a.sta(abs(vector));
            a.lda(imm(hi(handler)));
            a.sta(abs(vector + 1));
        }
        oscli(a, "EXEC NOPE");
        a.jmp("tried");
        // FINDV opens nothing.
        a.here("no_file");
        a.lda(imm(0));
        a.rts();
        a.here("caught");
        a.ldx(imm(0xFF));
        a.txs();
        a.ldy(imm(0));
        a.lda(zp(ERROR_POINTER).ind_y());
        a.sta(zp(ERROR));
        a.here("tried");
    });
    assert_eq!(run_rom(&rom).peek(ERROR), FILE_NOT_FOUND);
}
