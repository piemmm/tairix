//! The kernel's heaps (`plans/RISCOS-EMULATOR.md` §3): the system heap and
//! the RMA, dynamic areas 0 and 1 in the layout's places, each an `OS_Heap`
//! heap whose area grows when a claim finds it full.
//!
//! Their areas' handler is the kernel's: a grow or shrink keeps the heap's
//! size equal to the area's, and a shrink stops at the heap's high-water
//! mark. The growth is the kernel's claim's, not `OS_Heap`'s: a program
//! calling `OS_Heap` on either heap itself gets none.

use tairix_riscos_arm::mmu::PAGE_SIZE;

use crate::error::Error;
use crate::heap::{self, Heap, HeapError, Space};
use crate::memory::check::{PRIVILEGED_READ, PRIVILEGED_WRITE};
use crate::os::{Fail, Os};

/// One of the kernel's heaps.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum KernelHeap {
    /// The system heap, dynamic area 0.
    System,
    /// The relocatable module area, dynamic area 1.
    Rma,
}

impl KernelHeap {
    /// Its dynamic area's number.
    pub(crate) const fn area(self) -> u32 {
        match self {
            Self::System => 0,
            Self::Rma => 1,
        }
    }

    /// The error a claim it cannot grow for gives.
    pub(crate) const fn full(self) -> Error {
        match self {
            Self::System => crate::error::SYSTEM_HEAP_FULL,
            Self::Rma => RMA_FULL,
        }
    }
}

/// "Not enough memory in module area".
const RMA_FULL: Error = Error {
    number: 0x101,
    token: "ModuleAreaFull",
};

/// Guest memory as `OS_Heap` reaches it: a privileged mode's access, a range
/// valid where a privileged mode may read and write all of it.
impl Space for Os<'_> {
    fn read(&mut self, address: u32) -> Option<u32> {
        self.peek_word(address)
    }

    fn write(&mut self, address: u32, value: u32) -> Option<()> {
        self.poke_word(address, value)
    }

    fn valid(&mut self, start: u32, end: u64) -> bool {
        self.writable(start, end)
    }
}

/// The error a heap failure gives.
pub(crate) const fn heap_error(error: HeapError) -> Error {
    let (number, token) = match error {
        HeapError::BadReason => (0x180, "UnknownReasonCode"),
        HeapError::FailInit => (0x181, "HeapNotInitialised"),
        HeapError::BadDescriptor => (0x182, "BadHeapDescriptor"),
        HeapError::BadLink => (0x183, "HeapCorrupt"),
        HeapError::NoRoom => (0x184, "HeapFull"),
        HeapError::NotABlock => (0x185, "NotAHeapBlock"),
        HeapError::BadExtend => (0x186, "HeapCannotGrow"),
        HeapError::ExcessiveShrink { .. } => (0x187, "HeapShrinkLimited"),
    };
    Error { number, token }
}

/// "No such reason code": the heap's, which other calls give for a reason
/// they do not know, as genuine RISC OS 5's `OS_NVMemory` does.
pub(crate) const UNKNOWN_REASON: Error = heap_error(HeapError::BadReason);

impl Os<'_> {
    /// Whether a privileged mode may read and write every byte from `start`
    /// to `end`.
    pub(crate) fn writable(&mut self, start: u32, end: u64) -> bool {
        let both = PRIVILEGED_READ | PRIVILEGED_WRITE;
        self.memory.check_access(start, end) & both == both
    }

    /// The base of `heap`'s area.
    fn heap_base(&self, heap: KernelHeap) -> u32 {
        let layout = self.memory.layout();
        match heap {
            KernelHeap::System => layout.system_heap.start,
            KernelHeap::Rma => layout.rma.start,
        }
    }

    /// A block of at least `size` bytes from `heap`, its area grown once if
    /// the heap is full; the heap's "full" error when even that fails.
    pub(crate) fn heap_claim(&mut self, heap: KernelHeap, size: u32) -> Result<u32, Fail> {
        self.growing(heap, u64::from(size) + 8, |os, h| h.get(os, size))
    }

    /// `op` on `heap`, which may grow `need` bytes and be tried once more if
    /// the heap is full; the heap's "full" error when even that fails, and
    /// its own error for anything else.
    pub(crate) fn growing<T>(
        &mut self,
        heap: KernelHeap,
        need: u64,
        mut op: impl FnMut(&mut Self, &mut Heap) -> Result<T, HeapError>,
    ) -> Result<T, Fail> {
        let base = self.heap_base(heap);
        let mut attempt = |os: &mut Self| Heap::open(os, base).and_then(|mut h| op(os, &mut h));
        match attempt(self) {
            Ok(done) => return Ok(done),
            Err(HeapError::NoRoom) => {}
            Err(error) => return Err(self.error(heap_error(error))),
        }
        self.grow_heap(heap, need)?;
        attempt(self).map_err(|_| self.error(heap.full()))
    }

    /// `heap`'s largest free block and its free bytes in all.
    pub(crate) fn heap_describe(&mut self, heap: KernelHeap) -> Result<(u32, u32), Fail> {
        let base = self.heap_base(heap);
        Heap::open(self, base)
            .and_then(|h| h.describe(self))
            .map_err(|e| self.error(heap_error(e)))
    }

    /// The size of `block` of `heap`, header included, if it is one.
    pub(crate) fn heap_block_size(&mut self, heap: KernelHeap, block: u32) -> Option<u32> {
        let base = self.heap_base(heap);
        Heap::open(self, base)
            .and_then(|h| h.block_size(self, block))
            .ok()
    }

    /// Grows `heap`'s area enough that `need` more bytes fit past its
    /// high-water mark; its error if the area cannot grow so far.
    fn grow_heap(&mut self, heap: KernelHeap, need: u64) -> Result<(), Fail> {
        let base = self.heap_base(heap);
        let header = Heap::open(self, base).map_err(|e| self.error(heap_error(e)))?;
        let room = u64::from(header.room());
        let wanted = need.saturating_sub(room) + 8;
        let pages = wanted.div_ceil(u64::from(PAGE_SIZE));
        let bytes =
            i32::try_from(pages * u64::from(PAGE_SIZE)).map_err(|_| self.error(heap.full()))?;
        match self.change_area_by(heap.area(), bytes) {
            Ok(_) => Ok(()),
            Err(Fail::Error(_)) => Err(self.error(heap.full())),
            Err(unwind) => Err(unwind),
        }
    }

    /// Frees a block of `heap`.
    pub(crate) fn heap_free(&mut self, heap: KernelHeap, block: u32) -> Result<(), Fail> {
        let base = self.heap_base(heap);
        Heap::open(self, base)
            .and_then(|mut h| h.free(self, block))
            .map_err(|e| self.error(heap_error(e)))
    }

    /// A node of `size` bytes in the system heap for the kernel's own
    /// record of a guest's request: "system heap full" when there is none.
    pub(crate) fn system_node(&mut self, size: u32) -> Result<u32, Fail> {
        self.heap_claim(KernelHeap::System, size)
    }

    /// Frees a node `system_node` gave. A heap the guest has overwritten
    /// keeps it: the guest's own memory is all that is lost.
    pub(crate) fn free_system_node(&mut self, node: u32) {
        let _ = self.heap_free(KernelHeap::System, node);
    }

    /// Makes `heap` at its area's base, `size` bytes, at boot.
    pub(crate) fn initialise_heap(&mut self, heap: KernelHeap, size: u32) -> Result<(), HeapError> {
        let base = self.heap_base(heap);
        heap::initialise(self, base, size)
    }

    /// The heap areas' handler, after a grow or shrink of `heap`'s area to
    /// `size` bytes: the heap's size follows it.
    pub(crate) fn heap_area_resized(&mut self, heap: KernelHeap, size: u32) {
        let base = self.heap_base(heap);
        let _ = self.poke_word(base + 12, size);
    }

    /// The heap areas' handler, before a shrink of `heap`'s area by `bytes`:
    /// the whole pages above the heap's high-water mark may go, and the
    /// heap's size gives them up first.
    pub(crate) fn heap_area_shrink(&mut self, heap: KernelHeap, bytes: u32) -> u32 {
        let base = self.heap_base(heap);
        let Ok(header) = Heap::open(self, base) else {
            return 0;
        };
        let allowed = bytes.min(header.room() & !(PAGE_SIZE - 1));
        let _ = self.poke_word(base + 12, header.size() - allowed);
        allowed
    }

    /// The heap areas' handler asked what a shrink could free.
    pub(crate) fn heap_area_spare(&mut self, heap: KernelHeap) -> u32 {
        let base = self.heap_base(heap);
        Heap::open(self, base).map_or(0, |header| header.room() & !(PAGE_SIZE - 1))
    }
}
