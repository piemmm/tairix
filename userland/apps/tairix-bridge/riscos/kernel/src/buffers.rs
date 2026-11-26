//! The buffer manager, BufferManager 0.39 (`plans/RISCOS-EMULATOR.md` §10):
//! the buffers device drivers and DeviceFS share, the kernel's serial,
//! printer and mouse buffers among them, reached through `InsV`, `RemV` and
//! `CnpV`, whose default it is while it is active. A buffer is a ring in
//! guest memory that holds a byte fewer than its size.
//!
//! The records are the module's own, each charged a record's room in the
//! RMA, so the buffers a guest names are bounded by its own memory. A
//! buffer's internal ID is its record's place, as RISC OS 5 numbers them.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::error::Error;
use crate::events::number;
use crate::input::{BLOCK, MOUSE, PRINTER, SERIAL_IN, SERIAL_OUT};
use crate::memory::Bits;
use crate::module::{Builtin, Swis, VectorDefault, SWI_OUT_OF_RANGE};
use crate::os::{quietly, Entry, Fail, Os, Regs, Unwind};
use crate::swi::chunk::BUFFER as CHUNK;
use crate::swi::os::upcall;
use crate::vectors;

/// A buffer's flags.
pub mod flag {
    /// Data has come in since the buffer was last empty: it is awake, its
    /// wake-up routine called. Clear, the buffer is dormant.
    pub const AWAKE: u32 = 1;
    /// `Event_OutputEmpty` as a removal empties it.
    pub const OUTPUT_EMPTY: u32 = 1 << 1;
    /// `Event_InputFull` as an insertion fails.
    pub const INPUT_FULL: u32 = 1 << 2;
    /// `UpCall`s 8 and 9 as its free space crosses its threshold.
    pub const UPCALLS: u32 = 1 << 3;
    /// What a buffer made or registered keeps of the flags it is given.
    pub(super) const KEPT: u32 = 0x1F;
}

/// The bytes a buffer's record takes, an internal ID being its record's
/// offset in RISC OS 5's table.
const RECORD: u32 = 44;

/// The first handle the manager names a buffer by.
const FIRST_HANDLE: u32 = 0x100;

/// A handle the caller leaves the manager to choose.
const ANY_HANDLE: u32 = u32::MAX;

/// A threshold the caller only reads.
const READ: u32 = u32::MAX;

/// The reasons of the service routine `Buffer_InternalInfo` hands out, the
/// highest last: R2 a byte or a block's address, R3 its length.
pub mod reason {
    /// A byte in.
    pub const INSERT_BYTE: u32 = 0;
    /// A block in.
    pub const INSERT_BLOCK: u32 = 1;
    /// A byte out.
    pub const REMOVE_BYTE: u32 = 2;
    /// A block out.
    pub const REMOVE_BLOCK: u32 = 3;
    /// The next byte, left in.
    pub const EXAMINE_BYTE: u32 = 4;
    /// The next block, left in.
    pub const EXAMINE_BLOCK: u32 = 5;
    /// The bytes held.
    pub const USED: u32 = 6;
    /// The room left.
    pub const FREE: u32 = 7;
    /// Every byte let go.
    pub const PURGE: u32 = 8;
    /// The held bytes that lie together from the next out.
    pub const NEXT_FILLED: u32 = 9;
}

/// The routine `Buffer_InternalInfo` hands out.
const SERVICE: usize = 0;

const NOT_KNOWN: Error = Error {
    number: 0x2_0702,
    token: "BufferNotKnown",
};
const OWNER_STAYS: Error = Error {
    number: 0x2_0704,
    token: "BufferOwnerStays",
};
const HANDLE_IN_USE: Error = Error {
    number: 0x2_0705,
    token: "BufferHandleInUse",
};
const TOO_SMALL: Error = Error {
    number: 0x2_0706,
    token: "BufferTooSmall",
};

/// How big one of the kernel's buffers the manager holds is.
#[derive(Clone, Copy)]
enum Size {
    Bytes(u32),
    /// As `*Configure PrinterBufferSize` sets it.
    Printer,
}

/// The kernel's buffers the manager holds — the mouse's, serial input and
/// output, the printer's — in the order RISC OS 5 registers them, so their
/// internal IDs are its; each with its flags and size.
const SYSTEM: [(u32, u32, Size); 4] = [
    (MOUSE, flag::INPUT_FULL, Size::Bytes(64)),
    (
        SERIAL_IN,
        flag::INPUT_FULL | flag::UPCALLS,
        Size::Bytes(256),
    ),
    (SERIAL_OUT, flag::OUTPUT_EMPTY, Size::Bytes(192)),
    (PRINTER, flag::OUTPUT_EMPTY, Size::Printer),
];

/// The buffer manager.
pub(crate) static BUFFER_MANAGER: Builtin = Builtin {
    title: "BufferManager",
    help_name: Some("Buffer Manager"),
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "Buffer",
        names: &[
            "Create",
            "Remove",
            "Register",
            "Deregister",
            "ModifyFlags",
            "LinkDevice",
            "UnlinkDevice",
            "GetInfo",
            "Threshold",
            "InternalInfo",
        ],
        handler: swi,
    }),
    services: None,
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[service_routine],
    vectors: &[
        VectorDefault {
            vector: vectors::INS,
            code: insert_vector,
        },
        VectorDefault {
            vector: vectors::REM,
            code: remove_vector,
        },
        VectorDefault {
            vector: vectors::CNP,
            code: count_purge_vector,
        },
    ],
    resources: &[],
    code: None,
};

/// Who a buffer is linked to: the routines `Buffer_LinkDevice` gave, and the
/// private word and workspace they are called with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Link {
    /// Called as data enters the dormant buffer; none for zero.
    wake: u32,
    /// Called as the buffer's owner is to change; zero for an owner that
    /// never lets it.
    owner: u32,
    private: u32,
    workspace: u32,
    bits: Bits,
}

/// Whose memory a buffer is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Memory {
    /// Claimed from the RMA by the manager.
    Created,
    /// The caller's own.
    Registered,
}

/// One buffer.
#[derive(Clone, Copy, Debug)]
struct Buffer {
    handle: u32,
    flags: u32,
    start: u32,
    /// Its bytes, at least one; it holds one fewer.
    size: u32,
    /// The offsets of the next byte in and the next byte out.
    insert: u32,
    remove: u32,
    threshold: u32,
    link: Option<Link>,
    memory: Memory,
    /// Its record's place, and the RMA block charged for it.
    record: usize,
    charge: u32,
}

impl Buffer {
    const fn used(&self) -> u32 {
        if self.insert >= self.remove {
            self.insert - self.remove
        } else {
            self.size - self.remove + self.insert
        }
    }

    const fn free(&self) -> u32 {
        self.size - 1 - self.used()
    }

    /// The bytes from offset `from` to the end of the buffer's memory.
    const fn to_end(self, from: u32) -> u32 {
        self.size - from
    }

    const fn advanced(&self, offset: u32, by: u32) -> u32 {
        let to_end = self.to_end(offset);
        if by >= to_end {
            by - to_end
        } else {
            offset + by
        }
    }
}

/// What the manager holds while it is active.
pub(crate) struct State {
    /// The buffers, in handle order.
    buffers: Vec<Buffer>,
    /// Which handle each record holds.
    records: Vec<Option<u32>>,
    /// The records no buffer holds, which a buffer made takes first; its
    /// room is the records', so freeing one never needs memory.
    free_records: Vec<usize>,
    /// The workspace its private word holds, which the service routine is
    /// given.
    workspace: u32,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            buffers: Vec::new(),
            records: Vec::new(),
            free_records: Vec::new(),
            workspace: 0,
        }
    }

    /// Where `handle`'s buffer is, else where it would go.
    fn place(&self, handle: u32) -> Result<usize, usize> {
        self.buffers
            .binary_search_by_key(&handle, |buffer| buffer.handle)
    }

    fn find(&self, handle: u32) -> Option<usize> {
        self.place(handle).ok()
    }

    /// The lowest handle from [`FIRST_HANDLE`] no buffer has.
    fn unused_handle(&self) -> Option<u32> {
        let from = self.place(FIRST_HANDLE).unwrap_or_else(|at| at);
        let run = &self.buffers[from..];
        // Handles rise strictly, so those that are FIRST_HANDLE and each one
        // on are a prefix of the run, found by halving.
        let (mut low, mut high) = (0, run.len());
        while low < high {
            let mid = low + (high - low) / 2;
            let unbroken = u32::try_from(mid)
                .ok()
                .and_then(|mid| FIRST_HANDLE.checked_add(mid))
                .is_some_and(|handle| run[mid].handle == handle);
            if unbroken {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        FIRST_HANDLE.checked_add(u32::try_from(low).ok()?)
    }

    /// Where the buffer with internal ID `id` is.
    fn by_id(&self, id: u32) -> Option<usize> {
        let record = id.is_multiple_of(RECORD).then_some(id / RECORD)? as usize;
        let handle = (*self.records.get(record)?)?;
        self.find(handle)
    }
}

/// The handle `asked` names, or the one the manager chooses for
/// [`ANY_HANDLE`]; a handle in use refused.
fn handle_for(os: &mut Os<'_>, asked: u32) -> Result<u32, Fail> {
    let state = &os.world.buffers;
    if asked == ANY_HANDLE {
        return state.unused_handle().ok_or_else(|| os.error(HANDLE_IN_USE));
    }
    if state.find(asked).is_some() {
        return Err(os.error(HANDLE_IN_USE));
    }
    Ok(asked)
}

/// Adds a buffer of `size` bytes at `start` as `handle`, its record
/// charged to the RMA.
fn add(
    os: &mut Os<'_>,
    handle: u32,
    flags: u32,
    start: u32,
    size: u32,
    memory: Memory,
) -> Result<(), Fail> {
    let charge = os.rma_claim(RECORD)?;
    let state = &mut os.world.buffers;
    // A fresh record makes room among the free ones for every record, the
    // free list being empty.
    let fresh = state.free_records.is_empty();
    let room = state.buffers.try_reserve(1).is_ok()
        && (!fresh
            || state.records.try_reserve(1).is_ok()
                && state
                    .free_records
                    .try_reserve(state.records.len() + 1)
                    .is_ok());
    if !room {
        os.free_rma_quietly(charge);
        return Err(os.no_memory());
    }
    let record = state.free_records.pop().unwrap_or_else(|| {
        state.records.push(None);
        state.records.len() - 1
    });
    state.records[record] = Some(handle);
    let at = state.place(handle).unwrap_or_else(|at| at);
    state.buffers.insert(
        at,
        Buffer {
            handle,
            flags: flags & flag::KEPT,
            start,
            size,
            insert: 0,
            remove: 0,
            threshold: 0,
            link: None,
            memory,
            record,
            charge,
        },
    );
    Ok(())
}

/// Takes the buffer at `at` out of the records, its charge freed.
fn drop_buffer(os: &mut Os<'_>, at: usize) -> Buffer {
    let state = &mut os.world.buffers;
    let buffer = state.buffers.remove(at);
    if let Some(slot) = state.records.get_mut(buffer.record) {
        *slot = None;
        state.free_records.push(buffer.record);
    }
    os.free_rma_quietly(buffer.charge);
    buffer
}

/// The buffer at `at`, a copy to work on.
fn buffer_at(os: &Os<'_>, at: usize) -> Buffer {
    os.world.buffers.buffers[at]
}

/// Puts back a buffer worked on, unless it has gone meanwhile.
fn store(os: &mut Os<'_>, buffer: Buffer) {
    if let Some(at) = os.world.buffers.find(buffer.handle) {
        let held = &mut os.world.buffers.buffers[at];
        held.flags = buffer.flags;
        held.insert = buffer.insert;
        held.remove = buffer.remove;
    }
}

/// The size of the printer's buffer: the configured pages, else RISC OS 5's
/// default.
fn printer_size(os: &mut Os<'_>) -> Result<u32, Fail> {
    const DEFAULT: u32 = 1024;
    Ok(
        match u32::from(os.cmos_pages(crate::cmos::PRINTER_BUFFER)?) {
            0 => DEFAULT,
            pages => pages * tairix_riscos_arm::mmu::PAGE_SIZE,
        },
    )
}

/// Its initialisation: its workspace in its private word, and the kernel's
/// buffers it holds made.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(4)?;
    os.write_word(regs.r[12], workspace)?;
    os.world.buffers = State {
        workspace,
        ..State::new()
    };
    for (number, flags, size) in SYSTEM {
        let made = match size {
            Size::Bytes(bytes) => Ok(bytes),
            Size::Printer => printer_size(os),
        }
        .and_then(|size| create_buffer(os, number, flags, size));
        if let Err(fail) = made {
            release_all(os);
            return Err(fail);
        }
    }
    let mut starting = Regs::default();
    starting.r[1] = crate::service::BUFFER_STARTING;
    os.service(&mut starting)?;
    Ok(())
}

/// A buffer of `size` bytes in the RMA as `handle`.
fn create_buffer(os: &mut Os<'_>, handle: u32, flags: u32, size: u32) -> Result<(), Fail> {
    let start = os.rma_claim(size)?;
    if let Err(fail) = add(os, handle, flags, start, size, Memory::Created) {
        os.free_rma_quietly(start);
        return Err(fail);
    }
    Ok(())
}

/// Every buffer let go, at once — its record's charge and the memory of
/// those it made freed. The kernel frees its workspace from its private
/// word.
fn release_all(os: &mut Os<'_>) {
    let state = core::mem::replace(&mut os.world.buffers, State::new());
    for buffer in state.buffers {
        os.free_rma_quietly(buffer.charge);
        if buffer.memory == Memory::Created {
            os.free_rma_quietly(buffer.start);
        }
    }
}

/// Its finalisation: each linked buffer's owner asked to let it go, any
/// refusal keeping the module; then every buffer let go. While it dies no
/// SWI reaches it, so no owner removes a buffer the list still names.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let buffers = &os.world.buffers.buffers;
    let linked = buffers.iter().filter(|buffer| buffer.link.is_some());
    let mut handles = Vec::new();
    if handles.try_reserve_exact(linked.clone().count()).is_err() {
        return Err(os.no_memory());
    }
    handles.extend(linked.map(|buffer| buffer.handle));
    for handle in handles {
        let mut regs = Regs::default();
        regs.r[0] = handle;
        detach(os, handle, &regs)?;
    }
    release_all(os);
    Ok(())
}

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0x0 => create(os, regs),
        0x1 => remove(os, regs, true),
        0x2 => register(os, regs),
        0x3 => remove(os, regs, false),
        0x4 => modify_flags(os, regs),
        0x5 => link_device(os, regs),
        0x6 => unlink_device(os, regs),
        0x7 => get_info(os, regs),
        0x8 => threshold(os, regs),
        0x9 => internal_info(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[BUFFER_MANAGER.title.as_bytes()])),
    }
}

/// Where `handle`'s buffer is, else "not known".
fn named(os: &mut Os<'_>, handle: u32) -> Result<usize, Fail> {
    os.world
        .buffers
        .find(handle)
        .ok_or_else(|| os.error(NOT_KNOWN))
}

/// `Buffer_Create`: R1 bytes from the RMA as a buffer of R0's flags, named
/// R2, or by the manager for -1; R0 its handle.
fn create(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [flags, size, asked, ..] = regs.r;
    if size == 0 {
        return Err(os.error(TOO_SMALL));
    }
    let handle = handle_for(os, asked)?;
    create_buffer(os, handle, flags, size)?;
    regs.r[0] = handle;
    Ok(())
}

/// `Buffer_Register`: R1 to R2 as a buffer of R0's flags, named R3, or by
/// the manager for -1; R0 its handle.
fn register(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [flags, start, end, asked, ..] = regs.r;
    if end <= start {
        return Err(os.error(TOO_SMALL));
    }
    let handle = handle_for(os, asked)?;
    add(os, handle, flags, start, end - start, Memory::Registered)?;
    regs.r[0] = handle;
    Ok(())
}

/// `Buffer_Remove` (`freeing`) and `Buffer_Deregister`: R0's buffer let go
/// once its owner lets it, and for a removal its memory freed from the RMA,
/// as RISC OS 5 frees it whoever claimed it — a refusal coming after the
/// buffer is gone.
fn remove(os: &mut Os<'_>, regs: &mut Regs, freeing: bool) -> Result<(), Fail> {
    let handle = regs.r[0];
    named(os, handle)?;
    detach(os, handle, regs)?;
    let at = named(os, handle)?;
    let buffer = drop_buffer(os, at);
    if freeing {
        os.rma_free(buffer.start)?;
    }
    Ok(())
}

/// Asks the owner of `handle`'s buffer, if it is linked, to let it go: its
/// owner-change routine called with `regs` and the link's private word and
/// workspace, a refusal its error. A link whose owner never lets go
/// refuses.
fn detach(os: &mut Os<'_>, handle: u32, regs: &Regs) -> Result<(), Fail> {
    let at = named(os, handle)?;
    let Some(link) = buffer_at(os, at).link else {
        return Ok(());
    };
    if link.owner == 0 {
        return Err(os.error(OWNER_STAYS));
    }
    let mut call = *regs;
    call.r[8] = link.private;
    call.r[12] = link.workspace;
    os.call_routine(link.owner, link.bits, Entry::Supervisor, &mut call)
}

/// `Buffer_ModifyFlags`: R0's flags kept where R2 has a bit, then toggled
/// where R1 has one; R1 the old, R2 the new.
fn modify_flags(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = named(os, regs.r[0])?;
    let held = &mut os.world.buffers.buffers[at];
    let old = held.flags;
    held.flags = (old & regs.r[2]) ^ regs.r[1];
    regs.r[1] = old;
    regs.r[2] = held.flags;
    Ok(())
}

/// `Buffer_LinkDevice`: R0's buffer linked to R1's wake-up routine and R2's
/// owner-change routine, called with R3 and R4; the owner it had asked to
/// let it go first.
fn link_device(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[0];
    named(os, handle)?;
    detach(os, handle, regs)?;
    let at = named(os, handle)?;
    let bits = os.caller_bits();
    os.world.buffers.buffers[at].link = Some(Link {
        wake: regs.r[1],
        owner: regs.r[2],
        private: regs.r[3],
        workspace: regs.r[4],
        bits,
    });
    Ok(())
}

/// `Buffer_UnlinkDevice`: R0's buffer linked to nothing, unasked, and
/// purged.
fn unlink_device(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = named(os, regs.r[0])?;
    os.world.buffers.buffers[at].link = None;
    purge(os, at)
}

/// `Buffer_GetInfo`: R0's flags, its memory's start and end, the insertion
/// and removal offsets, its free space and its bytes, in R0 to R6.
fn get_info(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = named(os, regs.r[0])?;
    let buffer = buffer_at(os, at);
    regs.r[0] = buffer.flags;
    regs.r[1] = buffer.start;
    regs.r[2] = buffer.start.wrapping_add(buffer.size);
    regs.r[3] = buffer.insert;
    regs.r[4] = buffer.remove;
    regs.r[5] = buffer.free();
    regs.r[6] = buffer.used();
    Ok(())
}

/// `Buffer_Threshold`: R0's threshold set to R1, or read for -1; R1 the
/// one it had.
fn threshold(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = named(os, regs.r[0])?;
    let held = &mut os.world.buffers.buffers[at];
    let previous = held.threshold;
    if regs.r[1] != READ {
        held.threshold = regs.r[1];
    }
    regs.r[1] = previous;
    Ok(())
}

/// `Buffer_InternalInfo`: R0's internal ID, the service routine and the
/// workspace it is entered with, in R0 to R2.
fn internal_info(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = named(os, regs.r[0])?;
    let record = buffer_at(os, at).record;
    regs.r[0] = u32::try_from(record)
        .ok()
        .and_then(|record| record.checked_mul(RECORD))
        .ok_or_else(|| os.error(NOT_KNOWN))?;
    regs.r[1] = os.routine_of(&BUFFER_MANAGER, SERVICE).unwrap_or(0);
    regs.r[2] = os.world.buffers.workspace;
    Ok(())
}

/// Up to `count` bytes from guest memory at `from` into the buffer at
/// `at`, as many as there is room for: how many went in.
fn insert_bytes(os: &mut Os<'_>, at: usize, from: u32, count: u32) -> Result<u32, Fail> {
    let buffer = buffer_at(os, at);
    let count = count.min(buffer.free());
    let first = count.min(buffer.to_end(buffer.insert));
    os.copy_bytes(from, buffer.start.wrapping_add(buffer.insert), first)?;
    os.copy_bytes(from.wrapping_add(first), buffer.start, count - first)?;
    inserted(os, buffer, count)?;
    Ok(count)
}

/// One byte into the buffer at `at`: whether there was room.
fn insert_byte(os: &mut Os<'_>, at: usize, byte: u8) -> Result<bool, Fail> {
    let buffer = buffer_at(os, at);
    if buffer.free() == 0 {
        return Ok(false);
    }
    os.write_byte(buffer.start.wrapping_add(buffer.insert), byte)?;
    inserted(os, buffer, 1)?;
    Ok(true)
}

/// `count` bytes put in at `buffer`'s insertion point: the point moved on,
/// and a dormant buffer that took any woken — its wake-up routine called —
/// then `UpCall` 8 where its free space fell below its threshold.
fn inserted(os: &mut Os<'_>, mut buffer: Buffer, count: u32) -> Result<(), Fail> {
    let free_before = buffer.free();
    buffer.insert = buffer.advanced(buffer.insert, count);
    let woken = count > 0 && buffer.flags & flag::AWAKE == 0;
    if woken {
        buffer.flags |= flag::AWAKE;
    }
    store(os, buffer);
    if let Some(link) = buffer.link.filter(|link| woken && link.wake != 0) {
        let mut call = Regs::default();
        call.r[0] = buffer.handle;
        call.r[8] = link.private;
        call.r[12] = link.workspace;
        quietly(os.call_routine(link.wake, link.bits, Entry::SupervisorNoIrq, &mut call))?;
    }
    let free = free_now(os, buffer.handle);
    if buffer.flags & flag::UPCALLS != 0
        && buffer.threshold != 0
        && free_before >= buffer.threshold
        && free < buffer.threshold
    {
        tell(os, upcall::BUFFER_FILLING, buffer.handle, 0)?;
    }
    Ok(())
}

/// `count` bytes taken from `buffer`'s removal point: the point moved on, a
/// buffer left empty dormant again, then `UpCall` 9 where its free space rose
/// to its threshold.
fn removed(os: &mut Os<'_>, mut buffer: Buffer, count: u32) -> Result<(), Fail> {
    let free_before = buffer.free();
    buffer.remove = buffer.advanced(buffer.remove, count);
    if buffer.used() == 0 {
        buffer.flags &= !flag::AWAKE;
    }
    store(os, buffer);
    emptying(os, buffer, free_before)
}

/// `UpCall` 9 for `buffer` where its free space rose from `free_before` to
/// its threshold.
fn emptying(os: &mut Os<'_>, buffer: Buffer, free_before: u32) -> Result<(), Fail> {
    let free = free_now(os, buffer.handle);
    if buffer.flags & flag::UPCALLS != 0
        && buffer.threshold != 0
        && free_before < buffer.threshold
        && free >= buffer.threshold
    {
        tell(os, upcall::BUFFER_EMPTYING, buffer.handle, u32::MAX)?;
    }
    Ok(())
}

/// The free space of `handle`'s buffer now, the routines it called having
/// moved bytes or removed it.
fn free_now(os: &Os<'_>, handle: u32) -> u32 {
    let state = &os.world.buffers;
    state.find(handle).map_or(0, |at| state.buffers[at].free())
}

/// `UpCall` `reason` for `handle`, a refusal being nobody's.
fn tell(os: &mut Os<'_>, reason: u32, handle: u32, r2: u32) -> Result<(), Fail> {
    let mut call = Regs::default();
    call.r[0] = reason;
    call.r[1] = handle;
    call.r[2] = r2;
    quietly(os.up_call(&mut call)).map_err(Fail::from)
}

/// Up to `count` bytes out of the buffer at `at` into guest memory at `to`,
/// left in it if `examining`.
fn remove_bytes(
    os: &mut Os<'_>,
    at: usize,
    to: u32,
    count: u32,
    examining: bool,
) -> Result<u32, Fail> {
    let buffer = buffer_at(os, at);
    let count = count.min(buffer.used());
    let first = count.min(buffer.to_end(buffer.remove));
    os.copy_bytes(buffer.start.wrapping_add(buffer.remove), to, first)?;
    os.copy_bytes(buffer.start, to.wrapping_add(first), count - first)?;
    if !examining && count > 0 {
        removed(os, buffer, count)?;
    }
    Ok(count)
}

/// The next byte of the buffer at `at`, removed unless `examining`; none
/// for an empty buffer.
fn remove_byte(os: &mut Os<'_>, at: usize, examining: bool) -> Result<Option<u8>, Fail> {
    let buffer = buffer_at(os, at);
    if buffer.used() == 0 {
        return Ok(None);
    }
    let byte = os.read_byte(buffer.start.wrapping_add(buffer.remove))?;
    if !examining {
        removed(os, buffer, 1)?;
    }
    Ok(Some(byte))
}

/// The buffer at `at` emptied and dormant again.
fn purge(os: &mut Os<'_>, at: usize) -> Result<(), Fail> {
    let mut buffer = buffer_at(os, at);
    let free_before = buffer.free();
    buffer.insert = 0;
    buffer.remove = 0;
    buffer.flags &= !flag::AWAKE;
    store(os, buffer);
    emptying(os, buffer, free_before)
}

/// Event `number` for `handle`, with what a call leaves in R2 and R3.
fn raise(os: &mut Os<'_>, number: u32, handle: u32, r2: u32, r3: u32) -> Result<(), Unwind> {
    let mut event = Regs::default();
    event.r[0] = number;
    event.r[1] = handle;
    event.r[2] = r2;
    event.r[3] = r3;
    os.raise_event(&mut event).map(drop)
}

/// `InsV`'s default while the manager is active: its buffers' insertions,
/// any other passed to the kernel.
fn insert_vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[1] & !BLOCK;
    let Some(at) = os.world.buffers.find(handle) else {
        return os.default_insert(regs);
    };
    let flags = buffer_at(os, at).flags;
    regs.set_flag(psr::V, false);
    if regs.r[1] & BLOCK != 0 {
        let moved = insert_bytes(os, at, regs.r[2], regs.r[3])?;
        regs.r[2] = regs.r[2].wrapping_add(moved);
        regs.r[3] -= moved;
        let short = regs.r[3] != 0;
        regs.set_flag(psr::C, short);
        if short && flags & flag::INPUT_FULL != 0 {
            raise(os, number::INPUT_FULL, handle, regs.r[2], regs.r[3])?;
        }
        return Ok(());
    }
    let byte = regs.byte(0);
    regs.r[2] = u32::from(byte);
    let fitted = insert_byte(os, at, byte)?;
    regs.set_flag(psr::C, !fitted);
    if !fitted && flags & flag::INPUT_FULL != 0 {
        raise(os, number::INPUT_FULL, handle, regs.r[2], regs.r[3])?;
    }
    Ok(())
}

/// `RemV`'s default while the manager is active.
fn remove_vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[1] & !BLOCK;
    let Some(at) = os.world.buffers.find(handle) else {
        return os.default_remove(regs);
    };
    let examining = regs.flag(psr::V);
    regs.set_flag(psr::V, false);
    let before = buffer_at(os, at);
    if regs.r[1] & BLOCK != 0 {
        let moved = remove_bytes(os, at, regs.r[2], regs.r[3], examining)?;
        regs.r[2] = regs.r[2].wrapping_add(moved);
        regs.r[3] -= moved;
        regs.set_flag(psr::C, regs.r[3] != 0);
        emptied(os, before, examining, moved, regs.r[2], regs.r[3])?;
        return Ok(());
    }
    let Some(byte) = remove_byte(os, at, examining)? else {
        regs.set_flag(psr::C, true);
        return Ok(());
    };
    regs.r[0] = u32::from(byte);
    regs.r[2] = u32::from(byte);
    regs.set_flag(psr::C, false);
    emptied(os, before, examining, 1, regs.r[2], regs.r[3])
}

/// `Event_OutputEmpty` where a removal of `count` bytes emptied `before`'s
/// buffer and it asks for it.
fn emptied(
    os: &mut Os<'_>,
    before: Buffer,
    examining: bool,
    count: u32,
    r2: u32,
    r3: u32,
) -> Result<(), Fail> {
    if !examining && count > 0 && count == before.used() && before.flags & flag::OUTPUT_EMPTY != 0 {
        raise(os, number::OUTPUT_EMPTY, before.handle, r2, r3)?;
    }
    Ok(())
}

/// `CnpV`'s default while the manager is active: a purge, or a count of
/// bytes or free space into R1's low byte and R2.
fn count_purge_vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[1] & !BLOCK;
    let Some(at) = os.world.buffers.find(handle) else {
        return os.default_count_purge(regs);
    };
    let purging = regs.flag(psr::V);
    regs.set_flag(psr::V, false);
    if purging {
        return purge(os, at);
    }
    let buffer = buffer_at(os, at);
    let count = if regs.flag(psr::C) {
        buffer.free()
    } else {
        buffer.used()
    };
    regs.r[1] = count & 0xFF;
    regs.r[2] = count >> 8;
    Ok(())
}

/// The service routine `Buffer_InternalInfo` gives: R0 its reason, R1 the
/// buffer's internal ID; C set where a byte or block could not move. A
/// reason past the last, which RISC OS 5 jumps out of its table for, is
/// refused.
fn service_routine(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(at) = os.world.buffers.by_id(regs.r[1]) else {
        return Err(os.error(NOT_KNOWN));
    };
    let mut short = false;
    match regs.r[0] {
        reason::INSERT_BYTE => short = !insert_byte(os, at, regs.byte(2))?,
        reason::INSERT_BLOCK | reason::REMOVE_BLOCK | reason::EXAMINE_BLOCK => {
            let moved = if regs.r[0] == reason::INSERT_BLOCK {
                insert_bytes(os, at, regs.r[2], regs.r[3])?
            } else {
                remove_bytes(
                    os,
                    at,
                    regs.r[2],
                    regs.r[3],
                    regs.r[0] == reason::EXAMINE_BLOCK,
                )?
            };
            regs.r[2] = regs.r[2].wrapping_add(moved);
            regs.r[3] -= moved;
            short = regs.r[3] != 0;
        }
        reason::REMOVE_BYTE | reason::EXAMINE_BYTE => {
            match remove_byte(os, at, regs.r[0] == reason::EXAMINE_BYTE)? {
                Some(byte) => regs.r[2] = u32::from(byte),
                None => short = true,
            }
        }
        reason::USED => regs.r[2] = buffer_at(os, at).used(),
        reason::FREE => regs.r[2] = buffer_at(os, at).free(),
        reason::PURGE => purge(os, at)?,
        reason::NEXT_FILLED => {
            let buffer = buffer_at(os, at);
            regs.r[2] = buffer.start.wrapping_add(buffer.remove);
            regs.r[3] = buffer.used().min(buffer.to_end(buffer.remove));
        }
        _ => return Err(os.error(crate::kheap::UNKNOWN_REASON)),
    }
    regs.set_flag(psr::C, short);
    Ok(())
}

#[cfg(test)]
mod tests;
