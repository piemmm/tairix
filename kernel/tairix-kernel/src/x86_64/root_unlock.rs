//! The x86_64 (QEMU `q35`/`pc`, UEFI-class PC) live root-unlock bring-up
//! (`plans/ARCHSUPPORT.md` A2): the freestanding-x86_64 half of the in-kernel
//! root-unlock service, the cross-port sibling of
//! [`crate::riscv64::root_unlock`] and [`crate::aarch64::root_unlock`].
//!
//! It admits the in-kernel root-unlock kthread at the init seam, brings the
//! bootstrap virtio-blk-PCI root device up through the shared floor bring-up
//! ([`crate::x86_64::floor`]), and hands the opened disk to the shared
//! [`finish_virtio_unlock`] tail. The only floor block driver on the QEMU PC
//! target is virtio-blk, so there is one bring-up.

use core::convert::Infallible;

use alloc::boxed::Box;

use tairix_abi::HwNode;
use tairix_caps::CapabilitySet;
use tairix_kernel_core::{ConsoleRead, ConsoleWrite, CooperativeYield, InitSpawnCtx, YieldHandle};
use tairix_kernel_mem::FrameAllocator;
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::identity::UserId;
use tairix_log::{Level, Sink};
use tairix_reclaim::MemoryPressure;

use crate::driver_catalog::VIRTIO_BLK_PATH;
use crate::root_storage::RootBlockBinding;
use crate::unlock_orchestrate::{finish_virtio_unlock, UnlockConsole, UnlockEnv};
use crate::unlock_service::{note, service_caps, take_boot, CONSOLE0_GATE, UNLOCK_TASK};
use tairix_arch_x86_64::serial::SERIAL_SINK;

use crate::x86_64::serial_sink::COM1_CONSOLE;

/// Release console 0 to `login` and mark the users-database source resolved.
///
/// Both mean "the unlock window is over, `login` may take the console now",
/// so they flip together. Opening the gate lets `login`'s gated console
/// reads through; resolving the late users-database flips a `login` parked
/// on the pending `users_db_read` into its prompt — against the installed
/// database if the unlock succeeded, else fail-closed deny-all. The COM1
/// receive interrupt is (idempotently) armed here too, so a `login` reader
/// now parks off the run queue and a keystroke wakes it.
fn release_console0_to_login() {
    CONSOLE0_GATE.open();
    // Nudge the console wait-queue so any `login` already parked on the
    // (until now) withheld console-0 read re-polls the now-open gate; a
    // no-op before the wait-queue arch hook is installed.
    tairix_kernel_core::console_wake();
    crate::root_mount::LATE_USERS_DB.resolve();
    // Switch COM1 from the poll-backed to the interrupt-driven receive path
    // for the `login` session: a keystroke now wakes the parked reader
    // rather than requiring a poll. Idempotent with the `acquire_console0`
    // arm; a no-op if the boot path could not resolve the console GSI.
    crate::x86_64::com1_rx::enable_uart_console_irq();
}

/// The x86_64 console-0 seam the shared root-unlock orchestration reaches
/// the primary console through.
///
/// The COM1 UART is the primary console: its write half streams the
/// passphrase prompt, and its read half is the interrupt-fed
/// [`Com1ConsoleRead`](crate::x86_64::com1_rx::Com1ConsoleRead) — arming
/// the device's receive interrupt so a typed passphrase wakes the parked
/// unlock kthread rather than busy-polling the FIFO. A boot that could not
/// resolve the console interrupt leaves the receive line disabled and the
/// reader on the poll-backed path (fail closed), never a reader parked
/// forever.
struct X86UnlockConsole;

/// The single `'static` [`X86UnlockConsole`] the bring-up hands the shared
/// orchestration.
static X86_UNLOCK_CONSOLE: X86UnlockConsole = X86UnlockConsole;

impl UnlockConsole for X86UnlockConsole {
    fn acquire_console0(
        &self,
    ) -> (
        &'static dyn ConsoleWrite,
        &'static (dyn ConsoleRead + Sync + 'static),
    ) {
        // Arm COM1's interrupt-driven receive for the unlock window so a
        // keystroke wakes the parked passphrase reader (`console_wake`) —
        // the receive ISR drains the FIFO into `COM1_INPUT`, which
        // `COM1_CONSOLE_READ` reads. Idempotent with the
        // `release_console0_to_login` handoff arm.
        crate::x86_64::com1_rx::enable_uart_console_irq();
        let write: &'static dyn ConsoleWrite = &COM1_CONSOLE;
        // The unlock kthread reads the *ungated* interrupt-fed read half
        // directly; the console list installs the gate-wrapped sibling for
        // `login`.
        let read: &'static (dyn ConsoleRead + Sync + 'static) =
            &crate::x86_64::com1_rx::COM1_CONSOLE_READ;
        (write, read)
    }

    fn release_console0_to_login(&self) {
        release_console0_to_login();
    }
}

/// Admit the in-kernel root-unlock kthread if the boot path bound a
/// virtio-blk root block device, returning whether it was started.
///
/// With no binding (headless / no disk / ambiguous), a non-virtio-blk
/// binding (there is no other floor block driver on the QEMU PC target), or
/// no `'static` frame allocator, it starts nothing, opens the console-0 gate
/// so `login` proceeds (and fails closed, as no database is installed), and
/// returns `false`. The console-0 gate is also opened by the kthread body
/// once the unlock resolves, so it is never left latched closed.
#[must_use]
pub fn spawn_if_present(ctx: &'static (dyn InitSpawnCtx + Sync)) -> bool {
    let boot = take_boot();
    // Route the unlock service's security-relevant decisions onto the boot
    // audit channel when the init seam wired a `'static` audit sink; fall
    // back to the COM1 serial log otherwise. The kthread body and the unlock
    // policy share it.
    let audit: &'static (dyn Sink + Sync) = ctx.static_audit().unwrap_or(&SERIAL_SINK);
    let Some(binding) = boot.binding else {
        note(
            audit,
            Level::Info,
            "root-unlock: no root block device bound; root unbound, login refuses",
        );
        release_console0_to_login();
        // No disk means no on-disk application store this boot: resolve the
        // readiness latch so a store-bundle spawn fails closed instead of
        // parking forever.
        crate::app_store::APP_STORE.note_unavailable();
        return false;
    };
    if binding.driver_path != VIRTIO_BLK_PATH {
        // The bound driver is not the one bootstrap-floor block driver this
        // seam knows how to bring up (virtio-blk). Fail closed rather than
        // guess at a bring-up. `root_storage` only ever binds a
        // `provides_root_block` floor driver, so reaching here is a
        // packaging defect, not an expected path.
        note(
            audit,
            Level::Error,
            "root-unlock: bound block driver is not a known floor driver; root unbound",
        );
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
        return false;
    }
    let Some(frames) = ctx.static_frames() else {
        note(
            audit,
            Level::Error,
            "root-unlock: no kernel frame allocator; root unbound, login refuses",
        );
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
        return false;
    };

    let caps = service_caps();
    // The system memory-pressure gauge every mounted volume's cache samples,
    // over the same `'static` frame allocator the spawn path uses — physical
    // free frames are the authoritative reading. Fetched from the
    // memory-statistics registry so this boot path, every cache, and the
    // System Information export all share the one gauge.
    let pressure: &'static MemoryPressure =
        tairix_kernel_core::memstats::MEM_STATS.system_pressure(frames);
    let env = UnlockEnv {
        ctx,
        audit,
        pressure,
    };
    let body = move |yielder: &mut dyn YieldHandle| {
        // On success the root-unlock service never returns: it parks for life
        // as the persistent driver-store service, having already logged the
        // unlock outcome and released console 0. Only an early bring-up
        // failure returns here — and because the success arm is the
        // uninhabited [`Infallible`], the `Err` binding is irrefutable. Fail
        // closed: log the stage and open the console-0 gate so `login`
        // proceeds (it refuses every attempt, as a failed unlock installs no
        // database).
        let Err(stage) = run_unlock(yielder, &binding, frames, caps, env);
        note(audit, Level::Error, stage);
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
    };

    let admitted = ctx.spawn_kernel_service(Box::new(body));
    if let Some(task_id) = admitted {
        // Publish the disk-owning kthread's scheduler id so its driver-store
        // serve loop registers on `SERVE_WAITQ` and is unparked the instant a
        // request is posted (a real wake, never a busy-yield).
        crate::unlock_service::set_store_service_task(task_id);
    }
    let started = admitted.is_some();
    note(
        audit,
        if started { Level::Info } else { Level::Error },
        if started {
            "root-unlock service kthread admitted (bring-up runs on first dispatch)"
        } else {
            "root-unlock service kthread could not be admitted; opening console gate"
        },
    );
    if !started {
        // Admission failed: nothing will open the gate or publish the
        // `/System` mount, so do both here or console-0 `login` would park
        // forever.
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
    }
    started
}

/// Bring up the bound virtio-blk root device and run the interactive unlock
/// policy.
///
/// **On success this never returns:** the shared tail logs the outcome,
/// releases the console-0 gate, and parks the kthread for life as the
/// persistent driver-store service, so the [`Infallible`] `Ok` is never
/// produced. Only an early bring-up failure returns `Err` with a stable
/// stage string; on that path the caller logs it and opens the console-0
/// gate.
fn run_unlock(
    yielder: &mut dyn YieldHandle,
    binding: &RootBlockBinding,
    frames: &'static FrameAllocator,
    caps: CapabilitySet,
    env: UnlockEnv,
) -> Result<Infallible, &'static str> {
    // Move the kthread's single yield handle into the shared cell both the
    // re-arming IRQ waiter and the cooperative console reader suspend through
    // (one cooperative-yield definition).
    let coop = CooperativeYield::new(yielder);

    // The bus-driver task capability context: the unlock kthread's caps,
    // owner `UNLOCK_TASK`, audited onto the service's audit sink; the virtio
    // register-window map gates on its `CAP_MMIO_MAP`. Leaked to `'static`
    // because the brought-up device host borrows it for the life of the (now
    // `'static`, shared) disk (kernel state is never freed).
    let caller: &'static TaskCapabilities = Box::leak(Box::new(TaskCapabilities::derive(
        UNLOCK_TASK,
        UserId(0),
        caps,
        caps,
        env.audit,
    )));

    match binding.driver_path {
        VIRTIO_BLK_PATH => virtio_blk_unlock(&coop, caller, &binding.node, frames, env),
        _ => Err("root-unlock: bound block driver is not a known floor driver"),
    }
}

/// Bring the virtio-blk-PCI root device up over the production MSI-X
/// interrupt path and open it for the unlock (the QEMU PC root).
fn virtio_blk_unlock<'a>(
    coop: &'a CooperativeYield<'a>,
    caller: &'static TaskCapabilities,
    node: &HwNode,
    frames: &'static FrameAllocator,
    env: UnlockEnv,
) -> Result<Infallible, &'static str> {
    let device = crate::x86_64::floor::bring_up_virtio_pci(
        env.ctx,
        node,
        caller,
        env.audit,
        frames,
        crate::floor_dma::Confinement::WhereTranslated,
    )?;
    finish_virtio_unlock(
        device.transport,
        device.host,
        coop,
        env,
        &X86_UNLOCK_CONSOLE,
    )
}
