//! INT 67h: the expanded memory manager for the board the ROM was built
//! for, LIM EMS 4.0 complete.
//!
//! A device header named `EMMXXXX0` at the start of the segment INT 67h
//! points into tells programs a manager is there. The manager's tables — 255
//! handles with their names and saved maps, and each page's owner — live in
//! the board's last page, which no register can show, so they cost no
//! conventional memory and no program can reach them. The mapping is the
//! board's registers, read and written through their ports. A move or an
//! exchange copies at once and then lets the time it takes pass with
//! interrupts served. A map-and-call leaves on the caller's stack what
//! restoring the old map needs, and the called routine's far return lands in
//! the ROM, which restores it.

use super::guest::{Guest, Poll};
use super::layout::{entry, table, SEGMENT};
use crate::asm::{Reg16, Reg8};
use crate::cpu::Seg;
use crate::ems::{fits, page_of, register, BASES, FRAMES, MAX_PAGES, PAGE, WINDOWS};
use crate::time::Nanos;
use alloc::vec::Vec;

/// INT 67h's state on its stub's stack: when a move's time ends.
pub const STATE: u8 = 8;
/// The handles the manager keeps, the operating system's handle 0 among
/// them.
const HANDLES: u16 = 255;
/// How long a move takes a byte: an XT's string move.
const MOVE_BYTE: Nanos = 1_000;
/// The largest region a move or exchange takes.
const MOVE_LIMIT: u32 = 0x10_0000;
/// A page in bytes, and in paragraphs.
const PAGE_BYTES: usize = PAGE as usize;
const PAGE_PARAGRAPHS: u16 = 0x400;
/// A saved page map: the four registers, then a check of them.
const CONTEXT: usize = 2 * WINDOWS + 2;
/// What a saved map's check starts from, so zeros do not check.
const SEAL: u16 = 0x4C34;
/// What a map-and-call leaves beneath the caller's frame: the routine's
/// return frame, its far return into the ROM, and what restoring the old
/// map needs.
const CALL_FRAME: u16 = 6;
const CALL_RETURN: u16 = 4;
const CALL_RESTORE: u16 = 8;

mod status {
    pub const OK: u8 = 0x00;
    pub const SOFTWARE: u8 = 0x80;
    pub const HANDLE: u8 = 0x83;
    pub const FUNCTION: u8 = 0x84;
    pub const NO_HANDLES: u8 = 0x85;
    pub const SAVED: u8 = 0x86;
    pub const TOO_MANY: u8 = 0x87;
    pub const TOO_FEW: u8 = 0x88;
    pub const ZERO: u8 = 0x89;
    pub const LOGICAL: u8 = 0x8A;
    pub const PHYSICAL: u8 = 0x8B;
    pub const ALREADY_SAVED: u8 = 0x8D;
    pub const NOT_SAVED: u8 = 0x8E;
    pub const SUBFUNCTION: u8 = 0x8F;
    pub const ATTRIBUTE: u8 = 0x90;
    pub const UNSUPPORTED: u8 = 0x91;
    pub const OVERLAPPED: u8 = 0x92;
    pub const LENGTH: u8 = 0x93;
    pub const ALIASED: u8 = 0x94;
    pub const OFFSET: u8 = 0x95;
    pub const TOO_LONG: u8 = 0x96;
    pub const EXCHANGE_OVERLAP: u8 = 0x97;
    pub const MEMORY_TYPE: u8 = 0x98;
    pub const NO_SETS: u8 = 0x9C;
    pub const NOT_FOUND: u8 = 0xA0;
    pub const NAMED: u8 = 0xA1;
    pub const WRAPS: u8 = 0xA2;
    pub const CORRUPT: u8 = 0xA3;
    pub const DENIED: u8 = 0xA4;
}

/// The tables' layout in the board's last page.
mod at {
    pub const MAGIC: usize = 0;
    /// The operating system's function set: open with no key yet, open,
    /// or closed.
    pub const OS: usize = 4;
    pub const KEY: usize = 6;
    /// Alternate set 0's context save area: offset, then segment.
    pub const CONTEXT: usize = 10;
    pub const HANDLES: usize = 16;
    pub const HANDLE: usize = 24;
    pub const OWNERS: usize = HANDLES + 255 * HANDLE;
    pub const OWNER: usize = 4;
    /// A handle entry's fields.
    pub const FLAGS: usize = 0;
    pub const PAGES: usize = 2;
    pub const NAME: usize = 4;
    pub const SAVED: usize = 12;
}

const MAGIC: [u8; 4] = *b"LIM4";
const ALLOCATED: u8 = 0x01;
const MAP_SAVED: u8 = 0x02;
/// An owner entry's handle byte for a free page.
const FREE: u8 = 0xFF;

/// The operating system's function set.
mod os {
    pub const OPEN_UNKEYED: u8 = 0;
    pub const OPEN: u8 = 1;
    pub const CLOSED: u8 = 2;
}

/// The board, as the ROM records it.
#[derive(Clone, Copy, Debug)]
struct Board {
    port: u16,
    frame: u16,
    pages: u16,
}

impl Board {
    fn window_segment(self, w: usize) -> u16 {
        let w = u16::try_from(w).unwrap_or(0);
        self.frame + w * PAGE_PARAGRAPHS
    }

    fn window_at(self, segment: u16) -> Option<usize> {
        (0..WINDOWS).find(|&w| self.window_segment(w) == segment)
    }

    fn register_port(self, w: usize) -> u16 {
        self.port + 2 * u16::try_from(w).unwrap_or(0)
    }
}

/// The board the ROM records, if it records one a board can be.
fn board(g: &mut Guest<'_>) -> Option<Board> {
    let board = Board {
        port: g.read16(SEGMENT, table::EMS),
        frame: g.read16(SEGMENT, table::EMS + 2),
        pages: g.read16(SEGMENT, table::EMS + 4),
    };
    let frame = u32::from(board.frame) << 4;
    (BASES.contains(&board.port)
        && FRAMES.contains(&frame)
        && (1..=MAX_PAGES).contains(&board.pages))
    .then_some(board)
}

/// 512 pages' worth of one bit each.
type PageSet = [u64; 8];

fn contains(set: &PageSet, page: u16) -> bool {
    set.get(usize::from(page / 64))
        .is_some_and(|word| word >> (page % 64) & 1 != 0)
}

/// The manager's tables.
struct Tables<'a> {
    bytes: &'a mut [u8],
    pages: u16,
}

impl Tables<'_> {
    fn byte(&self, offset: usize) -> u8 {
        self.bytes.get(offset).copied().unwrap_or(0)
    }

    fn set_byte(&mut self, offset: usize, value: u8) {
        if let Some(byte) = self.bytes.get_mut(offset) {
            *byte = value;
        }
    }

    fn word(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.byte(offset), self.byte(offset + 1)])
    }

    fn set_word(&mut self, offset: usize, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.set_byte(offset, low);
        self.set_byte(offset + 1, high);
    }

    fn entry(handle: u8) -> usize {
        at::HANDLES + usize::from(handle) * at::HANDLE
    }

    fn allocated(&self, handle: u8) -> bool {
        u16::from(handle) < HANDLES && self.byte(Self::entry(handle)) & ALLOCATED != 0
    }

    /// The allocated handle DX names.
    fn handle(&self, dx: u16) -> Result<u8, u8> {
        u8::try_from(dx)
            .ok()
            .filter(|&handle| self.allocated(handle))
            .ok_or(status::HANDLE)
    }

    fn pages_of(&self, handle: u8) -> u16 {
        self.word(Self::entry(handle) + at::PAGES)
    }

    fn owner_at(page: u16) -> usize {
        at::OWNERS + usize::from(page) * at::OWNER
    }

    fn owner(&self, page: u16) -> Option<(u8, u16)> {
        let at = Self::owner_at(page);
        let handle = self.byte(at);
        (handle != FREE).then(|| (handle, self.word(at + 2)))
    }

    fn set_owner(&mut self, page: u16, owner: Option<(u8, u16)>) {
        let at = Self::owner_at(page);
        let (handle, logical) = owner.unwrap_or((FREE, 0));
        self.set_byte(at, handle);
        self.set_word(at + 2, logical);
    }

    fn free(&self) -> u16 {
        let free = (0..self.pages).filter(|&p| self.owner(p).is_none()).count();
        u16::try_from(free).unwrap_or(0)
    }

    /// The board page holding `handle`'s logical page `logical`.
    fn physical(&self, handle: u8, logical: u16) -> Result<u16, u8> {
        if logical >= self.pages_of(handle) {
            return Err(status::LOGICAL);
        }
        (0..self.pages)
            .find(|&p| self.owner(p) == Some((handle, logical)))
            .ok_or(status::SOFTWARE)
    }

    fn active(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=254).filter(|&handle| self.allocated(handle))
    }

    fn name(&self, handle: u8) -> [u8; 8] {
        let at = Self::entry(handle) + at::NAME;
        core::array::from_fn(|i| self.byte(at + i))
    }

    fn set_name(&mut self, handle: u8, name: [u8; 8]) {
        let at = Self::entry(handle) + at::NAME;
        for (i, byte) in name.into_iter().enumerate() {
            self.set_byte(at + i, byte);
        }
    }

    /// Every table empty but handle 0, the operating system's, with no
    /// pages.
    fn initialise(&mut self) {
        self.bytes.fill(0);
        for (i, byte) in MAGIC.into_iter().enumerate() {
            self.set_byte(at::MAGIC + i, byte);
        }
        self.set_byte(Self::entry(0), ALLOCATED);
        for page in 0..self.pages {
            self.set_owner(page, None);
        }
    }

    /// Gives `handle` logical pages `from..to` from the free pages; the
    /// caller has made sure there are enough.
    fn grow(&mut self, handle: u8, from: u16, to: u16) {
        let mut logical = from;
        for page in 0..self.pages {
            if logical >= to {
                break;
            }
            if self.owner(page).is_none() {
                self.set_owner(page, Some((handle, logical)));
                logical += 1;
            }
        }
        self.set_word(Self::entry(handle) + at::PAGES, logical);
    }

    /// Takes back `handle`'s logical pages from `to` on; returns the board
    /// pages freed.
    fn shrink(&mut self, handle: u8, to: u16) -> PageSet {
        let mut freed = [0; 8];
        for page in 0..self.pages {
            if let Some((owner, logical)) = self.owner(page) {
                if owner == handle && logical >= to {
                    self.set_owner(page, None);
                    if let Some(word) = freed.get_mut(usize::from(page / 64)) {
                        *word |= 1u64 << (page % 64);
                    }
                }
            }
        }
        let pages = self.pages_of(handle).min(to);
        self.set_word(Self::entry(handle) + at::PAGES, pages);
        freed
    }

    /// A new handle with `count` pages.
    fn allocate(&mut self, count: u16) -> Result<u8, u8> {
        if count > self.pages {
            return Err(status::TOO_MANY);
        }
        if count > self.free() {
            return Err(status::TOO_FEW);
        }
        let handle = (1..=254)
            .find(|&handle| !self.allocated(handle))
            .ok_or(status::NO_HANDLES)?;
        let entry = Self::entry(handle);
        self.set_byte(entry + at::FLAGS, ALLOCATED);
        self.set_word(entry + at::PAGES, 0);
        self.set_name(handle, [0; 8]);
        self.grow(handle, 0, count);
        Ok(handle)
    }
}

/// Runs `f` over the tables, if the board has them and they are intact.
fn with_tables<R>(
    g: &mut Guest<'_>,
    board: Board,
    f: impl FnOnce(&mut Tables<'_>) -> R,
) -> Option<R> {
    let store = g.machine().board_mut().expanded_store()?;
    let start = usize::from(board.pages) * PAGE_BYTES;
    let bytes = store.get_mut(start..start + PAGE_BYTES)?;
    let mut tables = Tables {
        bytes,
        pages: board.pages,
    };
    (tables.bytes.get(..MAGIC.len()) == Some(&MAGIC[..])).then(|| f(&mut tables))
}

/// As [`with_tables`], a missing or broken table being the manager's own
/// malfunction.
fn tables<R>(
    g: &mut Guest<'_>,
    board: Board,
    f: impl FnOnce(&mut Tables<'_>) -> Result<R, u8>,
) -> Result<R, u8> {
    with_tables(g, board, f).unwrap_or(Err(status::SOFTWARE))
}

/// The POST's part: the tables set up, every window empty, and INT 67h
/// pointed at the manager, if the machine has a board.
pub fn install(g: &mut Guest<'_>) {
    let Some(board) = board(g) else {
        return;
    };
    let store = g.machine().board_mut().expanded_store();
    let start = usize::from(board.pages) * PAGE_BYTES;
    if let Some(bytes) = store.and_then(|store| store.get_mut(start..start + PAGE_BYTES)) {
        Tables {
            bytes,
            pages: board.pages,
        }
        .initialise();
    }
    for w in 0..WINDOWS {
        show(g, board, w, None);
    }
    g.write16(0, 0x67 * 4, entry::EMS);
    g.write16(0, 0x67 * 4 + 2, SEGMENT);
}

fn read_register(g: &mut Guest<'_>, board: Board, w: usize) -> [u8; 2] {
    let port = board.register_port(w);
    [g.input(port), g.input(port + 1)]
}

fn write_register(g: &mut Guest<'_>, board: Board, w: usize, value: [u8; 2]) {
    let port = board.register_port(w);
    g.output(port, value[0]);
    g.output(port + 1, value[1]);
}

fn show(g: &mut Guest<'_>, board: Board, w: usize, page: Option<u16>) {
    write_register(g, board, w, register(page));
}

/// Empties every window showing one of `freed`.
fn hide(g: &mut Guest<'_>, board: Board, freed: &PageSet) {
    for w in 0..WINDOWS {
        let shown = page_of(read_register(g, board, w), board.pages);
        if shown.is_some_and(|page| contains(freed, page)) {
            show(g, board, w, None);
        }
    }
}

/// INT 67h.
pub fn service(g: &mut Guest<'_>) {
    set_deadline(g, 0);
    let status = match board(g) {
        Some(board) => function(g, board).err().unwrap_or(status::OK),
        None => status::FUNCTION,
    };
    g.set_r8(Reg8::Ah, status);
    finish(g);
}

/// After an interrupt while a move's time passes.
pub fn step(g: &mut Guest<'_>) {
    finish(g);
}

/// After a map-and-call's routine returns: the old map, then back to the
/// caller with its status.
pub fn returned(g: &mut Guest<'_>) {
    let (ss, sp) = (g.seg(Seg::Ss), g.r16(Reg16::Sp));
    let word = |g: &mut Guest<'_>, n: u16| g.read16(ss, sp.wrapping_add(n));
    let handle = word(g, 0);
    let [mode, count] = word(g, 2).to_le_bytes();
    let (offset, segment) = (word(g, 4), word(g, 6));
    g.set_r16(Reg16::Sp, sp.wrapping_add(CALL_RESTORE));
    let status = match board(g) {
        Some(board) => tables(g, board, |t| t.handle(handle))
            .and_then(|handle| map_list(g, board, handle, mode, u16::from(count), segment, offset))
            .err()
            .unwrap_or(status::OK),
        None => status::FUNCTION,
    };
    g.set_r8(Reg8::Ah, status);
}

fn deadline(g: &mut Guest<'_>) -> Nanos {
    Nanos::from_le_bytes(core::array::from_fn(|n| {
        g.local(u16::try_from(n).unwrap_or(0))
    }))
}

fn set_deadline(g: &mut Guest<'_>, deadline: Nanos) {
    for (n, byte) in (0..).zip(deadline.to_le_bytes()) {
        g.set_local(n, byte);
    }
}

/// Lets a move's time pass, serving interrupts, then returns.
fn finish(g: &mut Guest<'_>) {
    let deadline = deadline(g);
    match g.poll(deadline, |_| false) {
        Poll::Interrupted => g.done(false),
        Poll::Yielded => g.resume(),
        Poll::Ready | Poll::Expired => g.done(true),
    }
}

fn function(g: &mut Guest<'_>, board: Board) -> Result<(), u8> {
    let (ah, al) = (g.r8(Reg8::Ah), g.r8(Reg8::Al));
    let (bx, dx) = (g.r16(Reg16::Bx), g.r16(Reg16::Dx));
    match ah {
        0x40 => tables(g, board, |_| Ok(())),
        0x41 => {
            g.set_r16(Reg16::Bx, board.frame);
            Ok(())
        }
        0x42 => {
            let free = tables(g, board, |t| Ok(t.free()))?;
            g.set_r16(Reg16::Bx, free);
            g.set_r16(Reg16::Dx, board.pages);
            Ok(())
        }
        0x43 | 0x5A => {
            if ah == 0x43 && bx == 0 {
                return Err(status::ZERO);
            }
            if ah == 0x5A && al > 1 {
                return Err(status::SUBFUNCTION);
            }
            let handle = tables(g, board, |t| t.allocate(bx))?;
            g.set_r16(Reg16::Dx, u16::from(handle));
            Ok(())
        }
        0x44 => {
            let handle = tables(g, board, |t| t.handle(dx))?;
            map_page(g, board, handle, bx, usize::from(al))
        }
        0x45 => deallocate(g, board, dx),
        0x46 => {
            g.set_r8(Reg8::Al, 0x40);
            Ok(())
        }
        0x47 => save_map(g, board, dx),
        0x48 => restore_map(g, board, dx),
        0x4B => {
            let count = tables(g, board, |t| Ok(t.active().count()))?;
            g.set_r16(Reg16::Bx, u16::try_from(count).unwrap_or(0));
            Ok(())
        }
        0x4C => {
            let pages = tables(g, board, |t| t.handle(dx).map(|handle| t.pages_of(handle)))?;
            g.set_r16(Reg16::Bx, pages);
            Ok(())
        }
        0x4D => all_handle_pages(g, board),
        0x4E => page_map(g, board, al),
        0x4F => partial_map(g, board, al),
        0x50 => {
            if al > 1 {
                return Err(status::SUBFUNCTION);
            }
            let handle = tables(g, board, |t| t.handle(dx))?;
            let (count, segment, offset) = (g.r16(Reg16::Cx), g.seg(Seg::Ds), g.r16(Reg16::Si));
            map_list(g, board, handle, al, count, segment, offset)
        }
        0x51 => reallocate(g, board, dx, bx),
        0x52 => attribute(g, board, al, dx),
        0x53 => handle_name(g, board, al, dx),
        0x54 => directory(g, board, al),
        0x55 => map_and_jump(g, board, al, dx),
        0x56 => map_and_call(g, board, al, dx),
        0x57 => move_region(g, board, al),
        0x58 => mappable(g, board, al),
        0x59 => hardware(g, board, al),
        0x5B => alternate(g, board, al),
        0x5C => {
            for w in 0..WINDOWS {
                show(g, board, w, None);
            }
            Ok(())
        }
        0x5D => os_functions(g, board, al),
        _ => Err(status::FUNCTION),
    }
}

/// Shows `handle`'s logical page `logical` in window `w`, or empties it
/// for `FFFFh`.
fn map_page(g: &mut Guest<'_>, board: Board, handle: u8, logical: u16, w: usize) -> Result<(), u8> {
    if w >= WINDOWS {
        return Err(status::PHYSICAL);
    }
    let page = if logical == 0xFFFF {
        None
    } else {
        Some(tables(g, board, |t| t.physical(handle, logical))?)
    };
    show(g, board, w, page);
    Ok(())
}

/// Maps the `count` entries at `segment:offset` — a logical page and a
/// window, by number (`mode` 0) or by segment (1) — stopping at the first
/// that fails, as LIM leaves the ones before it mapped.
fn map_list(
    g: &mut Guest<'_>,
    board: Board,
    handle: u8,
    mode: u8,
    count: u16,
    segment: u16,
    offset: u16,
) -> Result<(), u8> {
    if mode > 1 {
        return Err(status::SUBFUNCTION);
    }
    if usize::from(count) > WINDOWS {
        return Err(status::PHYSICAL);
    }
    for n in 0..count {
        let at = offset.wrapping_add(4 * n);
        let logical = g.read16(segment, at);
        let target = g.read16(segment, at.wrapping_add(2));
        let w = if mode == 0 {
            usize::from(target)
        } else {
            board.window_at(target).ok_or(status::PHYSICAL)?
        };
        map_page(g, board, handle, logical, w)?;
    }
    Ok(())
}

fn deallocate(g: &mut Guest<'_>, board: Board, dx: u16) -> Result<(), u8> {
    let freed = tables(g, board, |t| {
        let handle = t.handle(dx)?;
        let entry = Tables::entry(handle);
        if t.byte(entry + at::FLAGS) & MAP_SAVED != 0 {
            return Err(status::SAVED);
        }
        let freed = t.shrink(handle, 0);
        t.set_name(handle, [0; 8]);
        // The operating system's handle stays, with no pages.
        if handle != 0 {
            t.set_byte(entry + at::FLAGS, 0);
        }
        Ok(freed)
    })?;
    hide(g, board, &freed);
    Ok(())
}

fn reallocate(g: &mut Guest<'_>, board: Board, dx: u16, bx: u16) -> Result<(), u8> {
    let outcome = tables(g, board, |t| {
        let handle = t.handle(dx)?;
        let held = t.pages_of(handle);
        let freed = if bx > held {
            if bx > t.pages {
                return Ok(Err((status::TOO_MANY, held)));
            }
            if bx - held > t.free() {
                return Ok(Err((status::TOO_FEW, held)));
            }
            t.grow(handle, held, bx);
            [0; 8]
        } else {
            t.shrink(handle, bx)
        };
        Ok(Ok(freed))
    })?;
    match outcome {
        Ok(freed) => {
            hide(g, board, &freed);
            g.set_r16(Reg16::Bx, bx);
            Ok(())
        }
        Err((status, held)) => {
            g.set_r16(Reg16::Bx, held);
            Err(status)
        }
    }
}

/// The four registers as a saved map holds them.
fn registers(g: &mut Guest<'_>, board: Board) -> [u8; 2 * WINDOWS] {
    let mut saved = [0; 2 * WINDOWS];
    for (w, pair) in saved.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        *pair = read_register(g, board, w);
    }
    saved
}

/// Writes saved registers back, every one checked first.
fn set_registers(g: &mut Guest<'_>, board: Board, saved: [u8; 2 * WINDOWS]) -> Result<(), u8> {
    let pairs = saved.as_chunks::<2>().0;
    if !pairs.iter().all(|&pair| fits(pair)) {
        return Err(status::CORRUPT);
    }
    for (w, &pair) in pairs.iter().enumerate() {
        write_register(g, board, w, pair);
    }
    Ok(())
}

/// The registers' words, for a saved map's check.
fn register_words(saved: &[u8; 2 * WINDOWS]) -> impl Iterator<Item = u16> + '_ {
    saved
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
}

fn save_map(g: &mut Guest<'_>, board: Board, dx: u16) -> Result<(), u8> {
    let saved = registers(g, board);
    tables(g, board, |t| {
        let handle = t.handle(dx)?;
        let entry = Tables::entry(handle);
        if t.byte(entry + at::FLAGS) & MAP_SAVED != 0 {
            return Err(status::ALREADY_SAVED);
        }
        for (i, byte) in saved.into_iter().enumerate() {
            t.set_byte(entry + at::SAVED + i, byte);
        }
        t.set_byte(entry + at::FLAGS, ALLOCATED | MAP_SAVED);
        Ok(())
    })
}

fn restore_map(g: &mut Guest<'_>, board: Board, dx: u16) -> Result<(), u8> {
    let saved = tables(g, board, |t| {
        let handle = t.handle(dx)?;
        let entry = Tables::entry(handle);
        if t.byte(entry + at::FLAGS) & MAP_SAVED == 0 {
            return Err(status::NOT_SAVED);
        }
        t.set_byte(entry + at::FLAGS, ALLOCATED);
        Ok(core::array::from_fn(|i| t.byte(entry + at::SAVED + i)))
    })?;
    set_registers(g, board, saved).map_err(|_| status::SOFTWARE)
}

fn all_handle_pages(g: &mut Guest<'_>, board: Board) -> Result<(), u8> {
    let list: Vec<(u8, u16)> = tables(g, board, |t| {
        Ok(t.active()
            .map(|handle| (handle, t.pages_of(handle)))
            .collect())
    })?;
    let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
    for (n, &(handle, pages)) in (0u16..).zip(&list) {
        g.write16(es, di.wrapping_add(4 * n), u16::from(handle));
        g.write16(es, di.wrapping_add(4 * n + 2), pages);
    }
    g.set_r16(Reg16::Bx, u16::try_from(list.len()).unwrap_or(0));
    Ok(())
}

/// The check a saved map carries.
fn seal(words: impl Iterator<Item = u16>) -> u16 {
    words.fold(SEAL, |sum, word| sum.rotate_left(3) ^ word)
}

fn page_map(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    match al {
        0x00 => {
            store_context(g, board);
            Ok(())
        }
        0x01 => load_context(g, board, g.seg(Seg::Ds), g.r16(Reg16::Si)),
        0x02 => {
            let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
            let incoming = read_context(g, ds, si);
            store_context(g, board);
            apply_context(g, board, &incoming)
        }
        0x03 => {
            g.set_r8(Reg8::Al, u8::try_from(CONTEXT).unwrap_or(0));
            Ok(())
        }
        _ => Err(status::SUBFUNCTION),
    }
}

/// The registers and their check, at ES:DI.
fn store_context(g: &mut Guest<'_>, board: Board) {
    let saved = registers(g, board);
    let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
    for (n, &byte) in (0u16..).zip(&saved) {
        g.write8(es, di.wrapping_add(n), byte);
    }
    let check = seal(register_words(&saved));
    g.write16(es, di.wrapping_add(8), check);
}

fn read_context(g: &mut Guest<'_>, segment: u16, offset: u16) -> [u8; CONTEXT] {
    core::array::from_fn(|n| g.read8(segment, offset.wrapping_add(u16::try_from(n).unwrap_or(0))))
}

/// Loads a saved map from `segment:offset`, if its check holds.
fn load_context(g: &mut Guest<'_>, board: Board, segment: u16, offset: u16) -> Result<(), u8> {
    let incoming = read_context(g, segment, offset);
    apply_context(g, board, &incoming)
}

fn apply_context(g: &mut Guest<'_>, board: Board, context: &[u8; CONTEXT]) -> Result<(), u8> {
    let saved: [u8; 2 * WINDOWS] = core::array::from_fn(|i| context[i]);
    let check = u16::from_le_bytes([context[8], context[9]]);
    if seal(register_words(&saved)) != check {
        return Err(status::CORRUPT);
    }
    set_registers(g, board, saved)
}

fn partial_map(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    match al {
        0x00 => {
            let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
            let count = g.read16(ds, si);
            if usize::from(count) > WINDOWS {
                return Err(status::CORRUPT);
            }
            let mut entries = Vec::new();
            for n in 0..count {
                let segment = g.read16(ds, si.wrapping_add(2 + 2 * n));
                let w = board.window_at(segment).ok_or(status::PHYSICAL)?;
                entries.push((segment, read_register(g, board, w)));
            }
            let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
            let mut words = Vec::new();
            words.push(count);
            for (segment, pair) in entries {
                words.push(segment);
                words.push(u16::from_le_bytes(pair));
            }
            let check = seal(words.iter().copied());
            words.push(check);
            for (n, &word) in (0u16..).zip(&words) {
                g.write16(es, di.wrapping_add(2 * n), word);
            }
            Ok(())
        }
        0x01 => {
            let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
            let count = g.read16(ds, si);
            if usize::from(count) > WINDOWS {
                return Err(status::CORRUPT);
            }
            let words: Vec<u16> = (0..=2 * count)
                .map(|n| g.read16(ds, si.wrapping_add(2 * n)))
                .collect();
            let check = g.read16(ds, si.wrapping_add(2 + 4 * count));
            if seal(words.iter().copied()) != check {
                return Err(status::CORRUPT);
            }
            let mut entries = Vec::new();
            for &[segment, value] in words[1..].as_chunks::<2>().0 {
                let w = board.window_at(segment).ok_or(status::CORRUPT)?;
                let value = value.to_le_bytes();
                if !fits(value) {
                    return Err(status::CORRUPT);
                }
                entries.push((w, value));
            }
            for (w, value) in entries {
                write_register(g, board, w, value);
            }
            Ok(())
        }
        0x02 => {
            let pages = g.r16(Reg16::Bx);
            if usize::from(pages) > WINDOWS {
                return Err(status::PHYSICAL);
            }
            g.set_r8(Reg8::Al, u8::try_from(4 + 4 * pages).unwrap_or(0));
            Ok(())
        }
        _ => Err(status::SUBFUNCTION),
    }
}

fn attribute(g: &mut Guest<'_>, board: Board, al: u8, dx: u16) -> Result<(), u8> {
    match al {
        0x00 => {
            tables(g, board, |t| t.handle(dx))?;
            g.set_r8(Reg8::Al, 0);
            Ok(())
        }
        0x01 => {
            tables(g, board, |t| t.handle(dx))?;
            match g.r8(Reg8::Bl) {
                0 => Ok(()),
                1 => Err(status::UNSUPPORTED),
                _ => Err(status::ATTRIBUTE),
            }
        }
        0x02 => {
            g.set_r8(Reg8::Al, 0);
            Ok(())
        }
        _ => Err(status::SUBFUNCTION),
    }
}

fn read_name(g: &mut Guest<'_>, segment: u16, offset: u16) -> [u8; 8] {
    core::array::from_fn(|n| g.read8(segment, offset.wrapping_add(u16::try_from(n).unwrap_or(0))))
}

fn handle_name(g: &mut Guest<'_>, board: Board, al: u8, dx: u16) -> Result<(), u8> {
    match al {
        0x00 => {
            let name = tables(g, board, |t| t.handle(dx).map(|handle| t.name(handle)))?;
            let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
            for (n, byte) in (0u16..).zip(name) {
                g.write8(es, di.wrapping_add(n), byte);
            }
            Ok(())
        }
        0x01 => {
            let name = read_name(g, g.seg(Seg::Ds), g.r16(Reg16::Si));
            tables(g, board, |t| {
                let handle = t.handle(dx)?;
                let taken = name != [0; 8]
                    && t.active()
                        .any(|other| other != handle && t.name(other) == name);
                if taken {
                    return Err(status::NAMED);
                }
                t.set_name(handle, name);
                Ok(())
            })
        }
        _ => Err(status::SUBFUNCTION),
    }
}

fn directory(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    match al {
        0x00 => {
            let list: Vec<(u8, [u8; 8])> = tables(g, board, |t| {
                Ok(t.active().map(|handle| (handle, t.name(handle))).collect())
            })?;
            let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
            for (n, &(handle, name)) in (0u16..).zip(&list) {
                let at = di.wrapping_add(10 * n);
                g.write16(es, at, u16::from(handle));
                for (i, byte) in (0u16..).zip(name) {
                    g.write8(es, at.wrapping_add(2 + i), byte);
                }
            }
            g.set_r8(Reg8::Al, u8::try_from(list.len()).unwrap_or(0));
            Ok(())
        }
        0x01 => {
            let name = read_name(g, g.seg(Seg::Ds), g.r16(Reg16::Si));
            if name == [0; 8] {
                return Err(status::NAMED);
            }
            let handle = tables(g, board, |t| {
                t.active()
                    .find(|&handle| t.name(handle) == name)
                    .ok_or(status::NOT_FOUND)
            })?;
            g.set_r16(Reg16::Dx, u16::from(handle));
            Ok(())
        }
        0x02 => {
            g.set_r16(Reg16::Bx, HANDLES);
            Ok(())
        }
        _ => Err(status::SUBFUNCTION),
    }
}

/// Maps the list a map-and-jump or map-and-call structure names at
/// `at` within it.
fn map_from(g: &mut Guest<'_>, board: Board, handle: u8, mode: u8, at: u16) -> Result<(), u8> {
    let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
    let count = g.read8(ds, si.wrapping_add(at));
    let offset = g.read16(ds, si.wrapping_add(at + 1));
    let segment = g.read16(ds, si.wrapping_add(at + 3));
    map_list(g, board, handle, mode, u16::from(count), segment, offset)
}

/// The caller's frame on the stub's stack: its IP, CS and flags.
fn frame(g: &Guest<'_>) -> (u16, u16) {
    (
        g.seg(Seg::Ss),
        g.r16(Reg16::Sp).wrapping_add(u16::from(STATE)),
    )
}

fn map_and_jump(g: &mut Guest<'_>, board: Board, al: u8, dx: u16) -> Result<(), u8> {
    if al > 1 {
        return Err(status::SUBFUNCTION);
    }
    let handle = tables(g, board, |t| t.handle(dx))?;
    map_from(g, board, handle, al, 4)?;
    let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
    let (target_ip, target_cs) = (g.read16(ds, si), g.read16(ds, si.wrapping_add(2)));
    let (ss, at) = frame(g);
    g.write16(ss, at, target_ip);
    g.write16(ss, at.wrapping_add(2), target_cs);
    Ok(())
}

fn map_and_call(g: &mut Guest<'_>, board: Board, al: u8, dx: u16) -> Result<(), u8> {
    if al == 0x02 {
        let peak = u16::from(STATE) + CALL_FRAME + CALL_RETURN + CALL_RESTORE + CALL_FRAME;
        g.set_r16(Reg16::Bx, peak);
        return Ok(());
    }
    if al > 1 {
        return Err(status::SUBFUNCTION);
    }
    let handle = tables(g, board, |t| t.handle(dx))?;
    map_from(g, board, handle, al, 4)?;
    let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
    let (target_ip, target_cs) = (g.read16(ds, si), g.read16(ds, si.wrapping_add(2)));
    let old_count = g.read8(ds, si.wrapping_add(9));
    let old_offset = g.read16(ds, si.wrapping_add(10));
    let old_segment = g.read16(ds, si.wrapping_add(12));
    let (ss, caller) = frame(g);
    let flags = g.read16(ss, caller.wrapping_add(4));
    // Beneath the caller's frame: the stub's state, the routine's frame,
    // its far return into the ROM, then what restoring the old map needs.
    let below = CALL_FRAME + CALL_RETURN + CALL_RESTORE;
    let sp = g.r16(Reg16::Sp).wrapping_sub(below);
    let routine = sp.wrapping_add(u16::from(STATE));
    let words = [
        target_ip,
        target_cs,
        flags,
        entry::EMS_RETURN,
        SEGMENT,
        u16::from(handle),
        u16::from_le_bytes([al, old_count]),
        old_offset,
        old_segment,
    ];
    for (n, word) in (0u16..).zip(words) {
        g.write16(ss, routine.wrapping_add(2 * n), word);
    }
    g.set_r16(Reg16::Sp, sp);
    set_deadline(g, 0);
    Ok(())
}

/// One side of a move or exchange.
#[derive(Clone, Copy)]
enum Region {
    /// From a linear address of the first megabyte.
    Conventional(u32),
    /// From byte `start` of a handle's pages taken end to end.
    Expanded { handle: u8, start: u32 },
}

impl Region {
    fn overlaps(self, other: Self, len: u32) -> bool {
        let (a, b) = match (self, other) {
            (Self::Conventional(a), Self::Conventional(b)) => (a, b),
            (
                Self::Expanded { handle, start: a },
                Self::Expanded {
                    handle: h,
                    start: b,
                },
            ) if handle == h => (a, b),
            _ => return false,
        };
        a < b + len && b < a + len
    }
}

fn region(g: &mut Guest<'_>, board: Board, at: u16, len: u32) -> Result<Region, u8> {
    let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
    let kind = g.read8(ds, si.wrapping_add(at));
    let handle = g.read16(ds, si.wrapping_add(at + 1));
    let offset = g.read16(ds, si.wrapping_add(at + 3));
    let base = g.read16(ds, si.wrapping_add(at + 5));
    match kind {
        0 => {
            let start = (u32::from(base) << 4) + u32::from(offset);
            if start + len > MOVE_LIMIT {
                return Err(status::WRAPS);
            }
            Ok(Region::Conventional(start))
        }
        1 => {
            if u32::from(offset) >= PAGE {
                return Err(status::OFFSET);
            }
            tables(g, board, |t| {
                let handle = t.handle(handle)?;
                let pages = u32::from(t.pages_of(handle));
                if u32::from(base) >= pages {
                    return Err(status::LOGICAL);
                }
                let start = u32::from(base) * PAGE + u32::from(offset);
                if start + len > pages * PAGE {
                    return Err(status::LENGTH);
                }
                Ok(Region::Expanded { handle, start })
            })
        }
        _ => Err(status::MEMORY_TYPE),
    }
}

/// Whether a conventional region shows, through a window, a page of an
/// expanded one.
fn aliased(
    g: &mut Guest<'_>,
    board: Board,
    conventional: u32,
    handle: u8,
    start: u32,
    len: u32,
) -> Result<bool, u8> {
    let frame = u32::from(board.frame) << 4;
    let mut shown = [None; WINDOWS];
    for (w, slot) in shown.iter_mut().enumerate() {
        *slot = page_of(read_register(g, board, w), board.pages);
    }
    let first = start / PAGE;
    let last = (start + len - 1) / PAGE;
    tables(g, board, |t| {
        Ok((0u32..).zip(shown).any(|(w, page)| {
            let window = frame + w * PAGE;
            let covers = conventional < window + PAGE && window < conventional + len;
            covers
                && page
                    .and_then(|page| t.owner(page))
                    .is_some_and(|(owner, logical)| {
                        owner == handle && (first..=last).contains(&u32::from(logical))
                    })
        }))
    })
}

/// The store offsets of a handle's pages, in logical order.
fn physical_pages(g: &mut Guest<'_>, board: Board, handle: u8) -> Result<Vec<usize>, u8> {
    tables(g, board, |t| {
        (0..t.pages_of(handle))
            .map(|logical| {
                t.physical(handle, logical)
                    .map(|page| usize::from(page) * PAGE_BYTES)
            })
            .collect()
    })
}

/// Reads a region's `len` bytes.
fn read_region(g: &mut Guest<'_>, board: Board, region: Region, len: usize) -> Result<Vec<u8>, u8> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| status::SOFTWARE)?;
    match region {
        Region::Conventional(start) => {
            for at in (start..).take(len) {
                bytes.push(g.read8(segment_of(at), offset_of(at)));
            }
        }
        Region::Expanded { handle, start } => {
            let pages = physical_pages(g, board, handle)?;
            let store = g
                .machine()
                .board_mut()
                .expanded_store()
                .ok_or(status::SOFTWARE)?;
            for at in (start..).take(len) {
                bytes.push(
                    expanded_byte(store, &pages, at)
                        .copied()
                        .ok_or(status::SOFTWARE)?,
                );
            }
        }
    }
    Ok(bytes)
}

fn write_region(g: &mut Guest<'_>, board: Board, region: Region, bytes: &[u8]) -> Result<(), u8> {
    match region {
        Region::Conventional(start) => {
            for (at, &byte) in (start..).zip(bytes) {
                g.write8(segment_of(at), offset_of(at), byte);
            }
        }
        Region::Expanded { handle, start } => {
            let pages = physical_pages(g, board, handle)?;
            let store = g
                .machine()
                .board_mut()
                .expanded_store()
                .ok_or(status::SOFTWARE)?;
            for (at, &byte) in (start..).zip(bytes) {
                *expanded_byte_mut(store, &pages, at).ok_or(status::SOFTWARE)? = byte;
            }
        }
    }
    Ok(())
}

fn segment_of(linear: u32) -> u16 {
    u16::try_from(linear >> 4).unwrap_or(0)
}

fn offset_of(linear: u32) -> u16 {
    u16::try_from(linear & 0xF).unwrap_or(0)
}

fn expanded_index(pages: &[usize], at: u32) -> Option<usize> {
    let page = pages.get(usize::try_from(at / PAGE).ok()?)?;
    Some(page + usize::try_from(at % PAGE).ok()?)
}

fn expanded_byte<'s>(store: &'s [u8], pages: &[usize], at: u32) -> Option<&'s u8> {
    store.get(expanded_index(pages, at)?)
}

fn expanded_byte_mut<'s>(store: &'s mut [u8], pages: &[usize], at: u32) -> Option<&'s mut u8> {
    store.get_mut(expanded_index(pages, at)?)
}

fn move_region(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    if al > 1 {
        return Err(status::SUBFUNCTION);
    }
    let exchange = al == 1;
    let (ds, si) = (g.seg(Seg::Ds), g.r16(Reg16::Si));
    let len = u32::from(g.read16(ds, si)) | u32::from(g.read16(ds, si.wrapping_add(2))) << 16;
    if len > MOVE_LIMIT {
        return Err(status::TOO_LONG);
    }
    if len == 0 {
        return Ok(());
    }
    let source = region(g, board, 4, len)?;
    let destination = region(g, board, 11, len)?;
    for (a, b) in [(source, destination), (destination, source)] {
        if let (Region::Conventional(linear), Region::Expanded { handle, start }) = (a, b) {
            if aliased(g, board, linear, handle, start, len)? {
                return Err(status::ALIASED);
            }
        }
    }
    let overlap = source.overlaps(destination, len);
    if exchange && overlap {
        return Err(status::EXCHANGE_OVERLAP);
    }
    let bytes = usize::try_from(len).map_err(|_| status::SOFTWARE)?;
    let moved = read_region(g, board, source, bytes)?;
    if exchange {
        let other = read_region(g, board, destination, bytes)?;
        write_region(g, board, source, &other)?;
    }
    write_region(g, board, destination, &moved)?;
    let copies = if exchange { 2 } else { 1 };
    let deadline = g.now() + u64::from(len) * MOVE_BYTE * copies;
    set_deadline(g, deadline);
    if overlap {
        Err(status::OVERLAPPED)
    } else {
        Ok(())
    }
}

fn mappable(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    let count = u16::try_from(WINDOWS).unwrap_or(0);
    match al {
        0x00 => {
            let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
            for (n, w) in (0u16..).zip(0..WINDOWS) {
                g.write16(es, di.wrapping_add(4 * n), board.window_segment(w));
                g.write16(es, di.wrapping_add(4 * n + 2), n);
            }
            g.set_r16(Reg16::Cx, count);
            Ok(())
        }
        0x01 => {
            g.set_r16(Reg16::Cx, count);
            Ok(())
        }
        _ => Err(status::SUBFUNCTION),
    }
}

/// Whether the operating system has closed its function set.
fn os_closed(g: &mut Guest<'_>, board: Board) -> Result<(), u8> {
    let state = tables(g, board, |t| Ok(t.byte(at::OS)))?;
    if state == os::CLOSED {
        Err(status::DENIED)
    } else {
        Ok(())
    }
}

fn hardware(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    match al {
        0x00 => {
            os_closed(g, board)?;
            let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
            let context = u16::try_from(CONTEXT).unwrap_or(0);
            for (n, word) in (0u16..).zip([PAGE_PARAGRAPHS, 0, context, 0, 0]) {
                g.write16(es, di.wrapping_add(2 * n), word);
            }
            Ok(())
        }
        0x01 => {
            let free = tables(g, board, |t| Ok(t.free()))?;
            g.set_r16(Reg16::Bx, free);
            g.set_r16(Reg16::Dx, board.pages);
            Ok(())
        }
        _ => Err(status::SUBFUNCTION),
    }
}

/// The alternate map register sets. The board has only set 0, so a set's
/// state is a context save area the operating system supplies.
fn alternate(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    os_closed(g, board)?;
    let set = g.r8(Reg8::Bl);
    match al {
        0x00 => {
            let (offset, segment) = tables(g, board, |t| {
                Ok((t.word(at::CONTEXT), t.word(at::CONTEXT + 2)))
            })?;
            g.set_r8(Reg8::Bl, 0);
            g.regs_mut().set_sreg(Seg::Es, segment);
            g.set_r16(Reg16::Di, offset);
            Ok(())
        }
        0x01 => {
            if set != 0 {
                return Err(status::NO_SETS);
            }
            let (es, di) = (g.seg(Seg::Es), g.r16(Reg16::Di));
            if (es, di) != (0, 0) {
                load_context(g, board, es, di)?;
            }
            tables(g, board, |t| {
                t.set_word(at::CONTEXT, di);
                t.set_word(at::CONTEXT + 2, es);
                Ok(())
            })
        }
        0x02 => {
            g.set_r16(Reg16::Dx, u16::try_from(CONTEXT).unwrap_or(0));
            Ok(())
        }
        0x03 | 0x05 => {
            g.set_r8(Reg8::Bl, 0);
            Ok(())
        }
        0x04 | 0x06..=0x08 => {
            if set == 0 {
                Ok(())
            } else {
                Err(status::NO_SETS)
            }
        }
        _ => Err(status::SUBFUNCTION),
    }
}

/// The operating system's function set and its access key: the first to
/// open or close it is given the key, and every later caller must hold it.
fn os_functions(g: &mut Guest<'_>, board: Board, al: u8) -> Result<(), u8> {
    if al > 2 {
        return Err(status::SUBFUNCTION);
    }
    let offered = u32::from(g.r16(Reg16::Bx)) | u32::from(g.r16(Reg16::Cx)) << 16;
    let fresh = key(g);
    let issued = tables(g, board, |t| {
        let state = t.byte(at::OS);
        let key = u32::from(t.word(at::KEY)) | u32::from(t.word(at::KEY + 2)) << 16;
        let keyed = state != os::OPEN_UNKEYED;
        if keyed && offered != key || al == 0x02 && !keyed {
            return Err(status::DENIED);
        }
        let next = match al {
            0x00 => os::OPEN,
            0x01 => os::CLOSED,
            _ => os::OPEN_UNKEYED,
        };
        t.set_byte(at::OS, next);
        if keyed || al == 0x02 {
            return Ok(None);
        }
        let low = u16::try_from(fresh & 0xFFFF).unwrap_or(0);
        let high = u16::try_from(fresh >> 16).unwrap_or(0);
        t.set_word(at::KEY, low);
        t.set_word(at::KEY + 2, high);
        Ok(Some((low, high)))
    })?;
    if let Some((low, high)) = issued {
        g.set_r16(Reg16::Bx, low);
        g.set_r16(Reg16::Cx, high);
    }
    Ok(())
}

/// A key no program can foresee: the moment it is asked for, to the
/// nanosecond, with the tick count, mixed.
fn key(g: &mut Guest<'_>) -> u32 {
    let ticks = u64::from(g.bda16(super::bda::TICKS));
    let mixed = tairix_mix::mix64(g.now() ^ (ticks << 48));
    u32::try_from(mixed & 0xFFFF_FFFF).unwrap_or(1).max(1)
}

#[cfg(test)]
#[path = "ems_tests.rs"]
mod tests;
