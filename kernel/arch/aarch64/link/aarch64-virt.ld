/* Linker script for TAIRiX aarch64 kernels on the QEMU `virt` board.
 *
 * QEMU `-M virt` places guest RAM at 0x4000_0000 and loads the kernel 2 MiB
 * above it — the ELF at its load address, an `Image` by the header `boot.s`
 * opens it with — entering at that physical address with the MMU off and
 * the Linux aarch64 boot hand-off (x0 = the device tree's physical address).
 *
 * The image is linked in the kernel's translation regime and loaded at its
 * physical address: `boot.s` runs there, PC-relatively, until it has turned
 * translation on and branched high. The kernel root maps each span below
 * with only the access it needs (`paging::kernel_root`), so a section is not
 * added without a span to map it. This script is identical to
 * `aarch64-rpi4.ld` but for the load address and the stack size, both fixed
 * before any device tree is read (`plans/PI.md` §0.2).
 *
 * Invariants enforced here:
 *   - `.text.boot` is the image's first byte, where the loader enters.
 *   - `.text` starts and ends on the image block (`kernel_image_block`), so
 *     the code is mapped with block leaves alone; its padding is zero, which
 *     A64 decodes as the permanently undefined `udf #0`.
 *   - `.text.idmap` is one page of its own: the only code ever mapped at its
 *     physical address.
 *   - `.bss` ends with the boot stack's guard and the stack, the guard
 *     mapped read-only.
 *   - `.early_tables` lie outside `.bss`, which `boot.s` zeroes only once
 *     translation is on, under them.
 *   - the boot heap is a declared `.heap` (NOLOAD) section: left an orphan,
 *     the linker may place it as PROGBITS or drop it.
 */

OUTPUT_ARCH(aarch64)
ENTRY(__kernel_entry_phys)

/* Where QEMU loads the image. A block boundary, so the image's virtual and
 * physical addresses agree in every block. */
KERNEL_LOAD = 0x40200000;


/* Boot-stack size: the determinism verticals' figure reference grid
 * measured above 64 KiB and below 128 KiB, so this is roughly twice that.
 * The Pi 4 script keeps the smaller boot-pipeline figure. */
BOOT_STACK_BYTES = 256K;

/* The image's place in the kernel regime: the top gigabyte, at the load
 * address's offset within its own. */
__kimage_voffset = kernel_image_gigabyte - (KERNEL_LOAD & ~0x3FFFFFFF);

SECTIONS
{
    . = KERNEL_LOAD + __kimage_voffset;
    __kernel_start = .;

    .text : AT(ADDR(.text) - __kimage_voffset)
    {
        __text_start = .;
        KEEP(*(.text.boot))
        . = ALIGN(4K);
        __idmap_text_start = .;
        KEEP(*(.text.idmap))
        . = ALIGN(4K);
        __idmap_text_end = .;
        *(.text .text.*)
        . = ALIGN(kernel_image_block);
        __text_end = .;
    }

    .rodata : AT(ADDR(.rodata) - __kimage_voffset)
    {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4K);
        __rodata_end = .;
    }

    .data : AT(ADDR(.data) - __kimage_voffset) ALIGN(4K)
    {
        __data_start = .;
        *(.data .data.*)
        *(.got .got.*)
    }

    .bss : AT(ADDR(.bss) - __kimage_voffset) ALIGN(4K)
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4K);
        __boot_stack_guard_bottom = .;
        . += boot_stack_guard_bytes;
        __boot_stack_bottom = .;
        . += BOOT_STACK_BYTES;
        __boot_stack_top = .;
        . = ALIGN(4K);
        __bss_end = .;
    }

    .early_tables (NOLOAD) : AT(ADDR(.early_tables) - __kimage_voffset) ALIGN(4K)
    {
        __early_tables_start = .;
        KEEP(*(.early_tables))
        . = ALIGN(4K);
        __early_tables_end = .;
    }

    .heap (NOLOAD) : AT(ADDR(.heap) - __kimage_voffset) ALIGN(4K)
    {
        __heap_start = .;
        KEEP(*(.heap))
        . = ALIGN(4K);
    }

    __kernel_end = .;

    /DISCARD/ :
    {
        *(.eh_frame .eh_frame.*)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}

__kernel_phys_start = __kernel_start - __kimage_voffset;
__kernel_image_size = __kernel_end - __kernel_start;
__kernel_phys_end = __kernel_end - __kimage_voffset;
__kernel_entry_phys = _start - __kimage_voffset;

ASSERT(KERNEL_LOAD % kernel_image_block == 0, "the kernel must load on an image block")
ASSERT(__text_start % kernel_image_block == 0 && __text_end % kernel_image_block == 0, "the kernel's text must fill whole blocks")
ASSERT(__idmap_text_end - __idmap_text_start == 4K, "the identity page must be one page")
ASSERT(__kernel_end - kernel_image_gigabyte <= kernel_image_reach, "the kernel image must end below the firmware tree's window")
ASSERT(_start == __kernel_start, "the trampoline must be the image's first byte")
