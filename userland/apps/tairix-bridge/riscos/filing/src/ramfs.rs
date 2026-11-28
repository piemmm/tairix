//! RamFS (`plans/RISCOS-EMULATOR.md` §6): `RAM:`, a FileCore client whose
//! one floppy drive holds the RAM disc, dynamic area 5, as on RISC OS 5.
//!
//! Its initialisation lays a fresh disc out over the whole area, in RISC OS
//! 5's layout for a disc that size, and refuses an empty area: the kernel
//! starts RamFS again after each resize of the area, which it allows only
//! while the disc holds nothing. The area's pages are mapped into its window
//! as the disc first reaches them.

use alloc::vec::Vec;

use tairix_filecore::format::{lay_out, new_map_record, BootRecord, Variant};
use tairix_filecore::record::DISC_RECORD_SIZE;
use tairix_filecore::Disc;
use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::memory::RAM_DISC as AREA;
use tairix_riscos_kernel::module::{Builtin, Command, Resource};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, quietly, Fail, Os, Regs, World};
use tairix_riscos_kernel::ramdisc::Occupant;
use tairix_riscos_kernel::swi::os::{
    fs_control, DYNAMIC_AREA, FS_CONTROL, MODULE, READ_DYNAMIC_AREA,
};
use tairix_riscos_kernel::swi::X;

use crate::filecore::{self, flag};

/// RamFS's filing system number, as RISC OS 5 numbers it.
pub const NUMBER: u8 = 0x17;
/// Its filing system's name, which its module's command selects.
const NAME: &[u8] = b"RAM";
/// The incarnation of FileCore that is its filing system.
const INSTANCE: &[u8] = b"FileCore%RAM";
/// What its descriptor tells FileCore: sector addresses, so a disc past 512
/// MiB is reachable, and errors numbered beside its filing system's number,
/// as RISC OS 5's RamFS gives them.
const FLAGS: u32 = flag::SECTOR_ADDRESSING | flag::NEW_ERRORS;
/// The name a fresh disc is given, padded with nothing as RISC OS 5 pads it.
const DISC_NAME: [u8; 10] = *b"RamDisc0\0\0";

/// Its messages, in the emulator's own words.
const MESSAGES: &[u8] = include_bytes!("../resources/RamFS/Messages");
/// What its workspace keeps: its messages' file; the descriptor block
/// `FileCore_Create` is given, the filing system's name, which is its boot
/// text too, and the media name `MiscOp` 4 points at.
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.RamFS.Messages\0");
const DESCRIPTOR_AT: u32 = MESSAGES_FILE.room();
const NAME_AT: u32 = DESCRIPTOR_AT + 4 * len32(filecore::DESCRIPTOR_WORDS);
const MEDIA_AT: u32 = NAME_AT + 4;
const WORKSPACE: u32 = MEDIA_AT + 8;
/// `MiscOp` 4's media name.
const MEDIA: &[u8] = b"disc\0";

/// "The RAM disc is empty": its initialisation refusing an empty area, as
/// RISC OS 5's does.
const EMPTY: Error = Error {
    number: 0xA5,
    token: "Empty",
};
/// A disc operation the RAM disc has nothing to do with.
const BAD_OP: Error = Error {
    number: 0xA6,
    token: "BadOp",
};

/// RamFS.
pub static RAM_FS: Builtin = Builtin {
    title: "RamFS",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[Command::new(
        "Ram",
        (0, 0),
        "*Ram makes the RAM disc's filing system the current one.",
        Some("Syntax: *Ram"),
        select,
    )],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[disc_op, misc_op],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.RamFS.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// What RamFS holds while it is active.
pub struct State {
    own: Own,
    workspace: u32,
    /// The RAM disc's area: its window's base and size.
    base: u32,
    size: u32,
    /// Which pages of the area its window shows yet.
    mapped: Vec<u64>,
    /// FileCore's instance for it, by its private word.
    instance: u32,
}

impl State {
    /// FileCore's instance for the RAM disc: the private word whose address
    /// FileCore's SWIs take in R8.
    #[must_use]
    pub const fn instance(&self) -> u32 {
        self.instance
    }
}

/// RamFS's own message file in the world, while it is active.
fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// RamFS's error `error`.
fn fail(os: &mut Os<'_>, error: Error) -> Fail {
    os.own_error(own, error, &[])
}

/// `work` on RamFS's state.
fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// Its initialisation: the RAM disc's area read — an empty one refused —
/// a fresh disc laid out over it, and its filing system made by FileCore.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let private_word = regs.r[12];
    let mut area = Regs::default();
    area.r[0] = AREA;
    os.perform(X | READ_DYNAMIC_AREA, &mut area)?;
    let (base, size) = (area.r[0], area.r[1]);
    let words = size.div_ceil(PAGE_SIZE).div_ceil(64) as usize;
    let mut mapped = Vec::new();
    if mapped.try_reserve_exact(words).is_err() {
        return Err(os.no_memory());
    }
    mapped.resize(words, 0);
    let workspace = os.rma_claim(WORKSPACE)?;
    let messages = match MESSAGES_FILE.place(os, workspace) {
        Ok(messages) => messages,
        Err(failure) => {
            os.rma_free(workspace)?;
            return Err(failure);
        }
    };
    if let Err(failure) = os.hold_extension(State {
        own: messages,
        workspace,
        base,
        size,
        mapped,
        instance: 0,
    }) {
        os.rma_free(workspace)?;
        return Err(failure);
    }
    let started = if size == 0 {
        Err(fail(os, EMPTY))
    } else {
        start(os, private_word, workspace, size)
    };
    if let Err(failure) = started {
        os.release_extension::<Occupant>();
        os.close_own(own)?;
        os.release_extension::<State>();
        os.rma_free(workspace)?;
        return Err(failure);
    }
    Ok(())
}

/// A fresh disc laid out over the area, and FileCore asked for the filing
/// system: `FileCore_Create` given the descriptor built in `workspace`.
fn start(os: &mut Os<'_>, private_word: u32, workspace: u32, size: u32) -> Result<(), Fail> {
    format(os, size)?;
    let Some(module) = os.module_of(&RAM_FS) else {
        return Err(os.no_memory());
    };
    os.write_terminated(workspace + NAME_AT, NAME, 0, NAME.len() + 1)?;
    os.write_bytes(workspace + MEDIA_AT, MEDIA)?;
    let routine = |os: &Os<'_>, index: usize| os.routine_of(&RAM_FS, index).unwrap_or(module);
    let name = (workspace + NAME_AT).wrapping_sub(module);
    let entries = [
        name,
        name,
        routine(os, 0).wrapping_sub(module),
        routine(os, 1).wrapping_sub(module),
    ];
    filecore::write_descriptor(os, workspace + DESCRIPTOR_AT, FLAGS, NUMBER, entries)?;
    let mut create = Regs::default();
    create.r[..4].copy_from_slice(&[workspace + DESCRIPTOR_AT, module, private_word, 1]);
    os.perform(X | filecore::swi::CREATE, &mut create)?;
    let instance = create.r[0];
    with(os, |state| state.instance = instance)?;
    os.hold_extension(Occupant { occupied })
}

/// A fresh disc of `size` bytes laid out over the area, named and numbered
/// as RISC OS 5's RamFS names and numbers one.
fn format(os: &mut Os<'_>, size: u32) -> Result<(), Fail> {
    let Ok(mut record) = new_map_record(Variant::RamDisc, u64::from(size)) else {
        return Err(fail(os, EMPTY));
    };
    record.disc_name = DISC_NAME;
    let [low, high, ..] = os.real_time().to_le_bytes();
    record.disc_id = u16::from_le_bytes([low, high]);
    let mut disc = AreaDisc {
        os,
        capacity: u64::from(size),
    };
    match lay_out(&mut disc, record, BootRecord::Blank, b"$") {
        Ok(_) => Ok(()),
        Err(tairix_filecore::Error::Disc(failure)) => Err(failure),
        Err(_) => Err(fail(disc.os, EMPTY)),
    }
}

/// Whether the RAM disc holds anything a resize would lose: anything in its
/// root, or a disc that cannot be read to tell.
fn occupied(os: &mut Os<'_>) -> bool {
    let Ok(instance) = with(os, |state| state.instance) else {
        return false;
    };
    filecore::root_occupied(os, instance, 0).unwrap_or(true)
}

/// Its finalisation: FileCore's instance for it killed, which removes its
/// filing system and closes its files.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = with(os, |state| state.workspace)?;
    os.release_extension::<Occupant>();
    os.close_own(own)?;
    let killed = os.with_text(INSTANCE, 0, |os, frame| {
        let mut kill = Regs::default();
        kill.r[0] = 4;
        kill.r[1] = frame;
        os.perform(X | MODULE, &mut kill)
    });
    os.release_extension::<State>();
    os.rma_free(workspace)?;
    quietly(killed).map_err(Fail::from)
}

/// `*Ram`: its filing system selected.
fn select(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = with(os, |state| state.workspace)?;
    let mut regs = Regs::default();
    regs.r[0] = fs_control::SELECT;
    regs.r[1] = workspace + NAME_AT;
    os.perform(X | FS_CONTROL, &mut regs)
}

/// The area's pages from `first` for `count` mapped into its window where
/// they are not yet.
fn map(os: &mut Os<'_>, first: u32, count: u32) -> Result<(), Fail> {
    let unmapped = with(os, |state| {
        let mut unmapped = Vec::new();
        for page in first..first + count {
            let (word, bit) = ((page / 64) as usize, page % 64);
            let Some(bits) = state.mapped.get(word) else {
                break;
            };
            if bits & 1 << bit == 0 {
                if unmapped.try_reserve(1).is_err() {
                    return None;
                }
                unmapped.push(page);
            }
        }
        Some(unmapped)
    })?
    .ok_or_else(|| os.no_memory())?;
    if unmapped.is_empty() {
        return Ok(());
    }
    let room = len32(unmapped.len()) * 12;
    os.below_supervisor_stack(room, |os, list| {
        for (n, &page) in (0..).zip(&unmapped) {
            let at = list + 12 * n;
            os.write_word(at, page)?;
            os.write_word(at + 4, page)?;
            os.write_word(at + 8, tairix_riscos_kernel::ramdisc::ACCESS)?;
        }
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[22, AREA, list, len32(unmapped.len())]);
        os.perform(X | DYNAMIC_AREA, &mut regs)
    })?;
    with(os, |state| {
        for page in unmapped {
            if let Some(bits) = state.mapped.get_mut((page / 64) as usize) {
                *bits |= 1 << (page % 64);
            }
        }
    })
}

/// The area's bytes from `at` for `length`, its pages mapped: their address
/// in its window; none past the disc.
fn reach(os: &mut Os<'_>, at: u64, length: u32) -> Result<Option<u32>, Fail> {
    let (base, size) = with(os, |state| (state.base, state.size))?;
    let Some(end) = at
        .checked_add(u64::from(length))
        .filter(|&end| end <= u64::from(size))
    else {
        return Ok(None);
    };
    if length == 0 {
        return Ok(Some(base));
    }
    let first = u32::try_from(at / u64::from(PAGE_SIZE)).unwrap_or(u32::MAX);
    let last = u32::try_from((end - 1) / u64::from(PAGE_SIZE)).unwrap_or(u32::MAX);
    map(os, first, last - first + 1)?;
    Ok(Some(
        base.wrapping_add(u32::try_from(at).unwrap_or(u32::MAX)),
    ))
}

/// The RAM disc as a disc of `capacity` bytes, for the layout of a fresh
/// one.
struct AreaDisc<'o, 'w> {
    os: &'o mut Os<'w>,
    capacity: u64,
}

impl AreaDisc<'_, '_> {
    fn at(&mut self, at: u64, length: usize) -> Result<u32, Fail> {
        match reach(self.os, at, len32(length))? {
            Some(address) => Ok(address),
            None => Err(fail(self.os, EMPTY)),
        }
    }
}

impl Disc for AreaDisc<'_, '_> {
    type Error = Fail;

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), Fail> {
        let from = self.at(at, out.len())?;
        self.os.read_bytes(from, out)
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), Fail> {
        let to = self.at(at, bytes.len())?;
        self.os.write_bytes(to, bytes)
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), Fail> {
        let length = u32::try_from(length).unwrap_or(u32::MAX);
        let to = self.at(at, length as usize)?;
        self.os.fill(to, length, 0)
    }

    fn flush(&mut self) -> Result<(), Fail> {
        Ok(())
    }
}

/// `DiscOp`'s reasons RamFS answers, and its flag for a scatter list at R3.
mod reason {
    pub(super) use crate::filecore::op::{READ, WRITE};
    pub(super) const VERIFY: u32 = 0;
    pub(super) const SEEK: u32 = 5;
    pub(super) const RESTORE: u32 = 6;
    pub(super) const SCATTER: u32 = 1 << 5;
}

/// A disc error at `address` on drive 0, as FileCore's entries report one:
/// bit 31, the disc error's number, the address in 256-byte units.
fn disc_error(address: u64) -> Fail {
    const NOT_THERE: u32 = 0x08;
    let unit = u32::try_from(address / 256).unwrap_or(0x1F_FFFF) & 0x1F_FFFF;
    Fail::Error(1 << 31 | NOT_THERE << 24 | unit)
}

/// An entry's failure as FileCore's entries give one: an error block with
/// bit 30 set, a disc error as it came.
fn tagged(outcome: Result<(), Fail>) -> Result<(), Fail> {
    match outcome {
        Err(Fail::Error(word)) if word & 1 << 31 == 0 => Err(Fail::Error(word | 1 << 30)),
        other => other,
    }
}

/// Its low-level disc entry: R1 the reason, R2 drive 0's sector address, R3
/// the memory — or, with R1's bit 5, a list of address and length pairs —
/// and R4 the length; R2 to R4 moved on past what was done.
fn disc_op(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    tagged(transfer(os, regs))
}

/// The transfer [`disc_op`] makes.
fn transfer(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let operation = regs.r[1] & 0xF;
    let scatter = regs.r[1] & reason::SCATTER != 0;
    if regs.r[2] >> 29 != 0 {
        return Err(disc_error(0));
    }
    let mut at = u64::from(regs.r[2] & 0x1FFF_FFFF) * u64::from(SECTOR);
    match operation {
        reason::SEEK | reason::RESTORE => return Ok(()),
        reason::VERIFY | reason::READ | reason::WRITE => {}
        _ => return Err(fail(os, BAD_OP)),
    }
    let mut left = regs.r[4];
    while left > 0 {
        let (ram, room) = if scatter {
            (os.read_word(regs.r[3])?, os.read_word(regs.r[3] + 4)?)
        } else {
            (regs.r[3], left)
        };
        let take = left.min(room);
        if take == 0 {
            if scatter {
                regs.r[3] += 8;
                continue;
            }
            break;
        }
        let Some(disc) = reach(os, at, take)? else {
            return Err(disc_error(at));
        };
        match operation {
            reason::READ => os.copy_bytes(disc, ram, take)?,
            reason::WRITE => os.copy_bytes(ram, disc, take)?,
            _ => {}
        }
        at += u64::from(take);
        left -= take;
        if scatter {
            os.write_word(regs.r[3], ram.wrapping_add(take))?;
            os.write_word(regs.r[3] + 4, room - take)?;
            if room == take {
                regs.r[3] += 8;
            }
        } else if operation != reason::VERIFY {
            regs.r[3] = regs.r[3].wrapping_add(take);
        }
    }
    regs.r[2] = u32::try_from(at / u64::from(SECTOR)).unwrap_or(0) & 0x1FFF_FFFF;
    regs.r[4] = left;
    Ok(())
}

/// The RAM disc's sector.
const SECTOR: u32 = 1024;

/// Its miscellaneous entry: R0 the reason, R1 the drive.
fn misc_op(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    tagged(miscellaneous(os, regs))
}

/// What [`misc_op`] does.
fn miscellaneous(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        MOUNT => mount(os, regs),
        POLL_CHANGED => {
            regs.r[3] = NOT_CHANGED | READY | CHANGE_DETECTED;
            Ok(())
        }
        POLL_PERIOD => {
            let workspace = with(os, |state| state.workspace)?;
            regs.r[5] = u32::MAX;
            regs.r[6] = workspace + MEDIA_AT;
            Ok(())
        }
        DRIVE_STATUS => {
            regs.r[2] = 0;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `MiscOp`'s reasons RamFS answers beside locking and ejection, which a RAM
/// disc has nothing to do for.
const MOUNT: u32 = 0;
const POLL_CHANGED: u32 = 1;
const POLL_PERIOD: u32 = 4;
const DRIVE_STATUS: u32 = 7;
/// What a poll of the RAM disc answers: never changed, always ready, its
/// change detection sure.
const NOT_CHANGED: u32 = 1 << 0;
const READY: u32 = 1 << 4;
const CHANGE_DETECTED: u32 = 1 << 7;

/// `MiscOp` 0: R4 bytes of the disc from R2 read into R3, and the record at
/// R5 given the disc's geometry and size.
fn mount(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] != 0 {
        return Err(disc_error(0));
    }
    let size = with(os, |state| state.size)?;
    let Ok(record) = new_map_record(Variant::RamDisc, u64::from(size)) else {
        return Err(fail(os, EMPTY));
    };
    let encoded = record.encode();
    os.write_bytes(regs.r[5], &encoded[..DISC_RECORD_SIZE])?;
    let at = u64::from(regs.r[2]);
    let length = regs.r[4].min(size.saturating_sub(regs.r[2]));
    if let Some(disc) = reach(os, at, length)? {
        os.copy_bytes(disc, regs.r[3], length)?;
    }
    Ok(())
}
