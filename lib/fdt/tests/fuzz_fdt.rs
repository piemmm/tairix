//! Deterministic fuzz harness for the `lib/fdt` device-tree reader
//! (a parser of boot-supplied, untrusted input).
//!
//! A flattened device tree is the hardware description firmware or a
//! bootloader hands the kernel ([`tairix_fdt::Fdt::new`]); the aarch64 and
//! riscv64 ports build their platform discovery on it. Those bytes are
//! outside TAIRiX's trust boundary: a malformed header, a structure-block
//! offset that escapes the blob, an unterminated node name, or a property
//! length that runs past the value must all be **rejected**, never trusted
//! (fail closed). Per ("every parser of untrusted
//! input ... has a fuzz target") that decode path is driven here against
//! arbitrary device trees, with a single invariant:
//!
//! * feeding any byte stream to [`tairix_fdt::Fdt::new`] and draining every
//!   public reader ([`tairix_fdt::Fdt::first_memory_region`],
//!   `timebase_frequency`, `each_cpu`, `property`, `property_u64`, the node
//!   and property iterators, the phandle and compatible lookups, the
//!   supply decoders and the PCI host decoder) never panics and never reads out of bounds — the
//!   reader either returns a well-formed view or an [`tairix_fdt::FdtError`]
//!   (fail closed), and an iterator that has yielded an error yields nothing
//!   more. The run aborting *is* the failure.
//!
//! TAIRiX pulls in no external fuzz runner: a per-run-seeded
//! `Prng` draws pseudo-random byte strings, flips bytes inside real device trees
//! built by the shared `fixture` builder (one DTB builder,
//! not a second one rolled here), and splices a valid 40-byte header onto a
//! hostile structure block. A plain `cargo test` runs the fixed
//! [`SMOKE_ITERATIONS`] sweep; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend the PRNG loop to a wall-clock budget.

use tairix_fdt::fixture::{arm_with_cpus, ecam_host_arm, virt_like, DtbBuilder};
use tairix_fdt::iommu::{each_iommu_address, iommu_cells, stream_id};
use tairix_fdt::pci::each_pci_host;
use tairix_fdt::{
    gpio_enabled_regulator, gpio_selected_regulator, phandle_args, supply, Fdt, IdMap, Node,
};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 100_000;

/// Largest arbitrary byte string fed straight to the reader.
const MAX_NOISE: usize = 4096;

/// Node paths probed via [`Fdt::property`] / [`Fdt::property_u64`]; a mix of
/// present and absent paths exercises both the hit and miss walks.
const PROBE_PATHS: &[(&[&[u8]], &[u8])] = &[
    (&[b"cpus"], b"timebase-frequency"),
    (&[b"cpus"], b"#address-cells"),
    (&[b"memory@80000000"], b"device_type"),
    (&[b"memory@80000000"], b"reg"),
    (&[], b"#address-cells"),
    (&[b"absent"], b"missing"),
];

/// Build the corpus of real, well-formed device trees the harness mutates.
/// They all come from the shared `fixture` builder so this harness adds no
/// second DTB layout.
fn templates() -> Vec<Vec<u8>> {
    vec![
        virt_like(0x8000_0000, 0x4000_0000, 10_000_000),
        virt_like(0, 0, 0),
        arm_with_cpus(
            0x4000_0000,
            0x8000_0000,
            &[(0x0, Some(1024)), (0x1, None), (0x100, Some(512))],
        ),
        arm_with_cpus(0x8000_0000, 0x1000_0000, &[]),
        ecam_host_arm(true),
        {
            // A deeply nested tree with assorted property shapes, to drive the
            // node/property iterators and the cell decoders.
            let mut b = DtbBuilder::new();
            b.begin_node("");
            b.prop_u32("#address-cells", 2);
            b.prop_u32("#size-cells", 2);
            b.begin_node("soc");
            b.prop_str("compatible", "simple-bus");
            b.begin_node("virtio_mmio@a000000");
            b.prop_str("compatible", "virtio,mmio");
            b.prop("reg", &0xa00_0000u64.to_be_bytes());
            b.end_node();
            b.end_node();
            b.end_node();
            b.build()
        },
        {
            // A consumer naming GPIO-switched regulators through phandles, to
            // drive the supply and GPIO-specifier decoders.
            let cells = |values: &[u32]| -> Vec<u8> {
                values.iter().flat_map(|v| v.to_be_bytes()).collect()
            };
            let mut b = DtbBuilder::new();
            b.begin_node("");
            b.begin_node("gpio");
            b.prop_u32("#gpio-cells", 2);
            b.prop_u32("phandle", 1);
            b.end_node();
            b.begin_node("regulator-io");
            b.prop_str("compatible", "regulator-gpio");
            b.prop("gpios", &cells(&[1, 4, 0]));
            b.prop("states", &cells(&[1_800_000, 1, 3_300_000, 0]));
            b.prop_u32("regulator-settling-time-us", 5000);
            b.prop_str("status", "okay");
            b.prop_u32("phandle", 2);
            b.end_node();
            b.begin_node("regulator-card");
            b.prop_str("compatible", "regulator-fixed");
            b.prop("enable-active-high", &[]);
            b.prop("gpio", &cells(&[1, 6, 0]));
            b.prop_u32("off-on-delay-us", 1000);
            b.prop_u32("phandle", 3);
            b.end_node();
            b.begin_node("mmc");
            b.prop_u32("vqmmc-supply", 2);
            b.prop_u32("vmmc-supply", 3);
            b.end_node();
            b.end_node();
            b.build()
        },
        translation_topology(),
    ]
}

/// A translation topology: a unit, masters naming it through `iommus`, one
/// keeping a firmware window, a host mapping its requester ids under a mask,
/// and a disabled subtree.
fn translation_topology() -> Vec<u8> {
    let cells =
        |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_be_bytes()).collect() };
    let mut b = DtbBuilder::new();
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.begin_node("reserved-memory");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.prop("ranges", &[]);
    b.begin_node("framebuffer@80000000");
    b.prop("reg", &cells(&[0, 0x8000_0000, 0, 0x80_0000]));
    b.prop(
        "iommu-addresses",
        &cells(&[5, 0, 0x8000_0000, 0, 0x80_0000]),
    );
    b.prop_u32("phandle", 6);
    b.end_node();
    b.end_node();
    b.begin_node("smmuv3@9050000");
    b.prop_str("compatible", "arm,smmu-v3");
    b.prop("reg", &cells(&[0, 0x905_0000, 0, 0x2_0000]));
    b.prop_u32("#iommu-cells", 1);
    b.prop_u32("phandle", 4);
    b.end_node();
    b.begin_node("display@9100000");
    b.prop_str("compatible", "test,display");
    b.prop("iommus", &cells(&[4, 0x100, 4, 0x101]));
    b.prop_u32("memory-region", 6);
    b.prop_u32("phandle", 5);
    b.end_node();
    b.begin_node("bus@0");
    b.prop_str("status", "disabled");
    b.begin_node("dma@0");
    b.prop_u32("#dma-cells", 1);
    b.prop("iommus", &cells(&[4, 0x200]));
    b.end_node();
    b.end_node();
    b.begin_node("pcie@10000000");
    b.prop_str("compatible", "pci-host-ecam-generic");
    b.prop_u32("#address-cells", 3);
    b.prop_u32("#size-cells", 2);
    b.prop("reg", &cells(&[0, 0x1000_0000, 0, 0x1000_0000]));
    b.prop(
        "ranges",
        &cells(&[0x0200_0000, 0, 0x2000_0000, 0, 0x2000_0000, 0, 0x1000_0000]),
    );
    b.prop("iommu-map", &cells(&[0, 4, 0, 0, 0, 4, 0x1000, 0x1_0000]));
    b.prop_u32("iommu-map-mask", 0xFFF8);
    b.end_node();
    b.end_node();
    b.build()
}

/// Drive every translation-topology reader over `node`.
fn exercise_iommu_readers(fdt: &Fdt<'_>, node: &Node<'_>) {
    let _ = iommu_cells(node);
    if let Ok(Some(map)) = IdMap::of(node, "iommu-map", "iommu-map-mask") {
        for entry in map.entries() {
            let _ = entry.targets();
            let _ = map.map(entry.id_base);
        }
        let _ = map.map(u32::MAX);
    }
    if let Some(iommus) = node.property("iommus") {
        let width = |phandle| Some(((), iommu_cells(&fdt.node_by_phandle(phandle)?)?));
        for entry in phandle_args(iommus.value(), width) {
            let Ok(((), specifier)) = entry else { break };
            let _ = stream_id(&specifier);
            let _ = specifier.cells().count();
        }
    }
    let mut windows = 0u64;
    let _ = each_iommu_address(fdt, node, &mut |window| {
        windows = windows.wrapping_add(window.len);
        let _ = window.is_identity();
    });
    let _ = windows;
}

/// Parse `bytes` and drain every public reader: must never panic, whatever the
/// blob, and any structural defect must surface as a returned `FdtError` or a
/// `None`/empty iterator rather than an out-of-bounds read.
fn exercise_never_panics(bytes: &[u8]) {
    let Ok(fdt) = Fdt::new(bytes) else {
        return;
    };

    let _ = fdt.first_memory_region();
    let _ = fdt.timebase_frequency();

    // `each_cpu` returns `Err` on a malformed tree; either way it must not
    // panic. Accumulate to keep the closure side-effecting.
    let mut cpu_acc = 0u64;
    let _ = fdt.each_cpu(|cpu| {
        cpu_acc = cpu_acc
            .wrapping_add(cpu.reg)
            .wrapping_add(cpu.capacity.unwrap_or(0))
            .wrapping_add(cpu.spin_table_release.unwrap_or(0));
    });
    let _ = cpu_acc;

    for (path, name) in PROBE_PATHS {
        let _ = fdt.property(path, name);
        let _ = fdt.property_u64(path, name);
    }

    let _ = fdt.find_compatible("virtio,mmio");
    let _ = fdt.find_compatible(b"brcm,bcm2711-emmc2");

    each_pci_host(&fdt, |host| {
        let _ = host.windows().count();
        let _ = host.external_facing(&fdt, 0x0800);
        if let Ok(Some(map)) = host.iommu_map() {
            let _ = map.map(0x10);
        }
        for slot in [0, 1, 31] {
            for pin in 0..=5 {
                let _ = host.intx(&fdt, slot, pin);
            }
        }
    });

    let mut operational = fdt.operational_nodes();
    for node in operational.by_ref() {
        let Ok(node) = node else {
            assert!(
                operational.next().is_none(),
                "the operational walk ends at its error"
            );
            break;
        };
        let _ = node.is_operational();
        exercise_iommu_readers(&fdt, &node);
    }

    // Walk the whole tree, touching every node and property accessor, so a
    // corrupted token, name, or property length is forced through the
    // iterators' bounds checks. A malformed token surfaces as an `Err` item
    // (fail closed), and nothing past it is read as structure.
    let mut nodes = fdt.nodes();
    for node in nodes.by_ref() {
        let Ok(node) = node else {
            assert!(nodes.next().is_none(), "the node walk ends at its error");
            break;
        };
        let _ = node.name();
        let _ = node.depth();
        let _ = node.is_compatible("virtio,mmio");
        let _ = node.is_enabled();
        if let Some(phandle) = node.phandle() {
            let _ = fdt.node_by_phandle(phandle);
        }
        for name in ["vqmmc-supply", "vmmc-supply"] {
            if let Some(regulator) = supply(&fdt, &node, name) {
                if let Some(selected) = gpio_selected_regulator(&fdt, &regulator) {
                    let _ = selected.level_for(1_800_000);
                    let _ = selected.level_for(3_300_000);
                }
                let _ = gpio_enabled_regulator(&fdt, &regulator);
            }
        }
        let mut properties = node.properties();
        for prop in properties.by_ref() {
            let Ok(prop) = prop else {
                assert!(
                    properties.next().is_none(),
                    "a node's properties end at an error"
                );
                break;
            };
            let _ = prop.name();
            let value = prop.value();
            for off in [0usize, 1, value.len(), value.len().saturating_sub(1)] {
                let _ = prop.read_be_u32(off);
                let _ = prop.read_be_u64(off);
            }
            let mut strings = 0u64;
            for s in prop.iter_strings() {
                strings = strings.wrapping_add(u64::try_from(s.len()).unwrap_or(0));
            }
            let _ = strings;
        }
    }
}

#[test]
fn parsing_any_device_tree_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let corpus = templates();

    // The seed is drawn and logged by `tairix_fuzzseed::start`: fresh
    // per run, reproducible from the logged value via `TAIRIX_FUZZ_SEED`.
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "parsing_any_device_tree_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));

    let mut iteration: u64 = 0;
    loop {
        // 1. A real device tree with a handful of bytes flipped at random,
        //    hammering the header offsets, token stream, and string block.
        let template = rng.pick(&corpus);
        let mut mutated = template.clone();
        let flips = rng.at_most(12);
        for _ in 0..flips {
            if mutated.is_empty() {
                break;
            }
            let pos = rng.below(mutated.len());
            mutated[pos] ^= rng.next_u8();
        }
        exercise_never_panics(&mutated);

        // 2. A truncation of a real tree: a header that promises more blob
        //    than is present, driving the bounds checks in `Fdt::new` and the
        //    iterators.
        let keep = rng.at_most(template.len());
        exercise_never_panics(&template[..keep]);

        // 3. A structured-but-hostile blob: a valid 40-byte FDT magic header
        //    over a random structure/strings region, so the reader accepts the
        //    header and then meets an adversarial token stream.
        let blob_len = rng.at_most(256);
        let mut spliced = template[..40.min(template.len())].to_vec();
        for _ in 0..blob_len {
            spliced.push(rng.next_u8());
        }
        exercise_never_panics(&spliced);

        // 4. Pure noise straight into the reader.
        let nlen = rng.at_most(MAX_NOISE);
        let mut noise = vec![0u8; nlen];
        rng.fill(&mut noise);
        exercise_never_panics(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
