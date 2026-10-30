// Multiboot2 + PVH boot entries + 32→64 long-mode trampoline.
//
// Two boot protocols enter the same trampoline:
//
// * **Multiboot2**: any multiboot2 loader accepts the ELF64 kernel and
//   enters it in 32-bit protected mode at `_start` (QEMU's `-kernel`
//   multiboot1 loader refuses ELF64). It is the hand-off the first-party
//   boot loader is built around (`plans/BOOTLOADER.md`).
// * **PVH direct boot** (Xen HVM start-info protocol): QEMU's own ELF
//   loader honours the `XEN_ELFNOTE_PHYS32_ENTRY` note below and enters
//   `pvh_start` in 32-bit protected mode with `%ebx` = the physical
//   address of the `hvm_start_info` record — no firmware or bootloader
//   in the path. The QEMU integration tests boot this way, keeping
//   firmware nondeterminism out of the loop entirely.
//
// Both entries converge on `boot_common` with `%edi` = boot magic and
// `%esi` = boot-info physical address; the Rust side (`entry.rs`)
// re-validates the magic and records which protocol delivered the blob.
//
// SAFETY-INVARIANTS:
//
//  1. The multiboot2 header sits in `.multiboot_header`, placed by the
//     linker script (`linker.ld`) within the first 32 KiB of the kernel
//     ELF — required by the multiboot2 spec for the loader to find it.
//  2. `_start` runs in 32-bit protected mode with CR0.PE=1, paging off,
//     EAX=multiboot2 magic (0x36D76289), EBX=multiboot info pointer,
//     and a flat 4 GiB code/data segmentation set up by the loader. `pvh_start`
//     runs under the same CPU state contract (32-bit protected mode,
//     paging off, flat segments — Xen PVH ABI, xen.git
//     docs/misc/pvh.pandoc) with EBX=hvm_start_info pointer; it loads
//     the PVH magic (0x336EC578, the value the start_info record itself
//     begins with) into EDI so the Rust prologue can tell the protocols
//     apart, then falls through to the shared `boot_common`.
//  3. The 4 KiB-aligned `boot_pml4`/`boot_pdpt`/`boot_pdpt_high`/
//     `boot_pdpt_physmap`/`boot_pds`/`boot_pds_physmap`
//     tables sit in `.boot.bss` (linked 1:1 in low memory by `linker.ld`)
//     and are zero-initialised by the loader (BSS bytes are zero per the
//     multiboot spec; QEMU's PVH ELF loader zero-fills the
//     p_memsz - p_filesz tail of its load segments the same way).
//     They live in `.boot.bss` rather than the
//     high-half `.bss` so the 32-bit trampoline can name them with absolute
//     32-bit operands before paging is enabled.
//  4. The identity window maps the first GiB through one page directory of
//     2 MiB leaves, `boot_pds`: it is what the trampoline executes from,
//     and reads its GDT through, from the moment paging is on until it
//     jumps high. Nothing else reaches a physical address as itself: the
//     Rust entry leaves this root for the kernel root, which has no
//     identity window, before the kernel proper runs.
//  5. The long-mode GDT below has a single 64-bit code segment at
//     selector 0x08 with L=1 (long mode) and a 64-bit data segment at
//     selector 0x10. Both are flat (base 0, limit ignored in 64-bit).
//  6. On entry to `long_mode_start` interrupts are disabled (CLI is the
//     bootloader default) and the IDTR is invalid; the Rust side must
//     install an IDT before enabling interrupts.
//  7. `tairix_arch_x86_64_main` receives the boot magic (multiboot2 or
//     PVH) in `%rdi` and the boot-info pointer in `%rsi` (System V
//     AMD64 ABI). The Rust prologue re-validates the magic before
//     touching the info blob (validate every input).
//  8. If `tairix_arch_x86_64_main` ever returns we halt the CPU with
//     interrupts masked. The Rust contract (`-> !`) makes this branch
//     unreachable; the `hlt`/`jmp` loop is the belt-and-braces fallback.
//  9. The kernel is a -2 GiB higher-half kernel (`linker.ld`): its Rust
//     sections are linked at virtual `KERNEL_VMA_BASE (0xFFFFFFFF80000000)
//     + phys` but loaded into low physical memory. Before the long-mode
//     jump the trampoline maps that window (PML4[511] -> `boot_pdpt_high`,
//     `boot_pdpt_high`[510] -> the identity window's directory `boot_pds`),
//     so virtual `0xFFFFFFFF80000000 + X` resolves to physical `X` for the
//     whole kernel image. The Rust entry runs on it until it builds the
//     kernel root, whose own image window maps each section with only the
//     access it needs. After entering long mode the
//     trampoline transfers to the high half with an absolute
//     `movabs`+`jmp *%rax` to `higher_half_entry`, and re-points `%rsp` at
//     the boot stack's own higher-half alias (`boot_stack_top_high`,
//     derived in `linker.ld`) so the running stack is mapped under every
//     translation root and not only under the identity window.
// 10. The direct physical map's floor: PML4[256] -> `boot_pdpt_physmap`,
//     whose low four entries point at the page directories
//     `boot_pds_physmap`, whose 2 MiB leaves are read/write and
//     non-executable, so physical `X` is reachable at
//     `paging::PHYSMAP_VMA_BASE + X` for `X` in 0..4 GiB from the first
//     instruction after paging is on. The kernel root carries a copy of
//     that PDPT, with its first directory drawn afresh, so these tables are
//     never edited while live; it verifies this entry against its own slot
//     constant first, so an asm/Rust disagreement fails the boot rather
//     than copying something else. The widening
//     (`paging::install_boot_physmap`) extends the copy.
//     Laying the floor here rather than in the boot path is what lets the
//     LAPIC and IO-APIC register blocks be named at one address: every
//     consumer of this crate has the map, including one that runs no boot
//     pipeline of its own.
// 11. `CR0` is written whole (PE|PG) rather than amended: multiboot2 leaves
//     its other bits undefined, and a cache-disable bit inherited from the
//     loader would leave the kernel running uncached. The FPU set-up and the
//     kernel root's entry add the rest.

.section .multiboot_header, "a"
.align 8
multiboot_header_start:
    // Multiboot2 header (multiboot2 spec §3.1):
    .long 0xE85250D6                                // magic
    .long 0                                         // architecture: i386 32-bit protected mode
    .long multiboot_header_end - multiboot_header_start            // header_length
    // Checksum: -(magic + architecture + header_length), mod 2^32
    .long -(0xE85250D6 + 0 + (multiboot_header_end - multiboot_header_start))

    // End tag (type=0, flags=0, size=8). Required terminator.
    .align 8
    .short 0
    .short 0
    .long 8
multiboot_header_end:

// PVH boot note (Xen HVM direct-boot ABI, xen.git
// xen/include/public/elfnote.h): note type 18 = XEN_ELFNOTE_PHYS32_ENTRY,
// name "Xen", descriptor = the 32-bit physical entry point. QEMU's
// `-kernel` ELF loader finds this note in the PT_NOTE segment and enters
// `pvh_start` in 32-bit protected mode with %ebx = &hvm_start_info.
.section .note.tairix_pvh, "a", @note
.align 4
    .long 4                                         // namesz ("Xen\0")
    .long 4                                         // descsz
    .long 18                                        // XEN_ELFNOTE_PHYS32_ENTRY
    .asciz "Xen"
    .long pvh_start                                 // entry physical address

.section .boot.text, "ax"
.code32
.global _start
.type _start, @function
_start:
    cli
    movl %eax, %edi                                 // multiboot magic (preserved via %edi -> %rdi)
    movl %ebx, %esi                                 // multiboot info pointer
    jmp  boot_common

.size _start, . - _start

// PVH entry (SAFETY-INVARIANT 2): same CPU state contract as `_start`
// but %ebx points at the `hvm_start_info` record and no magic register
// is defined by the ABI, so load the PVH start-info magic into the
// protocol slot ourselves. The Rust prologue re-validates it against
// the blob's own leading magic field.
.global pvh_start
.type pvh_start, @function
pvh_start:
    cli
    movl $0x336EC578, %edi                          // PVH boot magic (hvm_start_info.magic)
    movl %ebx, %esi                                 // hvm_start_info pointer
    // fall through to boot_common

.size pvh_start, . - pvh_start

boot_common:
    // Neither boot protocol defines the direction flag; Rust needs it clear.
    cld
    movl $boot_stack_top, %esp
    xorl %ebp, %ebp

    // Poison the guard reserved below the stack, before anything descends
    // it, so a later overrun is a disturbed sentinel the panic path can
    // name rather than a silent clobber of the boot page tables under it.
    // %edi/%esi carry the boot hand-off into long mode and are left alone;
    // %ecx is dead until the page-table loop below resets it.
    movl $boot_stack_guard_bottom, %ecx
.Lpoison_guard:
    cmpl $boot_stack_bottom, %ecx
    jae  .Lpoison_guard_done
    movb ${GUARD_BYTE}, (%ecx)
    incl %ecx
    jmp  .Lpoison_guard
.Lpoison_guard_done:

    // PML4[0] -> boot_pdpt, PDPT[0] -> boot_pds: the identity window
    // (SAFETY-INVARIANT 4).
    movl $boot_pdpt, %eax
    orl  $0x3, %eax                                 // P|RW
    movl %eax, boot_pml4
    movl $boot_pds, %eax
    orl  $0x3, %eax                                 // P|RW
    movl %eax, boot_pdpt

    // PDS[k] = k*2 MiB | P|RW|PS, for k in 0..512: the first GiB.
    xorl %ecx, %ecx
1:
    movl %ecx, %eax
    shll $21, %eax                                  // ecx * 2 MiB
    orl  $0x83, %eax                                // P|RW|PS
    movl %eax, boot_pds(,%ecx,8)
    incl %ecx
    cmpl $512, %ecx
    jl   1b

    // The direct map's floor (SAFETY-INVARIANT 10): PDPT_PHYSMAP[i] ->
    // boot_pds_physmap + i*4096 | P|RW for i in 0..4, one directory per GiB.
    xorl %ecx, %ecx
2:
    movl %ecx, %eax
    shll $12, %eax                                  // ecx * 4096
    addl $boot_pds_physmap, %eax
    orl  $0x3, %eax                                 // P|RW
    movl %eax, boot_pdpt_physmap(,%ecx,8)
    incl %ecx
    cmpl $4, %ecx
    jl   2b

    // PDS_PHYSMAP[k] = k*2 MiB | P|RW|PS|NX, for k in 0..2048. The low
    // dword never carries a bit above 31 because k * 2 MiB < 2^32; NX is
    // bit 63, the high dword's bit 31.
    xorl %ecx, %ecx
3:
    movl %ecx, %eax
    shll $21, %eax                                  // ecx * 2 MiB
    orl  $0x83, %eax                                // P|RW|PS
    movl %eax, boot_pds_physmap(,%ecx,8)
    movl $0x80000000, boot_pds_physmap+4(,%ecx,8)   // NX
    incl %ecx
    cmpl $2048, %ecx
    jl   3b

    // Higher-half kernel window (SAFETY-INVARIANT 9). 0xFFFFFFFF80000000
    // decodes to PML4[511] -> PDPT[510] -> PD[0]; pointing PDPT[510] at the
    // identity window's directory maps virtual 0xFFFFFFFF80000000 + X to
    // physical X for X in 0..1 GiB, the whole kernel image's load window.
    //
    // PML4[511] -> boot_pdpt_high  (offset 511 * 8 = 0xFF8)
    movl $boot_pdpt_high, %eax
    orl  $0x3, %eax                                 // P|RW
    movl %eax, boot_pml4 + 0xFF8
    movl $0, boot_pml4 + 0xFFC

    // boot_pdpt_high[510] -> boot_pds  (offset 510 * 8 = 0xFF0)
    movl $boot_pds, %eax
    orl  $0x3, %eax                                 // P|RW
    movl %eax, boot_pdpt_high + 0xFF0
    movl $0, boot_pdpt_high + 0xFF4

    // PML4[256] -> boot_pdpt_physmap  (offset 256 * 8 = 0x800), the direct
    // physical map's floor (SAFETY-INVARIANT 10). The Rust side verifies
    // this entry against its own slot constant before it widens the map, so
    // a disagreement fails the boot rather than mapping nothing.
    movl $boot_pdpt_physmap, %eax
    orl  $0x3, %eax                                 // P|RW
    movl %eax, boot_pml4 + 0x800
    movl $0, boot_pml4 + 0x804

    // CR3 <- PML4
    movl $boot_pml4, %eax
    movl %eax, %cr3

    // CR4.PAE = 1
    movl %cr4, %eax
    orl  $(1 << 5), %eax
    movl %eax, %cr4

    // Arm long mode and execute-disable before paging can consume an NX leaf.
    movl $0xC0000080, %ecx
    rdmsr
    orl  $(1 << 8) | (1 << 11), %eax              // LME | NXE
    wrmsr

    // CR0 = PE|PG (SAFETY-INVARIANT 11): paging on, in compatibility mode.
    movl $((1 << 31) | (1 << 0)), %eax
    movl %eax, %cr0

    // Load 64-bit GDT, far-jump to 64-bit code segment.
    lgdt gdt64_ptr
    ljmp $0x08, $long_mode_start

.size boot_common, . - boot_common

.code64
long_mode_start:
    // Long mode ignores data segment bases/limits but the selectors must
    // be non-NULL writable; load our flat data selector everywhere.
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %fs
    movw %ax, %gs
    movw %ax, %ss

    // We are still executing at the low (identity-mapped) physical RIP of
    // `.boot.text`. The higher-half window is now mapped, so transfer to
    // the kernel's high virtual address space with an absolute jump: the
    // `movabs` materialises the full 64-bit link address of the high-half
    // landing pad (an R_X86_64_64 relocation the linker fills with its
    // KERNEL_VMA_BASE-relative VA), and `jmp *%rax` loads RIP with it.
    // rdi/rsi (multiboot magic / info pointer) are preserved — only %rax
    // is clobbered.
    movabs $higher_half_entry, %rax
    jmp *%rax

.size long_mode_start, . - long_mode_start

// -- Higher-half landing pad. Linked into `.text` at KERNEL_VMA_BASE + phys
//    (linker.ld), so reaching here means RIP is running in the higher-half
//    kernel window. From here a normal RIP-relative `call` reaches the Rust
//    entry point (both are high-half symbols).
.section .text, "ax"
.code64
.global higher_half_entry
.type higher_half_entry, @function
higher_half_entry:
    // Re-address the boot stack through the kernel window. The bytes do not
    // move — `boot_stack_top_high` is the same page at
    // `KERNEL_VMA_BASE + phys` (linker.ld) — but a low `%rsp` would live
    // only in the identity window, and a process root carries none: the
    // first push after a switch to one would fault. Nothing is live on the
    // stack here, so replacing the pointer outright is sound: the 32-bit
    // trampoline above pushes nothing and reaches this label by jump.
    movabsq $boot_stack_top_high, %rsp
    // The compiler may emit SSE in any Rust function, the first included.
    // The routine clobbers only %rax.
    call tairix_arch_x86_64_fpu_enable
    // rdi/rsi still hold the multiboot magic / info pointer (untouched by
    // the absolute jump above or the stack rebase); rdx hands the entry the
    // descriptor tables' reservation, through the kernel window.
    movabsq $boot_tables_high, %rdx
    call tairix_arch_x86_64_main

    // `tairix_arch_x86_64_main` is `-> !`; reaching here is a kernel bug.
    cli
.Lhang:
    hlt
    jmp .Lhang

.size higher_half_entry, . - higher_half_entry

// -- BSS-allocated bootstrap page tables and stack. The multiboot2 spec
//    guarantees BSS is zero-initialised before `_start` runs. These live in
//    the low `.boot.bss` section (linker.ld) so the 32-bit trampoline can
//    name them with absolute 32-bit operands before paging is enabled.
.section .boot.bss, "aw", @nobits
.align 4096
boot_pml4:
    .skip 4096
boot_pdpt:
    .skip 4096
// PDPT for the higher-half kernel window (SAFETY-INVARIANT 9). Its entry
// 510 points at `boot_pds` so 0xFFFFFFFF80000000 + X maps to physical X.
boot_pdpt_high:
    .skip 4096
// PDPT for the direct physical map's floor (SAFETY-INVARIANT 10). Its low
// entries point at `boot_pds_physmap`, and the kernel widens the remainder
// once the firmware memory map is known. Exposed so `paging.rs` can name
// the table it must agree with.
.global boot_pdpt_physmap
boot_pdpt_physmap:
    .skip 4096
// The identity window's directory (SAFETY-INVARIANT 4), which also backs
// the higher-half kernel window.
boot_pds:
    .skip 4096
// Four contiguous directories of the direct map's floor, one per GiB.
boot_pds_physmap:
    .skip 4096 * 4

// The bootstrap stack and its poison guard follow these tables in
// `.boot.bss`, reserved by `linker.ld`, which sizes the stack per image
// (`BOOT_STACK_BYTES`). A size fixed here would be one constant for every
// image, and a workload heavier than the boot pipeline silently outgrew it.

// -- Long-mode GDT. Lives in the low `.boot.rodata` section (linker.ld) so
//    `lgdt` can load it (by its low linear address) before paging is on.
//    Each descriptor's accessed bit is preset, so loading it writes nothing
//    to a read-only section.
.section .boot.rodata, "a"
.align 8
gdt64:
    .quad 0                                         // 0x00: null
    .quad 0x00AF9B000000FFFF                        // 0x08: 64-bit code (L=1)
    .quad 0x00AF93000000FFFF                        // 0x10: data
gdt64_end:
gdt64_ptr:
    .word gdt64_end - gdt64 - 1
    .quad gdt64
