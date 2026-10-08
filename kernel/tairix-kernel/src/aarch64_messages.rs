//! Message-signalled interrupts on aarch64 (`plans/IOMMU.md` IOM18.2): the
//! line space they take above the GIC's own INTIDs, and the LPIs a GICv3
//! interrupt translation service raises for each PCI function's MSI-X.
//!
//! Lines `0..=MAX_INTID` are the GIC's INTIDs, the root-complex MSI
//! controller's vectors follow, then one line per LPI the boot routed, in the
//! order it routed them. An LPI is raised only by the `DeviceID` and `EventID`
//! it was mapped for, so a device forging another event, or another device's,
//! raises nothing.
//!
//! The boot probe offers each function a route ([`LpiPlanner`]): the `DeviceID`
//! its host's `msi-map` gives its requester id, the next `EventID` of that
//! `DeviceID`, the next LPI. Once the GIC is up and frames exist, the port maps
//! every route the functions took at once, before any of them can master.

use alloc::vec::Vec;

use tairix_abi::driver::msix::MsiMessage;
use tairix_abi::HwResource;
use tairix_arch_aarch64::gicv3::FIRST_LPI;
use tairix_arch_aarch64::its::{ItsFeatures, ItsRoute, TRANSLATER, TRANSLATION_PAGE};
use tairix_fdt::pci::PciHost;
use tairix_fdt::Fdt;

use crate::pci_fdt::{MessageRoute, MessageRouter};

/// The first line of the root-complex MSI controller's vectors, just past
/// the highest SPI.
pub const MSI_LINE_BASE: u32 = tairix_arch_aarch64::gic::MAX_INTID + 1;

/// The last line of the root-complex MSI controller's vectors.
pub const MSI_LINE_TOP: u32 = MSI_LINE_BASE + tairix_arch_aarch64::brcm_msi::NUM_MSI_VECTORS - 1;

/// The line the first LPI the boot routes raises.
pub const LPI_LINE_BASE: u32 = MSI_LINE_TOP + 1;

/// The fewest INTID bits LPI tables cover: the first LPI's, and the
/// architecture's minimum.
const MIN_LPI_ID_BITS: u32 = 14;

/// One interrupt translation service the boot can route messages through.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Service {
    /// The phandle a host's `msi-map` names it by.
    pub phandle: u32,
    /// Its register frames' base.
    pub base: u64,
    /// What it takes.
    pub features: ItsFeatures,
}

/// A route a function took through `services[service]`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Lpi {
    /// Its service, by index.
    pub service: usize,
    /// Its `DeviceID`, `EventID` and LPI.
    pub route: ItsRoute,
}

/// The routes the boot probe recorded, for the port to map once the GIC is
/// up.
#[derive(Debug, Default)]
pub struct LpiRoutes {
    /// Every service a route goes through.
    pub services: Vec<Service>,
    /// Every route, in the order it was taken: route `i` raises LPI
    /// `FIRST_LPI + i` on line `LPI_LINE_BASE + i`.
    pub lpis: Vec<Lpi>,
}

impl LpiRoutes {
    /// The INTID bits LPI tables covering every route need.
    #[must_use]
    pub fn id_bits(&self) -> u32 {
        let highest = FIRST_LPI.saturating_add(u32::try_from(self.lpis.len()).unwrap_or(u32::MAX));
        (u32::BITS - highest.saturating_sub(1).leading_zeros()).max(MIN_LPI_ID_BITS)
    }

    /// The line LPI `intid` raises; [`None`] for one no route took.
    #[must_use]
    pub fn line_of(&self, intid: u32) -> Option<u32> {
        let index = intid.checked_sub(FIRST_LPI)?;
        (usize::try_from(index).ok()? < self.lpis.len()).then(|| LPI_LINE_BASE + index)
    }

    /// The last line a route raises; [`None`] where none was taken.
    #[must_use]
    pub fn last_line(&self) -> Option<u32> {
        let count = u32::try_from(self.lpis.len()).ok()?;
        count.checked_sub(1).map(|last| LPI_LINE_BASE + last)
    }
}

/// Offers each PCI function a route through the service its host's `msi-map`
/// names.
pub struct LpiPlanner {
    routes: LpiRoutes,
    /// How many events each `(service, DeviceID)` has taken, by key.
    events: Vec<((usize, u32), u32)>,
    /// The most LPIs the distributor's INTID bits leave above the first.
    capacity: u32,
    offered: Option<Lpi>,
}

impl LpiPlanner {
    /// A planner routing through `services`, at most `capacity` LPIs.
    #[must_use]
    pub fn new(services: Vec<Service>, capacity: u32) -> Self {
        Self {
            routes: LpiRoutes {
                services,
                lpis: Vec::new(),
            },
            events: Vec::new(),
            capacity,
            offered: None,
        }
    }

    /// A planner for a distributor of `id_bits` INTID bits, the LPIs above
    /// the first that they name.
    #[must_use]
    pub fn for_distributor(services: Vec<Service>, id_bits: u32) -> Self {
        let lpis = 1u64
            .checked_shl(id_bits)
            .map_or(0, |end| end.saturating_sub(u64::from(FIRST_LPI)));
        Self::new(services, u32::try_from(lpis).unwrap_or(u32::MAX))
    }

    /// The routes taken.
    #[must_use]
    pub fn into_routes(self) -> LpiRoutes {
        self.routes
    }

    fn next_event(&self, key: (usize, u32)) -> u32 {
        self.events
            .binary_search_by_key(&key, |&(at, _)| at)
            .map_or(0, |found| self.events[found].1)
    }
}

impl MessageRouter for LpiPlanner {
    fn offer(
        &mut self,
        _fdt: &Fdt<'_>,
        host: &PciHost<'_>,
        _node: u32,
        requester: u16,
    ) -> Option<MessageRoute> {
        self.offered = None;
        let (phandle, device) = host.msi_target(u32::from(requester)).ok()??;
        let service = self
            .routes
            .services
            .iter()
            .position(|service| service.phandle == phandle)?;
        let features = self.routes.services[service].features;
        let event = self.next_event((service, device));
        let index = u32::try_from(self.routes.lpis.len())
            .ok()
            .filter(|&index| index < self.capacity)?;
        if u64::from(device) >= 1 << features.device_bits
            || u64::from(event) >= 1 << features.event_bits
        {
            return None;
        }
        // Room is made now, so the acceptance cannot fail.
        self.routes.lpis.try_reserve(1).ok()?;
        self.events.try_reserve(1).ok()?;
        let base = self.routes.services[service].base;
        let doorbell =
            HwResource::msi_doorbell(base + TRANSLATION_PAGE, tairix_abi::PAGE_SIZE as u64).ok()?;
        self.offered = Some(Lpi {
            service,
            route: ItsRoute {
                device,
                event,
                lpi: FIRST_LPI.checked_add(index)?,
            },
        });
        Some(MessageRoute {
            message: MsiMessage {
                address: base + TRANSLATER,
                data: event,
            },
            line: LPI_LINE_BASE.checked_add(index)?,
            doorbell,
        })
    }

    fn accept(&mut self) {
        let Some(lpi) = self.offered.take() else {
            return;
        };
        self.routes.lpis.push(lpi);
        let key = (lpi.service, lpi.route.device);
        match self.events.binary_search_by_key(&key, |&(at, _)| at) {
            Ok(found) => self.events[found].1 += 1,
            Err(at) => self.events.insert(at, (key, 1)),
        }
    }
}

/// The routes the boot probe recorded, published once.
#[cfg(all(freestanding, kernel_isa = "aarch64"))]
static ROUTES: tairix_sync::once::OnceCell<LpiRoutes> = tairix_sync::once::OnceCell::new();

/// Publish the routes the boot probe recorded; the first publication wins.
#[cfg(all(freestanding, kernel_isa = "aarch64"))]
pub fn publish(routes: LpiRoutes) {
    let _ = ROUTES.set(routes);
}

/// The routes published; [`None`] before the probe published any.
#[cfg(all(freestanding, kernel_isa = "aarch64"))]
#[must_use]
pub fn published() -> Option<&'static LpiRoutes> {
    ROUTES.get().ok().flatten()
}

/// A planner over every interrupt translation service beneath the GICv3 in
/// `fdt` that answers as one, for a boot CPU whose redistributor among
/// `regions` can take LPIs: none on a GICv2, on a distributor or
/// redistributor without LPIs, or where firmware left them on.
#[cfg(all(freestanding, kernel_isa = "aarch64"))]
#[must_use]
pub fn discover(
    fdt: &Fdt<'_>,
    regions: &[tairix_arch_aarch64::gicv3::RedistributorRegion],
    stride: Option<u64>,
) -> LpiPlanner {
    use tairix_arch_aarch64::gic;
    use tairix_arch_aarch64::its::{Its, VolatileItsMmio, FRAMES_BYTES};

    let mut services = Vec::new();
    let id_bits = gic::lpi_id_bits().filter(|_| gic::local_lpis_available(regions, stride));
    if id_bits.is_some() {
        gic::for_each_its(fdt, |phandle, base, len| {
            let (Some(phandle), Ok(frame)) = (phandle, usize::try_from(base)) else {
                return;
            };
            if len < FRAMES_BYTES || services.try_reserve(1).is_err() {
                return;
            }
            // SAFETY: `frame` is a service's control frame the tree names
            // beneath the GIC, mapped as Device memory with the GIC's other
            // windows; reading its identification drives nothing.
            let its = Its::new(unsafe { VolatileItsMmio::new(frame) });
            if let Ok(features) = its.features() {
                services.push(Service {
                    phandle,
                    base,
                    features,
                });
            }
        });
    }
    LpiPlanner::for_distributor(services, id_bits.unwrap_or(0))
}

/// Map every published route: the device CPU's redistributor given LPI
/// tables covering them all, each service taken over with the routes through
/// it. Run once, after the GIC is up and before any interrupt is taken or
/// driver admitted, so no function raises a message the mapping has not
/// reached.
#[cfg(all(freestanding, kernel_isa = "aarch64"))]
pub fn route(
    frames: &'static tairix_kernel_mem::FrameAllocator,
    log: &dyn tairix_log::Sink,
) -> tairix_kernel_core::iommu::InterruptRouting {
    use tairix_arch_aarch64::gic;
    use tairix_arch_aarch64::gicv3::LpiTables;
    use tairix_arch_aarch64::its::{Its, VolatileItsMmio};
    use tairix_kernel_core::iommu::InterruptRouting;

    use crate::aarch64::gic_irq::DEVICE_IRQ_CPU;
    use crate::pci_fdt::{log_unrouted, routing_refused};

    let Some(routes) = published().filter(|routes| !routes.lpis.is_empty()) else {
        return InterruptRouting::Native;
    };
    let refused = |reason| routing_refused(log, reason);
    let Ok(tables) = crate::aarch64::spawn_producer::page_table_source(frames) else {
        return refused("no_frame_source");
    };
    let id_bits = routes.id_bits();
    let enabled = u32::try_from(routes.lpis.len()).unwrap_or(u32::MAX);
    let Some(lpis) = LpiTables::allocate(tables, id_bits, enabled) else {
        return refused("no_lpi_tables");
    };
    // SAFETY: once, for the redistributor of the CPU every device interrupt
    // is routed to; the tables were drawn for it alone and stay its own.
    if let Err(err) = unsafe { gic::enable_lpis(DEVICE_IRQ_CPU, lpis) } {
        return refused(err.as_str());
    }
    let mut failed = 0u32;
    for (index, service) in routes.services.iter().enumerate() {
        let through = || routes.lpis.iter().filter(|lpi| lpi.service == index);
        let count = through().count();
        if count == 0 {
            continue;
        }
        let mut mapped = Vec::new();
        if mapped.try_reserve_exact(count).is_err() {
            failed = failed.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
            log_unrouted(log, "no_memory");
            continue;
        }
        mapped.extend(through().map(|lpi| lpi.route));
        let outcome = gic::collection_target(DEVICE_IRQ_CPU, service.features.physical_targets)
            .map_err(gic::GicError::as_str)
            .and_then(|target| {
                let frame = usize::try_from(service.base).map_err(|_| "its_out_of_range")?;
                // SAFETY: the service `discover` found beneath the GIC,
                // mapped as Device memory; this is its one take-over.
                let its = Its::new(unsafe { VolatileItsMmio::new(frame) });
                its.take_over(tables, 1)
                    .and_then(|mut unit| unit.map(0, target, id_bits, &mut mapped))
                    .map_err(tairix_arch_aarch64::its::ItsError::as_str)
            });
        if let Err(reason) = outcome {
            failed = failed.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
            log_unrouted(log, reason);
        }
    }
    InterruptRouting::with_unrouted(failed)
}

#[cfg(test)]
#[path = "aarch64_messages_tests.rs"]
mod tests;
