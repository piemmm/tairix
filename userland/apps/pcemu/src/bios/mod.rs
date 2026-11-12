//! The firmware: the system ROM at `F0000h`, and the option ROMs of the EGA
//! or VGA and of the fixed-disk adapter, built for the machine they go in.
//!
//! Every service is a host call in a short stub at the address PC software
//! calls it at. A service that must wait — for a key, a disk's interrupt, a
//! timer tick — hands back to its stub, which halts with interrupts enabled
//! until the next one arrives and asks again; the machine runs on while it
//! waits, and an idle guest costs no host time. The services keep their state
//! in guest memory: the BIOS data area, and bytes the stub reserves on the
//! caller's stack. Rust holds none between calls.

pub(crate) mod bda;
mod clock;
mod ems;
mod fixed;
mod floppy;
pub(crate) mod guest;
mod keyboard;
pub use keyboard::{
    caps_lock, keys_waiting, keystrokes, on_keypad, shift, Keystrokes, Ring, CTRL_C_KEY,
};
pub(crate) mod layout;
pub mod modes;
mod ports;
mod post;
mod system;
mod vga;
pub(crate) mod video;

use crate::asm::AsmError;
use crate::board::{Board, BoardConfig};
use crate::machine::{Exit, Machine};
use crate::storage::hdc::DiskType;
use crate::time::Nanos;
use alloc::vec::Vec;
use guest::Guest;

pub use layout::SYSTEM_BASE;
pub(crate) use system::TICKS_PER_DAY;

/// INT 17h's status in AH: the port's status lines, Ack and Error inverted
/// so that set means asserted, and a timeout bit of its own.
pub(crate) mod printer_status {
    use crate::ports::parallel::status;
    pub const TIMED_OUT: u8 = 0x01;
    pub const ERROR: u8 = status::NO_ERROR;
    pub const SELECTED: u8 = status::SELECTED;
    pub const PAPER_OUT: u8 = status::PAPER_OUT;
    pub const READY: u8 = status::NOT_BUSY;
    /// The lines reported inverted from the port's.
    pub const INVERTED: u8 = status::ACK | status::NO_ERROR;
}

/// INT 14h's status in AH: the line status register, its top bit a timeout
/// of the service's own.
pub(crate) mod serial_status {
    pub const TIMED_OUT: u8 = 0x80;
}

/// INT 13h's statuses, as the diskette and fixed-disk services return them
/// in AH.
pub(crate) mod disk_status {
    pub const INVALID: u8 = 0x01;
    pub const NO_ADDRESS_MARK: u8 = 0x02;
    pub const WRITE_PROTECTED: u8 = 0x03;
    pub const NOT_FOUND: u8 = 0x04;
    pub const RESET_FAILED: u8 = 0x05;
    pub const CHANGED: u8 = 0x06;
    pub const DMA_OVERRUN: u8 = 0x08;
    pub const DMA_BOUNDARY: u8 = 0x09;
    pub const BAD_TRACK: u8 = 0x0B;
    pub const BAD_MEDIA: u8 = 0x0C;
    /// A sector's CRC, or a fixed disk's ECC, did not check.
    pub const CRC: u8 = 0x10;
    pub const ECC_CORRECTED: u8 = 0x11;
    pub const CONTROLLER: u8 = 0x20;
    pub const SEEK: u8 = 0x40;
    /// The drive never answered: a diskette drive with none in it.
    pub const TIMEOUT: u8 = 0x80;
    pub const NOT_READY: u8 = 0xAA;
    pub const WRITE_FAULT: u8 = 0xCC;
}

/// The host calls the firmware's stubs make.
mod call {
    pub const POST: u16 = 0x0001;
    pub const OPTION_ROM: u16 = 0x0002;
    pub const POST_DONE: u16 = 0x0003;
    pub const NMI: u16 = 0x0004;
    pub const TICK: u16 = 0x0008;
    pub const KEYBOARD_IRQ: u16 = 0x0009;
    pub const KEYBOARD_ACK: u16 = 0x000A;
    pub const PAUSE: u16 = 0x000B;
    pub const FLOPPY_IRQ: u16 = 0x000E;
    pub const VIDEO: u16 = 0x0010;
    pub const VIDEO_WAIT: u16 = 0x0110;
    pub const EQUIPMENT: u16 = 0x0011;
    pub const MEMORY: u16 = 0x0012;
    pub const DISKETTE_START: u16 = 0x0013;
    pub const DISKETTE_STEP: u16 = 0x0113;
    pub const SERIAL: u16 = 0x0014;
    pub const SERIAL_STEP: u16 = 0x0114;
    pub const SYSTEM: u16 = 0x0015;
    pub const KEYBOARD: u16 = 0x0016;
    pub const PRINTER: u16 = 0x0017;
    pub const PRINTER_STEP: u16 = 0x0117;
    pub const NO_BOOT: u16 = 0x0018;
    pub const BOOT: u16 = 0x0019;
    pub const BOOT_RESULT: u16 = 0x0119;
    pub const CLOCK: u16 = 0x001A;
    pub const PRINT_SCREEN: u16 = 0x0005;
    pub const PRINT_SCREEN_NEXT: u16 = 0x0105;
    pub const FIXED_INIT: u16 = 0x0040;
    pub const FIXED_START: u16 = 0x0041;
    pub const FIXED_STEP: u16 = 0x0141;
    pub const FIXED_IRQ: u16 = 0x000D;
    pub const VGA_INIT: u16 = 0x0050;
    pub const VGA_VIDEO: u16 = 0x0051;
    pub const VGA_PRINT_SCREEN: u16 = 0x0052;
    pub const EMS: u16 = 0x0067;
    pub const EMS_STEP: u16 = 0x0167;
    pub const EMS_RETURN: u16 = 0x0267;
}

/// Why firmware could not be built: a defect in its own layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// A stub could not be encoded.
    Asm(AsmError),
    /// Two pieces of a ROM were laid over each other at this offset.
    Overlap(u16),
}

impl From<AsmError> for BuildError {
    fn from(error: AsmError) -> Self {
        Self::Asm(error)
    }
}

/// The ROMs a machine runs.
#[derive(Clone, Debug)]
pub struct Firmware {
    system: Vec<u8>,
    video: Option<Vec<u8>>,
    fixed: Option<Vec<u8>>,
}

impl Firmware {
    /// The firmware for a machine built from `config`, its fixed-disk
    /// adapter's drives of `fixed`.
    pub fn new(config: &BoardConfig, fixed: [Option<DiskType>; 2]) -> Result<Self, BuildError> {
        let fixed = config
            .fixed
            .map(|adapter| fixed::rom(adapter.recording, fixed))
            .transpose()?;
        let mut video = None;
        for adapter in config.video.iter().flatten() {
            if let Some(rom) = vga::rom(*adapter)? {
                video = Some(rom);
            }
        }
        Ok(Self {
            system: layout::system_rom(config)?,
            video,
            fixed,
        })
    }

    /// The system ROM's image, mapped at [`SYSTEM_BASE`].
    #[must_use]
    pub fn system(&self) -> &[u8] {
        &self.system
    }

    /// Maps the ROMs into `board` as the emulator's own, where their host
    /// calls are honoured.
    pub fn install(&self, board: &mut Board) -> Result<(), crate::board::memory::MapError> {
        board.map_rom(SYSTEM_BASE, &self.system, true)?;
        if let Some(rom) = &self.video {
            board.map_rom(vga::BASE, rom, true)?;
        }
        if let Some(rom) = &self.fixed {
            board.map_rom(fixed::BASE, rom, true)?;
        }
        Ok(())
    }
}

/// Why a machine could not be put together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerError {
    /// The board refused its configuration.
    Board(crate::board::ConfigError),
    /// The firmware could not be built.
    Firmware(BuildError),
    /// The firmware could not be mapped.
    Map(crate::board::memory::MapError),
}

/// The machine `config` describes with its firmware installed, the
/// fixed-disk adapter's ROM built for drives of `fixed`, as power-on leaves
/// it: the first instruction it runs is POST's.
///
/// # Errors
///
/// [`PowerError`]: the configuration's own refusal, or a firmware defect.
pub fn machine(config: &BoardConfig, fixed: [Option<DiskType>; 2]) -> Result<Machine, PowerError> {
    let mut machine = Machine::new(config).map_err(PowerError::Board)?;
    Firmware::new(config, fixed)
        .map_err(PowerError::Firmware)?
        .install(machine.board_mut())
        .map_err(PowerError::Map)?;
    Ok(machine)
}

/// Moves a machine just resumed on by `away`, the time it was suspended:
/// its time of day, as the firmware keeps it.
pub fn resumed(machine: &mut Machine, away: Nanos) {
    system::resumed(&mut Guest::new(machine), away);
}

/// Serves host call `n` the firmware's ROM made, in a run that ends at
/// `until`.
fn serve(machine: &mut Machine, n: u16, until: Nanos) {
    let mut guest = Guest::within(machine, until);
    match n {
        call::POST => post::begin(&mut guest),
        call::OPTION_ROM => post::next_option_rom(&mut guest),
        call::POST_DONE => post::finish(&mut guest),
        call::NMI => system::nmi(&mut guest),
        call::TICK => system::tick(&mut guest),
        call::KEYBOARD_IRQ => keyboard::interrupt(&mut guest),
        call::KEYBOARD_ACK => keyboard::acknowledge(&mut guest),
        call::PAUSE => keyboard::paused(&mut guest),
        call::FLOPPY_IRQ => floppy::interrupt(&mut guest),
        call::VIDEO => video::service(&mut guest),
        call::VIDEO_WAIT => video::wait(&mut guest),
        call::EQUIPMENT => system::equipment(&mut guest),
        call::MEMORY => system::memory(&mut guest),
        call::DISKETTE_START => floppy::start(&mut guest),
        call::DISKETTE_STEP => floppy::step(&mut guest),
        call::SERIAL => ports::serial(&mut guest),
        call::SERIAL_STEP => ports::serial_step(&mut guest),
        call::SYSTEM => system::system(&mut guest),
        call::KEYBOARD => keyboard::service(&mut guest),
        call::PRINTER => ports::printer(&mut guest),
        call::PRINTER_STEP => ports::printer_step(&mut guest),
        call::NO_BOOT => system::no_boot(&mut guest),
        call::BOOT => system::boot(&mut guest),
        call::BOOT_RESULT => system::boot_result(&mut guest),
        call::CLOCK => system::clock(&mut guest),
        call::PRINT_SCREEN => system::print_screen(&mut guest, system::PRINT_ROWS),
        call::PRINT_SCREEN_NEXT => system::print_screen_next(&mut guest),
        call::FIXED_INIT => fixed::init(&mut guest),
        call::FIXED_START => fixed::start(&mut guest),
        call::FIXED_STEP => fixed::step(&mut guest),
        call::FIXED_IRQ => fixed::interrupt(&mut guest),
        call::VGA_INIT => vga::init(&mut guest),
        call::VGA_VIDEO => vga::service(&mut guest),
        call::VGA_PRINT_SCREEN => vga::print_screen(&mut guest),
        call::EMS => ems::service(&mut guest),
        call::EMS_STEP => ems::step(&mut guest),
        call::EMS_RETURN => ems::returned(&mut guest),
        n if crate::dos::CALLS.contains(&n) => crate::dos::serve(&mut guest, n),
        // Only the firmware's own ROM makes host calls, and it makes these.
        _ => {}
    }
}

/// Whether a program waits for a key through INT 16h: the CPU halted at
/// its stub's HLT, which only a read that found no key comes to.
#[must_use]
pub fn waits_in_keyboard(machine: &Machine) -> bool {
    use crate::cpu::Bus as _;
    const HLT: u8 = 0xF4;
    let regs = machine.cpu().regs();
    let at = regs.ip.wrapping_sub(1);
    let cs = regs.sreg(crate::cpu::Seg::Cs);
    cs == layout::SEGMENT
        && (layout::entry::KEYBOARD..layout::entry::KEYBOARD_IRQ).contains(&at)
        && machine.board().peek(crate::cpu::linear(cs, at)) == HLT
}

/// Runs `machine` until emulated time reaches `deadline` or the debugger's
/// probe stops it, serving the firmware's host calls on the way: which it
/// was.
pub fn run_until(machine: &mut Machine, deadline: Nanos) -> Exit {
    loop {
        match machine.run_until(deadline) {
            Exit::HostCall(n) => serve(machine, n, deadline),
            exit => return exit,
        }
    }
}

#[cfg(test)]
#[path = "bios_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "hostile_tests.rs"]
mod hostile_tests;
