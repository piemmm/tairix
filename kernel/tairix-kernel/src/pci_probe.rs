//! What the kernel's PCI probe makes of one segment, whatever the port
//! (`plans/IOMMU.md` IOM7, IOM8): how a translation unit knows each
//! function's DMA, which functions the probe stops mastering before any unit
//! takes over, and the record of the functions whose configuration space the
//! kernel owns.

use alloc::vec::Vec;

use tairix_abi::driver::pci::{config_address, PciBus, BUS_MASTER_ENABLE, COMMAND_OFFSET};
use tairix_abi::driver::virtio_pci::VIRTIO_PCI_VENDOR_ID;
use tairix_abi::{DriverError, HwNode, IommuGroup, IommuStreams, HW_NODE_MAX_RESOURCES};
use tairix_inline::ArrayVec;
use tairix_pci::topology::{Function as PciFunction, Topology};

use crate::pci_host::Function;

/// The aliases one function can carry: its node's resources bound them.
pub type Aliases = ArrayVec<IommuStreams, HW_NODE_MAX_RESOURCES>;

/// How a translation unit knows one function's DMA.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionDma {
    /// Its own stream, on the unit translating it.
    pub stream: IommuStreams,
    /// The further streams the fabric delivers its DMA as.
    pub aliases: Aliases,
    /// Its isolation group; [`None`] where no one unit can confine it — its
    /// group's members sit behind more than one unit, or the fabric tags its
    /// DMA with more streams than a node can name.
    pub group: Option<IommuGroup>,
}

/// How the units behind one segment know each of its functions' DMA.
pub struct SegmentDma<'t> {
    topology: &'t Topology,
    /// Each function's, by its index in the topology.
    functions: Vec<Option<FunctionDma>>,
}

impl<'t> SegmentDma<'t> {
    /// Each function of `topology`'s DMA identity, where `unit` names the
    /// node of the unit translating the function with a given requester id.
    /// The fabric delivers a function's DMA on its own unit under every
    /// alias, so a group is one unit's exactly when every member behind a
    /// unit is behind that one.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] when the identities cannot be held, and
    /// [`DriverError::DeviceFault`] for a function behind a unit whose streams
    /// cannot be named: it is never published as an untranslated one.
    pub fn new(
        topology: &'t Topology,
        unit: &dyn Fn(u16) -> Option<u32>,
    ) -> Result<Self, DriverError> {
        let count = topology.functions().len();
        let mut units: Vec<Option<u32>> = Vec::new();
        units
            .try_reserve_exact(count)
            .map_err(|_| DriverError::NoSpace)?;
        units.extend(topology.functions().iter().map(|f| unit(f.requester_id())));
        let mut groups: Vec<(u16, u32)> = Vec::new();
        groups
            .try_reserve_exact(count)
            .map_err(|_| DriverError::NoSpace)?;
        groups.extend(
            units
                .iter()
                .enumerate()
                .filter_map(|(index, unit)| Some((topology.group(index), (*unit)?))),
        );
        groups.sort_unstable();
        groups.dedup();
        let spans = |group: u16| {
            let first = groups.partition_point(|&(at, _)| at < group);
            groups[first..]
                .iter()
                .take_while(|&&(at, _)| at == group)
                .count()
                > 1
        };
        let mut functions = Vec::new();
        functions
            .try_reserve_exact(count)
            .map_err(|_| DriverError::NoSpace)?;
        for (index, unit) in units.iter().enumerate() {
            functions.push(
                unit.map(|unit| identity(topology, index, unit, &spans))
                    .transpose()?,
            );
        }
        Ok(Self {
            topology,
            functions,
        })
    }

    /// How a unit knows the DMA of the function at configuration `address`;
    /// [`None`] for one no unit translates.
    #[must_use]
    pub fn of(&self, address: u64) -> Option<&FunctionDma> {
        self.topology
            .index_of(address)
            .and_then(|index| self.functions[index].as_ref())
    }
}

/// The identity of the function at `index`, translated by `unit`.
fn identity(
    topology: &Topology,
    index: usize,
    unit: u32,
    spans: &dyn Fn(u16) -> bool,
) -> Result<FunctionDma, DriverError> {
    let own = topology.functions()[index].requester_id();
    let stream =
        IommuStreams::new(unit, u32::from(own), 1).map_err(|_| DriverError::DeviceFault)?;
    let mut aliases = Aliases::new();
    let mut confinable = true;
    for alias in topology.aliases(index) {
        let alias = u32::from(alias.requester);
        if alias == u32::from(own) || aliases.as_slice().iter().any(|a| a.first() == alias) {
            continue;
        }
        let range = IommuStreams::new(unit, alias, 1).map_err(|_| DriverError::DeviceFault)?;
        confinable &= aliases.try_push(range).is_ok();
    }
    let group = topology.group(index);
    Ok(FunctionDma {
        stream,
        aliases,
        group: (confinable && !spans(group)).then(|| IommuGroup::new(unit, u32::from(group))),
    })
}

/// Stop every function of `topology` that TAIRiX takes from firmware
/// mastering DMA, before any unit is taken over: TAIRiX makes a function a
/// bus master only as it hands it to an owner (`plans/IOMMU.md` IOM7).
///
/// That is every virtio function, which the probe hands to drivers, and every
/// function behind a unit (`dma` knows it), whose stream the unit blocks
/// anyway — whether or not its unit then comes up, since a function firmware
/// keeps no window for has no claim on DMA after the hand-off. A function
/// whose stream firmware keeps a window for (`keeps`) keeps mastering, as
/// firmware still uses it. A function that masters nothing of its own
/// ([`tairix_pci::topology::Function::masters_dma`]) is left alone, as is
/// every other function behind no unit: nothing would confine it. A bit
/// already clear is not written.
///
/// # Errors
///
/// The first configuration access that fails; the functions after it are not
/// reached.
pub fn stop_mastering(
    bus: &dyn PciBus,
    topology: &Topology,
    dma: &SegmentDma<'_>,
    keeps: &dyn Fn(IommuStreams) -> bool,
) -> Result<(), DriverError> {
    for function in topology.functions().iter().filter(|f| f.masters_dma()) {
        let taken = match dma.of(function.address) {
            Some(identity) => !keeps(identity.stream),
            None => function.vendor == VIRTIO_PCI_VENDOR_ID,
        };
        if !taken {
            continue;
        }
        let command = bus.read_config(function.address, COMMAND_OFFSET)?;
        if command != u32::MAX && command & BUS_MASTER_ENABLE != 0 {
            bus.set_bus_master(function.address, false)?;
        }
    }
    Ok(())
}

/// Whether a function found by a walk that formed no hierarchy is stopped
/// mastering. No unit's scope can be resolved without the hierarchy, so
/// where a unit covers the segment (`covered`) every function that masters
/// DMA of its own is taken to be behind one, no firmware window can be
/// vouched for, and every bridge is stopped too, so nothing below it reaches
/// memory through it however its buses were numbered; elsewhere only a
/// virtio function, which TAIRiX would have driven, is stopped, as
/// [`stop_mastering`] stops one behind no unit. A host bridge, the root
/// complex's own function, is never stopped.
#[must_use]
pub fn stopped_unresolved(function: &PciFunction, covered: bool) -> bool {
    if covered {
        function.masters_dma() || function.is_bridge()
    } else {
        function.masters_dma() && function.vendor == VIRTIO_PCI_VENDOR_ID
    }
}

/// The functions whose bus mastering the kernel owns: each one behind a unit
/// that masters DMA of its own, published or not, and each node the probe
/// published (`published`), known by the requester id the probe itself
/// recorded.
///
/// # Errors
///
/// [`DriverError::NoSpace`] when the record cannot be held.
pub fn record_functions(
    topology: &Topology,
    dma: &SegmentDma<'_>,
    published: &[HwNode],
) -> Result<Vec<Function>, DriverError> {
    let mut functions = Vec::new();
    functions
        .try_reserve_exact(topology.functions().len() + published.len())
        .map_err(|_| DriverError::NoSpace)?;
    functions.extend(
        topology
            .functions()
            .iter()
            .filter(|function| function.masters_dma())
            .filter_map(|function| {
                dma.of(function.address).map(|identity| Function {
                    address: function.address,
                    node: None,
                    stream: Some(identity.stream),
                })
            }),
    );
    // Ascending by address, as the topology holds them.
    let behind = functions.len();
    for node in published {
        let Ok(requester) = u16::try_from(node.address()) else {
            continue;
        };
        let address = config_address(requester);
        match functions[..behind].binary_search_by_key(&address, |function| function.address) {
            Ok(at) => functions[at].node = Some(node.id()),
            Err(_) => functions.push(Function {
                address,
                node: Some(node.id()),
                stream: None,
            }),
        }
    }
    Ok(functions)
}

#[cfg(test)]
#[path = "pci_probe_tests.rs"]
mod tests;
