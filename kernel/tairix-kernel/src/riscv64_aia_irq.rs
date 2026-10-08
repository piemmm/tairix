//! [`AiaIrqController`] — the `kernel/irq` [`IrqController`] over a riscv64
//! hart's supervisor-level APLIC domain and IMSIC file (`plans/IOMMU.md`
//! IOM18.3), and over the memory-resident interrupt files each confined
//! device's messages land in (IOM18.4).
//!
//! Lines `1..=sources` are the APLIC's sources, each sent to the hart's file
//! as an identity it is given when first armed. Lines from
//! [`MESSAGE_LINE_BASE`] are devices' message vectors: device `k`'s writes
//! set identity [`VECTOR`] of its own file `k`, and the unit then raises the
//! file's notice identity in the hart's file. A device can reach no other
//! file, so it raises its own vector or nothing.
//!
//! Host-buildable over the register seams, as the PLIC bridge is.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

use tairix_arch_riscv64::aplic::{Aplic, AplicError, AplicMmio, Sense};
use tairix_arch_riscv64::fdt::MAX_APLIC_SOURCES;
use tairix_arch_riscv64::imsic::{Imsic, InterruptFile};
use tairix_kernel_irq::{IrqController, IrqTable, MaskError, Trigger};
use tairix_sync::once::OnceCell;
use tairix_sync::SpinLock;

/// The line device message file `0` raises, past every APLIC source.
pub const MESSAGE_LINE_BASE: u32 = MAX_APLIC_SOURCES + 1;

/// The identity a device's one vector writes into its file.
pub const VECTOR: u32 = 1;

const VECTOR_BIT: u64 = 1 << VECTOR;

/// What an identity of the hart's file raises: `0` nothing, else an APLIC
/// source, or a file's notice with this bit set.
const NOTICE: u32 = 1 << 31;

/// A memory-resident interrupt file: for each 64 identities a pending
/// doubleword, then an enable one (RISC-V Advanced Interrupt Architecture,
/// "Memory-resident interrupt files").
#[repr(C, align(512))]
pub struct Mrif {
    words: [AtomicU64; 64],
}

const _: () = assert!(
    core::mem::size_of::<Mrif>() as u64 == tairix_kernel_iommu_api::MESSAGE_FILE_BYTES
        && core::mem::align_of::<Mrif>() as u64 == tairix_kernel_iommu_api::MESSAGE_FILE_BYTES,
    "a file is exactly the bytes, and the alignment, a unit writes"
);

impl Mrif {
    fn pending(&self) -> &AtomicU64 {
        &self.words[0]
    }

    fn enable(&self) -> &AtomicU64 {
        &self.words[1]
    }
}

/// Raises an identity of the hart's file as a message would: a write to the
/// file's page.
pub trait Doorbell: Sync {
    /// Raise `identity`.
    fn ring(&self, identity: u32);
}

/// The message files a controller has taken.
struct Files {
    files: &'static [Mrif],
    /// The notice identity of file `0`; file `k`'s is `first_notice + k`.
    first_notice: u32,
}

/// The controller over one hart's APLIC domain, IMSIC file and the message
/// files devices are confined to.
pub struct AiaIrqController<A, F, D> {
    aplic: Aplic<A>,
    imsic: Imsic<F>,
    doorbell: D,
    hart_index: u32,
    /// Per source: signalled by rising edges rather than a high level.
    edge: Box<[AtomicBool]>,
    /// Per source: the identity it is sent as, `0` until first armed.
    identity_of: Box<[AtomicU16]>,
    /// Per identity: what it raises.
    raises: Box<[AtomicU32]>,
    /// The next identity to give out, held across routing one.
    next: SpinLock<u32>,
    files: OnceCell<Files>,
}

fn atomics<T>(len: usize, zero: impl Fn() -> T) -> Option<Box<[T]>> {
    let mut slots = Vec::new();
    slots.try_reserve_exact(len).ok()?;
    slots.resize_with(len, zero);
    Some(slots.into_boxed_slice())
}

fn refusal(err: AplicError) -> MaskError {
    match err {
        AplicError::SourceOutOfRange => MaskError::OutOfRange,
        AplicError::NotMsi | AplicError::NotDelegated | AplicError::BadTarget => {
            MaskError::Unsupported
        }
    }
}

impl<A: AplicMmio, F: InterruptFile, D: Doorbell> AiaIrqController<A, F, D> {
    /// A controller sending `aplic`'s sources to the file of the hart at
    /// `hart_index`, which `imsic` and `doorbell` reach; [`None`] when its
    /// tables cannot be had.
    pub fn new(aplic: Aplic<A>, imsic: Imsic<F>, doorbell: D, hart_index: u32) -> Option<Self> {
        let sources = usize::try_from(aplic.sources()).ok()? + 1;
        let ids = usize::try_from(imsic.ids()).ok()? + 1;
        Some(Self {
            edge: atomics(sources, || AtomicBool::new(false))?,
            identity_of: atomics(sources, || AtomicU16::new(0))?,
            raises: atomics(ids, || AtomicU32::new(0))?,
            next: SpinLock::new(1),
            files: OnceCell::new(),
            aplic,
            imsic,
            doorbell,
            hart_index,
        })
    }

    /// The highest line it raises: its last source, or the last file's
    /// vector once it has taken files.
    #[must_use]
    pub fn max_line(&self) -> u32 {
        self.files().map_or(self.aplic.sources(), |files| {
            MESSAGE_LINE_BASE + u32::try_from(files.files.len()).unwrap_or(0) - 1
        })
    }

    /// Take `files` as the files of the message lines from
    /// [`MESSAGE_LINE_BASE`], each given a notice identity, and answer file
    /// `0`'s; later files' follow it. Every vector starts disabled.
    /// [`None`] for no files, more than the hart's file has identities left
    /// for, or a second taking.
    pub fn take_files(&self, files: &'static [Mrif]) -> Option<u32> {
        let count = u32::try_from(files.len()).ok().filter(|&count| count > 0)?;
        let mut next = self.next.lock();
        let first = *next;
        let last = first
            .checked_add(count - 1)
            .filter(|&last| last <= self.imsic.ids())?;
        if self.files().is_some() {
            return None;
        }
        for file in files {
            file.enable().store(0, Ordering::Relaxed);
            file.pending().store(0, Ordering::Relaxed);
        }
        for (k, identity) in (first..=last).enumerate() {
            self.raises[identity as usize]
                .store(NOTICE | u32::try_from(k).ok()?, Ordering::Release);
        }
        self.files
            .set(Files {
                files,
                first_notice: first,
            })
            .ok()?;
        *next = last + 1;
        Some(first)
    }

    /// The vector line file `k`'s notice raises, its pending bit taken; the
    /// unit sends a notice for a vector its file has disabled, too.
    fn take_notice(&self, k: u32) -> Option<u32> {
        let file = self.files()?.files.get(k as usize)?;
        let enabled = file.enable().load(Ordering::Acquire) & VECTOR_BIT;
        let taken = file.pending().fetch_and(!enabled, Ordering::AcqRel) & enabled;
        (taken != 0).then_some(MESSAGE_LINE_BASE + k)
    }

    fn files(&self) -> Option<&Files> {
        self.files.get().ok().flatten()
    }

    fn file_of(&self, line: u32) -> Option<(u32, &Mrif)> {
        let files = self.files()?;
        let k = line.checked_sub(MESSAGE_LINE_BASE)?;
        Some((files.first_notice + k, files.files.get(k as usize)?))
    }

    /// `line`, where it is one of the APLIC's sources.
    fn source(&self, line: u32) -> Option<u32> {
        (1..=self.aplic.sources()).contains(&line).then_some(line)
    }

    /// Route the source of `line`, giving it an identity the first time.
    fn route(&self, line: u32) -> Result<(), MaskError> {
        let source = line as usize;
        let sense = if self.edge[source].load(Ordering::Acquire) {
            Sense::Edge
        } else {
            Sense::Level
        };
        let mut next = self.next.lock();
        let given = u32::from(self.identity_of[source].load(Ordering::Acquire));
        let identity = if given == 0 { *next } else { given };
        if identity > self.imsic.ids() {
            return Err(MaskError::Unsupported);
        }
        self.aplic
            .route(line, sense, self.hart_index, identity)
            .map_err(refusal)?;
        if given == 0 {
            self.raises[identity as usize].store(line, Ordering::Release);
            let narrow = u16::try_from(identity).map_err(|_| MaskError::Unsupported)?;
            self.identity_of[source].store(narrow, Ordering::Release);
            *next = identity + 1;
        }
        Ok(())
    }
}

impl<A, F, D> AiaIrqController<A, F, D>
where
    A: AplicMmio + Send + Sync,
    F: InterruptFile + Send + Sync,
    D: Doorbell,
{
    /// Claim and raise every identity pending in the hart's file, at most
    /// one pass of them, answering whether any raised a line.
    pub fn dispatch(&self, table: &IrqTable) -> bool {
        let mut fired = false;
        for _ in 0..self.imsic.ids() {
            let Some(identity) = self.imsic.claim() else {
                break;
            };
            let raises = self
                .raises
                .get(identity as usize)
                .map_or(0, |slot| slot.load(Ordering::Acquire));
            let line = if raises & NOTICE != 0 {
                self.take_notice(raises & !NOTICE)
            } else {
                (raises != 0).then_some(raises)
            };
            if let Some(line) = line {
                let _ = table.fire(line, self);
                fired = true;
            }
        }
        fired
    }
}

impl<A, F, D> IrqController for AiaIrqController<A, F, D>
where
    A: AplicMmio + Send + Sync,
    F: InterruptFile + Send + Sync,
    D: Doorbell,
{
    fn mask(&self, line: u32) -> Result<(), MaskError> {
        if let Some(source) = self.source(line) {
            self.aplic.disable(source).map_err(refusal)?;
        } else {
            let (_, file) = self.file_of(line).ok_or(MaskError::OutOfRange)?;
            file.enable().fetch_and(!VECTOR_BIT, Ordering::SeqCst);
        }
        fence(Ordering::SeqCst);
        Ok(())
    }

    fn rearm(&self, line: u32) -> Result<(), MaskError> {
        if let Some(source) = self.source(line) {
            if self.identity_of[source as usize].load(Ordering::Acquire) == 0 {
                self.route(source)?;
            }
            self.aplic.enable(source).map_err(refusal)?;
            if !self.edge[source as usize].load(Ordering::Acquire) {
                self.aplic.retrigger(source).map_err(refusal)?;
            }
            return Ok(());
        }
        let (notice, file) = self.file_of(line).ok_or(MaskError::OutOfRange)?;
        file.enable().fetch_or(VECTOR_BIT, Ordering::SeqCst);
        // A vector raised while it was disabled sent no notice, or one taken
        // before it was enabled.
        if file.pending().load(Ordering::SeqCst) & VECTOR_BIT != 0 {
            self.doorbell.ring(notice);
        }
        Ok(())
    }

    fn set_trigger(&self, line: u32, trigger: Trigger) -> Result<(), MaskError> {
        match self.source(line) {
            Some(source) => {
                self.edge[source as usize].store(trigger == Trigger::Edge, Ordering::Release);
                self.route(source)
            }
            None => self.file_of(line).map(|_| ()).ok_or(MaskError::OutOfRange),
        }
    }
}

/// [`Doorbell`] writing the hart's file page.
#[cfg(all(freestanding, kernel_isa = "riscv64"))]
pub struct FilePage {
    page: usize,
}

#[cfg(all(freestanding, kernel_isa = "riscv64"))]
impl FilePage {
    /// The doorbell of the file whose page is at `page`.
    ///
    /// # Safety
    ///
    /// `page` must be the hart's interrupt-file page the tree names, mapped
    /// for writes for the life of the kernel.
    #[must_use]
    pub const unsafe fn new(page: usize) -> Self {
        Self { page }
    }
}

#[cfg(all(freestanding, kernel_isa = "riscv64"))]
impl Doorbell for FilePage {
    fn ring(&self, identity: u32) {
        // SAFETY: the file's little-endian `seteipnum` register is the first
        // word of the page the constructor's caller vouched for.
        unsafe { core::ptr::write_volatile(self.page as *mut u32, identity) };
    }
}

#[cfg(test)]
#[path = "riscv64_aia_irq_tests.rs"]
mod tests;
