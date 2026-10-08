//! riscv64 early-boot platform discovery.
//!
//! Implements the Arch HAL
//! [`PlatformDiscovery`](tairix_arch_api::PlatformDiscovery) slice over the
//! shared device-tree walk ([`tairix_arch_api::fdtwalk`]), which normalises
//! the flattened device tree the `virt` board hands the kernel into
//! [`tairix_abi::hwtree`] nodes generically: every node carrying a
//! `compatible` becomes a hardware-tree node whose match keys are that
//! property's strings, `reg` entries become capability-gated MMIO resources
//! translated through each ancestor bus's `ranges`, and `/memory` nodes
//! become `Memory` nodes.
//!
//! What is genuinely this port's, and so lives here, is the interrupt
//! specifier of the tree's supervisor-level controller: a PLIC's single cell
//! is the source number, an APLIC's two cells the source and its sense, and
//! source `0` is "no interrupt" on both. The controller's discovered source
//! count bounds a source, read once before the walk, so a device is never
//! bound to a line the controller cannot raise.

use crate::fdt::{
    is_aplic, is_imsic, is_plic, plic_line, plic_ndev, plic_phandle, supervisor_aplic, Fdt,
};
use tairix_abi::DmaCoherence;
use tairix_arch_api::fdtwalk::FdtPlatform;
use tairix_fdt::Node;

/// The `#interrupt-cells` sense of a rising edge.
const SENSE_EDGE_RISING: u32 = 1;
/// The `#interrupt-cells` sense of a high level.
const SENSE_LEVEL_HIGH: u32 = 4;

/// The supervisor-level controller a tree's devices name as their interrupt
/// parent.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Root {
    Plic {
        phandle: Option<u32>,
        sources: Option<u32>,
    },
    Aplic {
        phandle: u32,
        sources: u32,
    },
}

/// This port's half of the shared device-tree walk.
pub struct Riscv64Fdt {
    root: Root,
}

/// A line a specifier names, and how it signals.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Line {
    /// The line a granted driver binds.
    pub line: u32,
    /// Raised by edges rather than a level.
    pub edge: bool,
}

impl FdtPlatform for Riscv64Fdt {
    fn interrupt_cells(&self) -> usize {
        match self.root {
            Root::Plic { .. } => 1,
            Root::Aplic { .. } => 2,
        }
    }

    /// An APLIC that delivers by MSI to supervisor-level files is preferred
    /// to a PLIC, as the controller whose messages a unit can confine.
    fn from_tree(fdt: &Fdt<'_>) -> Self {
        let root = match supervisor_aplic(fdt) {
            Some(aplic) => Root::Aplic {
                phandle: aplic.phandle,
                sources: aplic.sources,
            },
            None => Root::Plic {
                phandle: plic_phandle(fdt),
                sources: plic_ndev(fdt),
            },
        };
        Self { root }
    }

    fn root_interrupt_controller(&self) -> Option<u32> {
        match self.root {
            Root::Plic { phandle, .. } => phandle,
            Root::Aplic { phandle, .. } => Some(phandle),
        }
    }

    fn kernel_driven(&self, node: &Node<'_>) -> bool {
        is_plic(node) || is_aplic(node) || is_imsic(node)
    }

    // A RISC-V tree states `dma-noncoherent` on every master that does
    // not snoop.
    const DEFAULT_DMA_COHERENCE: DmaCoherence = DmaCoherence::Snooped;

    fn interrupt_line(&self, specifier: &[u8]) -> Option<u32> {
        self.decode(specifier).map(|line| line.line)
    }

    fn edge_triggered(&self, specifier: &[u8]) -> bool {
        self.decode(specifier).is_some_and(|line| line.edge)
    }
}

impl Riscv64Fdt {
    /// The line a specifier of `cells` names: [`None`] for source `0`, a
    /// source past the controller's count, or a sense other than a high
    /// level or a rising edge, which a grant cannot carry.
    #[must_use]
    pub fn line(&self, cells: &[u32]) -> Option<Line> {
        match (self.root, cells) {
            (Root::Plic { sources, .. }, &[source]) => Some(Line {
                line: plic_line(source, sources)?,
                edge: false,
            }),
            (Root::Aplic { sources, .. }, &[source, sense]) => {
                let edge = match sense {
                    SENSE_EDGE_RISING => true,
                    SENSE_LEVEL_HIGH => false,
                    _ => return None,
                };
                (1..=sources)
                    .contains(&source)
                    .then_some(Line { line: source, edge })
            }
            _ => None,
        }
    }

    /// The line `node`'s first `interrupts` specifier names.
    #[must_use]
    pub fn node_line(&self, node: &Node<'_>) -> Option<Line> {
        let value = node.property("interrupts")?.value();
        let first = value.get(..4 * self.interrupt_cells())?;
        self.decode(first)
    }

    fn decode(&self, specifier: &[u8]) -> Option<Line> {
        let (cells, rest) = specifier.as_chunks::<4>();
        if !rest.is_empty() || cells.len() > 2 {
            return None;
        }
        let mut values = [0; 2];
        for (value, cell) in values.iter_mut().zip(cells) {
            *value = u32::from_be_bytes(*cell);
        }
        self.line(&values[..cells.len()])
    }
}

/// The [`PlatformDiscovery`](tairix_arch_api::PlatformDiscovery)
/// implementation the boot path constructs: the shared walk over this
/// port's [`Riscv64Fdt`].
pub type FdtDiscovery<'a> = tairix_arch_api::fdtwalk::FdtDiscovery<'a, Riscv64Fdt>;

#[cfg(test)]
mod tests {
    use super::FdtDiscovery;
    use crate::fdt::tests::virt_like;
    use crate::fdt::Fdt;
    use tairix_abi::{HwDeviceClass, HwNode, HwResourceKind};
    use tairix_arch_api::platform::{conformance, DiscoveryError, HwNodeSink, PlatformDiscovery};

    /// The `virt_like` fixture's PLIC source for its one virtio-mmio slot.
    const SLOT_PLIC_IRQ: u32 = 1;

    /// A PLIC source decodes to its line only inside the controller's
    /// count, and never as the reserved sentinel: the one rule every
    /// consumer of the specifier, PCI INTx included, reads it by.
    #[test]
    fn a_source_names_a_line_only_inside_the_controller_s_count() {
        use super::Riscv64Fdt;
        use tairix_arch_api::fdtwalk::FdtPlatform;
        let ndev = 96;
        let blob = crate::fdt::tests::virt_like_with_virtio(
            0x8000_0000,
            0x1000_0000,
            10_000_000,
            ndev,
            &[(0x1000_1000, 1)],
        );
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let plic = Riscv64Fdt::from_tree(&fdt);
        let line = |source| plic.line(&[source]).map(|line| line.line);
        assert_eq!(line(0), None);
        assert_eq!(line(1), Some(1));
        assert_eq!(line(ndev), Some(ndev));
        assert_eq!(line(ndev + 1), None);
        assert_eq!(plic.line(&[1, 4]), None, "a PLIC specifier is one cell");
    }

    /// An APLIC's two cells are the source and its sense: a high level or a
    /// rising edge names a line, any other sense none.
    #[test]
    fn an_aplic_specifier_names_its_source_and_sense() {
        use super::{Line, Riscv64Fdt};
        use tairix_arch_api::fdtwalk::FdtPlatform;
        use tairix_fdt::fixture::{virt_like_aia, VIRT_APLIC_PHANDLE};
        let blob = virt_like_aia(1, &[]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let aplic = Riscv64Fdt::from_tree(&fdt);
        assert_eq!(aplic.interrupt_cells(), 2);
        assert_eq!(aplic.root_interrupt_controller(), Some(VIRT_APLIC_PHANDLE));
        assert_eq!(
            aplic.line(&[10, 4]),
            Some(Line {
                line: 10,
                edge: false
            })
        );
        assert_eq!(
            aplic.line(&[0x24, 1]),
            Some(Line {
                line: 0x24,
                edge: true
            })
        );
        for refused in [[10, 2], [10, 8], [10, 0], [0, 4], [97, 4]] {
            assert_eq!(aplic.line(&refused), None, "{refused:?}");
        }
        assert_eq!(aplic.line(&[10]), None, "an APLIC specifier is two cells");
    }

    /// On an AIA tree every controller is the kernel's, and each slot carries
    /// the line and trigger its two cells name.
    #[test]
    fn an_aia_tree_grants_each_slot_its_source_with_its_trigger() {
        use tairix_fdt::fixture::virt_like_aia;
        let nodes = discover_all(&virt_like_aia(
            1,
            &[
                (0x1000_1000, 8, 4),
                (0x1000_2000, 9, 1),
                (0x1000_3000, 10, 8),
            ],
        ));
        for compatible in ["riscv,aplic", "riscv,imsics"] {
            let wanted = tairix_abi::HwMatchKey::compatible(compatible.as_bytes()).expect("fits");
            let controllers: std::vec::Vec<&HwNode> = nodes
                .iter()
                .filter(|n| n.match_keys().contains(&wanted))
                .collect();
            assert_eq!(controllers.len(), 2, "{compatible}: machine and supervisor");
            assert!(
                controllers.iter().all(|n| n.is_kernel_driven()),
                "{compatible}"
            );
        }
        let irqs = |base: u64| -> std::vec::Vec<(u64, bool)> {
            let slot = nodes
                .iter()
                .find(|n| {
                    n.resources()
                        .iter()
                        .any(|r| r.kind() == Some(HwResourceKind::Mmio) && r.base() == base)
                })
                .expect("the slot");
            slot.resources()
                .iter()
                .filter(|r| r.kind() == Some(HwResourceKind::Irq))
                .map(|r| (r.base(), r.is_edge_triggered()))
                .collect()
        };
        assert_eq!(irqs(0x1000_1000), [(8, false)]);
        assert_eq!(irqs(0x1000_2000), [(9, true)]);
        assert_eq!(
            irqs(0x1000_3000),
            [],
            "a low level is refused, not inverted"
        );
    }

    #[test]
    fn passes_platform_discovery_conformance() {
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let disco = FdtDiscovery::new(fdt);
        conformance::run(&disco);
    }

    /// Collects the emitted tree so a test can assert the exact nodes the
    /// blob yields.
    #[derive(Default)]
    struct CollectingSink {
        nodes: std::vec::Vec<HwNode>,
    }

    impl HwNodeSink for CollectingSink {
        fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
            self.nodes.push(node);
            Ok(())
        }
    }

    fn discover_all(blob: &[u8]) -> std::vec::Vec<HwNode> {
        let fdt = Fdt::new(blob).expect("valid fdt");
        let mut sink = CollectingSink::default();
        FdtDiscovery::new(fdt)
            .discover(&mut sink)
            .expect("discovery succeeds");
        sink.nodes
    }

    fn by_key(nodes: &[HwNode], compatible: &str) -> HwNode {
        let wanted = tairix_abi::HwMatchKey::compatible(compatible.as_bytes()).expect("fits");
        *nodes
            .iter()
            .find(|n| n.match_keys().contains(&wanted))
            .unwrap_or_else(|| panic!("a node carries {compatible}"))
    }

    /// The `virt`-shaped tree every emission test reads: `/memory`, the
    /// PLIC declaring one source, and one `virtio,mmio` slot on the source
    /// named by `slot_irq`.
    fn virt_tree(slot_irq: u32) -> std::vec::Vec<u8> {
        crate::fdt::tests::virt_like_with_virtio(
            0x8000_0000,
            0x1000_0000,
            10_000_000,
            1,
            &[(0x1000_1000, slot_irq)],
        )
    }

    #[test]
    fn emits_the_memory_window_and_every_compatible_node() {
        let nodes = discover_all(&virt_tree(SLOT_PLIC_IRQ));

        let memory: std::vec::Vec<&HwNode> = nodes
            .iter()
            .filter(|n| n.class() == Some(HwDeviceClass::Memory))
            .collect();
        assert_eq!(memory.len(), 1, "one described memory window");
        let window = memory[0].resources()[0];
        assert_eq!((window.base(), window.length()), (0x8000_0000, 0x1000_0000));

        // The PLIC is discovered as an interrupt controller with its own
        // register window — the node the boot path reads `riscv,ndev` and
        // `reg` off, now visible in the tree a tool can list.
        let plic = by_key(&nodes, "riscv,plic0");
        assert_eq!(plic.class(), Some(HwDeviceClass::InterruptController));
        assert!(
            plic.is_kernel_driven(),
            "no driver may be loaded for the PLIC"
        );
        let window = plic
            .resources()
            .iter()
            .find(|r| r.kind() == Some(tairix_abi::HwResourceKind::Mmio))
            .expect("the PLIC's window");
        assert_eq!(window.base(), 0x0c00_0000);

        // Every non-root node hangs off the root: the fixture nests no bus.
        assert!(nodes.iter().skip(1).all(|n| n.parent() == 0));
    }

    #[test]
    fn a_virtio_slot_carries_its_plic_source_as_an_irq_resource() {
        // The whole point of moving this port onto the shared walk: a
        // discovered device now carries the line its driver parks on, which
        // the previous shallow emission could not describe at all.
        let nodes = discover_all(&virt_tree(SLOT_PLIC_IRQ));
        let slot = by_key(&nodes, "virtio,mmio");
        let irqs: std::vec::Vec<u64> = slot
            .resources()
            .iter()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .map(tairix_abi::HwResource::base)
            .collect();
        assert_eq!(irqs, std::vec![u64::from(SLOT_PLIC_IRQ)]);
    }

    #[test]
    fn the_reserved_plic_sentinel_is_dropped_not_guessed() {
        // Source 0 routes to no line, so the node is still emitted (its
        // `compatible` binds a driver) but carries no interrupt.
        let nodes = discover_all(&virt_tree(0));
        let slot = by_key(&nodes, "virtio,mmio");
        assert!(
            slot.resources()
                .iter()
                .all(|r| r.kind() != Some(HwResourceKind::Irq)),
            "the reserved sentinel carries no line"
        );
    }

    #[test]
    fn a_source_above_the_controller_count_is_refused() {
        // The tree declares `riscv,ndev = 1`, so source 2 is a line this
        // PLIC cannot raise.
        let nodes = discover_all(&virt_tree(2));
        let slot = by_key(&nodes, "virtio,mmio");
        assert!(
            slot.resources()
                .iter()
                .all(|r| r.kind() != Some(HwResourceKind::Irq)),
            "an out-of-range source carries no line"
        );
    }
}
