//! riscv64 device-tree access.
//!
//! The flattened-device-tree parser itself is architecture-neutral and
//! lives once in [`tairix_fdt`] (no duplication); this
//! module re-exports it so the riscv64 boot path and the QEMU integration
//! tests keep naming `tairix_arch_riscv64::fdt::Fdt`. The riscv64-specific
//! normalisation of the tree into [`tairix_abi::hwtree`] nodes lives in
//! [`crate::platform`].
//!
//! It also carries the riscv64-specific decode of a device's **PLIC**
//! interrupt line from its device-tree `interrupts` cell — the riscv64
//! analogue of the aarch64 port's `gic_device_intid`. The QEMU `virt`
//! board declares `#interrupt-cells = <1>` on the PLIC, so a device names
//! its PLIC source number directly in a single `interrupts` cell (there is
//! no GIC-style `<type number flags>` triple). The bootstrap-floor
//! virtio-MMIO discovery walks
//! ([`tairix_kernel::hwdiscovery`](../../tairix_kernel/hwdiscovery/index.html))
//! feed this decode as their per-slot `slot_irq` resolver, so an emitted
//! device node carries the PLIC line its interrupt-driven user-space driver
//! parks on — a discovered value, never a board constant.

use tairix_fdt::Node;
pub use tairix_fdt::{Fdt, FdtError};

/// PLIC interrupt source `0` is the reserved "no interrupt" sentinel: a
/// device whose `interrupts` cell is `0` routes to no line. The RISC-V
/// PLIC numbers real sources from `1`.
pub const PLIC_SOURCE_NONE: u32 = 0;

/// Is `source` a routable PLIC interrupt line for a controller with
/// `ndev` sources?
///
/// A source is routable iff it is neither the [`PLIC_SOURCE_NONE`]
/// sentinel nor above the controller's discovered source count `ndev`
/// (PLIC sources are numbered `1..=ndev`). Everything else is refused so
/// a device is never bound to a line the controller cannot raise (fail
/// closed).
#[must_use]
pub fn plic_source_in_range(source: u32, ndev: u32) -> bool {
    source != PLIC_SOURCE_NONE && source <= ndev
}

/// The PLIC's `riscv,ndev` source count, read from the first PLIC node the
/// tree describes (`riscv,plic0` / `sifive,plic-1.0.0`).
///
/// [`None`] when the tree describes no PLIC node or its node carries no
/// readable `riscv,ndev` — the caller then falls back to the nonzero-only
/// check ([`plic_source_in_range`] is skipped), leaving the controller's
/// own arm-time range check as the backstop.
#[must_use]
pub fn plic_ndev(fdt: &Fdt<'_>) -> Option<u32> {
    plic_node(fdt)?.property("riscv,ndev")?.read_be_u32(0).ok()
}

/// The PLIC's phandle, read from the first PLIC node the tree describes: what
/// a device's effective `interrupt-parent` must name for its `interrupts`
/// cell to be a PLIC source.
#[must_use]
pub fn plic_phandle(fdt: &Fdt<'_>) -> Option<u32> {
    plic_node(fdt)?.phandle()
}

/// The `compatible` strings naming a PLIC.
const PLIC_COMPATIBLES: [&str; 2] = ["riscv,plic0", "sifive,plic-1.0.0"];

/// Whether `node` is a PLIC.
#[must_use]
pub fn is_plic(node: &Node<'_>) -> bool {
    PLIC_COMPATIBLES.iter().any(|name| node.is_compatible(name))
}

/// The first PLIC node the tree describes; a malformed node before it ends
/// the search.
fn plic_node<'a>(fdt: &Fdt<'a>) -> Option<Node<'a>> {
    for node in fdt.nodes() {
        let node = node.ok()?;
        if is_plic(&node) {
            return Some(node);
        }
    }
    None
}

/// The physical base and length of the PLIC register block, read from the
/// first PLIC node the tree describes (`riscv,plic0` / `sifive,plic-1.0.0`).
///
/// The `install_irq_dispatch` PLIC path maps the controller's registers at
/// this discovered window — values read from the firmware tree, never a
/// board constant. [`None`] when the tree describes no PLIC node or its node
/// carries no readable `reg` (the caller then wires no external-IRQ
/// dispatch and interrupt-driven bring-up fails closed).
#[must_use]
pub fn plic_window(fdt: &Fdt<'_>) -> Option<(u64, u64)> {
    let reg = plic_node(fdt)?.property("reg")?;
    Some((reg.read_be_u64(0).ok()?, reg.read_be_u64(8).ok()?))
}

/// The line PLIC source `source` raises on a controller of `ndev` sources:
/// [`None`] for the reserved sentinel, or a source past the count. With no
/// count discovered any other source is taken, the controller's own
/// arm-time guard bounding it.
#[must_use]
pub fn plic_line(source: u32, ndev: Option<u32>) -> Option<u32> {
    match ndev {
        Some(ndev) => plic_source_in_range(source, ndev).then_some(source),
        None => (source != PLIC_SOURCE_NONE).then_some(source),
    }
}

/// The cause an IMSIC's `interrupts-extended` names for a hart's
/// supervisor-level interrupt file.
const SUPERVISOR_EXTERNAL: u32 = 9;

/// One interrupt file's page; a file with guest files holds its own first.
pub const FILE_PAGE: u64 = 0x1000;

/// The most guest index bits an IMSIC states (RISC-V AIA 1.0, 3.6): at most
/// 63 guest files beside a hart's own.
const MAX_GUEST_INDEX_BITS: u32 = 6;

/// The most interrupt identities an IMSIC file implements.
pub const MAX_IDENTITIES: u32 = 2047;

/// The most sources an APLIC domain has.
pub const MAX_APLIC_SOURCES: u32 = 1023;

/// Whether `node` is an APLIC domain.
#[must_use]
pub fn is_aplic(node: &Node<'_>) -> bool {
    node.is_compatible("riscv,aplic")
}

/// Whether `node` is an IMSIC.
#[must_use]
pub fn is_imsic(node: &Node<'_>) -> bool {
    node.is_compatible("riscv,imsics")
}

/// The supervisor-level interrupt file of one hart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ImsicFile {
    /// The IMSIC's phandle, which an APLIC or host names as its MSI parent.
    pub phandle: u32,
    /// The file's page: writing an identity there raises it.
    pub page: u64,
    /// The identities it implements, `1..=ids`.
    pub ids: u32,
    /// The hart's index among the IMSIC's files, which an APLIC target names
    /// it by.
    pub hart_index: u32,
}

/// An APLIC domain delivering by MSI.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AplicDomain {
    /// The phandle devices name as their interrupt parent.
    pub phandle: u32,
    /// Its registers.
    pub base: u64,
    /// Their length.
    pub len: u64,
    /// Its sources, `1..=sources`.
    pub sources: u32,
}

/// A supervisor-level Advanced Interrupt Architecture: an APLIC domain
/// whose MSIs land in a hart's IMSIC file.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Aia {
    /// The wired sources' domain.
    pub aplic: AplicDomain,
    /// The hart's interrupt file.
    pub imsic: ImsicFile,
}

/// The APLIC domain that delivers by MSI to supervisor-level IMSIC files,
/// when the tree describes one.
#[must_use]
pub fn supervisor_aplic(fdt: &Fdt<'_>) -> Option<AplicDomain> {
    fdt.operational_nodes()
        .map_while(Result::ok)
        .find_map(|node| {
            let parent = node.property("msi-parent")?.read_be_u32(0).ok()?;
            if !is_aplic(&node)
                || !fdt
                    .node_by_phandle(parent)
                    .is_some_and(|imsic| is_supervisor_imsic(&imsic))
            {
                return None;
            }
            let sources = node.property("riscv,num-sources")?.read_be_u32(0).ok()?;
            let reg = node.property("reg")?;
            Some(AplicDomain {
                phandle: node.phandle()?,
                base: reg.read_be_u64(0).ok()?,
                len: reg.read_be_u64(8).ok()?,
                sources: (1..=MAX_APLIC_SOURCES)
                    .contains(&sources)
                    .then_some(sources)?,
            })
        })
}

/// The supervisor-level AIA delivering to hart `hart`: the
/// [`supervisor_aplic`] and the hart's file in the IMSIC it delivers to,
/// when its files are in one group.
#[must_use]
pub fn supervisor_aia(fdt: &Fdt<'_>, hart: u64) -> Option<Aia> {
    let aplic = supervisor_aplic(fdt)?;
    let parent = fdt
        .node_by_phandle(aplic.phandle)?
        .property("msi-parent")?
        .read_be_u32(0)
        .ok()?;
    let imsic = imsic_file(&fdt.node_by_phandle(parent)?, hart_intc(fdt, hart)?)?;
    Some(Aia { aplic, imsic })
}

/// Whether `node` is an enabled IMSIC whose files are every hart's
/// supervisor level.
fn is_supervisor_imsic(node: &Node<'_>) -> bool {
    let Some(harts) = node.property("interrupts-extended") else {
        return false;
    };
    let (pairs, rest) = harts.value().as_chunks::<8>();
    is_imsic(node)
        && node.is_enabled()
        && rest.is_empty()
        && !pairs.is_empty()
        && pairs
            .iter()
            .all(|&pair| be_cell(pair, 1) == SUPERVISOR_EXTERNAL)
}

/// The file in IMSIC `node` of the hart whose local controller is `intc`.
fn imsic_file(node: &Node<'_>, intc: u32) -> Option<ImsicFile> {
    if !is_supervisor_imsic(node) {
        return None;
    }
    let harts = node.property("interrupts-extended")?;
    let hart_index = harts
        .value()
        .as_chunks::<8>()
        .0
        .iter()
        .position(|&pair| be_cell(pair, 0) == intc)?;
    let read = |name: &str| {
        node.property(name)
            .map_or(Some(0), |bits| bits.read_be_u32(0).ok())
    };
    let (groups, guests) = (
        read("riscv,group-index-bits")?,
        read("riscv,guest-index-bits")?,
    );
    let ids = node.property("riscv,num-ids")?.read_be_u32(0).ok()?;
    let reg = node.property("reg")?;
    let (base, len) = (reg.read_be_u64(0).ok()?, reg.read_be_u64(8).ok()?);
    // Past the AIA's bound a shift would drop the page's bits, folding every
    // hart onto the first.
    if guests > MAX_GUEST_INDEX_BITS {
        return None;
    }
    let offset = FILE_PAGE
        .checked_shl(guests)?
        .checked_mul(u64::try_from(hart_index).ok()?)?;
    let fits = offset.checked_add(FILE_PAGE).is_some_and(|end| end <= len);
    (groups == 0 && fits && (63..=MAX_IDENTITIES).contains(&ids)).then_some(ImsicFile {
        phandle: node.phandle()?,
        page: base.checked_add(offset)?,
        ids,
        hart_index: u32::try_from(hart_index).ok()?,
    })
}

/// The phandle of hart `hart`'s local interrupt controller: the
/// `riscv,cpu-intc` child of the `cpu` node whose `reg` is the hart id.
fn hart_intc(fdt: &Fdt<'_>, hart: u64) -> Option<u32> {
    let mut in_hart = false;
    for node in fdt.nodes() {
        let node = node.ok()?;
        match node.depth() {
            2 => {
                in_hart = tairix_fdt::name_stem(node.name()) == b"cpu"
                    && node.property("reg").and_then(|reg| cell_value(reg.value())) == Some(hart);
            }
            3 if in_hart && node.is_compatible("riscv,cpu-intc") => return node.phandle(),
            _ => {}
        }
    }
    None
}

/// Hand `visit` each register window of a device the kernel drives itself
/// that the tree names, translated through its buses: every generic ECAM
/// host's configuration region and memory windows, every RISC-V IOMMU, and
/// the interrupt controllers. What the kernel reaches from a syscall must be
/// mapped in every root, so these are the windows the direct map carries.
pub fn kernel_register_windows(fdt: &Fdt<'_>, visit: &mut dyn FnMut(u64, u64)) {
    tairix_fdt::pci::each_pci_host(fdt, |host| {
        visit(host.ecam.0, host.ecam.1);
        for window in host
            .windows()
            .filter(|window| window.space != tairix_fdt::pci::PciSpace::Io)
        {
            visit(window.cpu, window.size);
        }
    });
    let _ = tairix_fdt::bus::scan_translated(fdt, |node, levels, depth| {
        if node.is_compatible("riscv,iommu") || is_plic(node) || is_aplic(node) || is_imsic(node) {
            let entries = tairix_fdt::bus::reg_entry_count(node, depth, levels).unwrap_or(0);
            for index in 0..entries {
                if let Some((base, len)) =
                    tairix_fdt::bus::translated_reg(node, depth, levels, index)
                {
                    visit(base, len);
                }
            }
        }
        None::<()>
    });
}

fn be_cell(cells: [u8; 8], index: usize) -> u32 {
    let at = index * 4;
    u32::from_be_bytes([cells[at], cells[at + 1], cells[at + 2], cells[at + 3]])
}

/// A one- or two-cell value.
fn cell_value(value: &[u8]) -> Option<u64> {
    match value.len() {
        4 => Some(u64::from(u32::from_be_bytes(value.try_into().ok()?))),
        8 => Some(u64::from_be_bytes(value.try_into().ok()?)),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Re-export of the shared DTB test fixtures so the `platform`
    //! discovery tests and the conformance handle drive the same builder
    //! as the parser's own tests. Enabled by the
    //! `tairix-fdt/test-fixtures` feature this crate turns on in its
    //! `[dev-dependencies]`.
    pub(crate) use tairix_fdt::fixture::{virt_like, virt_like_with_virtio};

    use super::{
        plic_ndev, plic_phandle, plic_source_in_range, plic_window, Fdt, PLIC_SOURCE_NONE,
    };
    use crate::platform::Riscv64Fdt;
    use tairix_arch_api::fdtwalk::FdtPlatform;

    #[test]
    fn the_supervisor_aia_is_the_aplic_whose_msis_reach_a_harts_supervisor_file() {
        use tairix_fdt::fixture::{
            virt_like_aia, VIRT_APLIC_PHANDLE, VIRT_APLIC_S_BASE, VIRT_IMSIC_PHANDLE,
            VIRT_IMSIC_S_BASE,
        };
        let blob = virt_like_aia(2, &[]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let aia = super::supervisor_aia(&fdt, 1).expect("an AIA");
        assert_eq!(aia.aplic.phandle, VIRT_APLIC_PHANDLE);
        assert_eq!(aia.aplic.base, VIRT_APLIC_S_BASE);
        assert_eq!(aia.aplic.len, 0x8000);
        assert_eq!(aia.aplic.sources, 96);
        assert_eq!(aia.imsic.phandle, VIRT_IMSIC_PHANDLE);
        assert_eq!(aia.imsic.ids, 255);
        assert_eq!(aia.imsic.hart_index, 1, "the second hart's file");
        assert_eq!(aia.imsic.page, VIRT_IMSIC_S_BASE + 0x1000);
        assert_eq!(super::supervisor_aia(&fdt, 2), None, "no such hart");
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(super::supervisor_aia(&fdt, 0), None, "a tree with no AIA");
    }

    /// A hart's file lies past the guest files of the harts before it, and a
    /// guest index wider than the AIA allows names no file rather than
    /// folding every hart onto the first.
    #[test]
    fn a_harts_file_lies_past_the_guest_files_before_it() {
        use tairix_fdt::fixture::{virt_like_aia_with_guest_bits, VIRT_IMSIC_S_BASE};
        let blob = virt_like_aia_with_guest_bits(2, 2);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let aia = super::supervisor_aia(&fdt, 1).expect("an AIA");
        assert_eq!(aia.imsic.page, VIRT_IMSIC_S_BASE + 4 * 0x1000);
        for bits in [7, 52, 63] {
            let blob = virt_like_aia_with_guest_bits(2, bits);
            let fdt = Fdt::new(&blob).expect("valid fdt");
            assert_eq!(
                super::supervisor_aia(&fdt, 1),
                None,
                "{bits} guest index bits"
            );
        }
    }

    #[test]
    fn the_plic_phandle_is_what_devices_name_as_their_interrupt_parent() {
        let blob = tree_with(96, &[(0x1000_1000, 1)]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(
            plic_phandle(&fdt),
            Some(tairix_fdt::fixture::VIRT_PLIC_PHANDLE)
        );
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(plic_phandle(&fdt), None);
    }

    /// A `virt`-shaped tree with the given virtio-MMIO slots and PLIC
    /// source count, ready for the resolver assertions.
    fn tree_with(ndev: u32, slots: &[(u64, u32)]) -> std::vec::Vec<u8> {
        virt_like_with_virtio(0x8000_0000, 0x1000_0000, 10_000_000, ndev, slots)
    }

    /// The source the node whose registers start at `base` raises, as the
    /// boot path resolves it.
    fn source_at(fdt: &Fdt<'_>, base: u64) -> Option<u32> {
        let node = fdt.nodes().filter_map(Result::ok).find(|node| {
            node.property("reg").and_then(|reg| reg.read_be_u64(0).ok()) == Some(base)
        })?;
        Riscv64Fdt::from_tree(fdt)
            .node_line(&node)
            .map(|line| line.line)
    }

    #[test]
    fn resolves_a_slots_declared_plic_source() {
        // The `virtio,mmio` slot at 0x1000_1000 declares PLIC source 1, so
        // the resolver returns exactly that discovered line.
        let blob = tree_with(96, &[(0x1000_1000, 1), (0x1000_2000, 2)]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(source_at(&fdt, 0x1000_1000), Some(1));
        assert_eq!(source_at(&fdt, 0x1000_2000), Some(2));
    }

    #[test]
    fn the_no_source_sentinel_is_rejected() {
        // A slot whose `interrupts` cell is the PLIC "no interrupt"
        // sentinel routes to no line, so it is refused rather than bound to
        // source 0 (fail closed).
        let blob = tree_with(96, &[(0x1000_1000, PLIC_SOURCE_NONE)]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(source_at(&fdt, 0x1000_1000), None);
    }

    #[test]
    fn a_source_above_ndev_is_rejected() {
        // A source the controller cannot raise (above its discovered
        // `riscv,ndev` count) is refused, never guessed.
        let blob = tree_with(4, &[(0x1000_1000, 5)]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(source_at(&fdt, 0x1000_1000), None);
        // The exact-boundary source is accepted.
        let blob = tree_with(5, &[(0x1000_1000, 5)]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(source_at(&fdt, 0x1000_1000), Some(5));
    }

    #[test]
    fn plic_ndev_reads_the_controller_source_count() {
        let blob = tree_with(53, &[(0x1000_1000, 1)]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(plic_ndev(&fdt), Some(53));
    }

    #[test]
    fn plic_window_reads_the_controller_register_window() {
        // The `virt`-shaped fixture places the PLIC at `plic@c000000`, so the
        // resolver reads exactly that discovered register window.
        let blob = tree_with(53, &[(0x1000_1000, 1)]);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(plic_window(&fdt), Some((0x0c00_0000, 0x60_0000)));
    }

    #[test]
    fn plic_window_on_a_tree_without_a_plic_is_none() {
        // A bare board (no PLIC node) yields no window, so the caller wires no
        // external-IRQ dispatch rather than guessing an address (fail closed).
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(plic_window(&fdt), None);
    }

    /// Every register window of a device the kernel drives is named, wherever
    /// the tree puts it: a generic ECAM host and its memory windows above 4
    /// GiB, its I/O window not, a RISC-V IOMMU above 4 GiB, and the
    /// interrupt controllers; a device a driver process drives is not.
    #[test]
    fn every_register_window_the_kernel_drives_is_named_wherever_it_lies() {
        let cells = |values: &[u32]| -> std::vec::Vec<u8> {
            values.iter().flat_map(|v| v.to_be_bytes()).collect()
        };
        let mut b = tairix_fdt::fixture::DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("pcie@2000000000");
        b.prop_str("compatible", "pci-host-ecam-generic");
        b.prop("device_type", b"pci\0");
        b.prop_u32("#address-cells", 3);
        b.prop_u32("#size-cells", 2);
        b.prop("reg", &cells(&[0x20, 0, 0, 0x1000_0000]));
        b.prop("bus-range", &cells(&[0, 0xFF]));
        b.prop(
            "ranges",
            &cells(&[
                0x0100_0000,
                0,
                0,
                0,
                0x0300_0000,
                0,
                0x1_0000, // I/O
                0x0300_0000,
                4,
                0,
                4,
                0,
                4,
                0, // 16 GiB of 64-bit memory at 16 GiB
            ]),
        );
        b.end_node();
        b.begin_node("iommu@1000000000");
        b.prop_str("compatible", "riscv,iommu");
        b.prop("reg", &cells(&[0x10, 0, 0, 0x1000]));
        b.end_node();
        b.begin_node("plic@c000000");
        b.prop_str("compatible", "riscv,plic0");
        b.prop("reg", &cells(&[0, 0x0c00_0000, 0, 0x60_0000]));
        b.end_node();
        b.begin_node("virtio_mmio@10001000");
        b.prop_str("compatible", "virtio,mmio");
        b.prop("reg", &cells(&[0, 0x1000_1000, 0, 0x1000]));
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut named = std::vec::Vec::new();
        super::kernel_register_windows(&fdt, &mut |base, len| named.push((base, len)));
        named.sort_unstable();
        assert_eq!(
            named,
            [
                (0x0c00_0000, 0x60_0000),
                (0x4_0000_0000, 0x4_0000_0000),
                (0x10_0000_0000, 0x1000),
                (0x20_0000_0000, 0x1000_0000),
            ]
        );
    }

    #[test]
    fn source_range_boundaries() {
        // 0 is the sentinel (out), 1..=ndev in, ndev+1 out.
        assert!(!plic_source_in_range(0, 8));
        assert!(plic_source_in_range(1, 8));
        assert!(plic_source_in_range(8, 8));
        assert!(!plic_source_in_range(9, 8));
    }
}
