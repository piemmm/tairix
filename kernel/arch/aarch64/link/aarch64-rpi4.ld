/* Linker script for TAIRiX aarch64 kernels on the Raspberry Pi 4 (BCM2711).
 *
 * The Pi firmware (`start4.elf`) places `kernel8.img` at physical 0x20_0000,
 * by the arm64 `Image` header `boot.s` opens it with, and enters it there at
 * EL2 with the MMU off and the Linux aarch64 boot hand-off (x0 = the device
 * tree's physical address). See `docs/src/platform/aarch64.md` ("Raspberry
 * Pi 4 (BCM2711)").
 *
 * This is the per-board boot artefact the boot-stub carve-out permits: it is
 * identical to `aarch64-virt.ld` but for the load address and the stack
 * size, both fixed before any device tree is read (`plans/PI.md` §0.2).
 * Every other board difference is discovered device-tree data. The
 * invariants are `aarch64-virt.ld`'s.
 */

OUTPUT_ARCH(aarch64)
ENTRY(__kernel_entry_phys)

/* Where the firmware places the image: 2 MiB above the base of RAM, for a
 * header whose `text_offset` is zero. A block boundary, so the image's
 * virtual and physical addresses agree in every block. */
KERNEL_LOAD = 0x00200000;


/* Boot-stack size: the single-CPU boot pipeline is all that runs on it, so
 * it keeps the smaller figure, not the `virt` script's test allowance. */
BOOT_STACK_BYTES = 64K;

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
