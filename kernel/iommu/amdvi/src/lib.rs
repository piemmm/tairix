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

use alloc::vec::Vec;

use tairix_arch_api::PageTableFrames;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_kernel_iommu_api::{
    drain_in_batches, reach_bits, wait_for, wait_within, Access, Binding, Bindings, Block, Clock,
    Command, CommandQueue, DomainId, Fault, FaultBatch, FaultRoute, Ids, InterruptRemapping,
    InterruptSource, InterruptTarget, IoPageTable, IommuError, IommuUnit, QueueRegisters, Reach,
    Remapped, Table, TableMemory, UnitFunction, UnitProfile, FAULT_QUEUE_RECORDS, MESSAGE_WINDOW,
    TABLE_BYTES,
};
use tairix_sync::SpinLock;

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
/// reading them.
pub struct AmdViUnit<'f, R: Registers> {
    memory: TableMemory<'f>,
    clock: &'f dyn Clock,
    features: Features,
    /// The PCI function the unit is and its node address, through whose MSI
    /// it raises its faults.
    function: Option<(&'f dyn UnitFunction, u32)>,
    state: SpinLock<State<'f, R>>,
}

struct State<'f, R> {
    regs: R,
    devices: Block,
    queue: CommandQueue,
    events: Table,
    /// Translation is on. Invalidations wait for it: until then nothing the
    /// unit caches is used, and the flush that turns it on drops it all.
    enabled: bool,
    /// Each domain's tables.
    domains: HashMap<u16, IoPageTable<'f, HostTables>, BuildFastHash>,
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
    /// Take over the unit behind `regs`, leaving every device blocked, its
    /// command buffer and event log running, and translation off:
    /// [`IommuUnit::enable`] turns it on. `function` is the PCI function the
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
            state: SpinLock::new(State {
                regs,
                devices,
                queue,
                events,
                enabled: false,
                domains: HashMap::with_hasher(BuildFastHash::new()),
                bindings: Bindings::new(),
                ids,
                remap: None,
            }),
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
        let state = self.state.lock();
        let regs = &state.regs;
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
            regs::device_table(state.devices.phys(), DEVICE_TABLE_FRAMES),
        )?;
        regs.write64(
            regs::COMMAND_BUFFER,
            regs::ring(state.queue.ring(), CommandQueue::SLOTS),
        )?;
        regs.write64(regs::COMMAND_HEAD, 0)?;
        regs.write64(regs::COMMAND_TAIL, 0)?;
        // A status firmware left set keeps the event log's base from being
        // taken, and raises no interrupt for the next event.
        regs.write64(regs::STATUS, regs::STATUS_CLEAR)?;
        regs.write64(
            regs::EVENT_LOG,
            regs::ring(state.events.phys(), EVENT_SLOTS),
        )?;
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

    fn run(
        &self,
        regs: &R,
        queue: &mut CommandQueue,
        commands: impl IntoIterator<Item = Command>,
    ) -> Result<(), IommuError> {
        queue.run(
            &self.memory,
            self.clock,
            &Commands(regs),
            commands,
            format::completion_wait,
        )
    }

    /// Run `commands` and wait for the unit to confirm them. Before
    /// translation is on nothing is run: the flush that turns it on drops
    /// whatever the unit cached.
    fn invalidate(
        &self,
        state: &mut State<'f, R>,
        commands: impl IntoIterator<Item = Command>,
    ) -> Result<(), IommuError> {
        if !state.enabled {
            return Ok(());
        }
        let State { regs, queue, .. } = state;
        self.run(regs, queue, commands)
    }

    /// Drop everything the unit caches: at once where it can, else every
    /// device's entry and remapping entries and every domain it was told of.
    fn flush_all(&self, state: &mut State<'f, R>) -> Result<(), IommuError> {
        if !state.enabled {
            return Ok(());
        }
        let State {
            regs,
            queue,
            domains,
            ..
        } = state;
        if self.features.invalidate_all() {
            return self.run(regs, queue, [format::INVALIDATE_EVERYTHING]);
        }
        let devices = (0..=u16::MAX).flat_map(|device| {
            [
                format::invalidate_device(device),
                format::invalidate_interrupts(device),
            ]
        });
        let domains = core::iter::once(0)
            .chain(domains.keys().copied())
            .map(format::invalidate_domain);
        self.run(regs, queue, devices.chain(domains))
    }

    /// Write device `device`'s DMA words, ordered so a unit reading the entry
    /// between the stores sees it blocked or whole: the domain before the
    /// translation using it, the block before the domain it drops.
    fn write_device(
        &self,
        state: &State<'f, R>,
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
        self.memory.write_block(&state.devices, first.0, first.1)?;
        tairix_dma_barrier::dma_wmb();
        self.memory.write_block(&state.devices, second.0, second.1)
    }

    /// Block `device` and confirm the unit kept none of its translation. One
    /// the unit cannot confirm keeps its domain, so the domain's tables
    /// outlive any walk the unit still holds and a later detach can try
    /// again; its silence ends with its entry, confirmed or not.
    fn detach(&self, state: &mut State<'f, R>, device: u16) -> Result<(), IommuError> {
        let stream = u32::from(device);
        let tag = match state.bindings.get(stream) {
            Some(Binding::Domain(id)) => u16::try_from(id).map_err(|_| IommuError::Hardware)?,
            Some(Binding::Silenced) => 0,
            // Blocked already: only the confirmation is owed.
            None => match state.bindings.held(stream) {
                Some(held) => u16::try_from(held).map_err(|_| IommuError::Hardware)?,
                None => return Ok(()),
            },
        };
        if state.bindings.get(stream).is_some() {
            self.write_device(
                state,
                device,
                format::DTE_BLOCKED,
                format::dte_domain(0, false),
            )
            .map_err(|_| IommuError::Unconfirmed)?;
            state.bindings.unbind(stream);
            state.bindings.end_silence(stream);
        }
        self.invalidate(
            state,
            [
                format::invalidate_device(device),
                format::invalidate_domain(tag),
            ],
        )
        .map_err(|_| IommuError::Unconfirmed)?;
        state.bindings.release(stream);
        Ok(())
    }

    /// Record `slot` of the event log. A unit may move its tail before the
    /// record lands, so one still blank is waited for, and skipped past the
    /// budget.
    ///
    /// The unit holds its lock meanwhile, so a drain waits for the first
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
        let state = self.state.lock();
        let regs = &state.regs;
        let (Ok(head), Ok(tail)) = (regs.read64(regs::EVENT_HEAD), regs.read64(regs::EVENT_TAIL))
        else {
            return false;
        };
        // The records the tail announces are read only after it.
        tairix_dma_barrier::dma_rmb();
        let mut head = regs::ring_index(head, EVENT_SLOTS);
        let tail = regs::ring_index(tail, EVENT_SLOTS);
        let mut patient = true;
        while head != tail {
            let record = self.read_event(&state.events, head, &mut patient);
            if let Some(fault) = record.and_then(format::decode_event) {
                if batch.try_push(fault).is_err() {
                    break;
                }
            }
            for word in 0..EVENT_WORDS {
                let _ = self
                    .memory
                    .write(&state.events, EVENT_WORDS * head + word, 0);
            }
            head = (head + 1) % EVENT_SLOTS;
        }
        // Every slot is read, and blanked, before it is handed back to fill.
        tairix_dma_barrier::dma_rmb();
        tairix_dma_barrier::dma_wmb();
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
            if self.restart_event_log(regs).is_err() {
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
struct Commands<'r, R>(&'r R);

impl<R: Registers> QueueRegisters for Commands<'_, R> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(regs::ring_index(
            self.0.read64(regs::COMMAND_HEAD)?,
            CommandQueue::SLOTS,
        ))
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        self.0.write64(regs::COMMAND_TAIL, regs::ring_offset(tail))
    }

    /// A rejected command halts the buffer with its head on it: a fence takes
    /// its place and the buffer is restarted, so the commands after it run.
    fn stopped(&self, queue: &CommandQueue, memory: &TableMemory<'_>) -> Result<bool, IommuError> {
        let control = self.0.read64(regs::CONTROL)?;
        if control & regs::CONTROL_COMMANDS == 0
            || self.0.read64(regs::STATUS)? & regs::STATUS_COMMANDS_RUNNING != 0
        {
            return Ok(false);
        }
        queue.replace(memory, self.head()?, format::FENCE)?;
        tairix_dma_barrier::dma_wmb();
        self.0
            .write64(regs::CONTROL, control & !regs::CONTROL_COMMANDS)?;
        self.0.write64(regs::CONTROL, control)?;
        Ok(true)
    }
}

fn device_id(stream: u32) -> Result<u16, IommuError> {
    u16::try_from(stream).map_err(|_| IommuError::OutOfRange)
}

impl<R: Registers> IommuUnit for AmdViUnit<'_, R> {
    fn profile(&self) -> UnitProfile {
        UnitProfile {
            stage: tairix_kernel_iommu_api::Stage::Second,
            reach: REACH,
            reserved: &RESERVED,
        }
    }

    fn interrupt_remapping(&self) -> Option<&dyn InterruptRemapping> {
        Some(self)
    }

    /// Turn translation on, then drop whatever the unit cached before it was
    /// taken over.
    fn enable(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        update_control(&state.regs, regs::CONTROL_IOMMU, 0)?;
        state.enabled = true;
        self.flush_all(&mut state)
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let table = IoPageTable::new(HostTables, LEVELS, self.memory, REACH)?;
        let mut state = self.state.lock();
        state
            .domains
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let id = state.ids.take_sixteen_bits()?;
        let _ = state.domains.try_insert(id, table);
        Ok(DomainId(u32::from(id)))
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        if !state.domains.contains_key(&id) {
            return Err(IommuError::OutOfRange);
        }
        if state.bindings.holders(u32::from(id)) != 0 {
            return Err(IommuError::DomainBusy);
        }
        // Nothing the unit cached for the id may outlive its tables, or
        // survive into the id's next owner.
        self.invalidate(&mut state, [format::invalidate_domain(id)])
            .map_err(|_| IommuError::Unconfirmed)?;
        state.domains.remove(&id);
        state.ids.release(u32::from(id), true);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let device = device_id(stream)?;
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        let root = state.domains.get(&id).ok_or(IommuError::OutOfRange)?.root();
        let Some(reserved) = state.bindings.prepare_attach(stream, u32::from(id))? else {
            return Ok(());
        };
        // A silenced stream may take an owner: it is blocked either way.
        self.detach(&mut state, device)?;
        self.write_device(
            &state,
            device,
            format::dte_translated(root, LEVELS),
            format::dte_domain(id, false),
        )?;
        state.bindings.hold(reserved, stream, u32::from(id));
        let commands = [
            format::invalidate_device(device),
            format::invalidate_domain(id),
        ];
        if let Err(err) = self.invalidate(&mut state, commands) {
            // The unit may already walk the entry, so it is taken back and
            // stays counted until that is confirmed.
            self.detach(&mut state, device)?;
            return Err(err);
        }
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let device = device_id(stream)?;
        let mut state = self.state.lock();
        if state.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        self.detach(&mut state, device)
    }

    /// Its entry suppresses the device's page faults; the invalid requests a
    /// device makes no entry can suppress, so those still reach the log,
    /// where the fault service's drain bound contains what they cost.
    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let device = device_id(stream)?;
        let mut state = self.state.lock();
        if state.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        let reserved = state.bindings.reserve()?;
        self.detach(&mut state, device)?;
        self.write_device(
            &state,
            device,
            format::DTE_BLOCKED,
            format::dte_domain(0, true),
        )?;
        state.bindings.silence(reserved, stream);
        self.invalidate(&mut state, [format::invalidate_device(device)])
    }

    /// A unit may cache an entry it found absent, so the mapped range is
    /// always flushed.
    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        state
            .domains
            .get_mut(&id)
            .ok_or(IommuError::OutOfRange)?
            .map(iova, phys, len, access)?;
        if let Err(err) = self.invalidate(&mut state, [format::invalidate_range(id, iova, len)]) {
            // The caller frees the frames once this fails, so no leaf may
            // stay behind to reach them.
            let taken_back = state
                .domains
                .get_mut(&id)
                .is_some_and(|table| table.unmap(iova, len).is_ok());
            return Err(if taken_back {
                err
            } else {
                IommuError::Unconfirmed
            });
        }
        Ok(())
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        self.state
            .lock()
            .domains
            .get_mut(&id)
            .ok_or(IommuError::OutOfRange)?
            .unmap(iova, len)
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.sixteen_bits()?;
        let mut state = self.state.lock();
        if !state.domains.contains_key(&id) {
            return Err(IommuError::OutOfRange);
        }
        self.invalidate(&mut state, [format::invalidate_domain(id)])?;
        if let Some(table) = state.domains.get_mut(&id) {
            table.release_retired();
        }
        Ok(())
    }

    /// The unit raises its faults through its own function's MSI, which it
    /// sends neither translated nor remapped.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        let FaultRoute::Message { address, data } = route else {
            return Err(IommuError::OutOfRange);
        };
        let (function, at) = self.function.ok_or(IommuError::Hardware)?;
        function.route_msi(at, address, data)?;
        let state = self.state.lock();
        update_control(&state.regs, regs::CONTROL_EVENT_INTERRUPT, 0)
    }

    /// At most one log's worth of records per call.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        drain_in_batches(EVENT_SLOTS, |batch| self.take_events(batch), sink)
    }
}

impl<'f, R: Registers> AmdViUnit<'f, R> {
    /// The table `wanted`'s devices raise their interrupts through: the one
    /// any of them already uses, grown to hold them all, or a new one. A
    /// device newly covered is pointed at it at once where remapping is on.
    fn table_for(
        &self,
        state: &mut State<'f, R>,
        wanted: &Range<usize>,
    ) -> Result<usize, IommuError> {
        let remap = state.remap.as_mut().ok_or(IommuError::OutOfRange)?;
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
                .write_block(&state.devices, DTE_WORDS * device + 2, pointer)?;
        }
        self.invalidate(
            state,
            added
                .into_iter()
                .flat_map(device_ids)
                .map(format::invalidate_device),
        )?;
        // Covered only once confirmed, so a retry points them again.
        if let Some(table) = state
            .remap
            .as_mut()
            .and_then(|remap| remap.tables.get_mut(id))
        {
            table.devices = grown;
        }
        Ok(id)
    }

    /// Point every device's interrupt word at its table, or, without one, at
    /// refusal; or, with remapping off, let every interrupt pass.
    fn point_interrupts(&self, state: &State<'f, R>, remapping: bool) -> Result<(), IommuError> {
        let remap = state.remap.as_ref().ok_or(IommuError::OutOfRange)?;
        let refused = if remapping {
            format::dte_interrupts(None)
        } else {
            format::DTE_INTERRUPTS_PASS
        };
        for device in 0..DEVICES {
            self.memory
                .write_block(&state.devices, DTE_WORDS * device + 2, refused)?;
        }
        if !remapping {
            return Ok(());
        }
        for table in &remap.tables {
            let pointer =
                format::dte_interrupts(Some((table.memory.phys(), INTERRUPT_TABLE_LENGTH)));
            for device in table.devices.clone() {
                self.memory
                    .write_block(&state.devices, DTE_WORDS * device + 2, pointer)?;
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
    fn invalidate_interrupts(
        &self,
        state: &mut State<'f, R>,
        devices: Range<usize>,
    ) -> Result<(), IommuError> {
        if devices.len() >= CommandQueue::SLOTS && self.features.invalidate_all() {
            return self.invalidate(state, [format::INVALIDATE_EVERYTHING]);
        }
        self.invalidate(
            state,
            device_ids(devices).map(format::invalidate_interrupts),
        )
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
        let mut state = self.state.lock();
        if state.remap.is_some() {
            return Err(IommuError::OutOfRange);
        }
        if extended {
            update_control(
                &state.regs,
                regs::CONTROL_GUEST_APIC | regs::CONTROL_X2APIC,
                0,
            )?;
        }
        state.remap = Some(Remap {
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
        let mut state = self.state.lock();
        let extended = state.remap.as_ref().ok_or(IommuError::OutOfRange)?.extended;
        let entry = format::irte(target, extended).ok_or(IommuError::OutOfRange)?;
        let id = self.table_for(&mut state, &wanted)?;
        let table = state
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
            .and_then(|()| self.invalidate_interrupts(&mut state, devices.clone()));
        if confirmed.is_err() {
            // The unit may hold the entry already, so it is taken back and
            // never handed out again.
            if let Some(table) = state.remap.as_ref().and_then(|remap| remap.tables.get(id)) {
                let _ = self.clear_irte(table, slot, extended);
            }
            let _ = self.invalidate_interrupts(&mut state, devices);
            if let Some(table) = state
                .remap
                .as_mut()
                .and_then(|remap| remap.tables.get_mut(id))
            {
                table.ids.release(taken, false);
            }
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
        let mut state = self.state.lock();
        let remap = state.remap.as_ref().ok_or(IommuError::NotMapped)?;
        let extended = remap.extended;
        let table = remap.tables.get(id).ok_or(IommuError::NotMapped)?;
        if !table.ids.is_live(u32::from(index)) {
            return Err(IommuError::NotMapped);
        }
        let devices = table.devices.clone();
        let confirmed = self
            .clear_irte(table, usize::from(index), extended)
            .and_then(|()| self.invalidate_interrupts(&mut state, devices))
            .is_ok();
        if let Some(table) = state
            .remap
            .as_mut()
            .and_then(|remap| remap.tables.get_mut(id))
        {
            table.ids.release(u32::from(index), confirmed);
        }
        if confirmed {
            Ok(())
        } else {
            Err(IommuError::Unconfirmed)
        }
    }

    /// From here every device's interrupts go through its source's table,
    /// or are refused.
    fn enable_remapping(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        self.point_interrupts(&state, true)?;
        if let Some(remap) = state.remap.as_mut() {
            remap.enabled = true;
        }
        self.flush_all(&mut state)
    }

    fn disable_remapping(&self) -> Result<(), IommuError> {
        let mut state = self.state.lock();
        self.point_interrupts(&state, false)?;
        if let Some(remap) = state.remap.as_mut() {
            remap.enabled = false;
        }
        self.flush_all(&mut state)
    }
}
