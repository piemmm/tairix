/* Linker script for TAIRiX aarch64 kernels on the Raspberry Pi 4 (BCM2711).
 *
 * The Pi firmware (`start4.elf`) places `kernel8.img` at physical 0x20_0000,
 * by the arm64 `Image` header `boot.s` opens it with, and enters it there at
 * EL2 with the MMU off and the Linux aarch64 boot hand-off (x0 = the device
 * tree's physical address). See `docs/src/platform/aarch64.md` ("Raspberry
 * Pi 4 (BCM2711)").
 *
 * This is the per-board boot artefact the boot-stub carve-out permits: the
 * load address and the stack size, the layout being every board's
 * (`aarch64-image.ld`). Every other board difference is discovered
 * device-tree data.
 */

/* Where the firmware places the image: 2 MiB above the base of RAM, for a
 * header whose `text_offset` is zero. A block boundary, so the image's
 * virtual and physical addresses agree in every block. */
KERNEL_LOAD = 0x00200000;

/* Boot-stack size: the single-CPU boot pipeline is all that runs on it, so
 * it keeps the smaller figure, not the `virt` script's test allowance. */
BOOT_STACK_BYTES = 64K;

INCLUDE aarch64-image.ld
