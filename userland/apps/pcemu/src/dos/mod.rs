//! TAIRiX DOS: a DOS 5.00 kernel booted from the disk, its code host calls
//! in the system ROM, its state in guest memory where MS-DOS documents it
//! (`plans/PCEMU.md` §2.1).
//!
//! A boot record checks the ROM for its signature and jumps to the
//! loader, which builds the kernel's data, initialises the devices through
//! their headers, reads `CONFIG.SYS` and starts the shell. Every call a
//! program makes runs as frames in the swappable data area (`engine`), and
//! every device is reached through the chain of headers in guest memory.

mod boot;
mod char;
mod country;
pub(crate) mod data;
mod device;
mod dir;
mod dir_write;
mod dirops;
mod drivers;
pub use drivers::MouseFollower;
mod engine;
mod exec;
mod fcb;
mod file;
mod formats;
mod fs;
mod int21;
mod ioctl;
mod memory;
mod message;
pub(crate) mod name;
mod program;
pub mod redirect;
pub(crate) mod rom;
pub mod setup;
mod shell;
mod stacks;
mod table;
mod utility;
pub mod volume;

pub use boot::{boot_record, directive, Directive, OEM};
pub use formats::Diskette;
pub use shell::text::VERSION;

/// Whether a key is waited for: TAIRiX DOS at the console, the CPU in the
/// kernel's idle stub where console input halts until the next interrupt,
/// or a program that reads the keyboard through the BIOS, as `EDIT` does.
#[must_use]
pub fn waits_for_key(machine: &crate::machine::Machine) -> bool {
    let regs = machine.cpu().regs();
    let idle = regs.sreg(crate::cpu::Seg::Cs) == crate::bios::layout::SEGMENT
        && (rom::entry::IDLE..rom::entry::IDLE + 3).contains(&regs.ip);
    idle || crate::bios::waits_in_keyboard(machine)
}

/// `ANSI.SYS`: the console driver a `DEVICE=` line loads, as its file holds
/// it.
#[must_use]
pub fn ansi_sys() -> alloc::vec::Vec<u8> {
    drivers::ansi_image()
}

/// `COMMAND.COM`: TAIRiX DOS's command interpreter, as its file holds it.
///
/// # Errors
///
/// [`crate::asm::AsmError`] should its stub not assemble, a defect of its
/// own.
pub fn command_com() -> Result<alloc::vec::Vec<u8>, crate::asm::AsmError> {
    shell::image()
}

/// The DOS utilities, each the name of its file on a system disk and the
/// file as it holds it.
///
/// # Errors
///
/// [`crate::asm::AsmError`] should a stub not assemble, a defect of its
/// own.
pub fn utilities(
) -> Result<alloc::vec::Vec<(&'static str, alloc::vec::Vec<u8>)>, crate::asm::AsmError> {
    utility::FILES
        .iter()
        .map(|&(number, name)| Ok((name, program::image(number)?)))
        .collect()
}

/// `MOUSE.COM`: the INT 33h mouse driver, as its file holds it.
///
/// # Errors
///
/// [`crate::asm::AsmError`] should its code not assemble, a defect of its
/// own.
pub fn mouse_com() -> Result<alloc::vec::Vec<u8>, crate::asm::AsmError> {
    drivers::mouse_image()
}

use crate::bios::guest::Guest;
use engine::Kernel;

/// The host calls the kernel's ROM entries make.
pub(crate) mod call {
    /// The boot record jumped to the loader.
    pub const BOOT: u16 = 0x0400;
    /// INT 20h.
    pub const INT20: u16 = 0x0420;
    /// INT 21h.
    pub const INT21: u16 = 0x0421;
    /// INT 25h.
    pub const INT25: u16 = 0x0425;
    /// INT 26h.
    pub const INT26: u16 = 0x0426;
    /// INT 27h.
    pub const INT27: u16 = 0x0427;
    /// INT 2Fh.
    pub const INT2F: u16 = 0x042F;
    /// INT 1Bh, as the console driver hooks it.
    pub const INT13: u16 = 0x0413;
    pub const INT1B: u16 = 0x041B;
    /// A guest call the kernel made has returned.
    pub const STEP: u16 = 0x0440;
    /// An INT 23h handler has returned.
    pub const BREAK_RETURN: u16 = 0x0441;
    /// A built-in device's strategy entry.
    pub const STRATEGY: u16 = 0x0450;
    /// `NUL`'s strategy and interrupt entry.
    pub const NUL: u16 = 0x0451;
    /// A built-in driver's BIOS call has returned.
    pub const DRIVER_STEP: u16 = 0x0452;
    /// A program's CP/M-style call through PSP offset 5.
    pub const CALL5: u16 = 0x0453;
    /// A program the boot ran has ended.
    pub const BOOT_RETURN: u16 = 0x0454;
    /// `ANSI.SYS`'s interrupt entry.
    pub const ANSI: u16 = 0x0455;
    /// `ANSI.SYS`'s INT 29h.
    pub const ANSI_FAST: u16 = 0x0456;
    /// INT 33h, to `MOUSE.COM`.
    pub const MOUSE: u16 = 0x0457;
    /// The mouse's port interrupted.
    pub const MOUSE_IRQ: u16 = 0x0458;
    /// `MOUSE.COM`'s installer: whether INT 33h may be asked.
    pub const MOUSE_LOOK: u16 = 0x0459;
    /// Its search for the mouse begun, and after each interrupt it waits for.
    pub const MOUSE_PROBE: u16 = 0x045A;
    pub const MOUSE_PROBE_STEP: u16 = 0x045B;
    /// Its way out: resident or not.
    pub const MOUSE_KEEP: u16 = 0x045C;
    /// A program's stub jumped to the ROM.
    pub const PROGRAM: u16 = 0x0470;
    /// A guest call a program made has returned.
    pub const PROGRAM_STEP: u16 = 0x0471;
    /// A program's handler was entered: this plus its number.
    pub const PROGRAM_HANDLER: u16 = 0x0474;
    /// A built-in device's interrupt entry: this plus its number among them.
    pub const DRIVER: u16 = 0x0460;
    /// A vector `STACKS=` switches was taken: this plus its place among
    /// them.
    pub const STACK_IN: u16 = 0x0480;
    /// A handler called on one of the stacks has returned.
    pub const STACK_OUT: u16 = STACK_IN + super::stacks::HOOKED_COUNT;
}

/// The host-call numbers the kernel owns.
pub(crate) const CALLS: core::ops::RangeInclusive<u16> = 0x0400..=0x04FF;

/// Serves the kernel's host call `n`.
pub(crate) fn serve(g: &mut Guest<'_>, n: u16) {
    match n {
        call::BOOT => boot::start(&mut Kernel::new(g)),
        call::INT21 => int21::enter(&mut Kernel::new(g), int21::Entry::Int21),
        call::INT20 => int21::enter(&mut Kernel::new(g), int21::Entry::Int20),
        call::INT27 => int21::enter(&mut Kernel::new(g), int21::Entry::Int27),
        call::INT25 => int21::enter(&mut Kernel::new(g), int21::Entry::Int25),
        call::INT26 => int21::enter(&mut Kernel::new(g), int21::Entry::Int26),
        call::INT2F => int21::enter(&mut Kernel::new(g), int21::Entry::Int2F),
        call::INT1B => char::break_pressed(&mut Kernel::new(g)),
        call::INT13 => drivers::int13(g),
        call::STEP => engine::resume(&mut Kernel::new(g)),
        call::BREAK_RETURN => int21::break_returned(&mut Kernel::new(g)),
        call::STRATEGY => drivers::strategy(g),
        call::NUL => drivers::nul(g),
        call::DRIVER_STEP => drivers::step(g),
        call::CALL5 => int21::call5(g),
        call::ANSI => drivers::start(g, drivers::Served::Ansi),
        call::ANSI_FAST => drivers::start(g, drivers::Served::AnsiFast),
        call::MOUSE => drivers::start(g, drivers::Served::Mouse),
        call::MOUSE_IRQ => drivers::start(g, drivers::Served::MouseIrq),
        call::MOUSE_LOOK => drivers::mouse_look(g),
        call::MOUSE_PROBE => drivers::mouse_probe(g),
        call::MOUSE_PROBE_STEP => drivers::mouse_probe_step(g),
        call::MOUSE_KEEP => drivers::mouse_keep(g),
        call::BOOT_RETURN => boot::returned(&mut Kernel::new(g)),
        call::PROGRAM => program::start(g),
        call::PROGRAM_STEP => program::resume(g),
        n if (call::PROGRAM_HANDLER..call::PROGRAM_HANDLER + program::at::HANDLER_COUNT)
            .contains(&n) =>
        {
            program::handler(g, n - call::PROGRAM_HANDLER);
        }
        n if (call::DRIVER..call::PROGRAM).contains(&n) => {
            if let Some(&device) = data::Builtin::ALL.get(usize::from(n - call::DRIVER)) {
                drivers::start(g, drivers::Served::Builtin(device));
            }
        }
        n if (call::STACK_IN..call::STACK_OUT).contains(&n) => stacks::enter(g, n - call::STACK_IN),
        call::STACK_OUT => stacks::leave(g),
        // Only the kernel's own ROM entries make host calls, and these.
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod harness;
#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "writeback_tests.rs"]
mod writeback_tests;
