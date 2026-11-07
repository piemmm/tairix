// TAIRiX aarch64 secondary-core entry trampolines.
//
// A core started by PSCI `CPU_ON` enters `_start_secondary_aarch64` with
// x0 = the context id the boot CPU passed, its dense CpuId. A core released
// through the spin table enters `_start_secondary_spintable_aarch64` with
// nothing, possibly at EL2, and finds its dense id from its affinity. Both
// enter at their physical address with the MMU off and turn translation on
// as the boot CPU did: from the identity page, onto the boot CPU's identity
// root and the kernel root, then into the high half, where the core's stack
// is taken from the pool the boot CPU registered.
//
// SAFETY-INVARIANTs:
//   1. Entered once per core, only after the boot CPU built the kernel root
//      and registered the stack pool, the entry and the affinity table: the
//      start paths refuse otherwise (`smp::start_secondary*`).
//   2. `SCTLR_EL1` is written whole with its known MMU-off value before the
//      first data access, as `boot.s` does, for the same reason.
//   3. The core's caches are in an UNKNOWN power-on state, so its data cache
//      is invalidated, never cleaned, before translation turns it on: a stale
//      line would shadow the tables or the stack, and cleaning one would
//      write it over them.
//   4. Nothing before the branch to the high half reads memory but the
//      release gate, which `smp::start_secondary_spintable` cleans to the
//      point of coherency before it wakes the core.
//   5. `tairix_arch_aarch64_secondary_main` never returns.

.section .text.boot, "ax"
.global _start_secondary_aarch64
_start_secondary_aarch64:
    msr     DAIFSet, #0xf
    mov     x19, x0
    mov     x0, #({SCTLR_MMU_OFF} & 0xffff)
    movk    x0, #({SCTLR_MMU_OFF} >> 16), lsl #16
    msr     sctlr_el1, x0
    isb
    ldr     x1, =.Lpsci_high
    b       .Lsecondary_enable_mmu

.global _start_secondary_spintable_aarch64
_start_secondary_spintable_aarch64:
    msr     DAIFSet, #0xf
    mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #2
    b.ne    .Lspintable_el1
    adr     x20, .Lspintable_el1
    b       _el2_establish_and_drop
.Lspintable_el1:
    mov     x0, #({SCTLR_MMU_OFF} & 0xffff)
    movk    x0, #({SCTLR_MMU_OFF} >> 16), lsl #16
    msr     sctlr_el1, x0
    isb
    // The core this release names, through the gate `boot.s`'s park and
    // `smp::release_gate_open` share: anyone else waits.
    mrs     x2, mpidr_el1
    mov     x3, #0xffff
    movk    x3, #0xff, lsl #16
    and     x21, x2, x3
.Lspintable_gate:
    adrp    x7, SECONDARY_KERNEL_RELEASE_TARGET
    add     x7, x7, :lo12:SECONDARY_KERNEL_RELEASE_TARGET
    ldr     x7, [x7]
    cmp     x7, x21
    b.eq    .Lspintable_go
    wfe
    b       .Lspintable_gate
.Lspintable_go:
    ldr     x1, =.Lspintable_high

// Turn translation on and branch to x1, the high continuation.
.Lsecondary_enable_mmu:
    msr     tpidrro_el0, xzr
    mov     x22, x1
    bl      _invalidate_local_dcache_to_poc
    ldr     x0, ={MAIR}
    msr     mair_el1, x0
    ldr     x0, ={TCR}
    msr     tcr_el1, x0
    adrp    x0, tairix_aarch64_idmap_l1
    msr     ttbr0_el1, x0
    adrp    x0, tairix_aarch64_kernel_root
    msr     ttbr1_el1, x0
    dsb     sy
    tlbi    vmalle1
    ic      iallu
    dsb     nsh
    isb
    ldr     x0, ={SCTLR_MMU_ON}
    mov     x1, x22
    b       tairix_aarch64_enable_mmu

// The spin-table core's dense id: its place in the affinity table.
.Lspintable_high:
    adrp    x3, SECONDARY_AFFINITY_BASE
    add     x3, x3, :lo12:SECONDARY_AFFINITY_BASE
    ldr     x3, [x3]
    cbz     x3, .Lsecondary_park
    adrp    x4, SECONDARY_AFFINITY_COUNT
    add     x4, x4, :lo12:SECONDARY_AFFINITY_COUNT
    ldr     x4, [x4]
    mov     x19, #0
1:
    cmp     x19, x4
    b.hs    .Lsecondary_park
    ldr     x6, [x3, x19, lsl #3]
    cmp     x6, x21
    b.eq    .Lpsci_high
    add     x19, x19, #1
    b       1b

// sp = base + (cpuid + 1) * stride: the top of this core's slice.
.Lpsci_high:
    adrp    x0, tairix_arch_aarch64_secondary_stack_base
    add     x0, x0, :lo12:tairix_arch_aarch64_secondary_stack_base
    ldr     x0, [x0]
    adrp    x1, SECONDARY_STACK_STRIDE
    add     x1, x1, :lo12:SECONDARY_STACK_STRIDE
    ldr     x1, [x1]
    add     x2, x19, #1
    madd    x0, x2, x1, x0
    mov     sp, x0
    mov     x0, x19
    bl      tairix_arch_aarch64_secondary_main
.Lsecondary_park:
    wfe
    b       .Lsecondary_park
.ltorg
