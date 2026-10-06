//! Interrupt remapping on the x86_64 port (`plans/IOMMU.md` IOM11).
//!
//! The boot sets every IO-APIC pin up masked, in compatibility format, and
//! routes each interrupt-driven PCI function's MSI-X in compatibility format
//! while the function cannot master. Once the translation units are up, and
//! before any interrupt is taken, [`plan`] gives every one of those sources an
//! entry on the unit that sees its messages and turns remapping on; the port
//! then rewrites each source with its entry. A machine that cannot remap every
//! source keeps compatibility delivery, as Linux does.

use alloc::vec::Vec;

use tairix_abi::driver::msix::MsiMessage;
use tairix_kernel_core::iommu::{
    InterruptRouting, InterruptSource, InterruptTarget, RemapEntry, RemapError, Translation,
};

use crate::pci_host::Published;
use crate::x86_64::ioapic_controller::ProgrammedPin;
use crate::x86_64::msi::MsiVector;

/// An I/O APIC a translation unit's scope names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IoApicSource {
    /// Its APIC id.
    pub id: u8,
    /// The node of the unit that sees its interrupt messages.
    pub unit: u32,
    /// The requester id its messages carry.
    pub requester: u16,
}

/// An interrupt-driven PCI function the boot probe routed in compatibility
/// format: its node and the vector its node was given.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PendingRoute {
    /// Its node.
    pub node: u32,
    /// Its vector.
    pub vector: MsiVector,
}

/// What the boot left for remapping to take over.
#[derive(Default)]
pub struct BootSources {
    /// Every I/O APIC a unit's scope names.
    pub ioapics: Vec<IoApicSource>,
    /// Every PCI function the probe routed.
    pub routes: Vec<PendingRoute>,
    /// Firmware says the platform supports interrupt remapping.
    pub remapping: bool,
    /// Firmware asks the OS to keep the local APICs out of x2APIC mode.
    pub x2apic_opt_out: bool,
}

/// What each source is to be rewritten with once remapping is on.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Plan {
    /// Each pin's remapped redirection entry, by global system interrupt.
    pub pins: Vec<(u32, u64)>,
    /// Each function's remapped message, with the entry it names.
    pub routes: Vec<(Published, MsiMessage, RemapEntry)>,
}

/// Why a function's message could not be had.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MsiRouteError {
    /// Its CPU's APIC id is past what a compatibility message names.
    Unaddressable,
    /// Remapping is on, a unit sees its messages, and the requester ids they
    /// carry are unknown: it could raise nothing it was not given.
    Unattributed,
    /// Its unit made no entry for it.
    Remap(RemapError),
}

/// The target an MSI vector is delivered to: edge-triggered, at the CPU it is
/// installed on.
const fn msi_target(vector: MsiVector) -> InterruptTarget {
    InterruptTarget {
        vector: vector.vector,
        destination: vector.destination,
        level: false,
    }
}

/// The message `function` raises `vector` with, and the entry made for it.
///
/// With `remapper` on and a unit seeing the function's messages, the unit's
/// entry, which only the requester ids the function's interrupts carry may
/// raise. Otherwise `compatibility`: remapping is off, or no unit sees the
/// function's messages to refuse them.
///
/// # Errors
///
/// [`MsiRouteError`] for a function that can be given no message.
pub fn message_for(
    remapper: Option<&Translation>,
    vector: MsiVector,
    function: &Published,
    compatibility: Option<MsiMessage>,
) -> Result<(MsiMessage, Option<RemapEntry>), MsiRouteError> {
    let translated = remapper.zip(function.stream);
    let Some((remapper, stream)) = translated else {
        return compatibility
            .map(|message| (message, None))
            .ok_or(MsiRouteError::Unaddressable);
    };
    let source = function.interrupts.ok_or(MsiRouteError::Unattributed)?;
    let entry = remapper
        .remap(stream.unit(), source, msi_target(vector))
        .map_err(MsiRouteError::Remap)?;
    let message = MsiMessage {
        address: entry.remapped.address,
        data: entry.remapped.data,
    };
    Ok((message, Some(entry)))
}

/// Give every source the boot set up an entry on `remapper`, for 32-bit
/// destinations where `extended` says so, from tables of `entries` entries,
/// and turn remapping on; or say why the machine keeps compatibility
/// delivery, leaving every source as it is.
///
/// `functions` pairs each pending route with the function the probe
/// published for its node. Remapping is the whole machine's or no one's: an
/// I/O APIC no unit's scope names, or one whose unit cannot remap, keeps it
/// off. A function a unit sees whose requester ids are unknown, or that its
/// unit refuses an entry, is left unrouted and counted.
#[must_use]
pub fn plan(
    remapper: &Translation,
    pins: &[ProgrammedPin],
    ioapics: &[IoApicSource],
    functions: &[(PendingRoute, Option<Published>)],
    extended: bool,
    entries: u32,
) -> (InterruptRouting, Plan) {
    let unremapped = (InterruptRouting::Unremapped, Plan::default());
    // Every record is had before a unit is touched, so running short of
    // memory leaves the machine as it was.
    let mut sources = Vec::new();
    let mut made = Vec::new();
    let mut plan = Plan::default();
    let reserved = sources
        .try_reserve_exact(pins.len())
        .and_then(|()| made.try_reserve_exact(pins.len()))
        .and_then(|()| plan.pins.try_reserve_exact(pins.len()))
        .and_then(|()| plan.routes.try_reserve_exact(functions.len()));
    if reserved.is_err() {
        return (
            InterruptRouting::Refused(RemapError::Unsupported),
            Plan::default(),
        );
    }
    for pin in pins {
        let Some(ioapic) = ioapics.iter().find(|ioapic| ioapic.id == pin.ioapic) else {
            return unremapped;
        };
        sources.push(*ioapic);
    }
    match remapper.prepare(extended, entries) {
        Ok(()) => {}
        Err(RemapError::Unsupported) => return unremapped,
        Err(refused) => return (InterruptRouting::Refused(refused), Plan::default()),
    }
    for (pin, ioapic) in pins.iter().zip(&sources) {
        let target = InterruptTarget {
            vector: pin.vector,
            destination: pin.destination,
            level: pin.level,
        };
        match remapper.remap(
            ioapic.unit,
            InterruptSource::Requester(ioapic.requester),
            target,
        ) {
            Ok(entry) => {
                plan.pins.push((pin.gsi, entry.remapped.redirection));
                made.push(entry);
            }
            Err(refused) => {
                release(remapper, made.into_iter());
                let routing = match refused {
                    RemapError::UnknownUnit | RemapError::Unsupported => {
                        InterruptRouting::Unremapped
                    }
                    refused => InterruptRouting::Refused(refused),
                };
                return (routing, Plan::default());
            }
        }
    }
    let mut unrouted: u32 = 0;
    for (route, function) in functions {
        let Some(function) = function else {
            continue;
        };
        match message_for(Some(remapper), route.vector, function, None) {
            Ok((message, Some(entry))) => plan.routes.push((*function, message, entry)),
            // No unit sees its messages, or its unit is not translating and
            // so refuses none: its compatibility route stands.
            Ok((_, None))
            | Err(MsiRouteError::Unaddressable | MsiRouteError::Remap(RemapError::UnknownUnit)) => {
            }
            Err(MsiRouteError::Unattributed | MsiRouteError::Remap(_)) => {
                unrouted = unrouted.saturating_add(1);
            }
        }
    }
    if let Err(refused) = remapper.enable() {
        release(remapper, made.into_iter());
        release(remapper, plan.routes.into_iter().map(|(_, _, entry)| entry));
        return (InterruptRouting::Refused(refused), Plan::default());
    }
    let routing = if unrouted == 0 {
        InterruptRouting::Remapped
    } else {
        InterruptRouting::Unrouted(unrouted)
    };
    (routing, plan)
}

fn release(remapper: &Translation, entries: impl Iterator<Item = RemapEntry>) {
    for entry in entries {
        remapper.release(entry);
    }
}

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub use live::{
    boot_sources, publish_boot_sources, publish_remapper, remapper, route, route_function,
};

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
mod live {
    use alloc::vec::Vec;

    use tairix_kernel_core::iommu::{InterruptRouting, Translation};
    use tairix_log::{Level, Sink};
    use tairix_sync::once::OnceCell;

    use super::{plan, BootSources, PendingRoute};
    use crate::pci_host::Published;
    use crate::x86_64::msi::MSIX_ENTRY;
    use crate::x86_64::registers::KernelRegisters;

    static BOOT_SOURCES: OnceCell<BootSources> = OnceCell::new();

    static REMAPPER: OnceCell<&'static Translation> = OnceCell::new();

    /// Publish what the boot probe left for remapping.
    pub fn publish_boot_sources(sources: BootSources) {
        let _ = BOOT_SOURCES.set(sources);
    }

    /// What the boot probe left for remapping.
    #[must_use]
    pub fn boot_sources() -> Option<&'static BootSources> {
        BOOT_SOURCES.get().ok().flatten()
    }

    /// Publish the remapping [`route`] turned on.
    pub fn publish_remapper(remapper: &'static Translation) {
        let _ = REMAPPER.set(remapper);
    }

    /// The kernel's interrupt remapping, once it is on.
    #[must_use]
    pub fn remapper() -> Option<&'static Translation> {
        REMAPPER.get().ok().flatten().copied()
    }

    /// Route MSI-X entry [`MSIX_ENTRY`] of the function the probe recorded
    /// as `function` to a vector of its own, raised by its unit's entry once
    /// remapping is on, else by a compatibility message.
    ///
    /// # Errors
    ///
    /// The step that failed; an entry made for it is let go.
    pub fn route_function(
        host: &crate::pci_host::PciHost,
        function: &Published,
    ) -> Result<crate::x86_64::msi::MsiVector, &'static str> {
        let vector = crate::x86_64::msi::allocate().map_err(|_| "no free MSI vector")?;
        let remapper = remapper();
        let (message, entry) = super::message_for(
            remapper,
            vector,
            function,
            crate::x86_64::msi::compatibility_message(vector),
        )
        .map_err(|_| "device interrupt unroutable")?;
        let routed = host
            .with(function.segment, |bus| {
                bus.route_msix(function.address, MSIX_ENTRY, message, &KernelRegisters)
            })
            .is_some_and(|result| result.is_ok());
        if routed {
            return Ok(vector);
        }
        if let (Some(remapper), Some(entry)) = (remapper, entry) {
            remapper.release(entry);
        }
        Err("MSI-X entry unwritable")
    }

    /// Route every source the boot set up through `remapper` where it can
    /// take them all, for a machine of `cpus` CPUs, and rewrite each with its
    /// entry; a function whose rewrite fails is logged to `log` and left
    /// unrouted.
    pub fn route(
        remapper: Option<&'static Translation>,
        cpus: u32,
        log: &dyn Sink,
    ) -> InterruptRouting {
        let empty = BootSources::default();
        let sources = boot_sources().unwrap_or(&empty);
        let Some(remapper) = remapper.filter(|_| sources.remapping) else {
            return InterruptRouting::Unremapped;
        };
        let Some(controller) = crate::x86_64::ioapic_controller::published_typed() else {
            return InterruptRouting::Native;
        };
        let mut pins = Vec::new();
        let mut gathered = true;
        controller.programmed(&mut |pin| {
            gathered &= pins.try_reserve(1).is_ok();
            if gathered {
                pins.push(pin);
            }
        });
        let host = crate::pci_host::published();
        let mut functions: Vec<(PendingRoute, Option<Published>)> = Vec::new();
        if !gathered || functions.try_reserve_exact(sources.routes.len()).is_err() {
            return InterruptRouting::Refused(tairix_kernel_core::iommu::RemapError::Unsupported);
        }
        functions.extend(
            sources
                .routes
                .iter()
                .map(|&route| (route, host.and_then(|host| host.published(route.node)))),
        );
        let vectors =
            u32::try_from(tairix_arch_x86_64::irq::EXTERNAL_VECTOR_COUNT).unwrap_or(u32::MAX);
        let firmware_x2apic = tairix_arch_x86_64::apic::x2apic();
        let extended = firmware_x2apic
            || tairix_arch_x86_64::apic::may_enter_x2apic(
                tairix_arch_x86_64::apic::x2apic_capable(),
                sources.x2apic_opt_out,
                remapper.extended(),
            );
        let (mut routing, plan) = plan(
            remapper,
            &pins,
            &sources.ioapics,
            &functions,
            extended,
            vectors.saturating_mul(cpus.max(1)),
        );
        if !matches!(
            routing,
            InterruptRouting::Remapped | InterruptRouting::Unrouted(_)
        ) {
            return routing;
        }
        if extended && !firmware_x2apic {
            // SAFETY: the boot CPU, before any other starts and before any
            // interrupt is taken; `may_enter_x2apic` allowed it only on a CPU
            // that has it, and remapping in extended mode now names its
            // destinations.
            unsafe { tairix_arch_x86_64::apic::enter_x2apic() };
        }
        for (gsi, redirection) in plan.pins {
            // Every pin came from the controller's own record.
            let _ = controller.remap_pin(gsi, redirection);
        }
        let mut failed: u32 = 0;
        for (function, message, entry) in plan.routes {
            let routed = host
                .and_then(|host| {
                    host.with(function.segment, |bus| {
                        bus.route_msix(function.address, MSIX_ENTRY, message, &KernelRegisters)
                    })
                })
                .is_some_and(|result| result.is_ok());
            if !routed {
                remapper.release(entry);
                failed = failed.saturating_add(1);
                crate::pci_probe::log_discovery(
                    log,
                    Level::Warn,
                    "pci function's remapped interrupt unwritable; left unrouted",
                );
            }
        }
        if failed != 0 {
            let before = match routing {
                InterruptRouting::Unrouted(count) => count,
                _ => 0,
            };
            routing = InterruptRouting::Unrouted(before.saturating_add(failed));
        }
        publish_remapper(remapper);
        routing
    }
}

#[cfg(test)]
#[path = "remapping_tests.rs"]
mod tests;
