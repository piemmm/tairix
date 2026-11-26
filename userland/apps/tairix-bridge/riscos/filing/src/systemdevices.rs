//! SystemDevices 1.34 (`plans/RISCOS-EMULATOR.md` §10): the filing systems
//! that are devices rather than stores — `null:`, `printer:`, `vdu:`,
//! `rawvdu:`, `kbd:` and `rawkbd:` — as the PRM's System devices chapter and
//! RISC OS 5's module, as recorded, describe them.
//!
//! - `entries`: the filing systems' entries, which FileSwitch calls, and each
//!   device's reads and writes.
//! - `printer`: the file a `printer:` stream writes to.
//!
//! The six filing systems share one set of entries, each told apart by the
//! R12 FileSwitch hands them: the address of its own information block.

mod entries;
mod printer;

use alloc::vec::Vec;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::fs::{block, info, Declaration};
use tairix_riscos_kernel::listing::{Listing, Progress};
use tairix_riscos_kernel::module::{Builtin, Resource, Services};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, quietly, Fail, Os, Regs, Unwind, World};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::os::{fs_control, FS_CONTROL};
use tairix_riscos_kernel::swi::X;

/// Its messages, in the emulator's own words.
const MESSAGES: &[u8] = include_bytes!("../resources/SystemDevices/Messages");
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.SystemDevices.Messages\0");

/// One of the filing systems, in the order they are declared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Device {
    Null,
    Printer,
    Vdu,
    RawVdu,
    Kbd,
    RawKbd,
}

const DEVICES: [Device; 6] = [
    Device::Null,
    Device::Printer,
    Device::Vdu,
    Device::RawVdu,
    Device::Kbd,
    Device::RawKbd,
];

impl Device {
    const fn number(self) -> u32 {
        match self {
            Self::Null => 0x0D,
            Self::Printer => 0x0E,
            Self::Vdu => 0x11,
            Self::RawVdu => 0x12,
            Self::Kbd => 0x13,
            Self::RawKbd => 0x14,
        }
    }

    const fn name(self) -> &'static [u8] {
        match self {
            Self::Null => b"null",
            Self::Printer => b"printer",
            Self::Vdu => b"vdu",
            Self::RawVdu => b"rawvdu",
            Self::Kbd => b"kbd",
            Self::RawKbd => b"rawkbd",
        }
    }

    /// Its information word: interactive, with names that may be empty and
    /// opened whether or not anything is there; `printer:` with special
    /// fields too, as RISC OS 5's are declared.
    const fn info(self) -> u32 {
        let special = if matches!(self, Self::Printer) {
            info::SPECIAL_FIELDS
        } else {
            0
        };
        self.number() | special | info::INTERACTIVE | info::NULL_NAMES | info::ALWAYS_OPEN
    }

    const fn index(self) -> usize {
        match self {
            Self::Null => 0,
            Self::Printer => 1,
            Self::Vdu => 2,
            Self::RawVdu => 3,
            Self::Kbd => 4,
            Self::RawKbd => 5,
        }
    }

    /// Where in the workspace its information block is built.
    const fn block_at(self) -> u32 {
        BLOCKS_AT + BLOCK_BYTES * len32(self.index())
    }

    /// Where its name is kept: after the names of those before it.
    const fn name_at(self) -> u32 {
        let mut at = NAMES_AT;
        let mut before = 0;
        while before < self.index() {
            at += len32(DEVICES[before].name().len() + 1);
            before += 1;
        }
        at
    }
}

/// Where its workspace keeps, after its messages' file, each filing
/// system's information block, their names, an empty start-up text, and the
/// line `kbd:` reads.
const BLOCK_BYTES: u32 = 4 * len32(block::WORDS);
const BLOCKS_AT: u32 = MESSAGES_FILE.room();
const NAMES_AT: u32 = BLOCKS_AT + BLOCK_BYTES * len32(DEVICES.len());
const EMPTY_AT: u32 =
    (Device::RawKbd.name_at() + len32(Device::RawKbd.name().len() + 1)).next_multiple_of(4);
const LINE_AT: u32 = EMPTY_AT + 4;
/// The line `kbd:` reads: the characters `OS_ReadLine` takes, as RISC OS 5
/// takes them, and the Return it ends with.
const LINE_MOST: u32 = 255;
const WORKSPACE: u32 = LINE_AT + LINE_MOST + 1;

/// "… cannot do that", numbered as RISC OS 5's is for every one of them,
/// and "There is no printer type …".
const BAD_OPERATION: Error = Error {
    number: 0x112,
    token: "BadOperation",
};
const UNKNOWN_PRINTER: Error = Error {
    number: 0x1_0E02,
    token: "UnknownPrinter",
};

/// SystemDevices.
pub static SYSTEM_DEVICES: Builtin = Builtin {
    title: "SystemDevices",
    help_name: Some("System Devices"),
    help_suffix: None,
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[service::FS_REDECLARE],
        handler: service_call,
    }),
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[
        entries::open,
        entries::get_bytes,
        entries::put_bytes,
        entries::args,
        entries::close,
        entries::file,
        entries::func,
        entries::gbpb,
    ],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.SystemDevices.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// The stream `kbd:` has open, which one at a time may have: its handle, and
/// how much of the line read for it is held and has been taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Kbd {
    handle: u32,
    line: Option<Line>,
}

/// A line `kbd:` holds: its length, its Return counted, and the bytes of it
/// taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Line {
    length: u32,
    taken: u32,
}

/// A `printer:` stream: its handle, and the one it writes through, none for
/// a printer type no file is set for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Printing {
    handle: u32,
    target: Option<u32>,
}

/// What SystemDevices holds while it is active.
pub struct State {
    own: Own,
    workspace: u32,
    /// How `vdu:` shows what it is given: as `DumpFormat` was configured when
    /// SystemDevices started, as RISC OS 5 reads it then and not after.
    listing: Listing,
    /// Where `vdu:`'s listing has got, shared by every stream on it.
    progress: Progress,
    kbd: Option<Kbd>,
    /// The `printer:` streams, in their handles' order.
    printers: Vec<Printing>,
    /// How many `printer:` opens are under way, one within another's.
    opening_printers: u8,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// `work` on SystemDevices' state; "not present" while it has none.
fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// Its initialisation: its workspace, `DumpFormat` read for `vdu:`, and the
/// filing systems declared.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let format = os.dump_format()?;
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    let messages = MESSAGES_FILE.place(os, workspace)?;
    for device in DEVICES {
        let name = device.name();
        os.write_terminated(workspace + device.name_at(), name, 0, name.len() + 1)?;
    }
    os.write_word(workspace + EMPTY_AT, 0)?;
    os.hold_extension(State {
        own: messages,
        workspace,
        listing: Listing::filtered(format),
        progress: Progress::default(),
        kbd: None,
        printers: Vec::new(),
        opening_printers: 0,
    })?;
    declare_all(os, workspace)
}

fn declare_all(os: &mut Os<'_>, workspace: u32) -> Result<(), Fail> {
    for device in DEVICES {
        let block = workspace + device.block_at();
        os.declare_filing_system(Declaration {
            module: &SYSTEM_DEVICES,
            first_entry: 0,
            gbpb: true,
            block,
            name: workspace + device.name_at(),
            startup: workspace + EMPTY_AT,
            info: device.info(),
            extra: 0,
            workspace: block,
        })?;
    }
    Ok(())
}

/// Its finalisation: the filing systems removed, which closes their
/// streams; the kernel frees its workspace from its private word.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = with(os, |state| state.workspace)?;
    for device in DEVICES {
        let mut remove = Regs::default();
        remove.r[..2].copy_from_slice(&[fs_control::REMOVE, workspace + device.name_at()]);
        os.perform_quietly(X | FS_CONTROL, &mut remove)?;
    }
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

/// Its service handler: `Service_FSRedeclare`, the filing systems declared
/// again.
fn service_call(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if regs.r[1] != service::FS_REDECLARE {
        return Ok(());
    }
    quietly(with(os, |state| state.workspace).and_then(|at| declare_all(os, at)))
}

/// The device whose entry FileSwitch called, from the R12 it handed over.
fn called(os: &mut Os<'_>, regs: &Regs) -> Result<Device, Fail> {
    let workspace = with(os, |state| state.workspace)?;
    let offset = regs.r[12].wrapping_sub(workspace + BLOCKS_AT);
    let device = offset
        .is_multiple_of(BLOCK_BYTES)
        .then(|| DEVICES.get((offset / BLOCK_BYTES) as usize).copied())
        .flatten();
    device.ok_or_else(|| os.error(tairix_riscos_kernel::filing::NOT_PRESENT))
}

/// "`device`: cannot do that".
fn refused(os: &mut Os<'_>, device: Device) -> Fail {
    let mut name = [0; 8];
    let length = device.name().len();
    name[..length].copy_from_slice(device.name());
    name[length] = b':';
    os.own_error(own, BAD_OPERATION, &[&name[..=length]])
}
