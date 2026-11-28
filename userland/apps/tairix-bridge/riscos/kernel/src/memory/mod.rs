//! The guest address space (`plans/RISCOS-EMULATOR.md` §3): the MMU under a
//! layout, the parts every session has, task slots and dynamic areas, all
//! within one ceiling.
//!
//! Every session has zero page (at &FFFF0000, and at zero in each 26-bit
//! task), ScratchSpace at &4000, the 32-bit profile's page at zero, the four
//! stacks, and the host pages. Those frames are the kernel's: no area can
//! take them.

mod areas;
pub mod check;
mod guest;
pub mod layout;
mod map;
pub mod policy;
mod slots;

use tairix_collections::TryReserveError;
use tairix_riscos_arm::bus::Bus;
use tairix_riscos_arm::mmu::{
    Access, Frame, FrameKind, Mmu, MmuError, Rights, Space, Table, FRAME_WORDS, PAGE_SIZE,
};

pub use areas::{
    access_privilege, handler_reason, AreaError, AreaFlags, Clamps, Create, Created, Failure,
    Handler, Info, Located, LogicalOp, Own, PageQuery, ParkedArea, PhysicalOp, Read, Shared,
    FONT_CACHE, FREE_POOL, RAM_DISC, RMA, SCREEN, SPRITES, SYSTEM, SYSTEM_HEAP, TITLE,
};
pub(crate) use areas::{CANT_GET_PHYSICAL, PAGE_RESERVED};
pub use guest::{Allowed, Guest, GuestError, PageEntry, Service};
use layout::{Layout, COMPATIBILITY, HIGH_ZERO_PAGE, SCRATCH, ZERO_PAGE_SIZE};
pub use map::{find_access_privilege, privilege_rights, rights_bits, ReserveError, RESERVED};
use policy::Ceiling;
pub use slots::{Bits, Slot};

/// Pages zero page and ScratchSpace each take.
const LOW_PAGES: usize = (ZERO_PAGE_SIZE / PAGE_SIZE) as usize;

/// Zero page and ScratchSpace: User mode reads them.
const WORKSPACE: Access = Access {
    user: Rights::Read,
    privileged: Rights::ReadWrite,
    execute: true,
};
/// The 32-bit profile's page at zero: read, never written or run.
const COMPATIBLE: Access = Access {
    user: Rights::Read,
    privileged: Rights::Read,
    execute: false,
};
/// A stack: the privileged modes' alone.
const STACK: Access = Access {
    user: Rights::None,
    privileged: Rights::ReadWrite,
    execute: true,
};
/// Host code: read and run, never written.
const HOST: Access = Access {
    user: Rights::Read,
    privileged: Rights::Read,
    execute: true,
};
/// The compatibility page's warning, after its copies of the processor
/// vectors, so a string read through null shows it. The rest of the page is
/// zero: the quiescent value of every zero page variable.
const NULL_WARNING: &[u8] =
    b"A program has read through a null pointer: there is no data at address zero.\0";
/// Where the warning starts: past the processor vectors' slots.
const NULL_WARNING_AT: usize = 0x20;

/// Why the address space refused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MemoryError {
    /// The ceiling, or the host, has no more memory to give.
    NoMemory,
    /// A 32-bit slot may not grow over the 26-bit window while 26-bit code
    /// is resident: the session window says so.
    Window,
    /// The slot would pass the most its program may have.
    TooLarge,
    /// The layout admits no 26-bit code.
    Not26Bit,
    /// No such slot, or the current one where another is needed.
    Slot,
    /// The host pages are full.
    HostFull,
}

impl From<MmuError> for MemoryError {
    fn from(error: MmuError) -> Self {
        match error {
            MmuError::Space => Self::Slot,
            _ => Self::NoMemory,
        }
    }
}

impl From<TryReserveError> for MemoryError {
    fn from(_: TryReserveError) -> Self {
        Self::NoMemory
    }
}

impl From<alloc::collections::TryReserveError> for MemoryError {
    fn from(_: alloc::collections::TryReserveError) -> Self {
        Self::NoMemory
    }
}

/// Who holds a page in use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Holder {
    /// The kernel: zero page, ScratchSpace, the stacks, the host pages.
    Kernel,
    /// A task's application space, at a page's index from &8000.
    Slot { slot: u32, index: u32 },
    /// A dynamic area, at a page's index in it: a pool's slot.
    Area { area: areas::Id, index: u32 },
}

/// A physical page's record: who holds it, and whether it is claimed for its
/// holder alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Record {
    holder: Option<Holder>,
    /// Exclusively claimed: no handler or physical op may take it.
    unavailable: bool,
    /// Reserved by `OS_Memory` 23 for its owner's later claim; it outlasts
    /// whoever holds the page meanwhile.
    reserved: bool,
}

const UNHELD: Record = Record {
    holder: None,
    unavailable: false,
    reserved: false,
};

/// Every page's record, by number.
struct Pages {
    records: alloc::vec::Vec<Record>,
}

impl Pages {
    fn get(&self, page: u32) -> Record {
        self.records.get(page as usize).copied().unwrap_or(UNHELD)
    }

    fn get_mut(&mut self, page: u32) -> Result<&mut Record, MemoryError> {
        let index = page as usize;
        if index >= self.records.len() {
            self.records.try_reserve(index + 1 - self.records.len())?;
            self.records.resize(index + 1, UNHELD);
        }
        Ok(&mut self.records[index])
    }

    fn hold(&mut self, page: Frame, holder: Holder) -> Result<(), MemoryError> {
        self.get_mut(page.number())?.holder = Some(holder);
        Ok(())
    }

    /// The page is free again; a reservation stays.
    fn free(&mut self, page: Frame) {
        if let Some(record) = self.records.get_mut(page.number() as usize) {
            *record = Record {
                reserved: record.reserved,
                ..UNHELD
            };
        }
    }
}

/// A frame the kernel holds for itself, which no area may take.
fn kernel_frame(mmu: &mut Mmu, pages: &mut Pages, kind: FrameKind) -> Result<Frame, MemoryError> {
    let frame = mmu.allocate(kind)?;
    let held = pages.hold(frame, Holder::Kernel).and_then(|()| {
        pages
            .get_mut(frame.number())
            .map(|record| record.unavailable = true)
    });
    if let Err(error) = held {
        pages.free(frame);
        let _ = mmu.release(frame);
        return Err(error);
    }
    Ok(frame)
}

/// A memory frame the kernel holds for itself.
fn kernel_page(mmu: &mut Mmu, pages: &mut Pages) -> Result<Frame, MemoryError> {
    kernel_frame(mmu, pages, FrameKind::Memory)
}

/// A session's address space.
pub struct Memory {
    mmu: Mmu,
    layout: &'static Layout,
    ceiling: Ceiling,
    /// Zero page, mapped at &FFFF0000 and at zero for each 26-bit task.
    zero_page: [Frame; LOW_PAGES],
    scratch: [Frame; LOW_PAGES],
    compatibility: Frame,
    /// Whether 32-bit tasks see the compatibility page (`OS_Memory` 20).
    compatibility_on: bool,
    /// Where the next host page goes.
    host_next: u32,
    pages: Pages,
    slots: slots::Slots,
    /// The space no slot's: application space mapping nothing, current
    /// while no task's slot is.
    no_slot: Space,
    areas: areas::Areas,
    /// 26-bit modules resident, which keep 32-bit slots out of the window
    /// as a 26-bit task does.
    modules_26bit: u32,
}

impl Memory {
    /// A session's address space under `layout` and `ceiling`: zero page,
    /// ScratchSpace, the compatibility page and the stacks made, no slot and
    /// no area yet.
    pub fn new(layout: &'static Layout, ceiling: Ceiling) -> Result<Self, MemoryError> {
        let mut mmu = Mmu::new(ceiling.pages())?;
        let mut pages = Pages {
            records: alloc::vec::Vec::new(),
        };
        let mut zero_page = [Frame::new(0); LOW_PAGES];
        for (frame, address) in zero_page
            .iter_mut()
            .zip((HIGH_ZERO_PAGE..).step_by(PAGE_SIZE as usize))
        {
            *frame = kernel_page(&mut mmu, &mut pages)?;
            mmu.map(Table::Global, address, *frame, WORKSPACE)?;
        }
        let mut scratch = [Frame::new(0); LOW_PAGES];
        for frame in &mut scratch {
            *frame = kernel_page(&mut mmu, &mut pages)?;
        }
        let compatibility = kernel_page(&mut mmu, &mut pages)?;
        if let Some(words) = mmu.words_mut(compatibility) {
            for (word, chunk) in words[NULL_WARNING_AT / 4..]
                .iter_mut()
                .zip(NULL_WARNING.chunks(4))
            {
                let mut bytes = [0; 4];
                bytes[..chunk.len()].copy_from_slice(chunk);
                *word = u32::from_le_bytes(bytes);
            }
        }
        for stack in layout.stacks.all() {
            for address in stack.clone().step_by(PAGE_SIZE as usize) {
                let frame = kernel_page(&mut mmu, &mut pages)?;
                mmu.map(Table::Global, address, frame, STACK)?;
                mmu.release(frame)?;
            }
        }
        let no_slot = mmu.current_space();
        let mut memory = Self {
            mmu,
            layout,
            ceiling,
            zero_page,
            scratch,
            compatibility,
            compatibility_on: true,
            host_next: layout.host.start,
            pages,
            slots: slots::Slots::new(),
            no_slot,
            areas: areas::Areas::new(),
            modules_26bit: 0,
        };
        memory.reserve_layout().map_err(|_| MemoryError::NoMemory)?;
        Ok(memory)
    }

    /// The layout.
    #[must_use]
    pub const fn layout(&self) -> &'static Layout {
        self.layout
    }

    /// The bus the processor runs through: the address space as the guest
    /// sees it, its mappings the kernel's alone to change.
    pub fn bus(&mut self) -> &mut impl Bus {
        &mut self.mmu
    }

    /// The frame showing `address` and the address's offset in it, if a
    /// privileged mode may read it, or write it when `write`, or user mode
    /// may where `privileged` is false: how the SWIs and the routines acting
    /// for a caller reach guest memory a page at a time.
    pub(crate) fn page_with(
        &self,
        address: u32,
        write: bool,
        privileged: bool,
    ) -> Option<(Frame, usize)> {
        let (frame, access) = self.mmu.lookup(self.mmu.table_of(address), address)?;
        let rights = if privileged {
            access.privileged
        } else {
            access.user
        };
        let allowed = match rights {
            Rights::None => false,
            Rights::Read => !write,
            Rights::ReadWrite => true,
        };
        allowed.then_some((frame, (address % PAGE_SIZE) as usize))
    }

    /// `frame`'s words, as the SWIs read them.
    pub(crate) fn frame_words(&self, frame: Frame) -> Option<&[u32; FRAME_WORDS]> {
        self.mmu.words(frame)
    }

    /// `frame`'s words, for the SWIs to write; whatever watches them is told.
    pub(crate) fn frame_words_mut(&mut self, frame: Frame) -> Option<&mut [u32; FRAME_WORDS]> {
        self.mmu.words_mut(frame)
    }

    /// The RAM the guest has: the ceiling.
    #[must_use]
    pub const fn ram(&self) -> u64 {
        self.ceiling.bytes()
    }

    /// The pages in use: every page any part of the address space holds.
    #[must_use]
    pub const fn pages_in_use(&self) -> u32 {
        self.mmu.used()
    }

    /// The free pool, in pages: the ceiling less every page in use.
    #[must_use]
    pub fn free_pages(&self) -> u32 {
        self.ceiling.pages().saturating_sub(self.mmu.used())
    }

    /// Lowers the ceiling. Pages in use past it stay; growth waits until
    /// fewer are.
    pub fn lower_ceiling(&mut self, bytes: u64) {
        self.ceiling = self.ceiling.lowered(bytes);
        self.mmu.set_limit(self.ceiling.pages());
    }

    /// Zero page's frames, for the kernel to keep its workspace in.
    #[must_use]
    pub const fn zero_page(&self) -> &[Frame; LOW_PAGES] {
        &self.zero_page
    }

    /// Copies the processor vectors into the compatibility page's first
    /// words, so reading them through null finds what the vectors hold.
    pub(crate) fn mirror_vectors(&mut self, vectors: &[u32]) {
        if let Some(words) = self.mmu.words_mut(self.compatibility) {
            let count = vectors.len().min(NULL_WARNING_AT / 4);
            words[..count].copy_from_slice(&vectors[..count]);
        }
    }

    /// Whether 32-bit tasks see the compatibility page at zero.
    #[must_use]
    pub const fn compatibility_page(&self) -> bool {
        self.compatibility_on
    }

    /// Shows or hides the compatibility page in every 32-bit task's space:
    /// all of them change, or none does.
    pub fn set_compatibility_page(&mut self, on: bool) -> Result<(), MemoryError> {
        if on == self.compatibility_on {
            return Ok(());
        }
        let spaces = self.slots.spaces(Bits::Bit32)?;
        for (done, &space) in spaces.iter().enumerate() {
            if let Err(error) = self.show_compatibility(space, on) {
                for &changed in &spaces[..done] {
                    let _ = self.show_compatibility(changed, !on);
                }
                return Err(error);
            }
        }
        self.compatibility_on = on;
        Ok(())
    }

    fn show_compatibility(&mut self, space: Space, on: bool) -> Result<(), MemoryError> {
        let table = Table::Space(space);
        if on {
            self.mmu
                .map(table, COMPATIBILITY.start, self.compatibility, COMPATIBLE)?;
        } else {
            self.mmu.unmap(table, COMPATIBILITY.start)?;
        }
        Ok(())
    }

    /// Installs `code` in the next host pages, returning its address. Host
    /// pages are never written again, by the guest or the host. All of it is
    /// installed, or nothing.
    pub fn install_host(&mut self, code: &[u32]) -> Result<u32, MemoryError> {
        let address = self.host_next;
        let pages =
            u32::try_from(code.len().div_ceil(FRAME_WORDS)).map_err(|_| MemoryError::HostFull)?;
        let end = pages
            .checked_mul(PAGE_SIZE)
            .and_then(|bytes| address.checked_add(bytes))
            .filter(|&end| end <= self.layout.host.end)
            .ok_or(MemoryError::HostFull)?;
        if pages > self.free_pages() {
            return Err(MemoryError::NoMemory);
        }
        self.mmu.reserve(Table::Global, address..end)?;
        let mut frames = alloc::vec::Vec::new();
        let taken = frames
            .try_reserve_exact(pages as usize)
            .map_err(MemoryError::from)
            .and_then(|()| {
                for _ in 0..pages {
                    let frame = kernel_frame(&mut self.mmu, &mut self.pages, FrameKind::Host)?;
                    frames.push(frame);
                }
                Ok(())
            });
        if let Err(error) = taken {
            for &frame in &frames {
                self.pages.free(frame);
                let _ = self.mmu.release(frame);
            }
            self.mmu.unreserve(Table::Global, address..end);
            return Err(error);
        }
        for ((chunk, frame), page) in code
            .chunks(FRAME_WORDS)
            .zip(frames)
            .zip((address..end).step_by(PAGE_SIZE as usize))
        {
            if let Some(target) = self.mmu.words_mut(frame) {
                target[..chunk.len()].copy_from_slice(chunk);
            }
            // A reserved, never-mapped page read-only: mapping cannot fail.
            let _ = self.mmu.map(Table::Global, page, frame, HOST);
            let _ = self.mmu.release(frame);
        }
        self.mmu.unreserve(Table::Global, address..end);
        self.host_next = end;
        Ok(address)
    }

    /// Notes a 26-bit module loaded or killed.
    pub const fn note_module_26bit(&mut self, loaded: bool) {
        if loaded {
            self.modules_26bit += 1;
        } else {
            self.modules_26bit = self.modules_26bit.saturating_sub(1);
        }
    }

    /// Whether any 26-bit code is resident: a 26-bit task or module.
    #[must_use]
    pub fn resident_26bit(&self) -> bool {
        self.modules_26bit > 0 || self.slots.any_26bit()
    }

    /// Maps the low pages a task of `bits` sees: zero page and ScratchSpace
    /// for a 26-bit task, the compatibility page and ScratchSpace for a
    /// 32-bit one.
    fn map_low(&mut self, table: Table, bits: Bits) -> Result<(), MemoryError> {
        if bits == Bits::Bit26 {
            for (frame, address) in self
                .zero_page
                .into_iter()
                .zip((0..).step_by(PAGE_SIZE as usize))
            {
                self.mmu.map(table, address, frame, WORKSPACE)?;
            }
        } else if self.compatibility_on {
            self.mmu
                .map(table, COMPATIBILITY.start, self.compatibility, COMPATIBLE)?;
        }
        for (frame, address) in self
            .scratch
            .into_iter()
            .zip(SCRATCH.step_by(PAGE_SIZE as usize))
        {
            self.mmu.map(table, address, frame, WORKSPACE)?;
        }
        Ok(())
    }

    /// Takes a free page into use for `holder`: the lowest free.
    fn take_page(&mut self, holder: Holder) -> Result<Frame, MemoryError> {
        let frame = self.mmu.allocate(FrameKind::Memory)?;
        if let Err(error) = self.pages.hold(frame, holder) {
            let _ = self.mmu.release(frame);
            return Err(error);
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests;
