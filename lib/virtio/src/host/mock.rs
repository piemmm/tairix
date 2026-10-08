//! [`MockHost`], the in-process [`VirtioHost`] every virtio driver's unit tests
//! run on. Built only for tests, behind the crate's `mock` feature.

use super::{CompletionSignal, DmaHost, VirtioHost};
use crate::dma::{DmaSlab, PoolId, SlabEnd};
use crate::transport::MockTransport;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;
use core::cell::RefCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, Ordering};
use tairix_abi::DriverError;

/// In-process [`VirtioHost`] the unit tests of this crate and of every virtio
/// driver run against.
///
/// A slab's device address is its CPU address with a tag no CPU pointer
/// carries, and a device that [reaches](MockTransport::reach) the host's
/// memory finds a slab only by such an address, so a driver that hands its
/// device a CPU pointer — or dereferences a device address — fails its tests
/// rather than passing them. A slab's drop only records the release, and
/// whether the slab came back zeroed (see [`Self::slabs_outstanding`] and
/// [`Self::released_zeroed`]); its bytes stay reachable by device address
/// until the host is gone. The host and the slab each hold a count of the
/// slab's storage, so whichever goes last frees it.
///
/// A wait plays the next [`MockWait`] a test [scripted](Self::script_waits),
/// else the host's standing one ([`MockWait::Answer`] unless built
/// [silent](Self::silent)). An [attached](Self::attach) device answers a wait
/// by draining the waited queue, so a driver's completion path runs as it does
/// against a device completing on its interrupt. The host keeps the clock its
/// waits spend.
pub struct MockHost {
    notify_log: RefCell<Vec<u16>>,
    bytes_allocated: Cell<usize>,
    quiesced: Cell<usize>,
    clock_ns: Cell<u64>,
    clock_reads: Cell<usize>,
    script: RefCell<VecDeque<MockWait>>,
    standing: MockWait,
    device: RefCell<Option<Rc<RefCell<MockTransport>>>>,
    memory: Rc<MockMemory>,
    /// No allocation succeeds: a host with no DMA memory left.
    exhausted: bool,
}

/// The memory a [`MockHost`] handed out: each slab's storage by slot, found
/// by device address and reached through the slab's own pointer, so a device
/// access keeps the provenance the driver's has.
#[derive(Default)]
pub(crate) struct MockMemory {
    slots: RefCell<Vec<Arc<Storage>>>,
    by_device: RefCell<BTreeMap<u64, usize>>,
}

/// One slab's bytes and what became of it, freed once neither the host's
/// memory nor the slab holds a count of it.
struct Storage {
    base: NonNull<u8>,
    len: usize,
    fate: AtomicU8,
}

// SAFETY: the bytes are reached by the one slab holding them, or by device
// address on the host's own thread; the record is atomic, and the allocation
// may be freed on any thread.
unsafe impl Send for Storage {}
// SAFETY: as above.
unsafe impl Sync for Storage {}

impl Storage {
    /// Whether every byte is zero.
    fn zeroed(&self) -> bool {
        // SAFETY: `base` covers `len` initialised bytes, alive while this
        // count is held, and the slab handing them back borrows them no more.
        let bytes = unsafe { core::slice::from_raw_parts(self.base.as_ptr(), self.len) };
        bytes.iter().all(|byte| *byte == 0)
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        let storage = core::ptr::slice_from_raw_parts_mut(self.base.as_ptr(), self.len);
        // SAFETY: `base` is the pointer `Box::leak` gave for exactly `len`
        // bytes, freed once, by the last count's drop.
        drop(unsafe { Box::from_raw(storage) });
    }
}

/// A slab just minted: where the device and the CPU find it, its slot, and
/// the count of its storage it holds.
struct Mint {
    device: u64,
    cpu: NonNull<u8>,
    slot: usize,
    storage: *const Storage,
}

impl MockMemory {
    /// The `len` bytes at device address `device`, or [`None`] unless they lie
    /// wholly inside one slab the host handed out: a device reaching past its
    /// buffer finds nothing, as one confined by a translation unit does.
    pub(crate) fn view(&self, device: u64, len: usize) -> Option<*mut u8> {
        let (&start, &slot) = self.by_device.borrow().range(..=device).next_back()?;
        let slots = self.slots.borrow();
        let storage = slots.get(slot)?;
        let offset = usize::try_from(device - start).ok()?;
        if offset.checked_add(len)? > storage.len {
            return None;
        }
        Some(storage.base.as_ptr().wrapping_add(offset))
    }

    /// Mint `len` zeroed bytes.
    fn mint(&self, len: usize) -> Result<Mint, DriverError> {
        let bytes: Box<[u8]> = alloc::vec![0u8; len].into_boxed_slice();
        let cpu = u64::try_from(bytes.as_ptr().addr()).map_err(|_| DriverError::OutOfRange)?;
        if cpu & DEVICE_TAG != 0 {
            return Err(DriverError::OutOfRange);
        }
        let device = cpu | DEVICE_TAG;
        let base = NonNull::from(Box::leak(bytes)).cast::<u8>();
        let storage = Arc::new(Storage {
            base,
            len,
            fate: AtomicU8::new(HELD),
        });
        let held = Arc::into_raw(Arc::clone(&storage));
        let mut slots = self.slots.borrow_mut();
        let slot = slots.len();
        slots.push(storage);
        self.by_device.borrow_mut().insert(device, slot);
        Ok(Mint {
            device,
            cpu: base,
            slot,
            storage: held,
        })
    }

    fn fate(&self, slot: usize) -> Option<u8> {
        self.slots
            .borrow()
            .get(slot)
            .map(|storage| storage.fate.load(Ordering::Relaxed))
    }
}

/// How one [`MockHost`] wait plays out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MockWait {
    /// The attached device, if any, completes what the waited queue holds,
    /// and the wait fires at once.
    Answer,
    /// The wait fires `after_ns` in, with nothing done: an early wake, or one
    /// for another queue on a shared line. One past the budget is silence.
    Spurious {
        /// How far into the wait the wake lands.
        after_ns: u64,
    },
    /// Nothing happens for the whole budget.
    Silent,
    /// The wait could not be made at all — a revoked or refused interrupt
    /// binding — so it times out at once, with no time spent.
    Refused,
    /// The attached device completes what the waited queue holds, but the
    /// interrupt is lost: the wait runs out its whole budget.
    Lost,
}

impl Default for MockHost {
    fn default() -> Self {
        Self::with_standing(MockWait::Answer)
    }
}

/// A slab still held, or withheld and so never released.
const HELD: u8 = 0;
/// A slab released with every byte zero.
const RELEASED_ZEROED: u8 = 1;
/// A slab released still holding data.
const RELEASED_DIRTY: u8 = 2;

/// Records one mock slab's end, and whether a released one came back
/// zeroed, then gives up the slab's count of its storage.
///
/// # Safety
///
/// `storage` is the count [`MockMemory::mint`] gave the slab, taken back once.
unsafe fn record_mock_release(
    storage: *const (),
    _cpu: NonNull<u8>,
    _slot: usize,
    _len: usize,
    end: SlabEnd,
) {
    // SAFETY: the slab's own count, and a slab's drop runs once.
    let storage = unsafe { Arc::from_raw(storage.cast::<Storage>()) };
    if end == SlabEnd::Released {
        let outcome = if storage.zeroed() {
            RELEASED_ZEROED
        } else {
            RELEASED_DIRTY
        };
        storage.fate.store(outcome, Ordering::Relaxed);
    }
}

impl MockHost {
    /// Construct an empty mock host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A host none of whose unscripted waits is ever answered: a device whose
    /// interrupt is lost for good.
    #[must_use]
    pub fn silent() -> Self {
        Self::with_standing(MockWait::Silent)
    }

    /// A host with no DMA memory left to hand out.
    #[must_use]
    pub fn exhausted() -> Self {
        Self {
            exhausted: true,
            ..Self::default()
        }
    }

    fn with_standing(standing: MockWait) -> Self {
        Self {
            notify_log: RefCell::new(Vec::new()),
            bytes_allocated: Cell::new(0),
            quiesced: Cell::new(0),
            clock_ns: Cell::new(0),
            clock_reads: Cell::new(0),
            script: RefCell::new(VecDeque::new()),
            standing,
            device: RefCell::new(None),
            memory: Rc::new(MockMemory::default()),
            exhausted: false,
        }
    }

    /// Answer waits by draining `device`, the mock the driver under test
    /// drives through its own handle, which reaches this host's memory.
    pub fn attach(&self, device: &Rc<RefCell<MockTransport>>) {
        device.borrow_mut().reach(self);
        *self.device.borrow_mut() = Some(Rc::clone(device));
    }

    /// The memory this host hands out, as its device reaches it.
    pub(crate) fn memory(&self) -> Rc<MockMemory> {
        Rc::clone(&self.memory)
    }

    /// Play `waits`, in order, for the next waits.
    pub fn script_waits(&self, waits: impl IntoIterator<Item = MockWait>) {
        self.script.borrow_mut().extend(waits);
    }

    /// Have the attached device, if any, complete what `queue_index` holds.
    fn play_device(&self, queue_index: u16) {
        if let Some(device) = self.device.borrow().as_ref() {
            // A queue index the device lacks is the driver's own error, which
            // its next ring read reports.
            let _ = device.borrow_mut().drain_queue(queue_index);
        }
    }

    /// How many times a driver read the clock: each reading costs a real host
    /// a system call.
    #[must_use]
    pub fn clock_reads(&self) -> usize {
        self.clock_reads.get()
    }

    /// All notify events the host has seen so far, in order.
    #[must_use]
    pub fn notify_log(&self) -> Vec<u16> {
        self.notify_log.borrow().clone()
    }

    /// Total number of bytes ever handed out by this host: it keeps every
    /// allocation until it is dropped, so the counter only grows.
    #[must_use]
    pub fn bytes_allocated(&self) -> usize {
        self.bytes_allocated.get()
    }

    /// How many times a driver declared its device quiesced.
    #[must_use]
    pub fn quiesced_calls(&self) -> usize {
        self.quiesced.get()
    }

    /// Slabs this host minted that have not been released: what a driver
    /// still holds, or deliberately withheld from a device it could not stop.
    #[must_use]
    pub fn slabs_outstanding(&self) -> usize {
        self.memory
            .slots
            .borrow()
            .iter()
            .filter(|storage| storage.fate.load(Ordering::Relaxed) == HELD)
            .count()
    }

    /// Whether the slab minted as `slot` has been released with every byte
    /// zero, as memory that carried a secret must be.
    #[must_use]
    pub fn released_zeroed(&self, slot: usize) -> bool {
        self.memory.fate(slot) == Some(RELEASED_ZEROED)
    }
}

/// Set in every device address the mock hands out, and in no pointer of a
/// host test process: below the top byte an arm64 load ignores, and outside
/// every lower-half address on either host architecture, so a CPU dereference
/// of a device address faults.
const DEVICE_TAG: u64 = 1 << 55;

impl DmaHost for MockHost {
    /// Hand out a zeroed [`DmaSlab`] over memory the host owns, so the slab
    /// carries its pointer with no borrow; the 64 MiB cap bounds what one
    /// test can hold.
    fn alloc_dma_zeroed(&self, size: usize) -> Result<DmaSlab, DriverError> {
        if size == 0 {
            return Err(DriverError::BufferTooSmall);
        }
        if self.exhausted {
            return Err(DriverError::OutOfMemory);
        }
        // 64 MiB pool cap is far above the Stage-4 unit-test budget;
        // exceeding it signals a runaway test rather than real
        // allocator pressure. Failing closed.
        let bytes_now = self.bytes_allocated.get();
        let Some(bytes_after) = bytes_now.checked_add(size) else {
            return Err(DriverError::LengthOutOfRange);
        };
        if bytes_after > 64 * 1024 * 1024 {
            return Err(DriverError::OutOfMemory);
        }
        let mint = self.memory.mint(size)?;
        self.bytes_allocated.set(bytes_after);
        // SAFETY: `mint.cpu` is the only CPU handle on `size` bytes kept alive
        // by the count `mint.storage` the slab holds, so the slab owns them
        // alone; `record_mock_release` takes that count back once.
        Ok(unsafe {
            DmaSlab::from_pool(
                mint.device,
                mint.cpu,
                size,
                PoolId::MOCK,
                mint.slot,
                mint.storage.cast::<()>(),
                record_mock_release,
            )
        })
    }

    fn device_quiesced(&self) {
        self.quiesced.set(self.quiesced.get() + 1);
    }
}

impl VirtioHost for MockHost {
    /// Records the wait and plays the next [`MockWait`]; the mock never blocks,
    /// it only moves its clock by the time the wait would have taken.
    fn notify_wait(&self, queue_index: u16, timeout_ns: u64) -> CompletionSignal {
        self.notify_log.borrow_mut().push(queue_index);
        let wait = self
            .script
            .borrow_mut()
            .pop_front()
            .unwrap_or(self.standing);
        let (elapsed_ns, signal) = match wait {
            MockWait::Answer => {
                self.play_device(queue_index);
                (0, CompletionSignal::Fired)
            }
            MockWait::Spurious { after_ns } if after_ns < timeout_ns => {
                (after_ns, CompletionSignal::Fired)
            }
            MockWait::Spurious { .. } | MockWait::Silent => {
                (timeout_ns, CompletionSignal::TimedOut)
            }
            MockWait::Refused => (0, CompletionSignal::TimedOut),
            MockWait::Lost => {
                self.play_device(queue_index);
                (timeout_ns, CompletionSignal::TimedOut)
            }
        };
        self.clock_ns
            .set(self.clock_ns.get().saturating_add(elapsed_ns));
        signal
    }

    fn now_ns(&self) -> u64 {
        self.clock_reads.set(self.clock_reads.get() + 1);
        self.clock_ns.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::driver::BufferClass;

    #[test]
    fn mock_host_zero_initialises() {
        let host = MockHost::new();
        let slab = host.alloc_dma_zeroed(64).expect("alloc");
        assert_eq!(slab.len(), 64);
        assert!(slab.as_bytes().iter().all(|b| *b == 0));
        assert_eq!(slab.pool_id(), PoolId::MOCK);
    }

    #[test]
    fn mock_host_rejects_zero_size() {
        let host = MockHost::new();
        assert!(matches!(
            host.alloc_dma_zeroed(0),
            Err(DriverError::BufferTooSmall)
        ));
    }

    #[test]
    fn mock_host_records_notifies() {
        let host = MockHost::new();
        assert_eq!(host.notify_wait(0, u64::MAX), CompletionSignal::Fired);
        assert_eq!(host.notify_wait(1, u64::MAX), CompletionSignal::Fired);
        assert_eq!(host.notify_wait(0, u64::MAX), CompletionSignal::Fired);
        assert_eq!(host.notify_log(), alloc::vec![0u16, 1, 0]);
    }

    #[test]
    fn a_silent_host_spends_each_waits_whole_budget() {
        let host = MockHost::silent();
        assert_eq!(host.notify_wait(0, 7), CompletionSignal::TimedOut);
        assert_eq!(host.notify_wait(0, 5), CompletionSignal::TimedOut);
        assert_eq!(host.now_ns(), 12);
    }

    #[test]
    fn scripted_waits_play_in_order_then_the_standing_one() {
        let host = MockHost::new();
        host.script_waits([
            MockWait::Silent,
            MockWait::Spurious { after_ns: 3 },
            MockWait::Spurious { after_ns: 50 },
        ]);
        assert_eq!(host.notify_wait(0, 10), CompletionSignal::TimedOut);
        assert_eq!(host.notify_wait(0, 10), CompletionSignal::Fired);
        assert_eq!(host.notify_wait(0, 10), CompletionSignal::TimedOut);
        assert_eq!(host.now_ns(), 23, "10, then 3, then a whole 10");
        assert_eq!(host.notify_wait(0, 10), CompletionSignal::Fired);
        assert_eq!(host.now_ns(), 23, "an answer takes no time");
    }

    #[test]
    fn a_refused_wait_times_out_at_once_and_a_lost_one_spends_its_budget() {
        let host = MockHost::new();
        host.script_waits([MockWait::Refused, MockWait::Lost]);
        assert_eq!(host.notify_wait(0, 10), CompletionSignal::TimedOut);
        assert_eq!(host.now_ns(), 0);
        assert_eq!(host.notify_wait(0, 10), CompletionSignal::TimedOut);
        assert_eq!(host.now_ns(), 10);
    }

    #[test]
    fn an_attached_device_answers_the_waited_queue() {
        use crate::queue::{ChainSegment, SplitQueue};
        use crate::transport::{Direction, MockTransport};

        let host = MockHost::new();
        let device = MockTransport::new(2, 4, 0, 0).into_shared();
        let mut transport = alloc::rc::Rc::clone(&device);
        host.attach(&device);
        let mut q = SplitQueue::new(&mut transport, &host, 1, 4, 1).unwrap();
        device.borrow_mut().install_shim(
            1,
            Box::new(|_chain: &mut crate::transport::ChainView<'_>| Ok(0)),
        );
        let slab = host.alloc_dma_zeroed(4).unwrap();
        q.add_chain(&[ChainSegment {
            device_addr: slab.device_addr(),
            len: 4,
            direction: Direction::DeviceWrite,
        }])
        .unwrap();
        assert_eq!(host.notify_wait(0, 10), CompletionSignal::Fired);
        assert!(
            q.poll_used().is_err(),
            "another queue's wait drains nothing"
        );
        assert_eq!(host.notify_wait(1, 10), CompletionSignal::Fired);
        assert!(q.poll_used().is_ok());
    }

    #[test]
    fn mock_host_assigns_distinct_slots() {
        let host = MockHost::new();
        let a = host.alloc_dma_zeroed(4).unwrap();
        let b = host.alloc_dma_zeroed(4).unwrap();
        let c = host.alloc_dma_zeroed(4).unwrap();
        assert_ne!(a.slot(), b.slot());
        assert_ne!(b.slot(), c.slot());
        assert_ne!(a.slot(), c.slot());
    }

    /// A slab is released once, and one withheld from a device the driver
    /// could not stop is never released, though the host frees its bytes.
    #[test]
    fn a_withheld_slab_is_never_released_and_the_host_frees_it() {
        let host = MockHost::new();
        let kept = host.alloc_dma_zeroed(8).unwrap();
        let dropped = host.alloc_dma_zeroed(8).unwrap();
        let mut withheld = host.alloc_dma_zeroed(8).unwrap();
        assert_eq!(host.slabs_outstanding(), 3);
        drop(dropped);
        withheld.withhold();
        drop(withheld);
        assert_eq!(host.slabs_outstanding(), 2);
        drop(kept);
        assert_eq!(host.slabs_outstanding(), 1, "the withheld one");
        drop(host);
    }

    /// A slab holds a count of its own storage, so dropping its host first
    /// leaves the slab's bytes and its release sound.
    #[test]
    fn a_mock_slab_counts_its_release_once_even_past_its_host() {
        let host = MockHost::new();
        let mut outlives = host.alloc_dma_zeroed(8).unwrap();
        let mut withheld = host.alloc_dma_zeroed(8).unwrap();
        withheld.withhold();
        drop(host);
        outlives.as_bytes_mut()[7] = 0x5A;
        assert_eq!(outlives.as_bytes()[7], 0x5A);
        drop(outlives);
        drop(withheld);
    }

    /// A device finds only the bytes of one slab: an extent running past its
    /// slab, or an address no slab was minted at, is nothing.
    #[test]
    fn a_device_reaches_only_within_one_slab() {
        let host = MockHost::new();
        let slab = host.alloc_dma_zeroed(16).unwrap();
        let memory = host.memory();
        let base = slab.device_addr();
        assert!(memory.view(base, 16).is_some());
        assert!(memory.view(base + 8, 8).is_some());
        assert!(
            memory.view(base + 16, 0).is_some(),
            "one past its end, empty"
        );
        assert!(memory.view(base + 8, 9).is_none(), "past its end");
        assert!(memory.view(base - 1, 1).is_none(), "before it");
        let cpu = u64::try_from(slab.as_bytes().as_ptr().addr()).unwrap();
        assert!(memory.view(cpu, 1).is_none(), "a CPU address");
    }

    #[test]
    fn a_released_slab_reports_whether_it_came_back_zeroed() {
        let host = MockHost::new();
        let mut dirty = host.alloc_dma_zeroed(8).unwrap();
        let mut scrubbed = host.alloc_dma_zeroed(8).unwrap();
        let mut withheld = host.alloc_dma_zeroed(8).unwrap();
        for slab in [&mut dirty, &mut scrubbed, &mut withheld] {
            slab.as_bytes_mut()[7] = 0x5A;
        }
        let (dirty_slot, scrubbed_slot, withheld_slot) =
            (dirty.slot(), scrubbed.slot(), withheld.slot());
        assert!(!host.released_zeroed(scrubbed_slot), "still held");
        crate::dma::scrub(&mut scrubbed);
        withheld.withhold();
        drop((dirty, scrubbed, withheld));
        assert!(!host.released_zeroed(dirty_slot));
        assert!(host.released_zeroed(scrubbed_slot));
        assert!(!host.released_zeroed(withheld_slot), "never released");
        assert!(!host.released_zeroed(withheld_slot + 1), "never minted");
    }

    #[test]
    fn host_dma_slab_supports_bounce_buffer_round_trip() {
        let host = MockHost::new();
        let slab = host.alloc_dma_zeroed(32).expect("alloc");
        let mut bb = crate::dma::BounceBuffer::new(slab, BufferClass::NonSensitive);
        bb.stage(&[0x42; 16]).unwrap();
        assert_eq!(bb.staged(), &[0x42; 16]);
    }
}
