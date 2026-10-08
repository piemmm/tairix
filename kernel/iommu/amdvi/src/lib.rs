//! AMD-Vi: one AMD I/O virtualization unit, driven with v1 host page tables,
//! its command buffer and event log, and interrupt remapping tables.
//!
//! Every device id starts blocked: its device table entry is valid with no
//! translation information, so its DMA is aborted. A device reaches memory
//! only through the domain attached to it, and every removal is confirmed by
//! a completion wait before it is reported done. Its interrupts pass
//! untouched until remapping is turned on; from then a device's interrupts
//! go through the table made for its source, or are refused.
//!
//! Reference: AMD I/O Virtualization Technology (IOMMU) Specification,
//! rev. 3.08. The design and its staging are `plans/IOMMU.md`.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

extern crate alloc;

mod format;
mod regs;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use tairix_kernel_iommu_api::Registers;

use core::ops::Range;
use core::sync::atomic::{AtomicBool, Ordering};

use alloc::vec::Vec;

use tairix_arch_api::PageTableFrames;
use tairix_kernel_iommu_api::{
    drain_in_batches, reach_bits, wait_for, wait_within, Access, Binding, Bindings, Block, Clock,
    Command, CommandQueue, DomainId, DomainMap, Fault, FaultBatch, FaultRoute, FrameRun, Ids,
    InterruptRemapping, InterruptSource, InterruptTarget, Invalidator, IoPageTable, IommuError,
    IommuUnit, PageSpan, QueueRegisters, Reach, Remapped, Table, TableMemory, UnitFunction,
    UnitProfile, FAULT_QUEUE_RECORDS, MESSAGE_WINDOW, TABLE_BYTES,
};
use tairix_sync::{SpinLock, SpinLockGuard};

use crate::format::{HostTables, DTE_WORDS, EVENT_WORDS};
use crate::regs::Features;

/// The match key discovery gives an AMD-Vi unit and the kernel binds this
/// family to: the one definition both sides use.
pub const COMPATIBLE: &[u8] = b"amd,iommu";

/// Device ids one segment has. Each gets an entry that blocks it: a request
/// from an id past a shorter table's end is not one the format promises to
/// refuse.
const DEVICES: usize = 1 << 16;

/// Frames the device table spans.
const DEVICE_TABLE_FRAMES: usize = DEVICES * DTE_WORDS * 8 / TABLE_BYTES;

const _: () = assert!(DEVICE_TABLE_FRAMES.is_power_of_two());

/// Levels every domain's tables walk, which every unit with host
/// translation can.
const LEVELS: u32 = 4;

/// Domain id 0 is never handed out: blocked devices' entries name it.
const FIRST_DOMAIN: u32 = 1;

/// Domain ids are sixteen bits.
const DOMAIN_IDS: u32 = 1 << 16;

/// `log2` of the entries one remapping table holds: what an IO-APIC's vector
/// field indexes, and few enough that an MSI's index leaves the data bits
/// naming a delivery mode clear.
const INTERRUPT_TABLE_LENGTH: u32 = 8;

/// The window below 1 TiB an AMD fabric reserves for itself, claimed before
/// translation.
const FABRIC_WINDOW: Range<u64> = 0x00FD_0000_0000..0x0100_0000_0000;

/// IOVA windows the fabric claims before translation.
static RESERVED: [Range<u64>; 2] = [MESSAGE_WINDOW, FABRIC_WINDOW];

/// Records one event log frame holds.
const EVENT_SLOTS: usize = TABLE_BYTES / (8 * EVENT_WORDS);

const _: () = assert!(
    EVENT_SLOTS <= FAULT_QUEUE_RECORDS as usize,
    "the event log holds no more records than the containment bound allows"
);

/// Bits 51:12 of an entry name a frame, so no table can reach past them.
const ENTRY_ADDRESS_BITS: u32 = 52;

/// How long a drain waits for a record the unit announced to land: one lands
/// within a bus transaction of its tail, where an erratum lets it land late.
const RECORD_LANDING_NS: u64 = 1_000_000;

/// What a domain translates between: its four levels' reach, onto every
/// address an entry names.
const REACH: Reach = Reach {
    input_bits: reach_bits(LEVELS),
    output_bits: ENTRY_ADDRESS_BITS,
};

/// One AMD-Vi unit.
///
/// Its device table, command buffer, event log and remapping tables are
/// never freed once the unit was pointed at them: nothing proves it stopped
/// reading them. Its state is split so a domain's maps and syncs wait on
/// neither another domain nor a device's attach: the lifecycle lock, then
/// the domain map and a domain, then the command buffer's ring, then the
/// register window, never the reverse.
pub struct AmdViUnit<'f, R: Registers> {
    memory: TableMemory<'f>,
    clock: &'f dyn Clock,
    features: Features,
    /// The PCI function the unit is and its node address, through whose MSI
    /// it raises its faults.
    function: Option<(&'f dyn UnitFunction, u32)>,
    /// It may cache an entry it found absent (`NpCache`), so a map must be
    /// flushed too. Assumed unless its capability header says otherwise.
    caches_misses: bool,
    regs: Regs<R>,
    queue: CommandQueue,
    lifecycle: SpinLock<Lifecycle>,
    domains: DomainMap<IoPageTable<'f, HostTables>>,
    /// Translation is on, or about to be. Invalidations wait for it: until
    /// then nothing the unit caches is used, and the flush that turns it on
    /// drops it all.
    enabled: AtomicBool,
    /// The event log, which the one drain at a time reads and blanks.
    events: SpinLock<Table>,
}

/// The register window, held for one access or one handshake, so a
/// read-modify-write of `CONTROL` — which the queue's recovery, the event
/// log's restart and the lifecycle all change — is never interleaved.
struct Regs<R>(SpinLock<R>);

impl<R> Regs<R> {
    fn lock(&self) -> SpinLockGuard<'_, R> {
        self.0.lock()
    }
}

/// The PCI capability an AMD-Vi unit describes itself in.
const SECURE_DEVICE_CAPABILITY: u8 = 0x0F;

/// `NpCache` in that capability's header.
const CAPABILITY_NP_CACHE: u32 = 1 << 26;

/// `CapType` in that capability's header, and the value an IOMMU's carries.
const CAPABILITY_TYPE: u32 = 0b111 << 16;
const CAPABILITY_TYPE_IOMMU: u32 = 0b011 << 16;

/// Whether the unit at `function` may cache an absent entry: unless its
/// IOMMU capability header can be read and says not.
fn caches_misses(function: Option<(&dyn UnitFunction, u32)>) -> bool {
    !matches!(
        function.map(|(function, at)| function.capability_header(at, SECURE_DEVICE_CAPABILITY)),
        Some(Ok(Some(header)))
            if header & CAPABILITY_TYPE == CAPABILITY_TYPE_IOMMU
                && header & CAPABILITY_NP_CACHE == 0
    )
}

/// What attaching a device, silencing one and remapping interrupts change.
struct Lifecycle {
    devices: Block,
    /// What each device translates through.
    bindings: Bindings,
    ids: Ids,
    remap: Option<Remap>,
}

/// Interrupt remapping: a table per source, named in the entries of the
/// devices its interrupts arrive as.
struct Remap {
    extended: bool,
    enabled: bool,
    /// By the id their entries are handed out under.
    tables: Vec<InterruptTable>,
}

struct InterruptTable {
    /// The device ids whose interrupts it delivers.
    devices: Range<usize>,
    memory: Table,
    ids: Ids,
}

/// The device ids `source`'s interrupts can arrive as.
fn devices_of(source: InterruptSource) -> Range<usize> {
    match source {
        InterruptSource::Requester(id) => usize::from(id)..usize::from(id) + 1,
        InterruptSource::Buses { first, last } => {
            (usize::from(first) << 8)..((usize::from(last) + 1) << 8)
        }
    }
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

fn device_ids(devices: Range<usize>) -> impl Iterator<Item = u16> {
    devices.filter_map(|device| u16::try_from(device).ok())
}

fn update_control(regs: &impl Registers, set: u64, clear: u64) -> Result<(), IommuError> {
    let control = regs.read64(regs::CONTROL)?;
    regs.write64(regs::CONTROL, (control | set) & !clear)
}

impl<'f, R: Registers> AmdViUnit<'f, R> {
    /// Take over the unit behind `regs`, every device's entry blocking, its
    /// command buffer and event log running, and translation off — so a
    /// device's DMA passes untranslated until [`IommuUnit::enable`] turns it
    /// on (`plans/OPEN-DEFECTS.md`). `function` is the PCI function the
    /// unit is and its node address, through which it raises its faults.
    ///
    /// The unit is told to snoop the CPU's caches, as every AMD-Vi unit can,
    /// so its tables are never written back.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a unit this family cannot drive: one
    /// without host translation, or whose registers reach past `regs`.
    /// [`IommuError::Exhausted`] when its tables cannot be had, and the
    /// unit's own errors or timeouts.
    pub fn new(
        regs: R,
        frames: &'f dyn PageTableFrames,
        clock: &'f dyn Clock,
        function: Option<(&'f dyn UnitFunction, u32)>,
    ) -> Result<Self, IommuError> {
        if regs.window_len() < regs::WINDOW {
            return Err(IommuError::OutOfRange);
        }
        let features = Features(regs.read64(regs::FEATURES)?);
        if !features.host_translation() {
            return Err(IommuError::OutOfRange);
        }
        let memory = TableMemory::new(frames, None);
        let ids = Ids::new(FIRST_DOMAIN, DOMAIN_IDS);
        let (devices, queue, events) = Self::tables(&memory)?;
        let unit = Self {
            memory,
            clock,
            features,
            function,
            caches_misses: caches_misses(function),
            regs: Regs(SpinLock::new(regs)),
            queue,
            lifecycle: SpinLock::new(Lifecycle {
                devices,
                bindings: Bindings::new(),
                ids,
                remap: None,
            }),
            domains: DomainMap::new(),
            enabled: AtomicBool::new(false),
            events: SpinLock::new(events),
        };
        // From here the unit may hold the tables' addresses, so a failure
        // keeps them.
        unit.take_over()?;
        Ok(unit)
    }

    /// The device table, every entry blocking, the command queue and the
    /// event log; given back on a failure, since no unit was pointed at them.
    fn tables(memory: &TableMemory<'f>) -> Result<(Block, CommandQueue, Table), IommuError> {
        let events = memory.alloc()?;
        let queue = match CommandQueue::new(memory) {
            Ok(queue) => queue,
            Err(err) => {
                memory.free(events);
                return Err(err);
            }
        };
        let devices = memory
            .alloc_block(DEVICE_TABLE_FRAMES.trailing_zeros())
            .and_then(|devices| {
                let blocked = (0..DEVICES).try_for_each(|device| {
                    memory.write_block(&devices, DTE_WORDS * device, format::DTE_BLOCKED)
                });
                match blocked {
                    Ok(()) => Ok(devices),
                    Err(err) => {
                        memory.free_block(devices);
                        Err(err)
                    }
                }
            });
        match devices {
            Ok(devices) => Ok((devices, queue, events)),
            Err(err) => {
                queue.release(memory);
                memory.free(events);
                Err(err)
            }
        }
    }

    fn take_over(&self) -> Result<(), IommuError> {
        let devices = self.lifecycle.lock().devices.phys();
        let events = self.events.lock().phys();
        let regs = self.regs.lock();
        // Firmware may have left the unit translating, or its command buffer
        // or event log running, on tables of its own: all of it stops before
        // anything of ours is installed.
        regs.write64(regs::CONTROL, 0)?;
        let running = regs::STATUS_COMMANDS_RUNNING | regs::STATUS_EVENT_LOG_RUNNING;
        wait_for(self.clock, || Ok(regs.read64(regs::STATUS)? & running == 0))?;
        // An exclusion range passes DMA to it untranslated.
        regs.write64(regs::EXCLUSION_BASE, 0)?;
        regs.write64(regs::EXCLUSION_LIMIT, 0)?;
        regs.write64(
            regs::DEVICE_TABLE,
            regs::device_table(devices, DEVICE_TABLE_FRAMES),
        )?;
        regs.write64(
            regs::COMMAND_BUFFER,
            regs::ring(self.queue.ring(), CommandQueue::SLOTS),
        )?;
        regs.write64(regs::COMMAND_HEAD, 0)?;
        regs.write64(regs::COMMAND_TAIL, 0)?;
        // A status firmware left set keeps the event log's base from being
        // taken, and raises no interrupt for the next event.
        regs.write64(regs::STATUS, regs::STATUS_CLEAR)?;
        regs.write64(regs::EVENT_LOG, regs::ring(events, EVENT_SLOTS))?;
        regs.write64(regs::EVENT_HEAD, 0)?;
        regs.write64(regs::EVENT_TAIL, 0)?;
        regs.write64(
            regs::CONTROL,
            regs::CONTROL_COHERENT
                | regs::CONTROL_TIMEOUT_1S
                | regs::CONTROL_COMMANDS
                | regs::CONTROL_EVENT_LOG,
        )
    }

    fn invalidator(&self) -> Invalidator<'_> {
        Invalidator {
            queue: &self.queue,
            memory: self.memory,
            clock: self.clock,
            regs: &self.regs,
            completion: format::completion_wait,
        }
    }

    /// Install into `domain`'s tables what `install` maps from `iova`, and
    /// flush it once where the unit may cache an entry it found absent; one
    /// that caches none needs nothing, every entry written having been
    /// absent. A refusal takes it back, since the caller frees the frames
    /// once this fails.
    fn publish_mapped(
        &self,
        domain: DomainId,
        iova: u64,
        install: impl FnOnce(&mut IoPageTable<'f, HostTables>) -> Result<u64, IommuError>,
    ) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        self.domains.map_published(
            u32::from(id),
            iova,
            |table| table,
            install,
            |mapped| {
                if !self.caches_misses {
                    return Ok(());
                }
                let flush = PageSpan::of(iova, mapped)
                    .ok()
                    .and_then(|pages| format::invalidate_range(id, pages))
                    .unwrap_or(format::invalidate_domain(id));
                self.invalidate([flush])
            },
        )
    }

    /// Run `commands` and wait for the unit to confirm them. Before
    /// translation is on nothing is run: the flush that turns it on drops
    /// whatever the unit cached.
    fn invalidate(&self, commands: impl IntoIterator<Item = Command>) -> Result<(), IommuError> {
        if !self.enabled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.invalidator().run(commands)
    }

    /// Drop everything the unit caches: at once where it can, else every
    /// device's entry and remapping entries and every domain id, since what
    /// firmware left cached under an id survives into its next owner here.
    fn flush_all(&self) -> Result<(), IommuError> {
        if !self.enabled.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.features.invalidate_all() {
            return self.invalidator().run([format::INVALIDATE_EVERYTHING]);
        }
        let devices = (0..=u16::MAX).flat_map(|device| {
            [
                format::invalidate_device(device),
                format::invalidate_interrupts(device),
            ]
        });
        let domains = (0..=u16::MAX).map(format::invalidate_domain);
        self.invalidator().run(devices.chain(domains))
    }

    /// Write device `device`'s DMA words, ordered so a unit reading the entry
    /// between the stores sees it blocked or whole: the domain before the
    /// translation using it, the block before the domain it drops.
    fn write_device(
        &self,
        life: &Lifecycle,
        device: u16,
        translation: u64,
        domain: u64,
    ) -> Result<(), IommuError> {
        let at = DTE_WORDS * usize::from(device);
        let (first, second) = if translation == format::DTE_BLOCKED {
            ((at, translation), (at + 1, domain))
        } else {
            ((at + 1, domain), (at, translation))
        };
        self.memory.write_block(&life.devices, first.0, first.1)?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write_block(&life.devices, second.0, second.1)
    }

    /// Block `device` and confirm the unit kept none of its translation. One
    /// the unit cannot confirm keeps its domain, so the domain's tables
    /// outlive any walk the unit still holds and a later detach can try
    /// again; its silence ends with its entry, confirmed or not.
    fn detach(&self, life: &mut Lifecycle, device: u16) -> Result<(), IommuError> {
        let stream = u32::from(device);
        let tag = match life.bindings.get(stream) {
            Some(Binding::Domain(id)) => u16::try_from(id).map_err(|_| IommuError::Hardware)?,
            Some(Binding::Silenced) => 0,
            // Blocked already: only the confirmation is owed.
            None => match life.bindings.held(stream) {
                Some(held) => u16::try_from(held).map_err(|_| IommuError::Hardware)?,
                None => return Ok(()),
            },
        };
        if life.bindings.get(stream).is_some() {
            self.write_device(
                life,
                device,
                format::DTE_BLOCKED,
                format::dte_domain(0, false),
            )
            .map_err(|_| IommuError::Unconfirmed)?;
            life.bindings.unbind(stream);
            life.bindings.end_silence(stream);
        }
        self.invalidate([
            format::invalidate_device(device),
            format::invalidate_domain(tag),
        ])
        .map_err(|_| IommuError::Unconfirmed)?;
        life.bindings.release(stream);
        Ok(())
    }

    /// Record `slot` of the event log. A unit may move its tail before the
    /// record lands, so one still blank is waited for, and skipped past the
    /// budget.
    ///
    /// The drain holds the event log meanwhile, so it waits for the first
    /// record it misses alone: `patient` goes false once that wait expires,
    /// and the records after it are read once each.
    fn read_event(
        &self,
        events: &Table,
        slot: usize,
        patient: &mut bool,
    ) -> Option<[u64; EVENT_WORDS]> {
        let mut record = [0; EVENT_WORDS];
        let mut landed = || {
            record = [
                self.memory.read(events, EVENT_WORDS * slot)?,
                self.memory.read(events, EVENT_WORDS * slot + 1)?,
            ];
            Ok(format::event_code(record) != 0)
        };
        if !landed().ok()? {
            if !*patient {
                return None;
            }
            if wait_within(self.clock, RECORD_LANDING_NS, landed).is_err() {
                *patient = false;
                return None;
            }
        }
        Some(record)
    }

    /// Move pending event records into `batch`, oldest first, blanking each
    /// so a blank slot is one the unit has not written. Whether records may
    /// remain.
    fn take_events(&self, batch: &mut FaultBatch) -> bool {
        let events = self.events.lock();
        let read = {
            let regs = self.regs.lock();
            (regs.read64(regs::EVENT_HEAD), regs.read64(regs::EVENT_TAIL))
        };
        let (Ok(head), Ok(tail)) = read else {
            return false;
        };
        // The records the tail announces are read only after it.
        tairix_dma_barrier::dma_rmb();
        let mut head = regs::ring_index(head, EVENT_SLOTS);
        let tail = regs::ring_index(tail, EVENT_SLOTS);
        let mut patient = true;
        while head != tail {
            let record = self.read_event(&events, head, &mut patient);
            if let Some(fault) = record.and_then(format::decode_event) {
                if batch.try_push(fault).is_err() {
                    break;
                }
            }
            for word in 0..EVENT_WORDS {
                let _ = self.memory.write(&events, EVENT_WORDS * head + word, 0);
            }
            head = (head + 1) % EVENT_SLOTS;
        }
        // Every slot is read, and blanked, before it is handed back to fill.
        tairix_dma_barrier::dma_rmb();
        tairix_dma_barrier::dma_wmb();
        let regs = self.regs.lock();
        if regs
            .write64(regs::EVENT_HEAD, regs::ring_offset(head))
            .is_err()
        {
            return false;
        }
        if head != tail {
            return true;
        }
        // Cleared once the log is empty, so the next record raises the
        // interrupt again.
        let status = regs.read64(regs::STATUS).unwrap_or(0);
        let _ = regs.write64(regs::STATUS, regs::STATUS_EVENT_INTERRUPT);
        if status & regs::STATUS_EVENT_OVERFLOW != 0 {
            // A log stops at an overflow; it restarts empty, as it now is.
            head = 0;
            if self.restart_event_log(&regs).is_err() {
                return false;
            }
        }
        regs.read64(regs::EVENT_TAIL)
            .is_ok_and(|tail| regs::ring_index(tail, EVENT_SLOTS) != head)
    }

    fn restart_event_log(&self, regs: &R) -> Result<(), IommuError> {
        let control = regs.read64(regs::CONTROL)?;
        regs.write64(
            regs::CONTROL,
            control & !(regs::CONTROL_EVENT_LOG | regs::CONTROL_EVENT_INTERRUPT),
        )?;
        wait_for(self.clock, || {
            Ok(regs.read64(regs::STATUS)? & regs::STATUS_EVENT_LOG_RUNNING == 0)
        })?;
        regs.write64(regs::STATUS, regs::STATUS_EVENT_OVERFLOW)?;
        regs.write64(regs::EVENT_HEAD, 0)?;
        regs.write64(regs::EVENT_TAIL, 0)?;
        regs.write64(regs::CONTROL, control)
    }
}

/// An AMD-Vi unit's command buffer registers.
impl<R: Registers> QueueRegisters for Regs<R> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(regs::ring_index(
            self.lock().read64(regs::COMMAND_HEAD)?,
            CommandQueue::SLOTS,
        ))
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        self.lock()
            .write64(regs::COMMAND_TAIL, regs::ring_offset(tail))
    }

    /// A rejected command halts the buffer with its head on it.
    fn stopped_at(&self) -> Result<Option<usize>, IommuError> {
        let regs = self.lock();
        if regs.read64(regs::CONTROL)? & regs::CONTROL_COMMANDS == 0
            || regs.read64(regs::STATUS)? & regs::STATUS_COMMANDS_RUNNING != 0
        {
            return Ok(None);
        }
        Ok(Some(regs::ring_index(
            regs.read64(regs::COMMAND_HEAD)?,
            CommandQueue::SLOTS,
        )))
    }

    /// A fence takes the rejected command's place and the buffer is
    /// restarted, so the commands after it run.
    fn resume(
        &self,
        queue: &CommandQueue,
        memory: &TableMemory<'_>,
        slot: usize,
    ) -> Result<(), IommuError> {
        let regs = self.lock();
        queue.replace(memory, slot, format::FENCE)?;
        tairix_dma_barrier::dma_wmb();
        let control = regs.read64(regs::CONTROL)?;
        regs.write64(regs::CONTROL, control & !regs::CONTROL_COMMANDS)?;
        regs.write64(regs::CONTROL, control)
    }
}

fn device_id(stream: u32) -> Result<u16, IommuError> {
    u16::try_from(stream).map_err(|_| IommuError::OutOfRange)
}

impl<R: Registers> IommuUnit for AmdViUnit<'_, R> {
    fn profile(&self) -> UnitProfile {
        UnitProfile {
            tables: tairix_kernel_iommu_api::Tables::Walked(tairix_kernel_iommu_api::Stage::Second),
            reach: REACH,
            reserved: &RESERVED,
            write_only: true,
        }
    }

    fn interrupt_remapping(&self) -> Option<&dyn InterruptRemapping> {
        Some(self)
    }

    /// Turn translation on, then drop whatever the unit cached before it was
    /// taken over. Invalidations run from before it is on, so none skipped
    /// while it was off can be one it needed.
    fn enable(&self) -> Result<(), IommuError> {
        self.enabled.store(true, Ordering::Release);
        update_control(&*self.regs.lock(), regs::CONTROL_IOMMU, 0)?;
        self.flush_all()
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let table = IoPageTable::new(HostTables, LEVELS, self.memory, REACH)?;
        let mut life = self.lifecycle.lock();
        let id = life.ids.take_sixteen_bits()?;
        if let Err((err, _table)) = self.domains.insert(u32::from(id), table) {
            life.ids.release(u32::from(id), true);
            return Err(err);
        }
        Ok(DomainId(u32::from(id)))
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut life = self.lifecycle.lock();
        if !self.domains.contains(u32::from(id)) {
            return Err(IommuError::OutOfRange);
        }
        if life.bindings.holders(u32::from(id)) != 0 {
            return Err(IommuError::DomainBusy);
        }
        // Nothing the unit cached for the id may outlive its tables, or
        // survive into the id's next owner.
        self.invalidate([format::invalidate_domain(id)])
            .map_err(|_| IommuError::Unconfirmed)?;
        self.domains.remove(u32::from(id));
        life.ids.release(u32::from(id), true);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let device = device_id(stream)?;
        let id = domain.sixteen_bits()?;
        let mut life = self.lifecycle.lock();
        let root = self.domains.with(u32::from(id), |table| Ok(table.root()))?;
        let Some(reserved) = life.bindings.prepare_attach(stream, u32::from(id))? else {
            return Ok(());
        };
        // A silenced stream may take an owner: it is blocked either way.
        self.detach(&mut life, device)?;
        self.write_device(
            &life,
            device,
            format::dte_translated(root, LEVELS),
            format::dte_domain(id, false),
        )?;
        life.bindings.hold(reserved, stream, u32::from(id));
        // What the unit caches for the domain is its own tables', so only
        // the device's entry is stale.
        if let Err(err) = self.invalidate([format::invalidate_device(device)]) {
            // The unit may already walk the entry, so it is taken back and
            // stays counted until that is confirmed.
            self.detach(&mut life, device)?;
            return Err(err);
        }
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let device = device_id(stream)?;
        let mut life = self.lifecycle.lock();
        if life.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        self.detach(&mut life, device)
    }

    /// Its entry suppresses the device's page faults; the invalid requests a
    /// device makes no entry can suppress, so those still reach the log,
    /// where the fault service's drain bound contains what they cost.
    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let device = device_id(stream)?;
        let mut life = self.lifecycle.lock();
        if life.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        let reserved = life.bindings.reserve()?;
        self.detach(&mut life, device)?;
        self.write_device(
            &life,
            device,
            format::DTE_BLOCKED,
            format::dte_domain(0, true),
        )?;
        life.bindings.silence(reserved, stream);
        self.invalidate([format::invalidate_device(device)])
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        self.publish_mapped(domain, iova, |table| {
            table.map(iova, phys, len, access).map(|()| len)
        })
    }

    /// Every run installed, then flushed once where the unit caches misses.
    fn map_runs(
        &self,
        domain: DomainId,
        iova: u64,
        runs: &[FrameRun],
        access: Access,
    ) -> Result<(), IommuError> {
        self.publish_mapped(domain, iova, |table| table.map_runs(iova, runs, access))
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        self.domains
            .with(u32::from(id), |table| table.unmap(iova, len))
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        self.confirm(id, format::invalidate_domain(id), None)
    }

    /// One command, directories included, over the smallest span holding
    /// the range.
    fn sync_range(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        match format::invalidate_range(id, PageSpan::of(iova, len)?) {
            Some(range) => self.confirm(id, range, Some((iova, len))),
            None => self.confirm(id, format::invalidate_domain(id), None),
        }
    }

    /// The unit raises its faults through its own function's MSI, which it
    /// sends neither translated nor remapped.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        let FaultRoute::Message { address, data } = route else {
            return Err(IommuError::OutOfRange);
        };
        let (function, at) = self.function.ok_or(IommuError::Hardware)?;
        function.route_msi(at, address, data)?;
        update_control(&*self.regs.lock(), regs::CONTROL_EVENT_INTERRUPT, 0)
    }

    /// The event log's interrupt is the only one the family turns on.
    fn unroute_faults(&self) -> Result<(), IommuError> {
        let regs = self.regs.lock();
        update_control(&*regs, 0, regs::CONTROL_EVENT_INTERRUPT)?;
        if regs.read64(regs::CONTROL)? & regs::CONTROL_EVENT_INTERRUPT != 0 {
            return Err(IommuError::Hardware);
        }
        Ok(())
    }

    /// At most one log's worth of records per call.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        drain_in_batches(EVENT_SLOTS, |batch| self.take_events(batch), sink)
    }
}

impl<R: Registers> AmdViUnit<'_, R> {
    /// Confirm domain `id`'s retired tables in `range`, or all of them, gone
    /// through `flush`. Before translation is on there is nothing to confirm:
    /// the unit walked none of them.
    fn confirm(
        &self,
        id: u16,
        flush: Command,
        range: Option<(u64, u64)>,
    ) -> Result<(), IommuError> {
        if !self.enabled.load(Ordering::Acquire) {
            return self.domains.with(u32::from(id), |table| {
                table.release_retired();
                Ok(())
            });
        }
        self.domains.confirm(
            u32::from(id),
            |table| table,
            &self.invalidator(),
            |_| Ok(([flush], range)),
        )
    }
}

impl<R: Registers> AmdViUnit<'_, R> {
    /// The table `wanted`'s devices raise their interrupts through: the one
    /// any of them already uses, grown to hold them all, or a new one. A
    /// device newly covered is pointed at it at once where remapping is on.
    fn table_for(&self, life: &mut Lifecycle, wanted: &Range<usize>) -> Result<usize, IommuError> {
        let remap = life.remap.as_mut().ok_or(IommuError::OutOfRange)?;
        let mut overlapping = remap
            .tables
            .iter()
            .enumerate()
            .filter(|(_, table)| overlaps(&table.devices, wanted))
            .map(|(id, _)| id);
        let found = overlapping.next();
        // Entries of both are handed out already, so two tables cannot
        // become one.
        if overlapping.next().is_some() {
            return Err(IommuError::OutOfRange);
        }
        let id = if let Some(id) = found {
            id
        } else {
            remap
                .tables
                .try_reserve(1)
                .map_err(|_| IommuError::Exhausted)?;
            let ids = Ids::new(0, 1 << INTERRUPT_TABLE_LENGTH);
            let memory = self.memory.alloc()?;
            remap.tables.push(InterruptTable {
                devices: wanted.start..wanted.start,
                memory,
                ids,
            });
            remap.tables.len() - 1
        };
        let enabled = remap.enabled;
        let table = remap.tables.get_mut(id).ok_or(IommuError::Hardware)?;
        let had = table.devices.clone();
        let grown = had.start.min(wanted.start)..had.end.max(wanted.end);
        let added = [grown.start..had.start, had.end..grown.end];
        if !enabled || added.iter().all(Range::is_empty) {
            table.devices = grown;
            return Ok(id);
        }
        let pointer = format::dte_interrupts(Some((table.memory.phys(), INTERRUPT_TABLE_LENGTH)));
        for device in added.clone().into_iter().flatten() {
            self.memory
                .write_block(&life.devices, DTE_WORDS * device + 2, pointer)?;
        }
        self.invalidate(
            added
                .into_iter()
                .flat_map(device_ids)
                .map(format::invalidate_device),
        )?;
        // Covered only once confirmed, so a retry points them again.
        if let Some(table) = life
            .remap
            .as_mut()
            .and_then(|remap| remap.tables.get_mut(id))
        {
            table.devices = grown;
        }
        Ok(id)
    }

    /// Point every device's interrupts at its table, or let them pass, then
    /// flush outside the lifecycle lock: without a flush-everything command
    /// that is a command per device and domain.
    fn set_remapping(&self, remapping: bool) -> Result<(), IommuError> {
        {
            let mut life = self.lifecycle.lock();
            self.point_interrupts(&life, remapping)?;
            if let Some(remap) = life.remap.as_mut() {
                remap.enabled = remapping;
            }
        }
        self.flush_all()
    }

    /// Point every device's interrupt word at its table, or, without one, at
    /// refusal; or, with remapping off, let every interrupt pass.
    fn point_interrupts(&self, life: &Lifecycle, remapping: bool) -> Result<(), IommuError> {
        let remap = life.remap.as_ref().ok_or(IommuError::OutOfRange)?;
        let refused = if remapping {
            format::dte_interrupts(None)
        } else {
            format::DTE_INTERRUPTS_PASS
        };
        for device in 0..DEVICES {
            self.memory
                .write_block(&life.devices, DTE_WORDS * device + 2, refused)?;
        }
        if !remapping {
            return Ok(());
        }
        for table in &remap.tables {
            let pointer =
                format::dte_interrupts(Some((table.memory.phys(), INTERRUPT_TABLE_LENGTH)));
            for device in table.devices.clone() {
                self.memory
                    .write_block(&life.devices, DTE_WORDS * device + 2, pointer)?;
            }
        }
        Ok(())
    }

    /// Store entry `index` of `table`, the word holding its remap bit last so
    /// the unit never reads it enabled with a stale vector.
    fn write_irte(
        &self,
        table: &InterruptTable,
        index: usize,
        entry: [u64; 2],
        extended: bool,
    ) -> Result<(), IommuError> {
        if !extended {
            return self.write_narrow_irte(table, index, entry[0]);
        }
        self.memory.write(&table.memory, 2 * index + 1, entry[1])?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write(&table.memory, 2 * index, entry[0])
    }

    /// Clear entry `index` of `table`, its remap bit first.
    fn clear_irte(
        &self,
        table: &InterruptTable,
        index: usize,
        extended: bool,
    ) -> Result<(), IommuError> {
        if !extended {
            return self.write_narrow_irte(table, index, 0);
        }
        self.memory.write(&table.memory, 2 * index, 0)?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write(&table.memory, 2 * index + 1, 0)
    }

    /// Store the 32-bit entry `index` whole, the other entry sharing its word
    /// written back as it was.
    fn write_narrow_irte(
        &self,
        table: &InterruptTable,
        index: usize,
        entry: u64,
    ) -> Result<(), IommuError> {
        let word = index / 2;
        let shift = 32 * (index % 2);
        let old = self.memory.read(&table.memory, word)?;
        self.memory.write(
            &table.memory,
            word,
            (old & !(0xFFFF_FFFF << shift)) | (entry << shift),
        )
    }

    /// Drop the unit's copies of the remapping entries of every device in
    /// `devices`.
    /// Device by device where the commands fit one batch; a wider range — a
    /// bridge's buses — at once where the unit can, as each further batch
    /// is another round trip to it.
    fn invalidate_interrupts(&self, devices: Range<usize>) -> Result<(), IommuError> {
        if devices.len() >= CommandQueue::SLOTS && self.features.invalidate_all() {
            return self.invalidate([format::INVALIDATE_EVERYTHING]);
        }
        self.invalidate(device_ids(devices).map(format::invalidate_interrupts))
    }
}

/// An entry's handle: its table's id above its index within it.
fn entry_handle(table: usize, index: u8) -> Option<u32> {
    let table = u32::try_from(table).ok()?;
    (table < 1 << 24).then(|| (table << 8) | u32::from(index))
}

impl<R: Registers> InterruptRemapping for AmdViUnit<'_, R> {
    fn supports_extended(&self) -> bool {
        self.features.extended_interrupts()
    }

    /// Tables are made per source as its entries are, each as large as one
    /// source can name, so `entries` asks for nothing more.
    fn prepare_remapping(&self, extended: bool, _entries: u32) -> Result<(), IommuError> {
        if extended && !self.supports_extended() {
            return Err(IommuError::OutOfRange);
        }
        let mut life = self.lifecycle.lock();
        if life.remap.is_some() {
            return Err(IommuError::OutOfRange);
        }
        if extended {
            update_control(
                &*self.regs.lock(),
                regs::CONTROL_GUEST_APIC | regs::CONTROL_X2APIC,
                0,
            )?;
        }
        life.remap = Some(Remap {
            extended,
            enabled: false,
            tables: Vec::new(),
        });
        Ok(())
    }

    /// The unit finds an interrupt's entry by the requester id it arrives as
    /// alone, so a source's devices share one table, and the devices of a
    /// bridge's buses raise the same entries.
    fn remap_interrupt(
        &self,
        source: InterruptSource,
        target: InterruptTarget,
    ) -> Result<Remapped, IommuError> {
        if !source.names_any() {
            return Err(IommuError::OutOfRange);
        }
        let wanted = devices_of(source);
        let mut life = self.lifecycle.lock();
        let extended = life.remap.as_ref().ok_or(IommuError::OutOfRange)?.extended;
        let entry = format::irte(target, extended).ok_or(IommuError::OutOfRange)?;
        let id = self.table_for(&mut life, &wanted)?;
        let table = life
            .remap
            .as_mut()
            .and_then(|remap| remap.tables.get_mut(id))
            .ok_or(IommuError::Hardware)?;
        let taken = table.ids.take().ok_or(IommuError::Exhausted)?;
        let Some((index, handle)) = u8::try_from(taken)
            .ok()
            .and_then(|index| Some((index, entry_handle(id, index)?)))
        else {
            table.ids.release(taken, true);
            return Err(IommuError::Exhausted);
        };
        let devices = table.devices.clone();
        let slot = usize::from(index);
        let confirmed = self
            .write_irte(table, slot, entry, extended)
            .and_then(|()| self.invalidate_interrupts(devices.clone()));
        if confirmed.is_err() {
            // The unit may hold the entry already, so it is taken back and
            // never handed out again.
            let _ = self.clear_irte(table, slot, extended);
            let _ = self.invalidate_interrupts(devices);
            table.ids.release(taken, false);
            return Err(IommuError::Unconfirmed);
        }
        Ok(Remapped {
            entry: handle,
            address: MESSAGE_WINDOW.start,
            data: format::message_data(index, target.level),
            redirection: format::redirection(index, target.level),
        })
    }

    fn release_interrupt(&self, entry: u32) -> Result<(), IommuError> {
        let id = usize::try_from(entry >> 8).map_err(|_| IommuError::NotMapped)?;
        let [index, ..] = entry.to_le_bytes();
        let mut life = self.lifecycle.lock();
        let remap = life.remap.as_mut().ok_or(IommuError::NotMapped)?;
        let extended = remap.extended;
        let table = remap.tables.get_mut(id).ok_or(IommuError::NotMapped)?;
        if !table.ids.is_live(u32::from(index)) {
            return Err(IommuError::NotMapped);
        }
        let devices = table.devices.clone();
        let confirmed = self
            .clear_irte(table, usize::from(index), extended)
            .and_then(|()| self.invalidate_interrupts(devices))
            .is_ok();
        table.ids.release(u32::from(index), confirmed);
        if confirmed {
            Ok(())
        } else {
            Err(IommuError::Unconfirmed)
        }
    }

    /// From here every device's interrupts go through its source's table,
    /// or are refused.
    fn enable_remapping(&self) -> Result<(), IommuError> {
        self.set_remapping(true)
    }

    fn disable_remapping(&self) -> Result<(), IommuError> {
        self.set_remapping(false)
    }
}
