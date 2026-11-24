//! FileCore (`plans/RISCOS-EMULATOR.md` §6): each client's discs — a client
//! being a module that moves sectors, RamFS among them — as a filing system
//! of its own, over `lib/filecore`, the disc format the ADFS driver shares.
//!
//! A client registers with `FileCore_Create`, which makes FileCore an
//! incarnation of its own named after the client's filing system, as RISC OS
//! 5 does: the incarnation's private word is the instance's, and its
//! workspace what FileSwitch hands the filing system's entries.
//!
//! - `disc`: a drive's disc as `lib/filecore` reaches it, through the
//!   client's entries.
//! - `entries`: the filing system's entries, FileSwitch's calls.
//! - `located`: what holds a byte of a disc, named.
//! - `structure`: what `FileCore_FloppyStructure` lays out.
//! - `swis`: FileCore's SWIs.
//! - `commands`: its commands.

mod commands;
mod disc;
mod entries;
mod located;
mod names;
mod structure;
mod swis;

use alloc::vec::Vec;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::fs::{block, info, Declaration};
use tairix_riscos_kernel::module::{Builtin, Resource, Services, Swis};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, Target, Unwind, World};
use tairix_riscos_kernel::service::FS_REDECLARE;
use tairix_riscos_kernel::swi::os::{fs_control, FS_CONTROL};
use tairix_riscos_kernel::swi::X;

pub(crate) use disc::{op, Mounted};
pub use swis::number as swi;

/// FileCore's SWI chunk.
const CHUNK: u32 = 0x40540;

/// Its SWIs' names, as RISC OS 5 names them.
const NAMES: [&str; 14] = [
    "DiscOp",
    "Create",
    "Drives",
    "FreeSpace",
    "FloppyStructure",
    "DescribeDisc",
    "DiscardReadSectorsCache",
    "DiscFormat",
    "LayoutStructure",
    "MiscOp",
    "SectorDiscOp",
    "FreeSpace64",
    "DiscOp64",
    "Features",
];

/// What a client's descriptor block says of it, by bit.
pub(crate) mod flag {
    /// A hard disc's mount fills in its geometry as a floppy's does.
    pub(crate) const HARD_DISC_MOUNT: u32 = 1 << 4;
    /// Its `MiscOp` entry answers reason 7.
    pub(crate) const MISC_OP_7: u32 = 1 << 8;
    /// Its `DiscOp` entry takes sector addresses rather than byte ones.
    pub(crate) const SECTOR_ADDRESSING: u32 = 1 << 9;
    /// Its errors are numbered `&10000` beside its filing system's number.
    pub(crate) const NEW_ERRORS: u32 = 1 << 10;
}

/// The drives a filing system of FileCore's has: four floppies, then four
/// hard discs.
pub(crate) const DRIVES: u8 = 8;
/// The first hard disc's drive.
pub(crate) const FIRST_HARD_DISC: u8 = 4;

/// The information word FileCore registers a client's filing system with,
/// beside its number, as RISC OS 5's: told of flushes, a host of image
/// filing systems, loading through its open files, an extra word.
const INFO: u32 = info::FLUSH_NOTIFY | info::IMAGES | info::LOAD_BY_OPEN | info::EXTRA;

/// Its messages, in the emulator's own words.
const MESSAGES: &[u8] = include_bytes!("../../resources/FileCore/Messages");
/// FileCore's own workspace: its messages' file.
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.FileCore.Messages\0");
const OWN_WORKSPACE: u32 = MESSAGES_FILE.room();

/// Where an instance's workspace keeps the filing system's information
/// block, an empty start-up text, `MiscOp` 6's block, each drive's disc record
/// and a hard disc's boot block for its client, and the buffer a transfer
/// passes through.
const INFO_AT: u32 = 0;
const EMPTY_AT: u32 = INFO_AT + 4 * len32(block::WORDS);
const DESCRIBED_AT: u32 = EMPTY_AT + 4;
const RECORDS_AT: u32 = DESCRIBED_AT + 4 * len32(DESCRIPTOR_WORDS);
/// A disc record's room, as `FileCore_DescribeDisc` fills it.
pub(crate) const RECORD_ROOM: u32 = 64;
const BOOT_BLOCKS_AT: u32 = RECORDS_AT + RECORD_ROOM * DRIVES as u32;
/// A hard disc's boot block.
pub(crate) const BOOT_BLOCK_ROOM: u32 = len32(tairix_filecore::record::BOOT_BLOCK_SIZE);
const BOUNCE_AT: u32 = BOOT_BLOCKS_AT + BOOT_BLOCK_ROOM * (DRIVES - FIRST_HARD_DISC) as u32;
/// The buffer a transfer passes through: whole sectors of every size.
pub(crate) const BOUNCE: u32 = 4096;
const INSTANCE_WORKSPACE: u32 = BOUNCE_AT + BOUNCE;

/// A descriptor block's words: its flags and filing system number, then its
/// title's, boot text's, `DiscOp`'s and `MiscOp`'s.
pub(crate) const DESCRIPTOR_WORDS: usize = 5;
/// The bits of a descriptor block's first word its flags take; the filing
/// system's number is the byte above them.
const DESCRIPTOR_FLAGS: u32 = 0x00FF_FFFF;
/// Where a descriptor block's first word holds its filing system's number.
const DESCRIPTOR_NUMBER_SHIFT: u32 = 24;

/// A descriptor block written at `at`: `flags` beneath the filing system's
/// `number`, then `entries`, the title's, boot text's, `DiscOp`'s and
/// `MiscOp`'s.
pub(crate) fn write_descriptor(
    os: &mut Os<'_>,
    at: u32,
    flags: u32,
    number: u8,
    entries: [u32; DESCRIPTOR_WORDS - 1],
) -> Result<(), Fail> {
    let [title, boot_text, disc_op, misc_op] = entries;
    let words = [
        (flags & DESCRIPTOR_FLAGS) | u32::from(number) << DESCRIPTOR_NUMBER_SHIFT,
        title,
        boot_text,
        disc_op,
        misc_op,
    ];
    os.write_words(at, &words)
}

/// The descriptor block a client hands over at `at`: its flags, its filing
/// system's number, and its title's, boot text's, `DiscOp`'s and `MiscOp`'s
/// entries.
pub(crate) fn read_descriptor(
    os: &mut Os<'_>,
    at: u32,
) -> Result<(u32, u8, [u32; DESCRIPTOR_WORDS - 1]), Fail> {
    let first = os.read_word(at)?;
    let mut entries = [0; DESCRIPTOR_WORDS - 1];
    os.read_words(at.wrapping_add(4), &mut entries)?;
    let number = u8::try_from(first >> DESCRIPTOR_NUMBER_SHIFT).unwrap_or(u8::MAX);
    Ok((first & DESCRIPTOR_FLAGS, number, entries))
}

/// The first of its routines that are the entries each filing system's
/// information block names.
const ENTRIES: usize = 0;
/// The routines `FileCore_Create` hands its client: a background transfer's
/// completion, for floppies and hard discs, and the release of FIQ.
const BACKGROUND_FLOPPY: usize = 7;
const BACKGROUND_HARD_DISC: usize = 8;
const RELEASE_FIQ: usize = 9;

/// FileCore.
pub static FILE_CORE: Builtin = Builtin {
    title: "FileCore",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "FileCore",
        names: &NAMES,
        handler: swis::swi,
    }),
    services: Some(Services {
        numbers: &[FS_REDECLARE],
        handler: service,
    }),
    commands: &commands::COMMANDS,
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
        background_done,
        background_done,
        release_fiq,
    ],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.FileCore.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// A client: the module that moves a drive's sectors, and what its
/// descriptor block says of it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Client {
    /// Its descriptor's flags.
    pub(crate) flags: u32,
    /// Its filing system's number.
    pub(crate) number: u8,
    /// Its filing system's name.
    pub(crate) title: u32,
    /// Its boot text.
    pub(crate) boot_text: u32,
    /// Its low-level disc entry.
    pub(crate) disc_op: Target,
    /// Its miscellaneous entry.
    pub(crate) misc_op: Target,
    /// What its entries are given in R12: the private word it named.
    pub(crate) private_word: u32,
    /// Its floppy drives.
    pub(crate) floppies: u8,
    /// Its hard discs.
    pub(crate) hard_discs: u8,
    /// The drive it starts at.
    pub(crate) default_drive: u8,
}

impl Client {
    /// Whether `drive` is one it has.
    pub(crate) fn has(&self, drive: u8) -> bool {
        if drive < FIRST_HARD_DISC {
            drive < self.floppies
        } else {
            drive - FIRST_HARD_DISC < self.hard_discs
        }
    }
}

/// A file a filing system of FileCore's holds open: where its entry is and
/// the allocation it is read and written through. Its catalogue information
/// is its entry's alone, so a change made while it is open stands once it
/// closes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenFile {
    /// Its disc's drive.
    pub(crate) drive: u8,
    /// The directory whose entry it is.
    pub(crate) parent: u32,
    /// Its entry's name there.
    pub(crate) name: names::EntryName,
    /// Its object's disc address.
    pub(crate) indaddr: u32,
    /// Its extent.
    pub(crate) extent: u32,
    /// The bytes allocated to it.
    pub(crate) allocated: u32,
    /// Whether it may be written.
    pub(crate) writable: bool,
    /// Whether it is a directory, open to read.
    pub(crate) directory: bool,
}

/// One client's filing system.
pub(crate) struct Instance {
    /// Its incarnation's private word, which its client names it by.
    pub(crate) private_word: u32,
    /// Its workspace, which FileSwitch hands its entries.
    pub(crate) workspace: u32,
    /// Its client.
    pub(crate) client: Client,
    /// The drive a disc of no name is on.
    pub(crate) drive: u8,
    /// Each drive's mounted disc: absent while an operation holds it.
    pub(crate) discs: [Option<Mounted>; DRIVES as usize],
    /// The drives an operation holds.
    pub(crate) busy: [bool; DRIVES as usize],
    /// Its open files, by handle less one.
    pub(crate) files: Vec<Option<OpenFile>>,
}

/// What FileCore holds while it is active.
pub struct State {
    own: Own,
    workspace: u32,
    /// The client `FileCore_Create` describes, for the incarnation it starts.
    pending: Option<Client>,
    /// The private word of the instance that incarnation made.
    created: Option<u32>,
    pub(crate) instances: Vec<Instance>,
}

impl State {
    /// The instance whose workspace is `workspace`.
    pub(crate) fn instance(&mut self, workspace: u32) -> Option<&mut Instance> {
        self.instances
            .iter_mut()
            .find(|instance| instance.workspace == workspace)
    }

    /// The instance whose private word is at `private_word`.
    pub(crate) fn named(&mut self, private_word: u32) -> Option<&mut Instance> {
        self.instances
            .iter_mut()
            .find(|instance| instance.private_word == private_word)
    }
}

/// FileCore's own message file in the world, while it is active.
fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// `work` on FileCore's state; "not present" while it has none.
pub(crate) fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// `work` on the instance whose workspace is `workspace`.
pub(crate) fn with_instance<T>(
    os: &mut Os<'_>,
    workspace: u32,
    work: impl FnOnce(&mut Instance) -> T,
) -> Result<T, Fail> {
    match with(os, |state| state.instance(workspace).map(work))? {
        Some(done) => Ok(done),
        None => Err(fail(os, None, code::BAD_PARMS)),
    }
}

/// FileCore's errors, by the number each has in its filing system's range.
pub mod code {
    pub(crate) const DEFECT: u8 = 0x94;
    pub(crate) const TOO_MANY_DEFECTS: u8 = 0x95;
    pub(crate) const COMPACT_REQ: u8 = 0x98;
    pub(crate) const MAP_FULL: u8 = 0x99;
    pub(crate) const BAD_DISC: u8 = 0x9A;
    pub(crate) const AMBIG_DISC: u8 = 0x9E;
    pub(crate) const IN_USE: u8 = 0xA0;
    /// A parameter out of range.
    pub const BAD_PARMS: u8 = 0xA1;
    pub(crate) const BUFFER: u8 = 0xA5;
    pub(crate) const BROKEN_DIR: u8 = 0xA8;
    pub(crate) const BAD_FS_MAP: u8 = 0xA9;
    pub(crate) const BAD_DEFECT_LIST: u8 = 0xAB;
    pub(crate) const BAD_DRIVE: u8 = 0xAC;
    pub(crate) const BAD_RENAME: u8 = 0xB0;
    pub(crate) const DIR_FULL: u8 = 0xB3;
    pub(crate) const DIR_NOT_EMPTY: u8 = 0xB4;
    pub(crate) const ACCESS: u8 = 0xBD;
    /// The object is open.
    pub const OPEN: u8 = 0xC2;
    pub(crate) const LOCKED: u8 = 0xC3;
    pub(crate) const EXISTS: u8 = 0xC4;
    pub(crate) const DISC_FULL: u8 = 0xC6;
    pub(crate) const DISC: u8 = 0xC7;
    pub(crate) const WRITE_PROTECTED: u8 = 0xC9;
    pub(crate) const BAD_NAME: u8 = 0xCC;
    pub(crate) const DISC_NOT_FOUND: u8 = 0xD4;
    /// No object of the name.
    pub const NOT_FOUND: u8 = 0xD6;
    pub(crate) const DISC_NOT_FILE_CORE: u8 = 0xD7;
    pub(crate) const CHANNEL: u8 = 0xDE;
}

/// The token of FileCore's error `code`.
fn token(code: u8) -> &'static str {
    match code {
        code::DEFECT => "Defect",
        code::TOO_MANY_DEFECTS => "TooManyDefects",
        code::COMPACT_REQ => "CompactReq",
        code::MAP_FULL => "MapFull",
        code::BAD_DISC => "BadDisc",
        code::AMBIG_DISC => "AmbigDisc",
        code::IN_USE => "InUse",
        code::BUFFER => "Buffer",
        code::BROKEN_DIR => "BrokenDir",
        code::BAD_FS_MAP => "BadFsMap",
        code::BAD_DEFECT_LIST => "BadDefectList",
        code::BAD_DRIVE => "BadDrive",
        code::BAD_RENAME => "BadRename",
        code::DIR_FULL => "DirFull",
        code::DIR_NOT_EMPTY => "DirNotEmpty",
        code::ACCESS => "Access",
        code::OPEN => "Open",
        code::LOCKED => "Locked",
        code::EXISTS => "Exists",
        code::DISC_FULL => "DiscFull",
        code::DISC => "Disc",
        code::BAD_NAME => "BadName",
        code::DISC_NOT_FOUND => "DiscNotFound",
        code::NOT_FOUND => "NotFound",
        code::DISC_NOT_FILE_CORE => "DiscNotFileCore",
        code::CHANNEL => "Channel",
        _ => "BadParms",
    }
}

/// Error `code` of filing system `system`, numbered beside it: as a client
/// of FileCore's may ask FileCore to number its errors, and as RISC OS's
/// HostFS numbers its own.
#[must_use]
pub const fn numbered(system: u32, code: u8) -> u32 {
    0x1_0000 | system << 8 | code as u32
}

/// FileCore's error `code` for `client`, numbered as the client's
/// descriptor asks: beside its filing system's number, or plain; and one of
/// FileCore's own, for no client, beside FileCore's module number.
pub(crate) fn fail(os: &mut Os<'_>, client: Option<&Client>, code: u8) -> Fail {
    let number = match client {
        Some(client) if client.flags & flag::NEW_ERRORS != 0 => {
            numbered(u32::from(client.number), code)
        }
        Some(_) => u32::from(code),
        None => numbered(MODULE_NUMBER, code),
    };
    os.own_error(
        own,
        Error {
            number,
            token: token(code),
        },
        &[],
    )
}

/// What `lib/filecore` refused, as FileCore reports it for `client`, or as
/// its own for none; a disc's own failure passes through.
pub(crate) fn refused(
    os: &mut Os<'_>,
    client: Option<&Client>,
    error: tairix_filecore::Error<Fail>,
) -> Fail {
    use tairix_filecore::{Fault, Structure};
    let code = match error {
        tairix_filecore::Error::Disc(fail) => return fail,
        tairix_filecore::Error::Fault(fault) => match fault {
            Fault::DiscFull | Fault::TooLarge => code::DISC_FULL,
            Fault::Compaction => code::COMPACT_REQ,
            Fault::MapFull => code::MAP_FULL,
            Fault::DirectoryFull => code::DIR_FULL,
            Fault::NameTooLong | Fault::BadName => code::BAD_NAME,
            Fault::Exists => code::EXISTS,
            Fault::Broken(Structure::Directory) => code::BROKEN_DIR,
            Fault::Broken(Structure::DefectList) => code::BAD_DEFECT_LIST,
            Fault::NoFragment | Fault::Broken(_) => code::BAD_FS_MAP,
            Fault::BadDefect => code::BAD_PARMS,
            Fault::DefectInUse => code::DEFECT,
            Fault::TooManyDefects => code::TOO_MANY_DEFECTS,
            Fault::Unsupported => code::DISC_NOT_FILE_CORE,
            Fault::NoMemory => return os.no_memory(),
        },
    };
    fail(os, client, code)
}

/// The number FileCore's own errors are numbered beside: its module's.
pub const MODULE_NUMBER: u32 = 0x08;

/// Its initialisation, for each incarnation: the first is FileCore's own,
/// which keeps its messages; a later one is the instance `FileCore_Create`
/// made it for, its filing system registered.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let private_word = regs.r[12];
    let pending = os
        .extension::<State>()
        .and_then(|state| state.pending.take());
    match pending {
        None if os.extension::<State>().is_none() => start(os),
        None => Err(fail(os, None, code::BAD_PARMS)),
        Some(client) => create_instance(os, private_word, client),
    }
}

/// FileCore's own incarnation: its messages, and no instance yet. Its
/// private word stays zero, as RISC OS 5's does.
fn start(os: &mut Os<'_>) -> Result<(), Fail> {
    let workspace = os.rma_claim(OWN_WORKSPACE)?;
    let held = MESSAGES_FILE.place(os, workspace).and_then(|own| {
        os.hold_extension(State {
            own,
            workspace,
            pending: None,
            created: None,
            instances: Vec::new(),
        })
    });
    // With its private word zero, nothing else frees the workspace.
    if held.is_err() {
        os.rma_free(workspace)?;
    }
    held
}

/// The instance for `client`, its incarnation's private word at
/// `private_word`: its workspace, and its filing system registered.
fn create_instance(os: &mut Os<'_>, private_word: u32, client: Client) -> Result<(), Fail> {
    let workspace = os.rma_claim(INSTANCE_WORKSPACE)?;
    os.write_word(private_word, workspace)?;
    os.write_word(workspace + EMPTY_AT, 0)?;
    write_descriptor(
        os,
        workspace + DESCRIBED_AT,
        client.flags,
        client.number,
        [
            client.title,
            client.boot_text,
            client.disc_op.address,
            client.misc_op.address,
        ],
    )?;
    let drive = if client.has(client.default_drive) {
        client.default_drive
    } else {
        0
    };
    let placed = with(os, |state| {
        if state.instances.try_reserve(1).is_err() {
            return false;
        }
        state.instances.push(Instance {
            private_word,
            workspace,
            client,
            drive,
            discs: [const { None }; DRIVES as usize],
            busy: [false; DRIVES as usize],
            files: Vec::new(),
        });
        state.created = Some(private_word);
        true
    })?;
    if !placed {
        os.rma_free(workspace)?;
        return Err(os.no_memory());
    }
    match declare(os, workspace, &client) {
        Ok(()) => Ok(()),
        Err(fail) => {
            with(os, |state| {
                state.instances.retain(|i| i.workspace != workspace);
            })?;
            os.rma_free(workspace)?;
            Err(fail)
        }
    }
}

/// `client`'s filing system registered with FileSwitch from its information
/// block, built in the instance's `workspace`.
fn declare(os: &mut Os<'_>, workspace: u32, client: &Client) -> Result<(), Fail> {
    os.declare_filing_system(Declaration {
        module: &FILE_CORE,
        first_entry: ENTRIES,
        block: workspace + INFO_AT,
        name: client.title,
        startup: workspace + EMPTY_AT,
        info: u32::from(client.number) | INFO,
        workspace,
    })
}

/// Its service handler: `Service_FSRedeclare`, every instance's filing
/// system registered again.
fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if regs.r[1] != FS_REDECLARE {
        return Ok(());
    }
    let Ok(instances) = with(os, |state| {
        let mut held = Vec::new();
        if held.try_reserve(state.instances.len()).is_ok() {
            held.extend(state.instances.iter().map(|i| (i.workspace, i.client)));
        }
        held
    }) else {
        return Ok(());
    };
    for (workspace, client) in instances {
        match declare(os, workspace, &client) {
            Ok(()) | Err(Fail::Error(_)) => {}
            Err(Fail::Unwind(unwind)) => return Err(unwind),
        }
    }
    Ok(())
}

/// Its finalisation, for each incarnation: an instance's filing system
/// removed and its files let go; FileCore's own last, with its messages.
fn finalise(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let private_word = regs.r[12];
    let instance = with(os, |state| {
        let at = state
            .instances
            .iter()
            .position(|instance| instance.private_word == private_word)?;
        Some(state.instances.remove(at))
    })?;
    if let Some(instance) = instance {
        let mut regs = Regs::default();
        regs.r[0] = fs_control::REMOVE;
        regs.r[1] = instance.client.title;
        let removed = os.perform(X | FS_CONTROL, &mut regs);
        os.write_word(private_word, 0)?;
        os.rma_free(instance.workspace)?;
        return removed;
    }
    let (workspace, instances) = with(os, |state| (state.workspace, state.instances.len()))?;
    if instances != 0 {
        return Err(fail(os, None, code::IN_USE));
    }
    os.close_own(own)?;
    os.release_extension::<State>();
    os.rma_free(workspace)
}

/// A background transfer's completion, for a client that makes none.
#[expect(clippy::unnecessary_wraps, reason = "a module routine's signature")]
fn background_done(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

/// The release of FIQ after a transfer, which none of FileCore's holds.
#[expect(clippy::unnecessary_wraps, reason = "a module routine's signature")]
fn release_fiq(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

/// The addresses `FileCore_Create` answers with: the routines a client calls
/// when a background transfer completes and to release FIQ.
fn handed(os: &Os<'_>) -> [u32; 3] {
    [BACKGROUND_FLOPPY, BACKGROUND_HARD_DISC, RELEASE_FIQ]
        .map(|routine| os.routine_of(&FILE_CORE, routine).unwrap_or(0))
}

/// Whether the root of the disc in `drive` of the instance whose private
/// word is at `instance` holds anything; nothing for an instance that has
/// gone.
///
/// # Errors
///
/// The disc's failure, or the format's refusal, as FileCore reports it.
pub(crate) fn root_occupied(os: &mut Os<'_>, instance: u32, drive: u8) -> Result<bool, Fail> {
    let Some(workspace) = with(os, |state| state.named(instance).map(|i| i.workspace))? else {
        return Ok(false);
    };
    disc::with_volume(os, workspace, drive, |volume, _| {
        let root = volume.root_addr();
        Ok(!volume.dir_is_empty(root)?)
    })
}

/// The bytes free on `volume`'s disc, and the bytes it holds.
pub(crate) fn space_of(
    volume: &mut tairix_filecore::volume::Volume<disc::ClientDisc<'_, '_>>,
) -> Result<(u64, u64), tairix_filecore::Error<Fail>> {
    let space = volume.space()?;
    let unit = u64::from(space.unit);
    Ok((space.free * unit, space.total * unit))
}

/// `bytes` as a 32-bit answer gives a size: at most 2 GiB less a byte.
pub(crate) fn capped(bytes: u64) -> u32 {
    u32::try_from(bytes.min(0x7FFF_FFFF)).unwrap_or(0x7FFF_FFFF)
}

/// `bytes` as its low and high words.
pub(crate) const fn split64(bytes: u64) -> [u32; 2] {
    [(bytes & 0xFFFF_FFFF) as u32, (bytes >> 32) as u32]
}

/// Where in an instance's workspace each of its parts is.
pub(crate) const fn record_at(workspace: u32, drive: u8) -> u32 {
    workspace + RECORDS_AT + RECORD_ROOM * drive as u32
}

/// A hard disc's boot block in an instance's workspace.
pub(crate) const fn boot_block_at(workspace: u32, drive: u8) -> u32 {
    workspace + BOOT_BLOCKS_AT + BOOT_BLOCK_ROOM * (drive - FIRST_HARD_DISC) as u32
}

/// The buffer a transfer passes through.
pub(crate) const fn bounce_at(workspace: u32) -> u32 {
    workspace + BOUNCE_AT
}

/// `MiscOp` 6's block.
pub(crate) const fn described_at(workspace: u32) -> u32 {
    workspace + DESCRIBED_AT
}
