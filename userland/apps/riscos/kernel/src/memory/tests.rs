use alloc::vec;
use alloc::vec::Vec;

use tairix_riscos_arm::bus::{Abort, Bus, Width};

use super::layout::{APPLICATION, HIGH_ZERO_PAGE, RISC_OS_5, SHARED};
use super::*;

const MIB: u64 = 1024 * 1024;

/// A call the address space made of the rest of the kernel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Call {
    PreGrow { bytes: u32, size: u32, block: usize },
    PostGrow { bytes: u32, size: u32, block: usize },
    PreShrink { bytes: u32, size: u32 },
    PostShrink { bytes: u32, size: u32 },
    TestShrink { size: u32 },
    ResizePmp { pages: i32, area: i32 },
    Moved { bytes: i32, area: i32 },
    Memory { change: i32 },
    Created(u32),
    Removed(u32),
    Renumbered(u32, u32),
    Unsafe(usize),
    Safe(usize, usize),
    Fiq(bool),
    MovingMemory(u32),
}

/// How a grow handler refuses.
#[derive(Clone, Copy)]
pub(super) enum Refused {
    /// Naming no error.
    Silently,
}

/// A kernel that records every call and answers as its fields say.
#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "each a switch a test sets")]
pub(super) struct Recorder {
    pub calls: Vec<Call>,
    /// Whether every grow handler refuses.
    pub refuse_grow: Option<Refused>,
    /// The pages a grow handler names, from the first entry on.
    pub name_pages: Vec<u32>,
    /// The most a shrink handler allows.
    pub allow_shrink: Option<u32>,
    pub test_shrink: u32,
    pub claim_service: bool,
    pub claim_upcall: bool,
    /// The current active object; zero places it below application space.
    pub active: u32,
    pub interrupt: bool,
    /// Grows granted before every later one is refused.
    pub refuse_after: Option<usize>,
    /// A change a shrink handler makes from inside itself.
    pub regrow: Option<(i32, i32)>,
    /// An area a grow handler tries to remove from inside itself, and what
    /// each try answered.
    pub remove_inside: Option<u32>,
    pub nested: Vec<Result<(), AreaError>>,
    /// A sparse claim, area, base and size, a shrink handler makes from
    /// inside itself.
    pub claim_inside: Option<(u32, u32, u32)>,
    /// An area a shrink handler removes, making a pool in its place.
    pub replace_inside: Option<u32>,
    /// The pool it made.
    pub replacement: Option<u32>,
    /// Whether each shrink test and each `Service_MemoryMoved` makes
    /// another shrinkable area.
    pub breed: bool,
    /// Each `Service_PagesUnsafe` block.
    pub announced: Vec<Vec<PageEntry>>,
    /// Each `Service_PagesSafe` pair of blocks.
    pub swapped: Vec<(Vec<PageEntry>, Vec<PageEntry>)>,
}

/// A shrinkable area of a page, for a guest to make from inside a handler.
const CHILD: Create<'static> = Create {
    number: None,
    size: 0x1000,
    base: None,
    flags: AreaFlags::SHRINKABLE,
    max: Some(0x1000),
    handler: Some(0x03F0_0000),
    workspace: None,
    title: Some(b"Child"),
    pool: 0,
    caller: Bits::Bit32,
};

/// A pool of four slots and no window, for a guest to make from inside a
/// handler.
const POOL: Create<'static> = Create {
    number: None,
    size: 0,
    base: None,
    flags: AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES,
    max: Some(0),
    handler: Some(0x03F0_0000),
    workspace: None,
    title: Some(b"Pool"),
    pool: 4,
    caller: Bits::Bit32,
};

impl Recorder {
    pub fn new() -> Self {
        Self {
            active: u32::MAX,
            claim_upcall: true,
            ..Self::default()
        }
    }

    pub fn take(&mut self) -> Vec<Call> {
        core::mem::take(&mut self.calls)
    }
}

impl Guest for Recorder {
    fn pre_grow(
        &mut self,
        memory: &mut Memory,
        _: Handler,
        block: &mut [PageEntry],
        bytes: u32,
        size: u32,
    ) -> Result<(), Option<GuestError>> {
        self.calls.push(Call::PreGrow {
            bytes,
            size,
            block: block.len(),
        });
        if let Some(Refused::Silently) = self.refuse_grow {
            return Err(None);
        }
        let grows = self
            .calls
            .iter()
            .filter(|call| matches!(call, Call::PreGrow { .. }))
            .count();
        if self.refuse_after.is_some_and(|granted| grows > granted) {
            return Err(None);
        }
        if let Some(area) = self.remove_inside {
            let answer = memory.remove_area(area, &mut Self::new());
            self.nested.push(answer);
        }
        for (entry, &page) in block.iter_mut().zip(&self.name_pages) {
            entry.page = Some(page);
        }
        Ok(())
    }

    fn post_grow(
        &mut self,
        _: &mut Memory,
        _: Handler,
        block: &[PageEntry],
        bytes: u32,
        size: u32,
    ) {
        self.calls.push(Call::PostGrow {
            bytes,
            size,
            block: block.len(),
        });
    }

    fn pre_shrink(
        &mut self,
        _: &mut Memory,
        _: Handler,
        bytes: u32,
        size: u32,
    ) -> Result<u32, Option<GuestError>> {
        self.calls.push(Call::PreShrink { bytes, size });
        Ok(self.allow_shrink.map_or(bytes, |most| bytes.min(most)))
    }

    fn post_shrink(&mut self, memory: &mut Memory, _: Handler, bytes: u32, size: u32) {
        self.calls.push(Call::PostShrink { bytes, size });
        if let Some((area, bytes)) = self.regrow.take() {
            let _ = memory.change_area(area, bytes, &mut Self::new());
        }
        if let Some((area, base, size)) = self.claim_inside.take() {
            let _ = memory.sparse_claim(area, base, size, &mut Self::new());
        }
        if let Some(area) = self.replace_inside.take() {
            let _ = memory.remove_area(area, &mut Self::new());
            self.replacement = memory
                .create_area(&POOL, &mut Self::new())
                .ok()
                .map(|made| made.number);
        }
    }

    fn test_shrink(&mut self, memory: &mut Memory, _: Handler, size: u32) -> u32 {
        self.calls.push(Call::TestShrink { size });
        if self.breed {
            let _ = memory.create_area(&CHILD, &mut Self::new());
        }
        self.test_shrink
    }

    fn resize_pmp(
        &mut self,
        _: &mut Memory,
        _: Handler,
        pages: i32,
        area: i32,
    ) -> Result<(), Option<GuestError>> {
        self.calls.push(Call::ResizePmp { pages, area });
        Ok(())
    }

    fn service(&mut self, memory: &mut Memory, service: Service<'_>) -> bool {
        match service {
            Service::PagesUnsafe { pages } => self.announced.push(pages.to_vec()),
            Service::PagesSafe { old, new } => self.swapped.push((old.to_vec(), new.to_vec())),
            Service::MemoryMoved { .. } if self.breed => {
                let _ = memory.create_area(&CHILD, &mut Self::new());
            }
            _ => {}
        }
        self.calls.push(match service {
            Service::Memory { change, .. } => Call::Memory { change },
            Service::MemoryMoved { bytes, area } => Call::Moved { bytes, area },
            Service::DynamicAreaCreate { area } => Call::Created(area),
            Service::DynamicAreaRemove { area } => Call::Removed(area),
            Service::DynamicAreaRenumber { old, new } => Call::Renumbered(old, new),
            Service::PagesUnsafe { pages } => Call::Unsafe(pages.len()),
            Service::PagesSafe { old, new } => Call::Safe(old.len(), new.len()),
            Service::ClaimFiq => Call::Fiq(true),
            Service::ReleaseFiq => Call::Fiq(false),
        });
        matches!(service, Service::Memory { .. }) && self.claim_service
    }

    fn moving_memory(&mut self, _: &mut Memory, bytes: u32) -> bool {
        self.calls.push(Call::MovingMemory(bytes));
        self.claim_upcall
    }

    fn active_object(&self) -> u32 {
        self.active
    }

    fn in_interrupt(&self) -> bool {
        self.interrupt
    }
}

/// A shared session with 64 MiB of RAM.
pub(super) fn shared() -> Memory {
    Memory::new(&SHARED, Ceiling::derive(256 * MIB)).unwrap()
}

fn read(memory: &mut Memory, address: u32, privileged: bool) -> Result<u32, Abort> {
    memory.bus().read(address, Width::Word, privileged)
}

#[test]
fn every_session_has_zero_page_scratch_space_and_its_stacks() {
    let mut memory = shared();
    let used = memory.pages_in_use();
    assert_eq!(
        used,
        4 + 4 + 1 + 14,
        "zero page, scratch, the compatibility page, 56 KiB of stacks"
    );
    assert_eq!(memory.free_pages(), 64 * 256 - used, "a quarter of 256 MiB");
    memory
        .bus()
        .write(HIGH_ZERO_PAGE + 0x10C, Width::Word, 7, true)
        .unwrap();
    assert_eq!(
        read(&mut memory, HIGH_ZERO_PAGE + 0x10C, false),
        Ok(7),
        "User mode reads it"
    );
    assert_eq!(
        memory.bus().write(HIGH_ZERO_PAGE, Width::Word, 0, false),
        Err(Abort::Permission)
    );
    let svc = SHARED.stacks.supervisor.clone();
    memory
        .bus()
        .write(svc.end - 4, Width::Word, 1, true)
        .unwrap();
    assert_eq!(
        read(&mut memory, svc.end - 4, false),
        Err(Abort::Permission),
        "a stack is privileged"
    );
    assert_eq!(
        read(&mut memory, svc.start - 4, true),
        Err(Abort::Translation),
        "a guard below each stack"
    );
}

#[test]
fn a_task_sees_the_low_pages_its_profile_has() {
    let mut memory = shared();
    let old = memory.create_slot(Bits::Bit26).unwrap();
    let new = memory.create_slot(Bits::Bit32).unwrap();
    memory
        .bus()
        .write(HIGH_ZERO_PAGE + 0x104, Width::Byte, 0x5A, true)
        .unwrap();
    memory.switch_slot(old).unwrap();
    assert_eq!(
        read(&mut memory, 0x104, false),
        Ok(0x5A),
        "zero page at zero, one frame with &FFFF0000's"
    );
    memory
        .bus()
        .write(0x4000, Width::Word, 0x1234, true)
        .unwrap();
    memory.switch_slot(new).unwrap();
    assert_eq!(
        read(&mut memory, 0, false),
        Ok(0xFDFD_FDFD),
        "the compatibility page"
    );
    assert_eq!(read(&mut memory, 0x10, false), Ok(0));
    assert_eq!(
        memory.bus().write(0, Width::Word, 0, true),
        Err(Abort::Permission)
    );
    assert_eq!(
        memory.bus().fetch(0, true).map(|_| ()),
        Err(Abort::Permission),
        "nor run"
    );
    assert_eq!(
        read(&mut memory, 0x1000, true),
        Err(Abort::Translation),
        "the rest of it aborts"
    );
    assert_eq!(
        read(&mut memory, 0x4000, false),
        Ok(0x1234),
        "one ScratchSpace for both"
    );
}

#[test]
fn host_code_is_read_only_and_runs_as_host_code() {
    let mut memory = shared();
    let code = vec![0xE7F0_00F0, 0xE12F_FF1E];
    let address = memory.install_host(&code).unwrap();
    assert_eq!(address, SHARED.host.start);
    let fetch = memory.bus().fetch(address, false).unwrap();
    assert!(fetch.host);
    assert_eq!(fetch.word, 0xE7F0_00F0);
    assert_eq!(
        memory.bus().write(address, Width::Word, 0, true),
        Err(Abort::Permission)
    );
    let next = memory.install_host(&vec![0; 2000]).unwrap();
    assert_eq!(next, address + 0x1000, "each install takes whole pages");
    assert_eq!(
        memory.install_host(&vec![0; 1 << 20]),
        Err(MemoryError::HostFull)
    );
}

#[test]
fn the_guest_is_shown_its_ceiling_and_a_lowered_one_holds_growth_back() {
    let mut memory = Memory::new(&RISC_OS_5, Ceiling::derive(64 * MIB)).unwrap();
    assert_eq!(memory.ram(), 16 * MIB);
    let slot = memory.create_slot(Bits::Bit32).unwrap();
    let free = memory.free_pages();
    memory.set_slot_size(slot, 0x10_0000).unwrap();
    assert_eq!(memory.free_pages(), free - 256);
    memory.lower_ceiling(MIB);
    assert_eq!(memory.free_pages(), 0, "past the lowered ceiling");
    assert_eq!(
        memory.set_slot_size(slot, 0x10_1000),
        Err(MemoryError::NoMemory)
    );
    assert_eq!(memory.slot_size(slot), Some(0x10_0000), "all or nothing");
}

#[test]
fn a_slot_grows_and_shrinks_in_its_own_space() {
    let mut memory = shared();
    let a = memory.create_slot(Bits::Bit32).unwrap();
    let b = memory.create_slot(Bits::Bit32).unwrap();
    memory.set_slot_size(a, 0x2001).unwrap();
    assert_eq!(memory.slot_size(a), Some(0x3000), "whole pages");
    memory.switch_slot(a).unwrap();
    memory
        .bus()
        .write(APPLICATION + 0x2FFC, Width::Word, 0xA, false)
        .unwrap();
    memory.switch_slot(b).unwrap();
    assert_eq!(
        read(&mut memory, APPLICATION, false),
        Err(Abort::Translation),
        "b has none"
    );
    memory.set_slot_size(b, 0x1000).unwrap();
    assert_eq!(
        read(&mut memory, APPLICATION, false),
        Ok(0),
        "its own, zeroed"
    );
    memory.switch_slot(a).unwrap();
    assert_eq!(read(&mut memory, APPLICATION + 0x2FFC, false), Ok(0xA));
    memory.set_slot_size(a, 0x1000).unwrap();
    assert_eq!(
        read(&mut memory, APPLICATION + 0x1000, false),
        Err(Abort::Translation)
    );
    let free = memory.free_pages();
    memory.switch_slot(b).unwrap();
    memory.destroy_slot(a).unwrap();
    assert_eq!(memory.free_pages(), free + 1, "its page freed");
    assert_eq!(memory.slot_size(a), None, "the handle names nothing now");
    assert_eq!(
        memory.destroy_slot(b),
        Err(MemoryError::Slot),
        "the current one"
    );
}

#[test]
fn a_26bit_slot_stops_at_28_mib_and_the_32bit_only_layout_refuses_one() {
    let mut memory = Memory::new(&SHARED, Ceiling::derive(1024 * MIB)).unwrap();
    let old = memory.create_slot(Bits::Bit26).unwrap();
    assert_eq!(memory.set_slot_size(old, 0x01C0_0000 - APPLICATION), Ok(()));
    assert_eq!(
        memory.set_slot_size(old, 0x01C0_1000 - APPLICATION),
        Err(MemoryError::TooLarge)
    );
    let mut five = Memory::new(&RISC_OS_5, Ceiling::derive(64 * MIB)).unwrap();
    assert_eq!(five.create_slot(Bits::Bit26), Err(MemoryError::Not26Bit));
}

#[test]
fn a_32bit_slot_takes_the_window_only_while_no_26bit_code_is_resident() {
    let mut memory = Memory::new(&SHARED, Ceiling::derive(1024 * MIB)).unwrap();
    let new = memory.create_slot(Bits::Bit32).unwrap();
    let into_window = 0x0200_0000 - APPLICATION;
    assert_eq!(
        memory.set_slot_size(new, into_window),
        Ok(()),
        "no 26-bit code yet"
    );
    assert_eq!(
        memory.set_slot_size(new, SHARED.top_32bit + 0x1000 - APPLICATION),
        Err(MemoryError::TooLarge),
        "never past the RMA"
    );
    memory.set_slot_size(new, 0).unwrap();
    memory.note_module_26bit(true);
    assert_eq!(
        memory.set_slot_size(new, into_window),
        Err(MemoryError::Window),
        "a 26-bit module is resident, and the session window says why"
    );
    assert_eq!(memory.slot_limit(new), Some(SHARED.top_26bit - APPLICATION));
    memory.note_module_26bit(false);
    let old = memory.create_slot(Bits::Bit26).unwrap();
    assert_eq!(
        memory.set_slot_size(new, into_window),
        Err(MemoryError::Window),
        "a 26-bit task too"
    );
    memory.switch_slot(new).unwrap();
    memory.destroy_slot(old).unwrap();
    assert_eq!(memory.set_slot_size(new, into_window), Ok(()));
}

#[test]
fn the_application_limit_moves_only_over_nothing() {
    let mut memory = Memory::new(&RISC_OS_5, Ceiling::derive(1024 * MIB)).unwrap();
    let slot = memory.create_slot(Bits::Bit32).unwrap();
    memory.set_slot_size(slot, 0x10_0000).unwrap();
    assert_eq!(
        memory.set_application_limit(0),
        Ok(0x2000_0000),
        "zero reads it"
    );
    assert_eq!(
        memory.set_application_limit(0x0800_0001),
        Err(AreaError::BadParameters)
    );
    assert_eq!(
        memory.set_application_limit(0x2000_1000),
        Err(AreaError::BadParameters)
    );
    assert_eq!(
        memory.set_application_limit(0x8000),
        Err(AreaError::BadParameters)
    );
    assert_eq!(
        memory.set_application_limit(0x8000 + 0x8_0000),
        Err(AreaError::OverlappingAreas),
        "below a slot's top"
    );
    assert_eq!(memory.set_application_limit(0x0800_0000), Ok(0x2000_0000));
    assert_eq!(
        memory.set_slot_size(slot, 0x0800_0000),
        Err(MemoryError::TooLarge)
    );
    assert_eq!(memory.slot_limit(slot), Some(0x0800_0000 - APPLICATION));
}
