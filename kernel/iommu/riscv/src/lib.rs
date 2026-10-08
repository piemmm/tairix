//! The RISC-V IOMMU: one unit, driven through its device directory, second
//! stage translation where it has it and first stage otherwise, its command
//! queue and its fault queue.
//!
//! The unit is taken over with its directory off, so every inbound
//! transaction is refused until [`IommuUnit::enable`] points it at a
//! directory whose contexts are all invalid: from then on a device's DMA is
//! refused and recorded until its context names a domain. Every removal is
//! confirmed by an `IOFENCE.C` before it is reported done.
//!
//! Reference: The RISC-V IOMMU Architecture Specification, version 1.0. The
//! design and its staging are `plans/IOMMU.md` IOM16.

#![no_std]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod format;
mod regs;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use regs::{Capabilities, CAPABILITIES};
pub use tairix_kernel_iommu_api::Registers;

use tairix_arch_api::PageTableFrames;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_kernel_iommu_api::{
    drain_in_batches, reach_bits, wait_for, Access, Binding, Bindings, Block, Clock, Command,
    CommandQueue, DomainId, DomainMap, Fault, FaultBatch, FaultReason, FaultRoute, Ids,
    Invalidator, IoPageTable, IommuError, IommuUnit, MessageFiles, Notice, PageSpan, PteFormat,
    QueueRegisters, Reach, Signalling, Stage, Table, TableCoherence, TableMemory, Tables,
    UnitProfile, FAULT_QUEUE_RECORDS, IO_PAGE_SHIFT, IO_PAGE_SIZE, MESSAGE_FILE_BYTES,
    PAGE_INVALIDATIONS, TABLE_BYTES,
};
use tairix_sync::{SpinLock, SpinLockGuard};

use crate::format::{Cause, Context, RiscvTables, CONTEXT_WORDS, FAULT_WORDS};

/// The match key discovery gives a RISC-V IOMMU and the kernel binds this
/// family to: the one definition both sides use.
pub const COMPATIBLE: &[u8] = b"riscv,iommu";

/// The place among a unit's interrupts of the line it raises its faults on:
/// the first, as the cause each line raises is software's to choose.
pub const FAULT_INTERRUPT: u32 = 0;

/// PSCID and GSCID 0 are never handed out: the silenced streams' contexts
/// use 0, and it names no domain of ours.
const FIRST_DOMAIN: u32 = 1;

/// `log2` of the frames the fault queue spans.
const FAULT_QUEUE_ORDER: u32 =
    (FAULT_QUEUE_RECORDS as usize * FAULT_WORDS * core::mem::size_of::<u64>() / TABLE_BYTES)
        .trailing_zeros();

/// Bits of physical address the unit's registers and structures name.
const ADDRESS_BITS: u32 = 56;

/// The highest interrupt vector a cause can be given.
const LAST_VECTOR: u32 = 0xF;

/// The walk modes in order: `Sv39`, `Sv48`, `Sv57`, and at the second stage
/// their `x4` forms. How deep each walks, and the field value naming it.
const MODE_LEVELS: [u32; 3] = [3, 4, 5];
const MODE_FIELD: [u64; 3] = [8, 9, 10];

/// Bits of IOVA a domain may use in `mode` at `stage`: a first-stage address
/// is sign-extended, so only the half below its top bit is usable without the
/// upper bits set.
fn usable_bits(stage: Stage, mode: usize) -> u32 {
    let reach = reach_bits(MODE_LEVELS[mode]) + RiscvTables { stage }.root_order();
    match stage {
        Stage::First => reach - 1,
        Stage::Second => reach,
    }
}

/// Physical address bits an MSI page-table entry can name a file or a
/// notice's page by.
const MESSAGE_ADDRESS_BITS: u32 = 56;

/// One RISC-V IOMMU.
///
/// Its directory and queues are never freed: nothing proves the unit stopped
/// reading them. Its state is split so a domain's maps and syncs wait on
/// neither another domain nor a device's attach: the fault queue, then the
/// lifecycle lock, then the domain map and a domain, then the command
/// queue's ring, then the register window, never the reverse.
pub struct RiscvUnit<'f, R: Registers> {
    memory: TableMemory<'f>,
    clock: &'f dyn Clock,
    stage: Stage,
    /// Which of [`MODE_FIELD`] domains walk in.
    mode: usize,
    caps: Capabilities,
    profile: UnitProfile,
    regs: Regs<R>,
    queue: CommandQueue,
    lifecycle: SpinLock<Lifecycle<'f>>,
    domains: DomainMap<IoPageTable<'f, RiscvTables>>,
    faults: SpinLock<FaultQueue>,
}

/// The register window, held for one access or one handshake.
struct Regs<R>(SpinLock<R>);

impl<R> Regs<R> {
    fn lock(&self) -> SpinLockGuard<'_, R> {
        self.0.lock()
    }
}

/// What attaching a device, silencing one and confining its messages change.
struct Lifecycle<'f> {
    directory: Directory,
    /// The directory mode `ddtp` takes once translating.
    directory_mode: u64,
    /// How the unit was left signalling as it was taken over.
    signalling: Signalling,
    /// The empty tables a silenced stream's context walks, faulting
    /// unrecorded.
    silent: IoPageTable<'f, RiscvTables>,
    bindings: Bindings,
    ids: Ids,
    /// Each confined stream's MSI page table and the doorbell it recognises.
    /// Never freed: nothing proves the unit stopped caching it.
    messages: HashMap<u32, (Table, u64), BuildFastHash>,
}

/// The fault queue, which the one drain at a time reads.
struct FaultQueue {
    faults: Block,
    /// Its head as last written.
    head: u32,
}

/// The device directory: a root, and the tables below it, linked the first
/// time a device they hold is written.
struct Directory {
    root: Table,
    /// Levels from the root to the contexts.
    levels: u32,
    /// Device-id bits one table of contexts resolves.
    leaf_bits: u32,
    /// Words of one context.
    context_words: usize,
    /// The tables below the root, by the level they sit at (0 holds
    /// contexts) and the device-id bits above the ones they resolve.
    tables: HashMap<(u32, u32), Table, BuildFastHash>,
}

impl<'f, R: Registers> RiscvUnit<'f, R> {
    /// Take over the unit behind `regs`, leaving it refusing every inbound
    /// transaction, its directory of invalid contexts built and its command
    /// and fault queues running, its interrupts masked and `signalling` as
    /// it raises them, where it can. [`IommuUnit::enable`] starts
    /// translating.
    ///
    /// Tables are written back through `coherence` where the port offers it.
    /// What the unit writes, its records and completions, is read with no
    /// invalidation: the unit has no capability saying it snoops the CPUs'
    /// caches, and the kernel takes over only one whose node states it does.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a unit this family cannot drive: one of
    /// another version, with no stage of its own to translate with, fixed to
    /// big-endian or to a 32-bit guest's addresses, with a register window
    /// short of a page, holding queues smaller than this family's, or taking
    /// no directory mode. [`IommuError::Exhausted`] when its tables cannot be
    /// had, and the unit's own errors or timeouts.
    pub fn new(
        regs: R,
        frames: &'f dyn PageTableFrames,
        coherence: Option<&'f dyn TableCoherence>,
        clock: &'f dyn Clock,
        signalling: Signalling,
    ) -> Result<Self, IommuError> {
        let caps = Capabilities(regs.read64(regs::CAPABILITIES)?);
        if caps.version() >> 4 != 1 || regs.window_len() < regs::WINDOW {
            return Err(IommuError::OutOfRange);
        }
        let (stage, modes) = if caps.second_stage().contains(&true) {
            (Stage::Second, caps.second_stage())
        } else if caps.first_stage().contains(&true) {
            (Stage::First, caps.first_stage())
        } else {
            return Err(IommuError::OutOfRange);
        };
        let physical_bits = caps.physical_bits().min(ADDRESS_BITS);
        // Wide enough for an identity window anywhere the unit can reach,
        // and no wider: each mode deeper adds a level to every walk.
        let supported = || (0..MODE_LEVELS.len()).filter(|&mode| modes[mode]);
        let mode = supported()
            .find(|&mode| usable_bits(stage, mode) >= physical_bits)
            .or_else(|| supported().next_back())
            .ok_or(IommuError::OutOfRange)?;
        let ids = Ids::new(
            FIRST_DOMAIN,
            match stage {
                Stage::First => 1 << format::PSCID_BITS,
                Stage::Second => 1 << format::GSCID_BITS,
            },
        );
        let profile = UnitProfile {
            tables: Tables::Walked(stage),
            reach: Reach {
                input_bits: usable_bits(stage, mode),
                output_bits: physical_bits,
            },
            reserved: &[],
            // A writable entry that is not readable is a reserved encoding at
            // either stage.
            write_only: false,
        };
        let memory = TableMemory::new(frames, coherence);
        let silent = IoPageTable::new(
            RiscvTables { stage },
            MODE_LEVELS[mode],
            memory,
            profile.reach,
        )?;
        let directory = Directory::new(&memory, caps.extended_contexts())?;
        let queue = match CommandQueue::new(&memory) {
            Ok(queue) => queue,
            Err(err) => {
                memory.free(directory.root);
                return Err(err);
            }
        };
        let faults = match memory.alloc_block(FAULT_QUEUE_ORDER) {
            Ok(faults) => faults,
            Err(err) => {
                memory.free(directory.root);
                queue.release(&memory);
                return Err(err);
            }
        };
        let unit = Self {
            memory,
            clock,
            stage,
            mode,
            caps,
            profile,
            regs: Regs(SpinLock::new(regs)),
            queue,
            lifecycle: SpinLock::new(Lifecycle {
                directory,
                directory_mode: regs::DDTP_OFF,
                signalling,
                silent,
                bindings: Bindings::new(),
                ids,
                messages: HashMap::with_hasher(BuildFastHash::new()),
            }),
            domains: DomainMap::new(),
            faults: SpinLock::new(FaultQueue { faults, head: 0 }),
        };
        // From here the unit may hold the queues' addresses, so a failure
        // keeps them.
        unit.take_over()?;
        Ok(unit)
    }

    fn take_over(&self) -> Result<(), IommuError> {
        let mut life = self.lifecycle.lock();
        let faults = self.faults.lock().faults.phys();
        let regs = self.regs.lock();
        // Firmware may have left the unit translating, or passing everything
        // through; from here on nothing inbound is allowed.
        self.directory(&regs, regs::DDTP_OFF)?;
        // An error firmware left standing would stop our first batch.
        for (control, errors) in [
            (regs::CQCSR, regs::CQCSR_ERRORS),
            (regs::FQCSR, regs::FQCSR_ERRORS),
            (regs::PQCSR, regs::FQCSR_ERRORS),
        ] {
            self.queue_control(&regs, control, errors)?;
        }
        // A counter overflowing would raise a cause no one serves.
        regs.write32(regs::IOCOUNTINH, u32::MAX)?;
        regs.write32(regs::IPSR, regs::IPSR_ALL)?;
        // The unit is off and its queues stopped: the only time `fctl` may
        // change.
        let wanted = match life.signalling {
            Signalling::Wired => regs::FCTL_WSI,
            Signalling::Message => 0,
        };
        let fctl = regs.read32(regs::FCTL)?;
        let normal = fctl & !(regs::FCTL_BE | regs::FCTL_GXL | regs::FCTL_WSI) | wanted;
        if fctl != normal {
            regs.write32(regs::FCTL, normal)?;
        }
        let fctl = regs.read32(regs::FCTL)?;
        if fctl & (regs::FCTL_BE | regs::FCTL_GXL) != 0 {
            return Err(IommuError::OutOfRange);
        }
        let signalling = if fctl & regs::FCTL_WSI != 0 {
            Signalling::Wired
        } else {
            Signalling::Message
        };
        let slots = CommandQueue::SLOTS.trailing_zeros();
        Self::place_queue(&regs, regs::CQB, queue_base(self.queue.ring(), slots))?;
        regs.write32(regs::CQT, 0)?;
        self.queue_control(&regs, regs::CQCSR, regs::QUEUE_EN)?;
        let records = FAULT_QUEUE_RECORDS.trailing_zeros();
        Self::place_queue(&regs, regs::FQB, queue_base(faults, records))?;
        regs.write32(regs::FQH, 0)?;
        self.queue_control(&regs, regs::FQCSR, regs::QUEUE_EN)?;
        drop(regs);
        life.signalling = signalling;
        // Nothing firmware's configuration left cached may be read once a
        // directory is live, the probe's below included. A stage the unit
        // lacks caches nothing, and it refuses that stage's invalidation.
        let stages = [
            (self.caps.second_stage(), format::forget_second_stage(None)),
            (self.caps.first_stage(), format::forget_first_stage(None)),
        ];
        let forget_stages = stages
            .into_iter()
            .filter(|(modes, _)| modes.contains(&true))
            .map(|(_, forget)| forget);
        self.run(core::iter::once(format::forget_context(None)).chain(forget_stages))?;
        // The deepest directory the unit takes, found against an empty root:
        // every context it reaches is invalid, so nothing passes meanwhile.
        let regs = self.regs.lock();
        let root = life.directory.root.phys();
        let mut chosen = None;
        for (levels, mode) in regs::DDTP_DEPTHS.into_iter().rev() {
            if self.directory(&regs, directory_pointer(root, mode)).is_ok() {
                chosen = Some((levels, mode));
                break;
            }
        }
        self.directory(&regs, regs::DDTP_OFF)?;
        let (levels, mode) = chosen.ok_or(IommuError::OutOfRange)?;
        life.directory.levels = levels;
        life.directory_mode = mode;
        Ok(())
    }

    /// Point a queue at `base` and confirm the unit took it whole: its size
    /// and address fields keep only what the unit can hold.
    fn place_queue(regs: &R, register: usize, base: u64) -> Result<(), IommuError> {
        regs.write64(register, base)?;
        if regs.read64(register)? != base {
            return Err(IommuError::OutOfRange);
        }
        Ok(())
    }

    /// Write `ddtp` and wait for the unit to take it: `Err` where it kept
    /// another mode.
    fn directory(&self, regs: &R, value: u64) -> Result<(), IommuError> {
        let idle = || Ok(regs.read64(regs::DDTP)? & regs::DDTP_BUSY == 0);
        wait_for(self.clock, idle)?;
        regs.write64(regs::DDTP, value)?;
        wait_for(self.clock, idle)?;
        if regs.read64(regs::DDTP)? & regs::DDTP_MODE != value & regs::DDTP_MODE {
            return Err(IommuError::OutOfRange);
        }
        Ok(())
    }

    /// Write a queue's control register and wait for the queue to be running
    /// exactly when enabled.
    fn queue_control(&self, regs: &R, control: usize, value: u32) -> Result<(), IommuError> {
        regs.write32(control, value)?;
        let on = value & regs::QUEUE_EN != 0;
        wait_for(self.clock, || {
            let now = regs.read32(control)?;
            Ok(now & regs::QUEUE_BUSY == 0 && (now & regs::QUEUE_ON != 0) == on)
        })
    }

    fn invalidator(&self) -> Invalidator<'_> {
        Invalidator {
            queue: &self.queue,
            memory: self.memory,
            clock: self.clock,
            regs: &self.regs,
            completion: format::fence,
        }
    }

    /// Queue `commands` and a fence behind them, and return once the unit
    /// confirms every one is done.
    fn run(&self, commands: impl IntoIterator<Item = Command>) -> Result<(), IommuError> {
        self.invalidator().run(commands)
    }

    /// How invalidations name domain `id`, or [`None`] for a GSCID wider
    /// than its field, which only a global invalidation reaches; `Ids` hands
    /// out none.
    fn tag(&self, id: u32) -> Option<Tag> {
        match self.stage {
            Stage::Second => u16::try_from(id).ok().map(Tag::Gscid),
            Stage::First => Some(Tag::Pscid(id)),
        }
    }

    /// What forgets every translation the unit cached for domain `id`.
    fn forget_domain(&self, id: u32) -> Command {
        match self.tag(id) {
            Some(Tag::Gscid(gscid)) => format::forget_second_stage(Some(gscid)),
            Some(Tag::Pscid(pscid)) => format::forget_first_stage(Some(pscid)),
            None => format::forget_second_stage(None),
        }
    }

    /// The context translating through `table` as `id`, or silently.
    fn context_for(
        &self,
        id: u32,
        table: &IoPageTable<'f, RiscvTables>,
        silent: bool,
    ) -> Result<Context, IommuError> {
        format::context(self.stage, id, table.root(), MODE_FIELD[self.mode], silent)
            .ok_or(IommuError::OutOfRange)
    }

    /// `stream`'s context translating through domain `id`, its messages
    /// confined where they were.
    fn domain_context(
        &self,
        life: &Lifecycle<'f>,
        stream: u32,
        id: u32,
    ) -> Result<Context, IommuError> {
        let context = self
            .domains
            .with(id, |table| self.context_for(id, table, false))?;
        Ok(match life.messages.get(&stream) {
            Some((table, doorbell)) => format::with_messages(context, table.phys(), *doorbell),
            None => context,
        })
    }

    /// Before `device`'s valid context is replaced by valid `next`, make it
    /// invalid and confirm the unit forgot it: a context's words cannot change
    /// together, and a unit reading them part-written could tag one domain's
    /// translations with another's id.
    fn break_context(
        &self,
        life: &mut Lifecycle<'f>,
        device: u32,
        next: &Context,
    ) -> Result<(), IommuError> {
        if life.bindings.get(device).is_none() || !format::is_valid(next) {
            return Ok(());
        }
        self.write_context(life, device, &[0; CONTEXT_WORDS])?;
        life.bindings.unbind(device);
        life.bindings.end_silence(device);
        self.run([format::forget_context(Some(device))])?;
        life.bindings.release(device);
        Ok(())
    }

    /// Block `device` and confirm the unit forgot what it translated
    /// through, which it holds until then. A silenced device stays silent:
    /// only an attach ends silence.
    fn block_device(&self, life: &mut Lifecycle<'f>, device: u32) -> Result<(), IommuError> {
        if !life.bindings.holds_domain(device) {
            return Ok(());
        }
        if life.bindings.get(device) != Some(Binding::Silenced) {
            self.write_context(life, device, &[0; CONTEXT_WORDS])?;
            life.bindings.unbind(device);
        }
        self.run([format::forget_context(Some(device))])?;
        life.bindings.release(device);
        Ok(())
    }

    /// Write `context` as `device`'s. The unit reads a context a word at a
    /// time in no set order, so no word a fetch already under way could pair
    /// with a valid first word changes: an invalid context is its first word
    /// alone, and a valid one's other words are confirmed with the unit before
    /// its first word makes them live.
    fn write_context(
        &self,
        life: &mut Lifecycle<'f>,
        device: u32,
        context: &Context,
    ) -> Result<(), IommuError> {
        if format::is_valid(context) {
            let (table, first, words) = life.directory.slot(&self.memory, device)?;
            for (word, value) in context.iter().enumerate().take(words).skip(1) {
                self.memory.write(table, first + word, *value)?;
            }
            self.memory.publish(table, first, words);
            tairix_dma_barrier::dma_wmb();
            self.run([format::forget_context(Some(device))])?;
        }
        let (table, first, words) = life.directory.slot(&self.memory, device)?;
        self.memory.write(table, first, context[0])?;
        self.memory.publish(table, first, words);
        Ok(())
    }

    /// Move pending records into `batch`, oldest first, and answer whether
    /// more remain. A lost record is acknowledged; the queue runs on.
    fn take_records(&self, batch: &mut FaultBatch) -> bool {
        let mut queue = self.faults.lock();
        let tail = {
            let regs = self.regs.lock();
            // Cleared before the tail is read, so a record landing after it
            // raises the interrupt again.
            if regs.write32(regs::IPSR, regs::IPSR_FIP).is_err() {
                return false;
            }
            if let Ok(control) = regs.read32(regs::FQCSR) {
                let lost = control & (regs::FQCSR_FQOF | regs::QUEUE_MF);
                if lost != 0 {
                    let _ = regs.write32(
                        regs::FQCSR,
                        (control & (regs::QUEUE_EN | regs::QUEUE_IE)) | lost,
                    );
                }
            }
            let Ok(tail) = regs.read32(regs::FQT) else {
                return false;
            };
            tail
        };
        // The records the tail announces are read only after it.
        tairix_dma_barrier::dma_rmb();
        let tail = tail % FAULT_QUEUE_RECORDS;
        let mut head = queue.head;
        while head != tail && !batch.is_full() {
            let at = head as usize * FAULT_WORDS;
            let mut words = [0u64; FAULT_WORDS];
            for (word, value) in words.iter_mut().enumerate() {
                match self.memory.read_block(&queue.faults, at + word) {
                    Ok(read) => *value = read,
                    Err(_) => return false,
                }
            }
            let _ = batch.try_push(self.fault(&format::record(&words)));
            head = (head + 1) % FAULT_QUEUE_RECORDS;
        }
        queue.head = head;
        // Every slot is read before it is handed back to the unit to fill.
        tairix_dma_barrier::dma_rmb();
        if self.regs.lock().write32(regs::FQH, head).is_err() {
            return false;
        }
        head != tail
    }

    /// The fault `record` describes, a page fault told apart by the tables of
    /// the device's domain: a page they map, for an access they do not allow,
    /// was denied; any other was unmapped.
    fn fault(&self, record: &format::Record) -> Fault {
        let (reason, addressed) = match record.cause {
            Cause::Known(reason, addressed) => (reason, addressed),
            Cause::Page => {
                let bound = self.lifecycle.lock().bindings.get(record.device);
                let mapped = match bound {
                    Some(Binding::Domain(id)) => self
                        .domains
                        .with(id, |table| Ok(table.translate(record.iova)))
                        .ok()
                        .flatten(),
                    _ => None,
                };
                let denied = mapped.is_some_and(|(_, access)| {
                    if record.write {
                        !access.write()
                    } else {
                        !access.read()
                    }
                });
                let reason = if denied {
                    FaultReason::Denied
                } else {
                    FaultReason::Unmapped
                };
                (reason, true)
            }
        };
        Fault {
            stream: record.device,
            iova: if addressed { record.iova } else { 0 },
            write: addressed && record.write,
            reason,
        }
    }
}

/// `ddtp`'s value for a directory at `root` walked in `mode`.
const fn directory_pointer(root: u64, mode: u64) -> u64 {
    (root >> IO_PAGE_SHIFT) << regs::PPN_SHIFT | mode
}

/// A queue base register's value for a ring at `ring` of `2^log2` entries.
const fn queue_base(ring: u64, log2: u32) -> u64 {
    (ring >> IO_PAGE_SHIFT) << regs::PPN_SHIFT | (log2 - 1) as u64
}

/// The command queue's registers as the shared queue reads them.
impl<R: Registers> QueueRegisters for Regs<R> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(self.lock().read32(regs::CQH)? as usize % CommandQueue::SLOTS)
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        self.lock().write32(
            regs::CQT,
            u32::try_from(tail).map_err(|_| IommuError::Hardware)?,
        )
    }

    /// A rejected command stops the queue at its head; a fault on the
    /// queue's own memory, or a command timing out, is the unit's failure.
    fn stopped_at(&self) -> Result<Option<usize>, IommuError> {
        let regs = self.lock();
        let control = regs.read32(regs::CQCSR)?;
        if control & (regs::QUEUE_MF | regs::CQCSR_CMD_TO) != 0 {
            return Err(IommuError::Hardware);
        }
        if control & regs::CQCSR_CMD_ILL == 0 {
            return Ok(None);
        }
        Ok(Some(regs.read32(regs::CQH)? as usize % CommandQueue::SLOTS))
    }

    /// A rejected command is replaced by a fence that stores nothing, and the
    /// error acknowledged, which lets the unit consume on.
    fn resume(
        &self,
        queue: &CommandQueue,
        memory: &TableMemory<'_>,
        slot: usize,
    ) -> Result<(), IommuError> {
        let regs = self.lock();
        queue.replace(memory, slot, format::fence_quietly())?;
        tairix_dma_barrier::dma_wmb();
        regs.write32(regs::CQCSR, regs::QUEUE_EN | regs::CQCSR_CMD_ILL)
    }
}

impl Directory {
    fn new(memory: &TableMemory<'_>, extended: bool) -> Result<Self, IommuError> {
        Ok(Self {
            root: memory.alloc()?,
            levels: 1,
            leaf_bits: if extended { 6 } else { 7 },
            context_words: if extended {
                CONTEXT_WORDS
            } else {
                CONTEXT_WORDS / 2
            },
            tables: HashMap::with_hasher(BuildFastHash::new()),
        })
    }

    /// `Ok` where the directory reaches `device`: one table of contexts, then
    /// nine bits a level, up to the 24 bits a device id has.
    fn covers(&self, device: u32) -> Result<(), IommuError> {
        let bits = (self.leaf_bits + 9 * (self.levels - 1)).min(24);
        if u64::from(device) >> bits != 0 {
            return Err(IommuError::OutOfRange);
        }
        Ok(())
    }

    /// Where `device`'s context lives, linking each table on the way the
    /// first time a device it holds is written: the table, its first word,
    /// and the words the context spans.
    fn slot(
        &mut self,
        memory: &TableMemory<'_>,
        device: u32,
    ) -> Result<(&Table, usize, usize), IommuError> {
        let mut parent = None;
        for level in (0..self.levels - 1).rev() {
            let shift = self.leaf_bits + 9 * level;
            let key = (level, device >> shift);
            if !self.tables.contains_key(&key) {
                self.tables
                    .try_reserve(1)
                    .map_err(|_| IommuError::Exhausted)?;
                let child = memory.alloc()?;
                let above = match parent {
                    None => &self.root,
                    Some(above) => self.tables.get(&above).ok_or(IommuError::Hardware)?,
                };
                let index = ((device >> shift) & 0x1FF) as usize;
                if let Err(err) = memory.write(above, index, format::directory_entry(child.phys()))
                {
                    memory.free(child);
                    return Err(err);
                }
                memory.publish(above, index, 1);
                let _ = self.tables.try_insert(key, child);
            }
            parent = Some(key);
        }
        let table = match parent {
            None => &self.root,
            Some(leaf) => self.tables.get(&leaf).ok_or(IommuError::Hardware)?,
        };
        let first = (device & ((1 << self.leaf_bits) - 1)) as usize * self.context_words;
        Ok((table, first, self.context_words))
    }
}

impl<R: Registers> IommuUnit for RiscvUnit<'_, R> {
    fn profile(&self) -> UnitProfile {
        self.profile
    }

    fn enable(&self) -> Result<(), IommuError> {
        let life = self.lifecycle.lock();
        let pointer = directory_pointer(life.directory.root.phys(), life.directory_mode);
        self.directory(&self.regs.lock(), pointer)
            .map_err(|_| IommuError::Hardware)
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let mut life = self.lifecycle.lock();
        let id = life.ids.take().ok_or(IommuError::Exhausted)?;
        let made = IoPageTable::new(
            RiscvTables { stage: self.stage },
            MODE_LEVELS[self.mode],
            self.memory,
            self.profile.reach,
        )
        .and_then(|table| self.domains.insert(id, table).map_err(|(err, _table)| err));
        if let Err(err) = made {
            life.ids.release(id, true);
            return Err(err);
        }
        Ok(DomainId(id))
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.0;
        let mut life = self.lifecycle.lock();
        if !self.domains.contains(id) {
            return Err(IommuError::OutOfRange);
        }
        if life.bindings.holders(id) != 0 {
            return Err(IommuError::DomainBusy);
        }
        // Nothing the unit cached for the id may outlive its tables, or
        // survive into the id's next owner.
        self.run([self.forget_domain(id)])
            .map_err(|_| IommuError::Unconfirmed)?;
        self.domains.remove(id);
        life.ids.release(id, true);
        Ok(())
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.0;
        let mut life = self.lifecycle.lock();
        life.directory.covers(stream)?;
        let context = self.domain_context(&life, stream, id)?;
        let Some(reserved) = life.bindings.prepare_attach(stream, id)? else {
            return Ok(());
        };
        self.break_context(&mut life, stream, &context)?;
        self.write_context(&mut life, stream, &context)?;
        // From the write on the unit may translate the device through the
        // domain, so it holds the domain whether or not the unit confirms.
        life.bindings.hold(reserved, stream, id);
        if let Err(err) = self.run([format::forget_context(Some(stream))]) {
            // Taken back, and held until the unit confirms it forgot.
            let _ = self.block_device(&mut life, stream);
            return Err(err);
        }
        Ok(())
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        let mut life = self.lifecycle.lock();
        life.directory.covers(stream)?;
        self.block_device(&mut life, stream)
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        let mut life = self.lifecycle.lock();
        life.directory.covers(stream)?;
        if life.bindings.get(stream) == Some(Binding::Silenced) {
            return Ok(());
        }
        let reserved = life.bindings.reserve()?;
        let context = self.context_for(0, &life.silent, true)?;
        self.break_context(&mut life, stream, &context)?;
        self.write_context(&mut life, stream, &context)?;
        // From the write the device is silent, whether or not the unit
        // confirms; a domain it still held is let go once the unit confirms.
        life.bindings.silence(reserved, stream);
        self.run([format::forget_context(Some(stream))])?;
        life.bindings.release(stream);
        Ok(())
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        // No entry lets a device write what it may not read.
        if !access.read() {
            return Err(IommuError::OutOfRange);
        }
        // The unit caches no translation it faulted on, so a new mapping
        // needs no invalidation to be used.
        self.domains
            .with(domain.0, |table| table.map(iova, phys, len, access))
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        self.domains.with(domain.0, |table| table.unmap(iova, len))
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        let id = domain.0;
        let forget = [self.forget_domain(id)];
        self.domains.confirm(
            id,
            |table| table,
            &self.invalidator(),
            |_| Ok((forget, None)),
        )
    }

    /// A page at a time while the range holds few enough pages and the unmap
    /// changed leaves alone: a page's invalidation is promised to reach only
    /// its leaf, so a range whose tables went forgets the domain.
    fn sync_range(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        let id = domain.0;
        let span = PageSpan::of(iova, len)?;
        self.domains.confirm(
            id,
            |table| table,
            &self.invalidator(),
            |table| {
                let by_page =
                    span.pages() <= PAGE_INVALIDATIONS && !table.has_retired_touching(iova, len);
                let tag = self.tag(id).filter(|_| by_page);
                let pages = tag.map(|tag| span.addresses().map(move |page| forget_page(tag, page)));
                let whole = tag.is_none().then(|| self.forget_domain(id));
                Ok((
                    pages.into_iter().flatten().chain(whole),
                    tag.map(|_| (iova, len)),
                ))
            },
        )
    }

    /// Faults are the fault queue's records, raised on the vector every
    /// cause is sent to, as the unit was taken over to signal.
    fn route_faults(&self, route: FaultRoute) -> Result<(), IommuError> {
        let (signalling, vector, supported) = match route {
            FaultRoute::Wired { place } => (Signalling::Wired, place, self.caps.wired()),
            FaultRoute::Message { .. } => (Signalling::Message, 0, self.caps.msi()),
        };
        if signalling != self.lifecycle.lock().signalling || !supported || vector > LAST_VECTOR {
            return Err(IommuError::OutOfRange);
        }
        let regs = self.regs.lock();
        if let FaultRoute::Message { address, data } = route {
            if address & 0b11 != 0 || address >> ADDRESS_BITS != 0 {
                return Err(IommuError::OutOfRange);
            }
            regs.write64(regs::MSI_CFG_TBL, address)?;
            regs.write32(regs::MSI_CFG_TBL + 8, data)?;
            regs.write32(regs::MSI_CFG_TBL + 12, 0)?;
        }
        // Every cause on the one vector: only the fault queue's is enabled,
        // and the counters that could raise another are stopped.
        let every = u64::from(vector) * 0x1111;
        regs.write64(regs::ICVEC, every)?;
        if regs.read64(regs::ICVEC)? & 0xFFFF != every {
            return Err(IommuError::OutOfRange);
        }
        self.queue_control(&regs, regs::FQCSR, regs::QUEUE_EN | regs::QUEUE_IE)
    }

    /// The queue runs on; its pending bit is cleared once the interrupt is
    /// off, as a wired line stays asserted while it is set.
    fn unroute_faults(&self) -> Result<(), IommuError> {
        let regs = self.regs.lock();
        self.queue_control(&regs, regs::FQCSR, regs::QUEUE_EN)?;
        regs.write32(regs::IPSR, regs::IPSR_FIP)
    }

    fn message_files(&self) -> Option<&dyn MessageFiles> {
        // Messages are recognised only translating through a second stage.
        (self.caps.message_files() && self.stage == Stage::Second)
            .then_some(self as &dyn MessageFiles)
    }

    /// At most one queue's worth of records per call.
    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        drain_in_batches(
            FAULT_QUEUE_RECORDS as usize,
            |batch| self.take_records(batch),
            sink,
        )
    }
}

impl<R: Registers> MessageFiles for RiscvUnit<'_, R> {
    fn atomic_files(&self) -> bool {
        self.caps.atomic_files()
    }

    fn confine_messages(
        &self,
        stream: u32,
        doorbell: u64,
        file: u64,
        notice: Notice,
    ) -> Result<(), IommuError> {
        let named =
            |at: u64, align: u64| at.is_multiple_of(align) && at >> MESSAGE_ADDRESS_BITS == 0;
        if !named(file, MESSAGE_FILE_BYTES)
            || !named(notice.address, IO_PAGE_SIZE)
            || !doorbell.is_multiple_of(IO_PAGE_SIZE)
            || doorbell >> self.profile.reach.input_bits != 0
            || notice.data > format::MAX_IDENTITY
            || self.message_files().is_none()
        {
            return Err(IommuError::OutOfRange);
        }
        let mut life = self.lifecycle.lock();
        life.directory.covers(stream)?;
        if life.messages.contains_key(&stream) {
            return Err(IommuError::OutOfRange);
        }
        life.messages
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let table = self.memory.alloc()?;
        let entry = format::mrif_entry(file, notice.address, notice.data);
        for (index, word) in entry.into_iter().enumerate() {
            if let Err(err) = self.memory.write(&table, index, word) {
                self.memory.free(table);
                return Err(err);
            }
        }
        self.memory.publish(&table, 0, entry.len());
        // A stream translating now takes its confinement at once. It is
        // recorded once its context names the table, so a refusal before
        // leaves nothing in a retry's way and the table is let go.
        let Some(Binding::Domain(id)) = life.bindings.get(stream) else {
            let _ = life.messages.try_insert(stream, (table, doorbell));
            return Ok(());
        };
        let prepared = self
            .domains
            .with(id, |domain| self.context_for(id, domain, false))
            .map(|context| format::with_messages(context, table.phys(), doorbell))
            .and_then(|context| Ok((context, life.bindings.reserve()?)))
            .and_then(|(context, reserved)| {
                self.break_context(&mut life, stream, &context)?;
                Ok((context, reserved))
            });
        let (context, reserved) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => {
                self.memory.free(table);
                return Err(err);
            }
        };
        let written = self.write_context(&mut life, stream, &context);
        let _ = life.messages.try_insert(stream, (table, doorbell));
        written?;
        life.bindings.hold(reserved, stream, id);
        self.run([format::forget_context(Some(stream))])
    }
}

/// How the unit's invalidations name a domain.
#[derive(Copy, Clone)]
enum Tag {
    Gscid(u16),
    Pscid(u32),
}

/// The leaf of the page at `iova` in the domain `tag` names.
fn forget_page(tag: Tag, iova: u64) -> Command {
    match tag {
        Tag::Gscid(gscid) => format::forget_second_stage_page(gscid, iova),
        Tag::Pscid(pscid) => format::forget_first_stage_page(pscid, iova),
    }
}
