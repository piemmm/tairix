//! Host unit tests for the aarch64 stage-1 paging primitives.
//!
//! These cover the pure descriptor/index arithmetic, the typed direct map,
//! and the host-side table walk (`map_4k_with_attrs` and a manual translate
//! cross-check). Loading a root is freestanding and is exercised by the QEMU
//! verticals, not here.

use super::*;

#[test]
fn dcache_line_bytes_decodes_dminline() {
    // Cortex-A72 CTR_EL0: DminLine = 4 ⇒ 16 words ⇒ 64-byte lines.
    assert_eq!(dcache_line_bytes(0x8444_C004), 64);
    // Field extremes: 0 ⇒ one word (4 bytes); 0xF ⇒ 2^15 words.
    assert_eq!(dcache_line_bytes(0), 4);
    assert_eq!(dcache_line_bytes(0xF_0000), 4 << 0xF);
    // Neighbouring fields (IminLine, ERG/CWG) must not leak in.
    assert_eq!(dcache_line_bytes(0xFFF0_FFFF), 4);
}

/// Only a physically indexed instruction cache is invalidated line by line
/// through the direct map: the Cortex-A53's virtually indexed one, and the
/// encodings no ARMv8 PE may treat as physical, are invalidated whole.
#[test]
fn only_a_physically_indexed_instruction_cache_is_invalidated_by_line() {
    assert!(icache_is_pipt(0x8444_C004), "Cortex-A72");
    assert!(!icache_is_pipt(0x8444_8004), "Cortex-A53");
    assert!(!icache_is_pipt(0x8444_4004), "AIVIVT");
    assert!(!icache_is_pipt(0x8444_0004), "VPIPT");
    assert!(icache_is_pipt(0xC000), "neighbouring fields do not count");
}

/// A starting Cortex-A72 invalidates its own L1 and leaves alone the L2 the
/// running cores share, which the level of coherency would have reached.
#[test]
fn a_starting_core_invalidates_only_the_cache_levels_it_owns() {
    const CORTEX_A72_CLIDR: u64 = 0x0A20_0023;
    assert_eq!((CORTEX_A72_CLIDR >> CLIDR_LOUIS_SHIFT) & 0b111, 1);
    assert_eq!(
        (CORTEX_A72_CLIDR >> 24) & 0b111,
        2,
        "its level of coherency"
    );
}

#[test]
fn the_pool_hands_out_each_frame_once_and_then_fails_closed() {
    static POOL: PageTablePool = PageTablePool::new();
    let mut seen = [0usize; POOL_SIZE];
    for slot in &mut seen {
        let entries = POOL.alloc().expect("a frame below the capacity");
        assert!(entries.iter().all(|&e| e == 0), "frame not zeroed");
        *slot = entries.as_ptr() as usize;
    }
    seen.sort_unstable();
    assert!(seen.windows(2).all(|w| w[0] != w[1]), "an aliased frame");
    assert!(POOL.alloc().is_none());
    assert!(POOL.alloc().is_none(), "and stays exhausted");
}

/// A process root carries nothing of the kernel's: the kernel lives in the
/// other regime, so a null dereference in kernel mode faults.
#[test]
fn a_process_root_maps_nothing_until_asked() {
    static POOL: PageTablePool = PageTablePool::new();
    let space = AddressSpace::new_user(&POOL).expect("a root");
    for va in [0, 0x1000, 1 << 30, 0x4020_0000, 511 << 30] {
        assert_eq!(space.translate(va), None, "{va:#x}");
    }
    // SAFETY: the pool's own root, only read.
    let root = unsafe { &*POOL.table_at(space.root_phys()).expect("the root") };
    assert!(root.iter().all(|&entry| entry == 0));
}

/// Each direct-map gigabyte is typed once; a second type is refused, not
/// applied, so no address is ever reached under two memory types. The
/// gigabytes are this test's alone: the host tests share the kernel root.
#[test]
fn the_direct_map_types_each_gigabyte_once() {
    let gigabytes = |indices: &[usize]| {
        gigapage_mask_from_extents(
            &indices
                .iter()
                .map(|&index| ((index as u64) << 30, 1))
                .collect::<std::vec::Vec<_>>(),
        )
    };
    assert_eq!(holding(300), None);
    assert_eq!(reveal(&gigabytes(&[300]), Holding::Ram), gigabytes(&[300]));
    assert_eq!(
        reveal(&gigabytes(&[300, 301]), Holding::Registers),
        gigabytes(&[301]),
        "the RAM gigabyte keeps its type"
    );
    assert_eq!(
        reveal(&gigabytes(&[302]), Holding::Config),
        gigabytes(&[302])
    );
    assert_eq!(
        reveal(&gigabytes(&[300]), Holding::Ram),
        gigabytes(&[300]),
        "again, unchanged"
    );
    assert_eq!(holding(300), Some(Holding::Ram));
    assert_eq!(holding(301), Some(Holding::Registers));
    assert_eq!(holding(302), Some(Holding::Config));

    assert!(physmap_covers(300 << 30, PAGE_SIZE as u64));
    assert!(
        !physmap_covers((300 << 30) + (1 << 30) - 8, 16),
        "into the registers"
    );
    assert!(!physmap_covers(301 << 30, PAGE_SIZE as u64));
    assert_eq!(
        registers_virt(301 << 30, 0x100),
        Some(physmap_virt(301 << 30))
    );
    assert_eq!(
        registers_virt(302 << 30, 0x100),
        Some(physmap_virt(302 << 30))
    );
    assert_eq!(registers_virt(300 << 30, 0x100), None, "RAM is no register");
    assert_eq!(registers_virt(303 << 30, 0x100), None, "nor is nothing");
    assert_eq!(registers_virt(301 << 30, 0), None, "an empty window");
    assert_eq!(registers_virt(u64::MAX - 8, 16), None, "a wrapping one");

    let root = kernel_root_entries();
    assert_eq!(
        root[PHYSMAP_FIRST_SLOT + 301].load(Ordering::Acquire),
        descriptor(301 << 30, device_leaf_attrs(true))
    );
    assert_ne!(
        root[PHYSMAP_FIRST_SLOT + 300].load(Ordering::Acquire) & attrs::PXN,
        0,
        "RAM is never executed through the map"
    );
}

#[test]
fn a_gigabyte_past_the_direct_map_is_never_typed() {
    let mut past = [0u64; GIGAPAGE_MASK_WORDS];
    past[MAX_PHYSMAP_GIB / 64] |= 1 << (MAX_PHYSMAP_GIB % 64);
    assert_eq!(reveal(&past, Holding::Ram), [0; GIGAPAGE_MASK_WORDS]);
    assert_eq!(holding(MAX_PHYSMAP_GIB), None);
    assert!(!physmap_covers(PHYSMAP_REACH, PAGE_SIZE as u64));
}

/// The trampoline's early blocks are the one leaf the port builds both
/// writable and executable, and the kernel root replaces them; the identity
/// page executes but is never written.
#[test]
fn only_the_early_map_is_writable_and_executable() {
    assert!(writable_and_executable(EARLY_IMAGE_BLOCK));
    assert!(is_block(EARLY_IMAGE_BLOCK | attrs::VALID));
    for access in [
        KernelAccess::Execute,
        KernelAccess::Read,
        KernelAccess::Write,
    ] {
        for block in [false, true] {
            assert!(!writable_and_executable(access.leaf(block)), "{access:?}");
        }
    }
    assert!(!writable_and_executable(IDMAP_LEAF));
    assert_eq!(IDMAP_LEAF & attrs::PXN, 0, "the identity page executes");
}

#[test]
fn map_4k_with_attrs_refuses_a_writable_and_executable_leaf() {
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");
    let va = 64u64 << 30;
    let rwx = EARLY_IMAGE_BLOCK | attrs::TABLE_OR_PAGE;
    assert!(space
        .map_4k_with_attrs(&POOL, va, 0x4100_0000, rwx)
        .is_none());
    let el0_rwx = el0_data_leaf_attrs() & !attrs::UXN;
    assert!(space
        .map_4k_with_attrs(&POOL, va, 0x4100_0000, el0_rwx)
        .is_none());
    assert_eq!(space.translate(va), None, "nothing was mapped");
    assert_eq!(POOL.used.load(Ordering::SeqCst), 1, "no table was drawn");
}

#[test]
fn table_index_extracts_each_level() {
    // VA whose L1/L2/L3 indices are 1, 2, 3 with a 0x40 offset.
    let va = (1u64 << 30) | (2u64 << 21) | (3u64 << 12) | 0x40;
    assert_eq!(table_index(va, 1), 1);
    assert_eq!(table_index(va, 2), 2);
    assert_eq!(table_index(va, 3), 3);
}

#[test]
fn descriptor_round_trips_the_output_address() {
    let pa = 0x4_1234_5000;
    let d = descriptor(pa, KernelAccess::Write.leaf(false));
    assert_eq!(phys_from_descriptor(d), pa);
    // The offset bits are masked off the output address.
    let d2 = descriptor(pa | 0xABC, KernelAccess::Write.leaf(false));
    assert_eq!(phys_from_descriptor(d2), pa);
}

#[test]
fn block_vs_table_low_bits() {
    // Block descriptor: valid set, bit 1 clear (0b01).
    let block = descriptor(0x4000_0000, KernelAccess::Write.leaf(true));
    assert!(is_block(block));
    assert_eq!(block & 0b11, 0b01);
    // Table descriptor: 0b11.
    let table = table_descriptor(0x4_0000_0000);
    assert!(!is_block(table));
    assert_eq!(table & 0b11, 0b11);
    // Page (L3) descriptor: 0b11.
    let page = descriptor(0x4000_0000, KernelAccess::Write.leaf(false));
    assert!(!is_block(page));
    assert_eq!(page & 0b11, 0b11);
}

#[test]
fn leaf_attrs_select_the_right_mair_index() {
    assert_eq!(
        KernelAccess::Write.leaf(true) & (0b111 << 2),
        attrs::ATTR_IDX_NORMAL
    );
    assert_eq!(
        device_leaf_attrs(true) & (0b111 << 2),
        attrs::ATTR_IDX_DEVICE
    );
    assert_eq!(
        config_leaf_attrs(true) & (0b111 << 2),
        attrs::ATTR_IDX_DEVICE_NONPOSTED
    );
    assert_eq!(
        (MAIR_VALUE >> 24) & 0xFF,
        0x00,
        "attribute 3 is Device-nGnRnE"
    );
    assert!(page_flags_from_leaf(config_leaf_attrs(false)).contains(PageFlags::DEVICE));
    // Both set the access flag so first touch does not fault.
    assert_ne!(KernelAccess::Write.leaf(true) & attrs::AF, 0);
    assert_ne!(device_leaf_attrs(true) & attrs::AF, 0);
}

#[test]
fn el0_leaf_attrs_encode_unprivileged_access() {
    // The AP field lives in bits [7:6].
    const AP_MASK: u64 = 0b11 << 6;
    // Code: read-only at EL0 (AP=0b11), EL0-executable (UXN clear) but
    // privileged-execute-never (PXN set).
    let code = el0_code_leaf_attrs();
    assert_eq!(code & AP_MASK, attrs::AP_RO_EL0);
    assert_ne!(code & attrs::PXN, 0);
    assert_eq!(code & attrs::UXN, 0);
    assert_eq!(code & (0b111 << 2), attrs::ATTR_IDX_NORMAL);
    // It is a page descriptor (0b11) with the access flag set.
    assert_eq!(code & 0b11, 0b11);
    assert_ne!(code & attrs::AF, 0);

    // Data: read/write at EL0 (AP=0b01), execute-never at both ELs.
    let data = el0_data_leaf_attrs();
    assert_eq!(data & AP_MASK, attrs::AP_RW_EL0);
    assert_ne!(data & attrs::PXN, 0);
    assert_ne!(data & attrs::UXN, 0);
    assert_eq!(data & 0b11, 0b11);

    // Read-only data: read-only at EL0 (AP=0b11) and execute-never at both
    // ELs — unlike code, the page is *not* EL0-executable (UXN set).
    let rodata = el0_rodata_leaf_attrs();
    assert_eq!(rodata & AP_MASK, attrs::AP_RO_EL0);
    assert_ne!(rodata & attrs::PXN, 0);
    assert_ne!(rodata & attrs::UXN, 0);
    assert_eq!(rodata & (0b111 << 2), attrs::ATTR_IDX_NORMAL);
    assert_eq!(rodata & 0b11, 0b11);
    assert_ne!(rodata & attrs::AF, 0);
}

#[test]
fn el0_device_leaf_is_unprivileged_device_memory() {
    const AP_MASK: u64 = 0b11 << 6;
    // EL0-accessible device window (a user-space driver's `mmio_map` window,
    // `plans/PI.md` P10 chunk 5d-0): read/write at EL0 (AP=0b01), Device MAIR
    // index, execute-never at both ELs, a page descriptor with the access
    // flag set. The kernel-only `device_leaf_attrs` differs only in the AP
    // field (EL1-only), which is exactly the permission-fault regression this
    // fixes — an EL0 driver reading its own mapped register.
    let dev = el0_device_leaf_attrs();
    assert_eq!(dev & AP_MASK, attrs::AP_RW_EL0);
    assert_eq!(dev & (0b111 << 2), attrs::ATTR_IDX_DEVICE);
    assert_ne!(dev & attrs::PXN, 0);
    assert_ne!(dev & attrs::UXN, 0);
    assert_eq!(dev & 0b11, 0b11);
    assert_ne!(dev & attrs::AF, 0);
    assert_eq!(device_leaf_attrs(false) & AP_MASK, attrs::AP_RW_EL1);
}

#[test]
fn leaf_attrs_for_device_user_is_el0_accessible() {
    // A `DEVICE | USER` mapping (the `mmio_map` window) must be EL0-accessible,
    // not the kernel-only device leaf — otherwise the driver permission-faults
    // reading its own register (`plans/PI.md` P10 chunk 5d-0). A `DEVICE`-only
    // (kernel) mapping stays EL1-only.
    assert_eq!(
        AddressSpace::leaf_attrs_for(PageFlags::DEVICE | PageFlags::USER | PageFlags::WRITE),
        el0_device_leaf_attrs()
    );
    assert_eq!(
        AddressSpace::leaf_attrs_for(PageFlags::DEVICE | PageFlags::WRITE),
        device_leaf_attrs(false)
    );
}

#[test]
fn map_4k_with_attrs_uses_the_supplied_leaf_attrs() {
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");

    let va: u64 = 96u64 << 30;
    let pa: u64 = 0x4567_8000;
    space
        .map_4k_with_attrs(&POOL, va, pa, el0_code_leaf_attrs())
        .expect("map the EL0 page");

    // Walk to the leaf descriptor and confirm it carries the EL0 attrs.
    let leaf = host_leaf_descriptor(&POOL, space.root_phys(), va).expect("va is mapped");
    assert_eq!(phys_from_descriptor(leaf), pa);
    assert_eq!(leaf & (0b11 << 6), attrs::AP_RO_EL0);
    assert_eq!(leaf & attrs::UXN, 0);
    assert_ne!(leaf & attrs::PXN, 0);
}

#[test]
fn tcr_value_encodes_two_39_bit_regimes() {
    // T0SZ [5:0] and T1SZ [21:16] are both 25 → 64 - 25 = 39-bit VA, so
    // user space keeps the whole low regime and the kernel gets its own.
    assert_eq!(TCR_VALUE & 0x3F, 25);
    assert_eq!((TCR_VALUE >> 16) & 0x3F, 25);
    // TTBR1 walks *enabled*: EPD1 (bit 23) clear. A set EPD1 would make
    // every kernel-regime address fault, taking the direct physical map
    // and the remap window with it.
    assert_eq!(TCR_VALUE & (1 << 23), 0);
    // 4 KiB granule in both regimes — deliberately different encodings:
    // TG0 [15:14] = 0b00, TG1 [31:30] = 0b10 (ARM ARM D13.2.120). Getting
    // TG1 wrong would walk the kernel root at the wrong level.
    assert_eq!((TCR_VALUE >> 14) & 0b11, 0b00);
    assert_eq!((TCR_VALUE >> 30) & 0b11, 0b10);
    // `TTBR0_EL1` defines the ASID (A1, bit 22, clear), so a user-space
    // switch carries the address-space tag and the kernel regime does not.
    assert_eq!(TCR_VALUE & (1 << 22), 0);
    // Both regimes walk inner-shareable write-back cacheable, so the
    // walker is coherent with a table store the kernel makes with caches
    // on — which is what lets the kernel root be published without a
    // point-of-coherency sweep.
    assert_eq!((TCR_VALUE >> 8) & 0b11, 0b01);
    assert_eq!((TCR_VALUE >> 10) & 0b11, 0b01);
    assert_eq!((TCR_VALUE >> 12) & 0b11, 0b11);
    assert_eq!((TCR_VALUE >> 24) & 0b11, 0b01);
    assert_eq!((TCR_VALUE >> 26) & 0b11, 0b01);
    assert_eq!((TCR_VALUE >> 28) & 0b11, 0b11);
}

#[test]
fn mair_pairs_normal_device_and_normal_nc() {
    // Attr0 = 0xFF (Normal WB RW-allocate), Attr1 = 0x04 (Device-nGnRE),
    // Attr2 = 0x44 (Normal Non-Cacheable, the coherent-DMA memory type).
    assert_eq!(MAIR_VALUE & 0xFF, 0xFF);
    assert_eq!((MAIR_VALUE >> 8) & 0xFF, 0x04);
    assert_eq!((MAIR_VALUE >> 16) & 0xFF, 0x44);
}

#[test]
fn el0_dma_coherent_leaf_is_unprivileged_normal_non_cacheable() {
    const AP_MASK: u64 = 0b11 << 6;
    // The coherent-DMA buffer leaf (a user-space driver's DMA carve): EL0
    // read/write (AP=0b01), Normal Non-Cacheable MAIR index (so the device
    // and CPU stay coherent without cache maintenance), execute-never at
    // both ELs, a page descriptor with the access flag set.
    let dma = el0_dma_coherent_leaf_attrs();
    assert_eq!(dma & AP_MASK, attrs::AP_RW_EL0);
    assert_eq!(dma & (0b111 << 2), attrs::ATTR_IDX_NORMAL_NC);
    assert_ne!(dma & attrs::PXN, 0);
    assert_ne!(dma & attrs::UXN, 0);
    assert_eq!(dma & 0b11, 0b11);
    assert_ne!(dma & attrs::AF, 0);
    // Distinct memory type from both Normal-WB and Device.
    assert_ne!(dma & (0b111 << 2), attrs::ATTR_IDX_NORMAL);
    assert_ne!(dma & (0b111 << 2), attrs::ATTR_IDX_DEVICE);
}

#[test]
fn leaf_attrs_for_dma_coherent_user_is_normal_non_cacheable() {
    // A `DMA_COHERENT | USER | WRITE` mapping (the DMA carve) selects the
    // Normal-NC EL0 leaf, never the cacheable `el0_data` leaf — otherwise a
    // non-I/O-coherent device would never see the driver's descriptors. `DMA_COHERENT` takes precedence over the generic
    // user-data leaf.
    assert_eq!(
        AddressSpace::leaf_attrs_for(
            PageFlags::DMA_COHERENT | PageFlags::USER | PageFlags::READ | PageFlags::WRITE
        ),
        el0_dma_coherent_leaf_attrs()
    );
}

/// A DMA buffer's leaf carries its software mark whatever its memory type,
/// and decodes back to it; no other leaf does.
#[test]
fn the_dma_mark_rides_its_own_software_bit() {
    let shared = PageFlags::READ | PageFlags::WRITE | PageFlags::USER;
    for memory in [PageFlags::empty(), PageFlags::DMA_COHERENT] {
        let marked = page_flags_from_leaf(AddressSpace::leaf_attrs_for(
            shared | memory | PageFlags::DMA,
        ));
        assert!(marked.contains(PageFlags::DMA));
        assert_eq!(
            marked.contains(PageFlags::DMA_COHERENT),
            memory == PageFlags::DMA_COHERENT
        );
        let plain = page_flags_from_leaf(AddressSpace::leaf_attrs_for(shared | memory));
        assert!(!plain.contains(PageFlags::DMA));
    }
}

/// A kernel page mapped for data executes nowhere, so a write into the heap
/// or a stack can never be run; one asked to execute is read-only at EL1 and
/// unreachable from EL0. What each decodes back is what was asked.
#[test]
fn kernel_pages_are_never_both_writable_and_executable() {
    let data = AddressSpace::leaf_attrs_for(PageFlags::READ | PageFlags::WRITE);
    assert_eq!(data, KernelAccess::Write.leaf(false));
    assert_ne!(data & attrs::PXN, 0, "the kernel cannot run its data");
    assert_ne!(data & attrs::UXN, 0);
    assert_eq!(
        page_flags_from_leaf(data),
        PageFlags::READ | PageFlags::WRITE
    );
    let code = AddressSpace::leaf_attrs_for(PageFlags::READ | PageFlags::EXEC);
    assert_eq!(code, KernelAccess::Execute.leaf(false));
    assert_eq!(code & attrs::PXN, 0);
    assert_ne!(code & attrs::UXN, 0, "EL0 never runs kernel code");
    assert_eq!(code & (0b11 << 6), attrs::AP_RO_EL1);
    assert_eq!(
        page_flags_from_leaf(code),
        PageFlags::READ | PageFlags::EXEC
    );
}

#[test]
fn page_flags_round_trip_through_the_dma_coherent_leaf() {
    // The Normal-NC leaf decodes back to a `DMA_COHERENT` user RW page —
    // not `DEVICE`, and not a bare cacheable page (the [4:2] attr-index
    // decode must distinguish index 2 from index 0/1).
    let decoded = page_flags_from_leaf(el0_dma_coherent_leaf_attrs());
    assert!(decoded.contains(PageFlags::DMA_COHERENT));
    assert!(decoded.contains(PageFlags::USER));
    assert!(decoded.contains(PageFlags::WRITE));
    assert!(!decoded.contains(PageFlags::DEVICE));
    assert!(!decoded.contains(PageFlags::EXEC));
    // A cacheable user-data leaf must *not* decode as coherent-DMA.
    assert!(!page_flags_from_leaf(el0_data_leaf_attrs()).contains(PageFlags::DMA_COHERENT));
    // …and a device leaf decodes as DEVICE, not DMA_COHERENT.
    let dev = page_flags_from_leaf(el0_device_leaf_attrs());
    assert!(dev.contains(PageFlags::DEVICE));
    assert!(!dev.contains(PageFlags::DMA_COHERENT));
}

#[test]
fn sctlr_mmu_off_pins_the_trampoline_value() {
    // `boot.s` (`.Lin_el1`) and `smp.s` (`_start_secondary_aarch64`)
    // hard-code this exact value with `mov`/`movk`; regression test for
    // the Pi 4 hang where the architecturally UNKNOWN EL1 reset state
    // was never replaced before use.
    assert_eq!(SCTLR_MMU_OFF, 0x30D0_0800);
    // The MMU-off value is exactly the ARMv8.0 RES1 bits: no
    // translation, no caches, nothing else.
    assert_eq!(SCTLR_MMU_OFF, SCTLR_RES1);
}

#[test]
fn sctlr_mmu_on_enables_translation_and_caches_only() {
    // M (bit 0), C (bit 2), I (bit 12) on top of the RES1 bits, nothing
    // more — the whole-register write in `AddressSpace::switch` must not
    // smuggle any other behaviour in.
    assert_eq!(SCTLR_MMU_ON, SCTLR_RES1 | (1 << 0) | (1 << 2) | (1 << 12));
}

#[test]
fn sctlr_values_keep_the_unknown_reset_traps_clear() {
    // The bits whose UNKNOWN reset state broke real silicon stay clear
    // in both installed values: A (1, alignment check), SA (3) / SA0 (4,
    // SP alignment), WXN (19, writable ⇒ execute-never), E0E (24) / EE
    // (25, big-endian data).
    for sctlr in [SCTLR_MMU_OFF, SCTLR_MMU_ON] {
        for trap_bit in [1, 3, 4, 19, 24, 25] {
            assert_eq!(sctlr & (1 << trap_bit), 0, "bit {trap_bit} must be clear");
        }
        // And every ARMv8.0 RES1 bit is set.
        assert_eq!(sctlr & SCTLR_RES1, SCTLR_RES1);
    }
}

#[test]
fn map_4k_walks_and_translates() {
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");

    // Map a 4 KiB page at 64 GiB, where the walk allocates fresh L2/L3
    // tables.
    let va: u64 = 64u64 << 30;
    let pa: u64 = 0x4123_4000;
    space
        .map_4k_with_attrs(&POOL, va, pa, KernelAccess::Write.leaf(false))
        .expect("map the page");

    // Manually walk the just-built hierarchy and confirm it translates
    // `va` to `pa` (the host analogue of an MMU lookup).
    let translated = host_translate(&POOL, space.root_phys(), va).expect("va is mapped");
    assert_eq!(translated, pa);

    // A neighbouring page in the same L3 table is absent.
    assert!(host_translate(&POOL, space.root_phys(), va + PAGE_SIZE as u64).is_none());
}

#[test]
fn map_4k_rejects_misaligned_inputs() {
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");
    assert!(space
        .map_4k_with_attrs(
            &POOL,
            0x1_0001,
            0x4000_0000,
            KernelAccess::Write.leaf(false)
        )
        .is_none());
    assert!(space
        .map_4k_with_attrs(
            &POOL,
            0x1_0000,
            0x4000_0001,
            KernelAccess::Write.leaf(false)
        )
        .is_none());
}

/// Host-side translation of `va` through the table hierarchy rooted at
/// `root_phys`, following table descriptors and returning the output
/// physical address of the leaf (block or page) that maps `va`, or
/// `None` if no valid leaf is reached.
fn host_translate(frames: &dyn PageTableFrames, root_phys: u64, va: u64) -> Option<u64> {
    host_leaf_descriptor(frames, root_phys, va).map(phys_from_descriptor)
}

/// As [`host_translate`], but returns the full leaf *descriptor* (output
/// address plus attributes) so a test can assert the leaf's permission
/// bits, not just its translation.
fn host_leaf_descriptor(frames: &dyn PageTableFrames, root_phys: u64, va: u64) -> Option<u64> {
    let mut phys = root_phys;
    for level in 1..=LEVELS {
        let table = frames.table_at(phys)?;
        // SAFETY: `phys` names a live table page the walk under test drew
        // from `frames`, so the source's view of it is readable;
        // `table_index` is < 512.
        let entry = unsafe { &*table }[table_index(va, level)];
        if (entry & attrs::VALID) == 0 {
            return None;
        }
        if is_block(entry) || level == LEVELS {
            return Some(entry);
        }
        phys = phys_from_descriptor(entry);
    }
    None
}

/// A recording [`PageTableFrames`] double: identity-phys bump storage
/// (like the boot pool) plus an atomic log of every `free_table` return,
/// so the reclaim test can assert teardown hands back exactly the frames
/// the hierarchy drew — no more, no fewer, none twice.
struct RecordingFrames {
    storage: [core::cell::UnsafeCell<Table>; Self::CAPACITY],
    used: core::sync::atomic::AtomicUsize,
    freed: [AtomicU64; Self::CAPACITY],
    freed_len: core::sync::atomic::AtomicUsize,
}

// SAFETY: each storage slot is handed out exactly once via the monotonic
// `used` counter, so the `&'static mut` views never alias; the freed log
// is plain atomics.
unsafe impl Sync for RecordingFrames {}

impl RecordingFrames {
    const CAPACITY: usize = 8;

    const fn new() -> Self {
        // The array initialiser needs a `const`, and copying it per slot is
        // the point: each element must be its own independent cell.
        #[allow(clippy::declare_interior_mutable_const)]
        const SLOT: core::cell::UnsafeCell<Table> = core::cell::UnsafeCell::new(Table::new());
        // The array initialiser needs a `const`, and copying it per slot is
        // the point: each element must be its own independent cell.
        #[allow(clippy::declare_interior_mutable_const)]
        const FREED: AtomicU64 = AtomicU64::new(0);
        // `const`, so the pool lives in `.bss` — never a runtime stack
        // frame (the same discipline as `PageTablePool::new`).
        #[allow(clippy::large_stack_arrays)]
        let storage = [SLOT; Self::CAPACITY];
        Self {
            storage,
            used: core::sync::atomic::AtomicUsize::new(0),
            freed: [FREED; Self::CAPACITY],
            freed_len: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn allocated(&self) -> usize {
        self.used.load(Ordering::SeqCst).min(Self::CAPACITY)
    }

    fn freed_phys(&self) -> impl Iterator<Item = u64> + '_ {
        let len = self.freed_len.load(Ordering::SeqCst);
        self.freed
            .iter()
            .take(len)
            .map(|a| a.load(Ordering::SeqCst))
    }
}

impl PageTableFrames for RecordingFrames {
    fn alloc_table(&self) -> Option<TableFrame> {
        let idx = self.used.fetch_add(1, Ordering::SeqCst);
        if idx >= Self::CAPACITY {
            self.used.store(Self::CAPACITY, Ordering::SeqCst);
            return None;
        }
        // SAFETY: the monotonic index makes this slot exclusively ours.
        let table: &'static mut Table = unsafe { &mut *self.storage[idx].get() };
        let entries = &mut table.0;
        let phys = image_phys(entries.as_ptr() as u64);
        Some(TableFrame { phys, entries })
    }

    fn table_at(&self, phys: u64) -> Option<*mut [u64; ENTRIES_PER_TABLE]> {
        let base = image_phys(self.storage.as_ptr() as u64);
        let index = tairix_arch_api::frames::pool_slot_of(base, Self::CAPACITY, phys)?;
        Some(self.storage[index].get().cast())
    }

    fn free_table(&self, phys: u64) {
        let slot = self.freed_len.fetch_add(1, Ordering::SeqCst);
        assert!(slot < Self::CAPACITY, "more frees than the pool can hold");
        self.freed[slot].store(phys, Ordering::SeqCst);
    }
}

#[test]
fn reclaim_table_frames_returns_every_drawn_table_exactly_once() {
    static POOL: RecordingFrames = RecordingFrames::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");
    let root_phys = space.root_phys();

    // Two pages in distinct gigapages, so the walk draws two independent
    // L2+L3 pairs: 1 root + 4 tables.
    let va_a: u64 = 64u64 << 30;
    let va_b: u64 = 65u64 << 30;
    let pa: u64 = 0x4123_4000;
    space
        .map_4k_with_attrs(&POOL, va_a, pa, KernelAccess::Write.leaf(false))
        .expect("map A");
    space
        .map_4k_with_attrs(
            &POOL,
            va_b,
            pa + PAGE_SIZE as u64,
            KernelAccess::Write.leaf(false),
        )
        .expect("map B");
    assert_eq!(POOL.allocated(), 5, "root + two L2/L3 pairs were drawn");

    // SAFETY: the space is no CPU's active translation (host test) and no
    // other reference into its tables is live.
    unsafe { tairix_arch_api::mmu::AddressSpace::reclaim_table_frames(&mut space) };

    // Every drawn table frame came back exactly once, the root last, and
    // no leaf frame (the mapped `pa` pages) was ever freed.
    let mut count = 0usize;
    let mut last = 0u64;
    for phys in POOL.freed_phys() {
        assert_ne!(phys, pa, "a leaf frame is never freed");
        assert_ne!(phys, pa + PAGE_SIZE as u64, "a leaf frame is never freed");
        let mut earlier = POOL.freed_phys().take(count);
        assert!(earlier.all(|e| e != phys), "no table is freed twice");
        last = phys;
        count += 1;
    }
    assert_eq!(count, POOL.allocated(), "every drawn table was returned");
    assert_eq!(last, root_phys, "the root is freed last (post-order)");
}

#[test]
fn passes_mmu_conformance() {
    use tairix_arch_api::mmu;
    static POOL: PageTablePool = PageTablePool::new();
    static POOL2: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");
    // A VA at 64 GiB, so the conformance map allocates fresh L2/L3 tables;
    // the phys frame is RAM.
    let va = 64u64 << 30;
    let pa = 0x4123_4000;
    mmu::conformance::run_all(&mut space, va, pa);
    // And over the object-safe erasure the kernel registry stores.
    let mut dynamic = AddressSpace::new_user(&POOL2).expect("a root");
    let erased: &mut dyn mmu::AddressSpace = &mut dynamic;
    mmu::conformance::run_all(erased, va, pa);
}

#[test]
fn passes_tlb_conformance() {
    use tairix_arch_api::tlb;
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");
    // The host has no TLB, so `flush_page` is a vacuous no-op here; the
    // suite proves it is object-safe and panic-free for any address (the
    // real `tlbi` is exercised by the spawn QEMU vertical).
    tlb::conformance::run_all(&mut space, 64u64 << 30);
    let mut dynamic = AddressSpace::new_user(&POOL).expect("a root");
    let erased: &mut dyn tlb::TlbShootdown = &mut dynamic;
    tlb::conformance::run_all(erased, 64u64 << 30);
}

#[test]
fn passes_frames_conformance() {
    use tairix_arch_api::frames::{self, PageTableFrames};
    // The static pool is the boot/bootstrap `PageTableFrames` source; its
    // frames' physical addresses are the image's own translation
    // (`image_phys`), the identity on the host, so the suite runs there. A
    // fresh pool hands out `POOL_SIZE` frames before failing closed; a second
    // pool exercises the object-safe erasure.
    static POOL: PageTablePool = PageTablePool::new();
    static POOL2: PageTablePool = PageTablePool::new();
    frames::conformance::run_all(&POOL, super::POOL_SIZE);
    let erased: &dyn PageTableFrames = &POOL2;
    assert!(erased.alloc_table().is_some());
}

#[test]
fn map_page_translates_neutral_user_flags_to_wx_safe_leaves() {
    use tairix_arch_api::mmu::{self, PageFlags};
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");

    // A USER + EXEC page must be mapped EL0-executable and read-only
    // (W^X): the leaf is `el0_code_leaf_attrs` (AP_RO_EL0, UXN clear).
    let code_va = 64u64 << 30;
    mmu::AddressSpace::map_page(
        &mut space,
        code_va,
        0x4100_0000,
        PageFlags::READ | PageFlags::EXEC | PageFlags::USER,
    )
    .expect("user code map");
    let code_leaf = host_leaf_descriptor(&POOL, space.root_phys(), code_va).expect("mapped");
    assert_eq!(
        code_leaf & attrs::UXN,
        0,
        "user code must be EL0-executable"
    );
    assert_ne!(code_leaf & attrs::PXN, 0, "user code must be PXN at EL1");

    // A USER + WRITE page must be execute-never at both ELs (W^X).
    let data_va = (64u64 << 30) + (1u64 << 21);
    mmu::AddressSpace::map_page(
        &mut space,
        data_va,
        0x4101_0000,
        PageFlags::READ | PageFlags::WRITE | PageFlags::USER,
    )
    .expect("user data map");
    let data_leaf = host_leaf_descriptor(&POOL, space.root_phys(), data_va).expect("mapped");
    assert_ne!(data_leaf & attrs::UXN, 0, "user data must be EL0 XN");
    assert_ne!(data_leaf & attrs::PXN, 0, "user data must be EL1 XN");
}

#[test]
fn declares_access_tracking_supported() {
    use tairix_arch_api::mmu::{self, AccessTracking};
    static POOL: PageTablePool = PageTablePool::new();
    let space = AddressSpace::new_user(&POOL).expect("a root");
    // aarch64 manages the Access Flag in software (`TCR_EL1.HA` clear on
    // every part), so the referenced bit is honestly Supported.
    assert_eq!(
        mmu::AddressSpace::access_tracking(&space),
        AccessTracking::Supported
    );
}

#[test]
fn test_and_clear_accessed_drives_the_clock_round_trip() {
    use tairix_arch_api::mmu::{self, MapError, PageFlags};
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");

    // Map an EL0 data page at 64 GiB, where the walk builds fresh L2/L3
    // tables. `map_page` sets AF eagerly, so a fresh leaf reads accessed.
    let va = 64u64 << 30;
    let pa = 0x4123_4000;
    mmu::AddressSpace::map_page(&mut space, va, pa, PageFlags::READ | PageFlags::WRITE)
        .expect("map the probe page");

    // Fail-closed edges first: a misaligned address and an unmapped one
    // report a typed error, never a fabricated verdict.
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va + 0x123),
        Err(MapError::Misaligned)
    );
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va + PAGE_SIZE as u64),
        Err(MapError::NotMapped)
    );

    // Probe 1: the eager map left AF set, so the first probe reads
    // accessed and clears AF.
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va),
        Ok(true)
    );
    // The clear took effect on the descriptor.
    let leaf = host_leaf_descriptor(&POOL, space.root_phys(), va).expect("mapped");
    assert_eq!(leaf & attrs::AF, 0, "AF must be cleared after a probe");

    // Probe 2: no access since the clear (the host has no CPU to re-set
    // AF), so the page reads cold. This is the "genuinely untouched"
    // verdict the cold-page scanner acts on.
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va),
        Ok(false)
    );

    // Simulate a touch the way the exception path does on real hardware:
    // an Access-Flag fault sets AF back on the leaf.
    // SAFETY: `root_phys` is the live L1 table of this exclusively-owned
    // space, drawn from `POOL`; no other reference walks it here.
    assert!(unsafe { set_accessed_flag_in_root(&POOL, space.root_phys(), va) });

    // Probe 3: the page now reads accessed again — the full clock/
    // second-chance transition, end to end.
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va),
        Ok(true)
    );
}

#[test]
fn set_accessed_flag_in_root_retries_on_every_valid_leaf() {
    use tairix_arch_api::mmu::{self, PageFlags};
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");

    let va = 64u64 << 30;
    mmu::AddressSpace::map_page(
        &mut space,
        va,
        0x4123_4000,
        PageFlags::READ | PageFlags::WRITE,
    )
    .expect("map the probe page");
    let root = space.root_phys();

    // An unmapped address: nothing to set, returns false (fail closed).
    // SAFETY: `root` is the live L1 table of this exclusively-owned
    // space, drawn from `POOL`.
    assert!(!unsafe { set_accessed_flag_in_root(&POOL, root, va + PAGE_SIZE as u64) });

    // The leaf still carries AF (eager map): another CPU's fix-up got there
    // first, so the access is retried and the leaf left as it is.
    let eager = host_leaf_descriptor(&POOL, root, va).expect("mapped");
    // SAFETY: as above.
    assert!(unsafe { set_accessed_flag_in_root(&POOL, root, va) });
    assert_eq!(host_leaf_descriptor(&POOL, root, va), Some(eager));

    // Clear AF: the first fix-up sets it, and the CPU that finds it set
    // already retries too.
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va),
        Ok(true)
    );
    // SAFETY: as above.
    assert!(unsafe { set_accessed_flag_in_root(&POOL, root, va) });
    // SAFETY: as above.
    assert!(unsafe { set_accessed_flag_in_root(&POOL, root, va) });
    let leaf = host_leaf_descriptor(&POOL, root, va).expect("mapped");
    assert_ne!(leaf & attrs::AF, 0, "AF must be set after the fault fix-up");
    assert!(retries_access(leaf) && !retries_access(0));
}

/// A leaf that faults on its Access flag is cached in no TLB, so the fix-up
/// orders its store for the walkers and invalidates nothing.
#[test]
fn the_access_flag_fix_up_invalidates_nothing() {
    let source = include_str!("paging.rs");
    let start = source
        .find("unsafe fn set_accessed_flag_in_root(")
        .expect("the fix-up is defined");
    let body = &source[start..];
    let body = &body[..body.find("\n}\n").expect("the fix-up ends")];
    assert!(body.contains("publish_table_update();"), "{body}");
    assert!(!body.contains("invalidate_"), "{body}");
}

#[test]
fn the_kernel_regime_holds_the_map_below_the_remap_window() {
    // Both regimes are 39-bit, so the kernel's is the top `2^39` bytes and
    // no user address can name any of it — the property that replaced the
    // old slot-range refusal inside `TTBR0`.
    const USER_VA_TOP: u64 = 1 << 39;
    // Both sides are constants, so this holds at compile time or not at all.
    const _: () = assert!(USER_VA_TOP <= KERNEL_VA_BASE);
    assert_eq!(KERNEL_VA_BASE, !0u64 << 39);

    // The map starts at the regime's base and runs up to the window.
    assert_eq!(PHYSMAP_VMA_BASE, KERNEL_VA_BASE);
    assert_eq!(PHYSMAP_SLOTS, KERNEL_WINDOW_FIRST_SLOT);
    assert_eq!(MAX_PHYSMAP_GIB, PHYSMAP_SLOTS);

    let base = kernel_window_base();
    assert!(
        KernelWindow::is_representable(base, KERNEL_WINDOW_PAGES),
        "the window extent is representable"
    );
    let window_bytes = (KERNEL_WINDOW_PAGES as u64) * PAGE_SIZE as u64;
    assert!(base >= KERNEL_VA_BASE, "the window is in the kernel regime");
    assert_eq!(base % (1 << 30), 0, "the base is gigapage-aligned");
    assert_eq!(table_index(base, 1), KERNEL_WINDOW_FIRST_SLOT);
    // One gigapage short of the very top, so the extent's exclusive end is
    // representable and no consumer needs wrap arithmetic.
    assert_eq!(
        base + window_bytes,
        KERNEL_VA_BASE + ((ENTRIES_PER_TABLE as u64 - 1) << 30)
    );
    assert!(
        base >= physmap_virt((MAX_PHYSMAP_GIB as u64) << 30),
        "the window begins at or above the map's ceiling"
    );
}

#[test]
fn a_root_refuses_every_address_outside_its_own_regime() {
    use tairix_arch_api::mmu::{self, PageFlags};
    static POOL: PageTablePool = PageTablePool::new();

    // The L1 index of a window address and of a user address 447 GiB up are
    // the same nine bits, so without the regime check a kernel mapping
    // walked through a process root would land at a *user* address.
    let window_va = kernel_window_base();
    let aliasing_user_va = (KERNEL_WINDOW_FIRST_SLOT as u64) << 30;
    assert_eq!(
        table_index(window_va, 1),
        table_index(aliasing_user_va, 1),
        "the two addresses share an L1 slot, which is the hazard"
    );

    let mut user = AddressSpace::new_user(&POOL).expect("a root");
    assert_eq!(
        mmu::AddressSpace::map_page(&mut user, window_va, 0x4123_4000, PageFlags::READ),
        Err(MapError::InvalidFlags)
    );
    assert_eq!(mmu::AddressSpace::translate(&user, window_va), None);
    assert_eq!(
        mmu::AddressSpace::unmap(&mut user, window_va),
        Err(MapError::NotMapped)
    );
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut user, window_va),
        Err(MapError::NotMapped)
    );
    // The direct map's own addresses are refused the same way.
    assert_eq!(
        mmu::AddressSpace::map_page(&mut user, physmap_virt(0x1000), 0x1000, PageFlags::READ),
        Err(MapError::InvalidFlags)
    );

    // And the window handle refuses everything outside the window: a user
    // address, and the first address past the window's extent.
    let mut kernel = AddressSpace::new_kernel_window(&POOL).expect("kernel window root");
    assert_eq!(
        mmu::AddressSpace::map_page(&mut kernel, aliasing_user_va, 0x4123_4000, PageFlags::READ),
        Err(MapError::InvalidFlags)
    );
    let past = kernel_window_base() + (KERNEL_WINDOW_PAGES as u64) * PAGE_SIZE as u64;
    assert_eq!(
        mmu::AddressSpace::map_page(&mut kernel, past, 0x4123_4000, PageFlags::READ),
        Err(MapError::InvalidFlags)
    );
}

#[test]
fn tearing_down_a_kernel_window_handle_frees_nothing() {
    // Every table the handle can reach is the live regime's shared
    // sub-hierarchy, so a teardown walk that treated them as its own would
    // free the kernel heap's page tables. Plant a descriptor in a window
    // slot by hand and prove the reclaim never names it.
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_kernel_window(&POOL).expect("kernel window root");
    let shared = POOL.alloc().expect("a stand-in shared window table");
    let shared_phys = shared.as_ptr() as u64;

    let root_table = POOL
        .table_at(space.root_phys())
        .expect("the pool's own root");
    // SAFETY: `root_table` is this handle's live L1 table from the
    // process-static pool; writing its own window slot is what the
    // reservation does into the kernel root.
    unsafe {
        (*root_table)[KERNEL_WINDOW_FIRST_SLOT] = table_descriptor(shared_phys);
    }

    // SAFETY: the space is not the active translation regime (the host has
    // none), which is `reclaim_table_frames`' contract.
    unsafe {
        MmuAddressSpace::reclaim_table_frames(&mut space);
    }
    // SAFETY: as above — reading the root's own window slot.
    let slot = unsafe { (*root_table)[KERNEL_WINDOW_FIRST_SLOT] };
    assert_eq!(
        slot,
        table_descriptor(shared_phys),
        "a window handle must retire without walking the shared hierarchy"
    );
}

#[test]
fn a_range_past_the_ceiling_invalidates_the_whole_domain_and_one_at_it_does_not() {
    assert!(!invalidates_whole_domain(1));
    assert!(!invalidates_whole_domain(RANGE_INVALIDATION_CEILING));
    assert!(invalidates_whole_domain(RANGE_INVALIDATION_CEILING + 1));
    assert!(invalidates_whole_domain(usize::MAX));
}

#[test]
fn the_tlbi_operand_carries_the_va_field_and_nothing_above_it() {
    // A low address: `VA[63:56]` is zero, so the naive shift and the masked
    // one agree — which is why the missing mask went unnoticed while the
    // remap window lived in the low regime.
    let low = 0x4123_4000u64;
    assert_eq!(tlbi_page_operand(low), low >> 12);

    // A kernel-regime address: the mask is what keeps `VA[63:56]` out of
    // `TTL` (bits 47:44) and `ASID` (bits 63:48). Left in, `TTL` would read
    // `0b1111` — a 64 KiB-granule level-3 hint that permits the
    // implementation to leave this port's 4 KiB entry cached.
    let window = kernel_window_base();
    let operand = tlbi_page_operand(window);
    assert_eq!(operand >> 44, 0, "TTL and ASID must be clear");
    assert_eq!(operand, (window >> 12) & ((1u64 << 44) - 1));
    assert_ne!(operand, window >> 12, "the mask must actually drop bits");
    // And the field still names the right page: VA[55:12].
    assert_eq!(operand, (window << 8) >> 20);

    // Consecutive pages produce consecutive operands, so a range walk
    // invalidates each page exactly once.
    assert_eq!(
        tlbi_page_operand(window + PAGE_SIZE as u64),
        tlbi_page_operand(window) + 1
    );
}

#[test]
fn physmap_virt_offsets_by_the_map_base() {
    assert_eq!(physmap_virt(0), PHYSMAP_VMA_BASE);
    assert_eq!(physmap_virt(0x8123_4000), PHYSMAP_VMA_BASE + 0x8123_4000);
    // A gigabyte in is one L1 slot along.
    assert_eq!(
        table_index(physmap_virt(1 << 30), 1),
        PHYSMAP_FIRST_SLOT + 1
    );
}

/// A parent descriptor whose output address the frame source never handed
/// out is what a clobbered or hostile table looks like. Every walk must
/// read it as "nothing mapped here" rather than dereference the address
/// the integer happens to name.
#[test]
fn a_descriptor_the_source_cannot_reach_fails_the_walk_closed() {
    use tairix_arch_api::mmu::{self, PageFlags};
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");
    let va = 64u64 << 30;
    mmu::AddressSpace::map_page(&mut space, va, 0x4123_4000, PageFlags::READ)
        .expect("map the probe page");
    // A page-aligned table the pool never handed out, holding a valid
    // block at the index the walk would read next. Recovering a table by
    // dereferencing its address — what the walk did before it asked the
    // frame source — would read this and answer with a mapping; asking
    // the source refuses the address outright.
    let mut foreign = Table::new();
    foreign.0[table_index(va, 2)] = descriptor(0x4000_0000, KernelAccess::Write.leaf(true));
    let foreign_phys = foreign.0.as_ptr() as u64;

    // Overwrite the L1 table descriptor to point at it, valid and
    // non-block so the walk would follow it.
    let root_table = POOL
        .table_at(space.root_phys())
        .expect("the pool's own root");
    // SAFETY: this space's live L1 table from the process-static pool,
    // exclusively owned here.
    unsafe {
        (*root_table)[table_index(va, 1)] = table_descriptor(foreign_phys);
    }

    assert_eq!(mmu::AddressSpace::translate(&space, va), None);
    assert_eq!(
        mmu::AddressSpace::unmap(&mut space, va),
        Err(MapError::NotMapped)
    );
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va),
        Err(MapError::NotMapped)
    );
    // SAFETY: `root_phys` is the live L1 table of this exclusively-owned
    // space, drawn from `POOL`.
    assert!(!unsafe { set_accessed_flag_in_root(&POOL, space.root_phys(), va) });
    // And a fresh map over the unreachable branch is refused rather than
    // walked into: the leaf walk reads it as absent, then `ensure_child`
    // refuses the descriptor it cannot recover.
    assert_eq!(
        mmu::AddressSpace::map_page(&mut space, va, 0x4123_4000, PageFlags::READ),
        Err(MapError::PoolExhausted)
    );
}

/// The `PAR_EL1` decoders: the fault bit is bit 0, and a successful result's
/// physical address is its `PA` field plus the offset within the page.
///
/// Pure, so both are pinned on the host even though the `AT` probe that
/// produces the value only exists on the real PE.
#[test]
fn par_decoders_read_the_fault_bit_and_rebuild_the_physical_address() {
    use super::{par_faulted, par_phys};

    // Bit 0 set = translation faulted; the rest of the word is fault status.
    assert!(par_faulted(0x9));
    assert!(par_faulted(1));
    assert!(!par_faulted(0));
    assert!(!par_faulted(0x0000_0000_3e40_0000));

    // A success carries the page's PA in [47:12]; the low 12 bits come from
    // the probed address, so the result addresses the exact byte.
    let par = 0x0000_0000_3e40_2000;
    assert_eq!(par_phys(par, 0x0000_0000_3e40_2abc), 0x0000_0000_3e40_2abc);
    // Attribute bits above the PA field never leak into the address.
    assert_eq!(
        par_phys(0xff00_0000_3e40_2000 | 0xf00, 0x10),
        0x0000_0000_3e40_2010
    );
}

/// Every leaf a user root maps is tagged with the ASID it runs under,
/// whatever its kind; the kernel window's leaves and the direct map's
/// gigabytes are global, so one entry of each serves every ASID.
#[test]
fn a_user_roots_leaves_are_not_global_and_the_kernels_are() {
    use tairix_arch_api::mmu;
    static POOL: PageTablePool = PageTablePool::new();
    let page = PAGE_SIZE as u64;
    let mut user = AddressSpace::new_user(&POOL).expect("a root");
    let kinds = [
        PageFlags::READ | PageFlags::USER,
        PageFlags::READ | PageFlags::WRITE | PageFlags::USER,
        PageFlags::READ | PageFlags::EXEC | PageFlags::USER,
        PageFlags::DEVICE | PageFlags::USER | PageFlags::WRITE,
        PageFlags::DMA_COHERENT | PageFlags::USER | PageFlags::WRITE,
        PageFlags::WRITE_COMBINE | PageFlags::USER | PageFlags::WRITE,
        PageFlags::READ | PageFlags::WRITE,
        PageFlags::DEVICE | PageFlags::WRITE,
    ];
    for (index, flags) in (0u64..).zip(kinds) {
        let va = (64u64 << 30) + index * page;
        mmu::AddressSpace::map_page(&mut user, va, 0x4100_0000 + index * page, flags)
            .expect("a user leaf");
        let leaf = host_leaf_descriptor(&POOL, user.root_phys(), va).expect("mapped");
        assert_ne!(leaf & attrs::NOT_GLOBAL, 0, "{flags:?} maps a global leaf");
    }

    let mut kernel = AddressSpace::new_kernel_window(&POOL).expect("kernel window root");
    let va = kernel_window_base();
    mmu::AddressSpace::map_page(
        &mut kernel,
        va,
        0x4123_4000,
        PageFlags::READ | PageFlags::WRITE,
    )
    .expect("a window leaf");
    let leaf = host_leaf_descriptor(&POOL, kernel.root_phys(), va).expect("mapped");
    assert_eq!(
        leaf & attrs::NOT_GLOBAL,
        0,
        "a kernel window leaf is per-ASID"
    );

    for holding in [Holding::Ram, Holding::Registers, Holding::Config] {
        assert_eq!(
            holding.leaf() & attrs::NOT_GLOBAL,
            0,
            "the direct map is every ASID's"
        );
    }
    assert_ne!(
        IDMAP_LEAF & attrs::NOT_GLOBAL,
        0,
        "the identity page goes with the ASID parking discards"
    );
}

/// The ASID rides above the table base, where `TCR_EL1.A1` clear reads it,
/// and every tag the port offers loads a distinct `TTBR0_EL1` value.
#[test]
fn ttbr0_carries_the_asid_above_the_table_base() {
    assert_eq!(ttbr0(0x4123_4000, 0), 0x4123_4000);
    assert_eq!(ttbr0(0x4123_4000, 7), 0x4123_4000 | (7 << 48));
    assert_eq!(ttbr0(0x4123_4000, 255) >> 56, 0, "an 8-bit ASID only");
    tairix_arch_api::tags_conformance::run_all(&TAGS, u16::from(u8::MAX), 0x4123_4000, 0x4124_4000);
    assert!(TAGS.invalidation_reaches_every_tag());
}

/// A kernel-only Normal Non-Cacheable page — the write-combined boot
/// framebuffer, an in-kernel DMA buffer — is EL1's alone: a leaf EL0 may
/// access hands it to user code, and under PAN faults the kernel's own use.
#[test]
fn non_cacheable_leaves_reach_el0_only_when_asked() {
    const AP_MASK: u64 = 0b11 << 6;
    let shared = PageFlags::READ | PageFlags::WRITE;
    for memory in [PageFlags::WRITE_COMBINE, PageFlags::DMA_COHERENT] {
        let kernel = AddressSpace::leaf_attrs_for(shared | memory);
        let user = AddressSpace::leaf_attrs_for(shared | memory | PageFlags::USER);
        assert_eq!(kernel & AP_MASK, attrs::AP_RW_EL1, "{memory:?}");
        assert_eq!(user & AP_MASK, attrs::AP_RW_EL0, "{memory:?}");
        assert_eq!(kernel & !AP_MASK, user & !AP_MASK, "only the reach differs");
        let decoded = page_flags_from_leaf(kernel);
        assert!(decoded.contains(memory) && !decoded.contains(PageFlags::USER));
    }
    assert_eq!(dma_coherent_leaf_attrs() & AP_MASK, attrs::AP_RW_EL1);
}

/// The fault walks a leaf whose Access Flag the scanner cleared, and the
/// space's owner unmaps the page on another CPU before the fault sets the
/// flag: the update must not map the freed frame again.
#[test]
fn an_access_flag_fix_up_never_restores_a_leaf_unmapped_after_its_walk() {
    use tairix_arch_api::mmu;
    static POOL: PageTablePool = PageTablePool::new();
    let mut space = AddressSpace::new_user(&POOL).expect("a root");
    let va = 64u64 << 30;
    mmu::AddressSpace::map_page(
        &mut space,
        va,
        0x4100_0000,
        PageFlags::READ | PageFlags::USER,
    )
    .expect("map");
    assert_eq!(
        mmu::AddressSpace::test_and_clear_accessed(&mut space, va),
        Ok(true)
    );
    // SAFETY: the space's root table, alive for the whole test.
    let walked = unsafe { leaf_in(&POOL, space.root_phys(), va) }.expect("the fault's walk");
    mmu::AddressSpace::unmap(&mut space, va).expect("unmap");
    assert_eq!(walked.set_while(attrs::AF, retries_access), None);
    assert_eq!(
        mmu::AddressSpace::translate(&space, va),
        None,
        "the unmap stands"
    );
}
