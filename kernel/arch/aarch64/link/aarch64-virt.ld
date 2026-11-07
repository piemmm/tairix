/* Linker script for TAIRiX aarch64 kernels on the QEMU `virt` board.
 *
 * QEMU `-M virt` places guest RAM at 0x4000_0000 and loads the kernel 2 MiB
 * above it — the ELF at its load address, an `Image` by the header `boot.s`
 * opens it with — entering at that physical address with the MMU off and
 * the Linux aarch64 boot hand-off (x0 = the device tree's physical address).
 * The layout is every board's (`aarch64-image.ld`).
 */

/* Where QEMU loads the image. A block boundary, so the image's virtual and
 * physical addresses agree in every block. */
KERNEL_LOAD = 0x40200000;

/* Boot-stack size: the determinism verticals' figure reference grid
 * measured above 64 KiB and below 128 KiB, so this is roughly twice that.
 * The Pi 4 script keeps the smaller boot-pipeline figure. */
BOOT_STACK_BYTES = 256K;

INCLUDE aarch64-image.ld
