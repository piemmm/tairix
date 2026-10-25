//! The x86_64 (QEMU `q35`/`pc`, UEFI-class PC) half of the root unlock
//! (`plans/ARCHSUPPORT.md` A2): the shared admission and tail are
//! [`crate::unlock_orchestrate`]'s, and this port brings up a virtio-blk-PCI
//! disk through its floor bring-up ([`crate::x86_64::floor`]). It has no
//! host to lend a floor driver that opens its own device, so it refuses one.

use core::convert::Infallible;

use tairix_abi::HwNode;
use tairix_arch_x86_64::serial::SERIAL_SINK;
use tairix_kernel_core::{CooperativeYield, InitSpawnCtx};
use tairix_log::Sink;

use crate::floor_dma::Confinement;
use crate::unlock_orchestrate::{
    finish_virtio_unlock, spawn_unlock, Console0, FloorEnv, KeySource, UnlockConsole, UnlockEnv,
    UnlockPort,
};
use crate::unlock_service::CONSOLE0_GATE;
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

/// The x86_64 half of the shared root unlock: its console-0 seam and its
/// virtio floor bring-up.
///
/// The COM1 UART is the primary console: its write half streams the
/// passphrase prompt, and its read half is the interrupt-fed
/// [`Com1ConsoleRead`](crate::x86_64::com1_rx::Com1ConsoleRead) — arming
/// the device's receive interrupt so a typed passphrase wakes the parked
/// unlock kthread rather than busy-polling the FIFO. A boot that could not
/// resolve the console interrupt leaves the receive line disabled and the
/// reader on the poll-backed path (fail closed), never a reader parked
/// forever.
struct X86Unlock;

/// The single `'static` [`X86Unlock`] the shared orchestration runs.
static X86_UNLOCK: X86Unlock = X86Unlock;

impl UnlockConsole for X86Unlock {
    fn acquire_console0(&self) -> Console0 {
        crate::console_layout::framebuffer_console0().unwrap_or_else(|| {
            // Arm COM1's interrupt-driven receive for the unlock window so a
            // keystroke wakes the parked passphrase reader; idempotent with
            // the `login` handoff arm. The unlock kthread reads the ungated
            // read half; the console list installs the gated sibling for
            // `login`.
            crate::x86_64::com1_rx::enable_uart_console_irq();
            Console0 {
                write: &COM1_CONSOLE,
                read: &crate::x86_64::com1_rx::COM1_CONSOLE_READ,
                keys: KeySource::Live,
            }
        })
    }

    fn release_console0_to_login(&self) {
        release_console0_to_login();
    }
}

impl UnlockPort for X86Unlock {
    fn serial_audit(&self) -> &'static (dyn Sink + Sync) {
        &SERIAL_SINK
    }

    /// Over the production MSI-X interrupt path; the PC hands over no device
    /// tree.
    fn virtio_unlock(
        &'static self,
        coop: &CooperativeYield<'_>,
        node: &HwNode,
        floor: FloorEnv,
        env: UnlockEnv,
    ) -> Result<Infallible, &'static str> {
        let device = crate::x86_64::floor::bring_up_virtio_pci(
            env.ctx,
            node,
            floor.caller,
            env.audit,
            floor.frames,
            Confinement::WhereTranslated,
        )?;
        finish_virtio_unlock(device.transport, device.host, coop, env, self)
    }
}

/// Admit the in-kernel root-unlock kthread for the floor device the boot
/// bound, returning whether it was started ([`spawn_unlock`]).
#[must_use]
pub fn spawn_if_present(ctx: &'static (dyn InitSpawnCtx + Sync)) -> bool {
    spawn_unlock(ctx, &X86_UNLOCK)
}
