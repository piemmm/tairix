//! riscv64 external-interrupt dispatch: the supervisor-level controller the
//! firmware tree describes — a PLIC, or an APLIC delivering by MSI to the
//! boot hart's IMSIC file (`plans/IOMMU.md` IOM18.3) — built once, published
//! beside the kernel-core [`IrqTable`], and wired to the trap path.
//!
//! The boot path records what it discovered while it holds the tree
//! ([`record`]); the core's `Irq` phase sizes the table to it ([`routing`])
//! and installs the dispatcher ([`install_dispatch`]). `sstatus.SIE` stays
//! the dispatch loop's, so nothing is taken until the scheduler runs a task.

use alloc::boxed::Box;

use tairix_arch_riscv64::aplic::{Aplic, VolatileAplicMmio};
use tairix_arch_riscv64::fdt::Aia;
use tairix_arch_riscv64::imsic::{HartFile, Imsic};
use tairix_arch_riscv64::plic::{s_mode_context, Plic, PlicController, VolatilePlicMmio};
use tairix_arch_riscv64::{halt_current_hart, trap};
use tairix_kernel_core::IrqRouting;
use tairix_kernel_irq::{IrqController, IrqTable};
use tairix_sync::once::OnceCell;

use crate::riscv64_aia_irq::{AiaIrqController, FilePage, MESSAGE_LINE_BASE};
use crate::riscv64_plic_irq::PlicIrqController;

/// The AIA controller as the production kernel builds it.
pub type AiaController = AiaIrqController<VolatileAplicMmio, HartFile, FilePage>;

/// The controller the tree describes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Discovered {
    /// A PLIC at `base` of `sources` sources.
    Plic {
        /// Its registers.
        base: u64,
        /// Their length.
        len: u64,
        /// Its sources, `1..=sources`.
        sources: u32,
    },
    /// An APLIC and the boot hart's IMSIC file, and the device message files
    /// the boot planned.
    Aia {
        /// The domain and the file.
        aia: Aia,
        /// Message files planned, each one line from [`MESSAGE_LINE_BASE`].
        files: u32,
    },
}

#[derive(Copy, Clone)]
enum Controller {
    Plic(&'static PlicIrqController<VolatilePlicMmio>),
    Aia(&'static AiaController),
}

impl Controller {
    fn as_dyn(self) -> &'static (dyn IrqController + Send + Sync) {
        match self {
            Self::Plic(plic) => plic,
            Self::Aia(aia) => aia,
        }
    }
}

static DISCOVERED: OnceCell<Discovered> = OnceCell::new();
static IRQ_TABLE: OnceCell<&'static IrqTable> = OnceCell::new();
static CONTROLLER: OnceCell<Controller> = OnceCell::new();

/// Record the controller the boot path discovered; the first record wins.
pub fn record(discovered: Discovered) {
    let _ = DISCOVERED.set(discovered);
}

/// The [`IrqTable`] [`install_dispatch`] published, which an in-kernel
/// service binds its device's line on.
#[must_use]
pub fn published_irq_table() -> Option<&'static IrqTable> {
    IRQ_TABLE.get().ok().flatten().copied()
}

/// The published controller, through which a line is armed and re-armed.
#[must_use]
pub fn controller() -> Option<&'static (dyn IrqController + Send + Sync)> {
    ensure_controller().map(Controller::as_dyn)
}

/// The published AIA controller, which takes the device message files.
#[must_use]
pub fn aia_controller() -> Option<&'static AiaController> {
    match ensure_controller()? {
        Controller::Aia(aia) => Some(aia),
        Controller::Plic(_) => None,
    }
}

/// The page of the boot hart's interrupt file, where the AIA was discovered.
#[must_use]
pub fn file_page() -> Option<u64> {
    match discovered()? {
        Discovered::Aia { aia, .. } => Some(aia.imsic.page),
        Discovered::Plic { .. } => None,
    }
}

/// Where the kernel reaches the registers at `[base, base + len)` in every
/// root, or [`None`] where it cannot.
fn registers(base: u64, len: u64) -> Option<usize> {
    crate::riscv64::boot::device_registers(base, usize::try_from(len).ok()?)
        .map(|at| at.as_ptr().addr())
}

fn discovered() -> Option<Discovered> {
    DISCOVERED.get().ok().flatten().copied()
}

/// Build the discovered controller once and publish it.
fn ensure_controller() -> Option<Controller> {
    if let Some(controller) = CONTROLLER.get().ok().flatten().copied() {
        return Some(controller);
    }
    let controller = match discovered()? {
        Discovered::Plic { base, len, sources } => {
            let base = registers(base, len)?;
            // SAFETY: the PLIC's registers from the firmware tree, mapped in
            // every root, driven by this controller alone. `s_mode_context(0)`
            // is the boot hart's supervisor context.
            let mmio = unsafe { VolatilePlicMmio::new(base) };
            let plic = PlicController::new(Plic::new(mmio, s_mode_context(0)), sources);
            Controller::Plic(Box::leak(Box::new(PlicIrqController::new(plic))))
        }
        Discovered::Aia { aia, .. } => {
            let (base, page) = (
                registers(aia.aplic.base, aia.aplic.len)?,
                registers(aia.imsic.page, tairix_arch_riscv64::fdt::FILE_PAGE)?,
            );
            // SAFETY: the supervisor domain's registers from the firmware
            // tree, mapped in every root, driven by this controller alone.
            let aplic =
                Aplic::take_msi(unsafe { VolatileAplicMmio::new(base) }, aia.aplic.sources).ok()?;
            // Built in the core's `Irq` phase on the boot hart, whose file
            // `HartFile` reaches.
            let imsic = Imsic::take(HartFile, aia.imsic.ids)?;
            // SAFETY: the boot hart's file page from the firmware tree, mapped
            // in every root.
            let doorbell = unsafe { FilePage::new(page) };
            let aia = AiaIrqController::new(aplic, imsic, doorbell, aia.imsic.hart_index)?;
            Controller::Aia(Box::leak(Box::new(aia)))
        }
    };
    match CONTROLLER.set(controller) {
        Ok(()) => Some(controller),
        Err(_) => CONTROLLER.get().ok().flatten().copied(),
    }
}

/// The routing the core sizes its [`IrqTable`] to: every source the
/// controller has and every planned message line. Unsupported where the tree
/// describes no controller the kernel can take.
#[must_use]
pub fn routing() -> IrqRouting {
    let (Some(controller), Some(discovered)) = (ensure_controller(), discovered()) else {
        return IrqRouting::unsupported();
    };
    let max_line = match discovered {
        Discovered::Plic { sources, .. } => sources,
        Discovered::Aia { files: 0, aia } => aia.aplic.sources,
        Discovered::Aia { files, .. } => MESSAGE_LINE_BASE + files - 1,
    };
    IrqRouting {
        max_line,
        controller: controller.as_dyn(),
    }
}

/// One supervisor external interrupt: the PLIC's claimed source.
extern "C" fn plic_dispatch() {
    let (Some(Controller::Plic(plic)), Some(table)) = (ensure_controller(), published_irq_table())
    else {
        return;
    };
    let source = plic.claim();
    if source != 0 {
        let _ = table.fire(source, plic as &dyn IrqController);
        plic.complete(source);
        woke();
    }
}

/// One supervisor external interrupt: every identity pending in the boot
/// hart's file.
extern "C" fn aia_dispatch() {
    let (Some(Controller::Aia(aia)), Some(table)) = (ensure_controller(), published_irq_table())
    else {
        return;
    };
    if aia.dispatch(table) {
        woke();
    }
}

/// Wake any `irq_wait` caller a fire marked ready, and reschedule at the next
/// preemption point so the woken work runs.
fn woke() {
    tairix_kernel_core::irq_wake();
    tairix_kernel_core::note_preempt_tick(tairix_arch_riscv64::smp::current_hartid());
}

/// Publish `table` and wire the discovered controller's dispatcher, then
/// enable `sie.SEIE`. With no controller the table is published and nothing
/// is wired, so interrupt-driven bring-up fails closed. A second publication
/// halts the hart.
pub fn install_dispatch(table: &'static IrqTable) {
    if IRQ_TABLE.set(table).is_err() {
        halt_current_hart();
    }
    let dispatch = match ensure_controller() {
        Some(Controller::Plic(_)) => plic_dispatch as trap::TrapDispatchFn,
        Some(Controller::Aia(_)) => aia_dispatch,
        None => return,
    };
    if trap::set_trap_dispatch(dispatch).is_err() {
        halt_current_hart();
    }
    // SAFETY: the trap vector is installed and the dispatcher published, so
    // a taken external interrupt reaches a handler; this sets `sie.SEIE`
    // alone.
    unsafe {
        core::arch::asm!("csrs sie, {}", in(reg) trap::SIE_SEIE, options(nomem, nostack));
    }
}
