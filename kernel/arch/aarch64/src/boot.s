// TAIRiX aarch64 boot trampoline, the same on every board: only the load
// address differs, and it lives in the board's linker script
// (`plans/PI.md` §0.2).
//
// Entered at the image's first byte, at its physical load address, with the
// MMU off and the Linux aarch64 boot hand-off (x0 = the device tree's
// physical address). QEMU `virt` enters at EL1, or EL2 with virtualization;
// the Pi firmware at EL2, which is dropped to EL1 here.
//
// Everything before the branch to the high half runs at the load address, so
// it addresses memory PC-relatively only: `adrp` there yields physical
// addresses, and after the branch the linked virtual ones.
//
// The Pi 4 firmware without a spin-table `armstub` releases every core
// here. A core whose affinity is not zero parks in `wfe` before touching
// anything shared, until the SMP bring-up opens the kernel release word *and*
// names this core in the release target (`smp::start_secondary_spintable`):
// one core at a time, never a race through bring-up. Both words are `.data`,
// so a parked core reads the loaded image's values until they are published,
// not whatever DRAM held where `.bss` is.
//
// SAFETY-INVARIANTs:
//   1. The boot CPU's body runs once; every other core is held in the park.
//      Interrupts stay masked throughout.
//   2. x19 holds the tree's address, x23 its length (0 without one), from
//      the hand-off to the Rust entry.
//   3. `SCTLR_EL1` is written whole with its known MMU-off value before the
//      first EL1 data access: its reset value is UNKNOWN on real silicon, and
//      an UNKNOWN EE or WXN wrecks EL1. The EL2 path writes every EL2 control
//      register whole likewise (`el2::*_HANDOFF`).
//   4. An image not at the address it was linked to load at parks: the early
//      map's blocks and the kernel root assume it.
//   5. The early tables are written with the cache off and invalidated
//      before and after, so no line a cache held can shadow them or be
//      written back over them once the walker reads them cacheably.
//   6. `.bss` is zeroed only through the cache, after translation is on, so
//      no line the firmware left can later be written back over the zeroes.

.section .text.boot, "ax"
.global _start
_start:
    // The arm64 `Image` header (`image_header`): with a zero
    // `text_offset` the Pi firmware and QEMU place the image 2 MiB above the
    // base of RAM, where it is linked to load. Without it the firmware moves
    // the image to `0x8_0000`.
    b       .Lentry                 // code0
    .long   0                       // code1
    .quad   0                       // text_offset
    .quad   __kernel_image_size     // image_size
    .quad   {IMAGE_FLAGS}           // flags
    .quad   0, 0, 0                 // res2..res4
    .long   {IMAGE_MAGIC}           // magic
    .long   0                       // res5
.Lentry:
    msr     DAIFSet, #0xf
    mov     x19, x0

    // Park every core but the boot CPU, whose affinity (Aff0..Aff3) is zero.
    mrs     x4, mpidr_el1
    mov     x5, #0xffff             // Aff0 | Aff1
    movk    x5, #0xff, lsl #16      // | Aff2: `smp::MPIDR_AFFINITY_MASK`
    and     x6, x4, x5
    ubfx    x7, x4, #32, #8         // Aff3
    orr     x6, x6, x7
    cbz     x6, .Lboot_cpu
    and     x8, x4, x5
.Lpark_secondary:
    // One `sev` wakes every parked core; only the named one goes on, through
    // the gate `smp::release_gate_open` states and `smp.s` checks again.
    wfe
    adrp    x4, SECONDARY_KERNEL_RELEASE
    add     x4, x4, :lo12:SECONDARY_KERNEL_RELEASE
    ldr     x4, [x4]
    cbz     x4, .Lpark_secondary
    adrp    x5, SECONDARY_KERNEL_RELEASE_TARGET
    add     x5, x5, :lo12:SECONDARY_KERNEL_RELEASE_TARGET
    ldr     x5, [x5]
    cmp     x5, x8
    b.ne    .Lpark_secondary
    br      x4

.Lboot_cpu:
    mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #2
    b.ne    .Lin_el1
    adr     x20, .Lin_el1
    b       _el2_establish_and_drop

.Lin_el1:
    // Immediates, not a literal: until this write even a load's byte order
    // is UNKNOWN.
    mov     x0, #({SCTLR_MMU_OFF} & 0xffff)
    movk    x0, #({SCTLR_MMU_OFF} >> 16), lsl #16
    msr     sctlr_el1, x0
    isb
    // TPIDR_EL1 is the dense CPU id, the boot CPU's zero; TPIDRRO_EL0 is the
    // EL1 vector's scratch, which EL0 can read, so it starts empty.
    msr     tpidr_el1, xzr
    msr     tpidrro_el0, xzr

    adr     x0, _start
    ldr     x1, =__kernel_phys_start
    cmp     x0, x1
    b.ne    .Lmisloaded

    // The tree's header, read with Device semantics: an aligned address
    // carrying the magic names a tree, whose `totalsize` the entry bounds.
    mov     x23, #0
    cbz     x19, .Ltree_read
    tst     x19, #7
    b.ne    .Ltree_read
    ldr     w0, [x19]
    rev     w0, w0
    ldr     w1, ={FDT_MAGIC}
    cmp     w0, w1
    b.ne    .Ltree_read
    ldr     w0, [x19, #4]
    rev     w23, w0
.Ltree_read:

    adrp    x0, __early_tables_start
    add     x0, x0, :lo12:__early_tables_start
    adrp    x1, __early_tables_end
    add     x1, x1, :lo12:__early_tables_end
    bl      .Linvalidate_to_poc
    adrp    x0, __early_tables_start
    add     x0, x0, :lo12:__early_tables_start
1:
    stp     xzr, xzr, [x0], #16
    cmp     x0, x1
    b.lo    1b

    // The identity root maps the one page translation is turned on from.
    adrp    x2, __idmap_text_start
    adrp    x3, tairix_aarch64_idmap_l1
    adrp    x4, tairix_aarch64_idmap_l2
    adrp    x5, tairix_aarch64_idmap_l3
    ubfx    x6, x2, #30, #9
    orr     x7, x4, #{TABLE}
    str     x7, [x3, x6, lsl #3]
    ubfx    x6, x2, #21, #9
    orr     x7, x5, #{TABLE}
    str     x7, [x4, x6, lsl #3]
    ubfx    x6, x2, #12, #9
    ldr     x7, ={IDMAP_LEAF}
    orr     x7, x7, x2
    str     x7, [x5, x6, lsl #3]

    // The high root maps the image at its link address: the top slot of the
    // kernel regime, with its blocks at their physical offsets in a gigabyte.
    adrp    x3, tairix_aarch64_early_l1
    adrp    x4, tairix_aarch64_early_l2
    orr     x7, x4, #{TABLE}
    str     x7, [x3, #{IMAGE_SLOT_OFFSET}]
    adr     x2, _start
    adrp    x6, __kernel_end
    add     x6, x6, :lo12:__kernel_end
    ldr     x8, ={EARLY_BLOCK}
2:
    lsr     x9, x2, #21
    lsl     x10, x9, #21
    orr     x10, x10, x8
    and     x9, x9, #0x1ff
    str     x10, [x4, x9, lsl #3]
    add     x2, x2, #(1 << 21)
    cmp     x2, x6
    b.lo    2b

    adrp    x0, __early_tables_start
    add     x0, x0, :lo12:__early_tables_start
    bl      .Linvalidate_to_poc

    ldr     x0, ={MAIR}
    msr     mair_el1, x0
    ldr     x0, ={TCR}
    msr     tcr_el1, x0
    adrp    x0, tairix_aarch64_idmap_l1
    msr     ttbr0_el1, x0
    adrp    x0, tairix_aarch64_early_l1
    msr     ttbr1_el1, x0
    dsb     sy
    tlbi    vmalle1
    ic      iallu
    dsb     nsh
    isb
    ldr     x0, ={SCTLR_MMU_ON}
    ldr     x1, =.Lhigh
    b       tairix_aarch64_enable_mmu

.Lhigh:
    adrp    x0, __bss_start
    add     x0, x0, :lo12:__bss_start
    adrp    x1, __bss_end
    add     x1, x1, :lo12:__bss_end
3:
    cmp     x0, x1
    b.hs    4f
    stp     xzr, xzr, [x0], #16
    b       3b
4:
    // The guard's sentinel, which the panic path reads back; the kernel root
    // then maps the guard read-only.
    adrp    x0, __boot_stack_guard_bottom
    add     x0, x0, :lo12:__boot_stack_guard_bottom
    adrp    x1, __boot_stack_bottom
    add     x1, x1, :lo12:__boot_stack_bottom
    mov     w2, #{GUARD_BYTE}
5:
    cmp     x0, x1
    b.hs    6f
    strb    w2, [x0], #1
    b       5b
6:
    adrp    x0, __boot_stack_top
    add     x0, x0, :lo12:__boot_stack_top
    mov     sp, x0
    mov     x0, x19
    mov     x1, x23
    bl      tairix_arch_aarch64_main
.Lmisloaded:
    wfi
    b       .Lmisloaded

// Invalidate the data cache over [x0, x1) to the point of coherency, by
// line, then `dsb sy`. Clobbers x0, x2, x3.
.Linvalidate_to_poc:
    mrs     x3, ctr_el0
    ubfx    x3, x3, #16, #4
    mov     x2, #4
    lsl     x3, x2, x3
    sub     x2, x3, #1
    bic     x0, x0, x2
7:
    dc      ivac, x0
    add     x0, x0, x3
    cmp     x0, x1
    b.lo    7b
    dsb     sy
    ret

// Entered at EL2 by the boot CPU and by a spin-table secondary
// (`smp.s`): establish fully known EL2 state and `eret` to EL1h at x20 with
// DAIF masked. Clobbers x0 only.
//
// Every EL2 control register is UNKNOWN at first entry on real silicon (the
// Pi stub sets only SCTLR_EL2 and CPUECTLR_EL1.SMPEN), so each is written
// whole with its known hand-off value (`tairix_arch_aarch64::el2`); an `orr`
// into the live register would keep UNKNOWN bits such as HCR_EL2.TVM, which
// traps EL1's first MAIR/TCR/TTBR/SCTLR write into vector-less EL2.
.global _el2_establish_and_drop
_el2_establish_and_drop:
    mov     x0, #{HCR_EL2}
    msr     hcr_el2, x0
    mov     x0, #{CNTHCTL_EL2}
    msr     cnthctl_el2, x0
    msr     cntvoff_el2, xzr
    mov     x0, #{CPTR_EL2}
    msr     cptr_el2, x0
    // MDCR_EL2 traps nothing and leaves EL1 every event counter: an HPMN of
    // zero is UNPREDICTABLE before FEAT_HPMN0, and a core with no
    // architected PMU has no PMCR_EL0 to read.
    mrs     x0, id_aa64dfr0_el1
    ubfx    x0, x0, #{PMUVER_SHIFT}, #4
    cmp     x0, #0                  // not implemented
    ccmp    x0, #0xf, #4, ne        // or IMPLEMENTATION DEFINED
    mov     x0, xzr
    b.eq    2f
    mrs     x0, pmcr_el0
    ubfx    x0, x0, #{PMCR_N_SHIFT}, #5
2:
    msr     mdcr_el2, x0
    // A core with the GICv3 system-register interface must let EL1 use it.
    mrs     x0, id_aa64pfr0_el1
    ubfx    x0, x0, #{GIC_SHIFT}, #4
    cbz     x0, 1f
    mov     x0, #{ICC_SRE_EL2}
    msr     S3_4_C12_C9_5, x0       // ICC_SRE_EL2
    isb
    msr     S3_4_C12_C11_0, xzr     // ICH_HCR_EL2
1:
    // EL1 reads MIDR/MPIDR through VPIDR/VMPIDR: mirror the real ones.
    mrs     x0, midr_el1
    msr     vpidr_el2, x0
    mrs     x0, mpidr_el1
    msr     vmpidr_el2, x0
    mov     x0, #0x3c5              // EL1h, DAIF masked
    msr     spsr_el2, x0
    msr     elr_el2, x20
    eret
    dsb     nsh
    isb
.ltorg

// The only code mapped at its physical address: what runs while one
// translation regime is replaced by another.
.section .text.idmap, "ax"

// Write SCTLR_EL1 = x0, turning translation on, and branch to x1.
.global tairix_aarch64_enable_mmu
tairix_aarch64_enable_mmu:
    msr     sctlr_el1, x0
    isb
    ic      iallu
    dsb     nsh
    isb
    br      x1

// Load x0 as TTBR1_EL1 by way of the empty table x1, so no walk of the
// kernel regime can meet entries of both roots; returns into the new one.
.global tairix_aarch64_replace_ttbr1
tairix_aarch64_replace_ttbr1:
    msr     ttbr1_el1, x1
    isb
    tlbi    vmalle1
    dsb     nsh
    isb
    msr     ttbr1_el1, x0
    isb
    ret

.section .early_tables, "aw", %nobits
.balign 4096
.global tairix_aarch64_idmap_l1
tairix_aarch64_idmap_l1:
    .skip   4096
tairix_aarch64_idmap_l2:
    .skip   4096
tairix_aarch64_idmap_l3:
    .skip   4096
tairix_aarch64_early_l1:
    .skip   4096
tairix_aarch64_early_l2:
    .skip   4096
