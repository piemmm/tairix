//! The clean-room KERNAL: the Commodore 64's operating system at its
//! documented interface (the jump table at `$FF81`, the RAM vectors at
//! `$0314`, the documented workspace) and at the addresses inside the
//! original that software is found calling (`plans/C64.md` §0.4, §5).

mod editor;
pub(crate) mod files;
mod irq;
mod keyboard;
mod reset;
mod rs232;
mod serial;
mod tape;
pub(crate) mod ws;

#[cfg(test)]
mod tests;

use crate::cpu::NMI_VECTOR;
use crate::rom::asm::{abs, Asm};
use ws::vector;

/// Addresses inside the original that software is found calling, each
/// kept doing what the original does there.
pub(crate) mod fixed {
    /// The reset, which the reset vector holds: `SYS 64738`.
    pub const RESET: u16 = 0xFCE2;
    /// The NMI's entry, which the NMI vector holds.
    pub const NMI_ENTRY: u16 = 0xFE43;
    /// CINV's default: the jiffy interrupt's handler.
    pub const IRQ: u16 = 0xEA31;
    /// CINV's handler's end: CIA1's interrupts acknowledged, then
    /// [`IRQ_EXIT`].
    pub const IRQ_ACK: u16 = 0xEA7E;
    /// The registers restored and the interrupt returned from.
    pub const IRQ_EXIT: u16 = 0xEA81;
    /// CBINV's and USRCMD's default: BRK's warm start.
    pub const BRK: u16 = 0xFE66;
    /// NMINV's default.
    pub const NMI: u16 = 0xFE47;
    /// The NMI handler's end.
    pub const NMI_EXIT: u16 = 0xFEBC;
    /// IOPEN's default.
    pub const OPEN: u16 = 0xF34A;
    /// ICLOSE's default.
    pub const CLOSE: u16 = 0xF291;
    /// ICHKIN's default.
    pub const CHKIN: u16 = 0xF20E;
    /// ICKOUT's default.
    pub const CHKOUT: u16 = 0xF250;
    /// ICLRCH's default, which CLALL's runs on into.
    pub const CLRCHN: u16 = 0xF333;
    /// IBASIN's default.
    pub const CHRIN: u16 = 0xF157;
    /// IBSOUT's default.
    pub const CHROUT: u16 = 0xF1CA;
    /// ISTOP's default.
    pub const STOP: u16 = 0xF6ED;
    /// IGETIN's default.
    pub const GETIN: u16 = 0xF13E;
    /// ICLALL's default.
    pub const CLALL: u16 = 0xF32F;
    /// ILOAD's default.
    pub const LOAD: u16 = 0xF4A5;
    /// ISAVE's default.
    pub const SAVE: u16 = 0xF5ED;
    /// The screen editor's clear, ending at [`HOME`]'s.
    pub const CLEAR_SCREEN: u16 = 0xE544;
    /// The screen editor's home.
    pub const HOME: u16 = 0xE566;
    /// KEYLOG's default: the shift keys' table picked, the key decoded.
    pub const KEYLOG: u16 = 0xEB48;
}

/// What is at a fixed address: a jump to a routine, or code a part of the
/// KERNAL pins there itself.
#[derive(Clone, Copy)]
enum Lead {
    Jump(&'static str),
    Placed,
}

/// Each RAM vector's default after RESTOR, in vector order.
const VECTOR_DEFAULTS: [(u16, Lead); 16] = [
    (fixed::IRQ, Lead::Placed),
    (fixed::BRK, Lead::Jump("warm_start")),
    (fixed::NMI, Lead::Placed),
    (fixed::OPEN, Lead::Jump("open")),
    (fixed::CLOSE, Lead::Jump("close")),
    (fixed::CHKIN, Lead::Jump("chkin")),
    (fixed::CHKOUT, Lead::Jump("chkout")),
    (fixed::CLRCHN, Lead::Placed),
    (fixed::CHRIN, Lead::Jump("chrin")),
    (fixed::CHROUT, Lead::Jump("chrout")),
    (fixed::STOP, Lead::Jump("stop")),
    (fixed::GETIN, Lead::Jump("getin")),
    (fixed::CLALL, Lead::Placed),
    (fixed::BRK, Lead::Jump("warm_start")),
    (fixed::LOAD, Lead::Jump("load_vectored")),
    (fixed::SAVE, Lead::Jump("save_vectored")),
];

/// The other fixed addresses that are a jump to their routine.
const ENTRY_JUMPS: [(u16, &str); 4] = [
    (fixed::RESET, "reset"),
    (fixed::CLEAR_SCREEN, "clear_screen"),
    (fixed::HOME, "home"),
    (fixed::KEYLOG, "keylog"),
];

/// A jump-table entry: a jump to a routine, or through a RAM vector.
#[derive(Clone, Copy)]
enum Entry {
    Direct(&'static str),
    Vectored(u16),
}

/// The jump table's first entry.
pub const JUMP_TABLE: u16 = 0xFF81;

/// The jump table's entries BASIC and the tests call.
pub(crate) mod entry {
    use super::{ENTRIES, JUMP_TABLE};

    /// The entry at `index`, which must be the one named `name`.
    const fn at(index: usize, name: &str) -> u16 {
        let (named, _) = ENTRIES[index];
        let (named, name) = (named.as_bytes(), name.as_bytes());
        assert!(
            named.len() == name.len(),
            "an entry named for its place in the jump table"
        );
        let mut at = 0;
        while at < name.len() {
            assert!(
                named[at] == name[at],
                "an entry named for its place in the jump table"
            );
            at += 1;
        }
        JUMP_TABLE + 3 * crate::rom::asm::byte(index) as u16
    }

    pub const SETMSG: u16 = at(5, "SETMSG");
    pub const MEMTOP: u16 = at(8, "MEMTOP");
    pub const MEMBOT: u16 = at(9, "MEMBOT");
    pub const READST: u16 = at(18, "READST");
    pub const SETLFS: u16 = at(19, "SETLFS");
    pub const SETNAM: u16 = at(20, "SETNAM");
    pub const OPEN: u16 = at(21, "OPEN");
    pub const CLOSE: u16 = at(22, "CLOSE");
    pub const CHKIN: u16 = at(23, "CHKIN");
    pub const CHKOUT: u16 = at(24, "CHKOUT");
    pub const CLRCHN: u16 = at(25, "CLRCHN");
    pub const CHRIN: u16 = at(26, "CHRIN");
    pub const CHROUT: u16 = at(27, "CHROUT");
    pub const LOAD: u16 = at(28, "LOAD");
    pub const SAVE: u16 = at(29, "SAVE");
    pub const SETTIM: u16 = at(30, "SETTIM");
    pub const RDTIM: u16 = at(31, "RDTIM");
    pub const STOP: u16 = at(32, "STOP");
    pub const GETIN: u16 = at(33, "GETIN");
    pub const CLALL: u16 = at(34, "CLALL");
    pub const PLOT: u16 = at(37, "PLOT");
}

/// The jump table from [`JUMP_TABLE`], three bytes an entry, each under
/// the Programmer's Reference Guide's name for it.
const ENTRIES: [(&str, Entry); 39] = [
    ("CINT", Entry::Direct("cint")),
    ("IOINIT", Entry::Direct("ioinit")),
    ("RAMTAS", Entry::Direct("ramtas")),
    ("RESTOR", Entry::Direct("restor")),
    ("VECTOR", Entry::Direct("vector")),
    ("SETMSG", Entry::Direct("setmsg")),
    ("SECOND", Entry::Direct("second")),
    ("TKSA", Entry::Direct("tksa")),
    ("MEMTOP", Entry::Direct("memtop")),
    ("MEMBOT", Entry::Direct("membot")),
    ("SCNKEY", Entry::Direct("scnkey")),
    ("SETTMO", Entry::Direct("settmo")),
    ("ACPTR", Entry::Direct("acptr")),
    ("CIOUT", Entry::Direct("ciout")),
    ("UNTLK", Entry::Direct("untlk")),
    ("UNLSN", Entry::Direct("unlsn")),
    ("LISTEN", Entry::Direct("listen")),
    ("TALK", Entry::Direct("talk")),
    ("READST", Entry::Direct("readst")),
    ("SETLFS", Entry::Direct("setlfs")),
    ("SETNAM", Entry::Direct("setnam")),
    ("OPEN", Entry::Vectored(vector::IOPEN)),
    ("CLOSE", Entry::Vectored(vector::ICLOSE)),
    ("CHKIN", Entry::Vectored(vector::ICHKIN)),
    ("CHKOUT", Entry::Vectored(vector::ICKOUT)),
    ("CLRCHN", Entry::Vectored(vector::ICLRCH)),
    ("CHRIN", Entry::Vectored(vector::IBASIN)),
    ("CHROUT", Entry::Vectored(vector::IBSOUT)),
    ("LOAD", Entry::Direct("load")),
    ("SAVE", Entry::Direct("save")),
    ("SETTIM", Entry::Direct("settim")),
    ("RDTIM", Entry::Direct("rdtim")),
    ("STOP", Entry::Vectored(vector::ISTOP)),
    ("GETIN", Entry::Vectored(vector::IGETIN)),
    ("CLALL", Entry::Vectored(vector::ICLALL)),
    ("UDTIM", Entry::Direct("udtim")),
    ("SCREEN", Entry::Direct("screen")),
    ("PLOT", Entry::Direct("plot")),
    ("IOBASE", Entry::Direct("iobase")),
];

/// The KERNAL's fixed entry points and tables, pinned where software finds
/// them before any code flows, so code flowing from anywhere passes them.
pub fn pin(a: &mut Asm) {
    jump_table(a);
    pin_jumps(a);
    reset::pin(a);
    irq::pin(a);
    files::pin(a);
}

/// The KERNAL's routines from the flow's present address.
pub fn emit(a: &mut Asm) {
    reset::emit(a);
    irq::emit(a);
    keyboard::emit(a);
    editor::emit(a);
    files::emit(a);
    serial::emit(a);
    tape::emit(a);
    rs232::emit(a);
}

fn jump_table(a: &mut Asm) {
    a.pin(JUMP_TABLE, |a| {
        for (name, entry) in ENTRIES {
            a.here(name);
            match entry {
                Entry::Direct(routine) => a.jmp(routine),
                Entry::Vectored(vector) => a.jmp(abs(vector).ind()),
            }
        }
    });
    a.pin(NMI_VECTOR, |a| {
        a.word(fixed::NMI_ENTRY);
        a.word(fixed::RESET);
        a.word("irq_entry");
    });
}

/// A jump at each fixed address that leads to its routine, once an address.
fn pin_jumps(a: &mut Asm) {
    let jumps = VECTOR_DEFAULTS
        .iter()
        .enumerate()
        .filter_map(|(index, &(address, lead))| match lead {
            Lead::Jump(routine)
                if !VECTOR_DEFAULTS[..index]
                    .iter()
                    .any(|&(earlier, _)| earlier == address) =>
            {
                Some((address, routine))
            }
            _ => None,
        })
        .chain(ENTRY_JUMPS);
    for (address, routine) in jumps {
        a.pin(address, |a| a.jmp(routine));
    }
}

/// The vector defaults as RESTOR copies them.
fn vector_table(a: &mut Asm) {
    a.here("vector_defaults");
    for (address, _) in VECTOR_DEFAULTS {
        a.word(address);
    }
}

/// Restore Y, X and A from the stack and return from the interrupt: the
/// end of the IRQ and NMI handlers, at each of their addresses.
fn restore_and_return(a: &mut Asm) {
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.rti();
}
