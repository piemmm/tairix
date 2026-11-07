# FIX-AARCH64-KERNEL-MAP — the aarch64 kernel in the high half, mapped per section

Binding under `AGENTS.md`. Closes `plans/OPEN-DEFECTS.md` D1577 and the
aarch64 parts of D1590, D1594 and D1595: the x86_64 kernel-map shape (D1571)
on the aarch64 port.

Read first: `plans/OPEN-DEFECTS.md` D1571 (the x86_64 shape and its tests),
`kernel/arch/x86_64/src/kernel_root.rs`, `kernel/arch/aarch64/src/paging.rs`,
`kernel/arch/aarch64/src/boot.s` and `smp.s`, the two aarch64 linker scripts,
`kernel/tairix-kernel/src/aarch64/boot.rs`, `plans/PI.md`.

## Ledger

| # | Item | Status |
|---|---|---|
| KM1 | The image is linked in the `TTBR1_EL1` half and loaded where the board puts it: one link address for every board, a physical entry, section bounds exported, text in whole 2 MiB blocks, the Pi 4 image loaded at 2 MiB | done |
| KM2 | The boot and secondary trampolines turn the MMU on in assembly, before any Rust, under an early map of the image, and branch to the high half | done |
| KM3 | The port entry switches to the kernel root: text read-and-execute, read-only data and the boot-stack guard read-only, data and heap read/write, none of it writable and executable; `TTBR0_EL1` maps nothing, so address 0 faults | done |
| KM4 | A boot-stack overrun faults on the read-only guard and is reported from a reserve stack | done |
| KM5 | The direct map is the kernel's only way to physical memory and registers: RAM Normal and execute-never, registers Device, the image's own text and read-only data aliased read-only, each gigapage typed once | done |
| KM6 | Every register, firmware buffer and device-visible address is reached through the direct map or translated (`paging::image_phys`, `paging::kernel_phys`), never used as its own physical address | done |
| KM7 | The production boot discovers and types the board before bringing a device up, with translation on throughout | done |
| KM8 | A process root carries nothing of the kernel: the identity window, its masks and `new_identity_gigapages` are deleted | done |
| KM9 | Secondaries and the machine takeover run on the kernel root | done |
| KM10 | Every aarch64 QEMU fixture boots under the new map | done |
| KM11 | Docs and the defect ledger | done |
| KM12 | The Pi 4 image boots on the board: it opens with the arm64 `Image` header the firmware places it by (`plans/OPEN-DEFECTS.md` D1829) | done |

## Design

- **Where the image lives.** `TTBR1_EL1` slot 511 (the top gigapage) holds
  the image. Its link address keeps the load address's offset within its
  gigapage, so one link address, `0xFFFF_FFFF_C020_0000`, serves the QEMU
  `virt` image (loaded at `0x4020_0000`) and the Pi 4 image (loaded at
  `0x20_0000`), each placed there by the arm64 `Image` header the
  trampoline opens the image with: a zero `text_offset` puts it 2 MiB above
  the base of RAM. Both load
  addresses are 2 MiB aligned, so a virtual address and its physical one
  agree modulo 2 MiB and the image can be mapped with 2 MiB blocks. The
  translation is the link-time constant `__kimage_voffset`.
- **Turning translation on.** No Rust runs with the MMU off: Rust code linked
  high holds absolute addresses (vtables, statics naming statics) that mean
  nothing at the load address, and an exclusive on MMU-off memory may never
  complete. The trampoline refuses an image not loaded where it was linked
  to load, reads the device tree's header with Device semantics, and builds
  the early tables in `.early_tables` (an identity root mapping only the
  `.text.idmap` page, tagged with the boot ASID; a high root of 2 MiB
  read/write/execute blocks over the image), invalidating them to the point
  of coherency before and after. It turns translation on from the identity
  page, branches high, and zeroes `.bss` through the cache. A secondary
  crosses the same way, straight onto the kernel root, after invalidating
  by set/way the UNKNOWN cache levels it owns, never one it shares.
- **The kernel root.** Built by the port entry from a static table pool
  (`paging::kernel_root`, over the run mapper the x86_64 port shares,
  `tairix_arch_api::imagemap`), then loaded in place of the early high root
  from the identity page by way of an empty table — a root switch, so no
  live entry is changed and break-before-make never arises. Text is mapped
  with 2 MiB read-and-execute leaves (text fills whole blocks); everything
  else with page-granular leaves only where a block mixes sections.
  `TTBR0_EL1` is loaded with an empty root, so nothing physical is mapped as
  itself and a null dereference faults.
- **The direct map.** `TTBR1_EL1` slots 0–446 map physical gigapages at
  `PHYSMAP_VMA_BASE + pa`, each typed once (`paging::reveal`): RAM Normal
  and execute-never, registers Device-nGnRE, a PCI configuration region
  Device-nGnRnE. The gigapage holding the image is typed RAM by the port
  entry and split so its text, read-only data and early tables are aliased
  read-only. A boot handed no tree types `virt`'s register gigapage too; the
  production boot types the registers it discovers, then the RAM left,
  before it brings a device up.
- **The firmware tree.** Mapped read only, page by page, in a window below
  the top of the image's gigabyte, wherever the firmware put it, and handed
  to `kernel_main` by that address (`paging::firmware_tree`).
- **The boot-stack guard.** Mapped read-only. The EL1 synchronous vector
  checks, with `TPIDRRO_EL0` as its one scratch register, whether the frame
  it is about to push would land in the guard: if so it records the
  interrupted stack pointer as `SP_EL0` and moves to a reserve stack, so the
  overrun's report runs (`fatal-stack-overrun-qemu-aarch64`).
- **What a kernel body is handed.** Every CPU reaches its `kernel_main` or
  secondary entry with FP/SIMD on, the vectors armed and the kernel root
  loaded, so `enable_fp_el1` is the port's own. A test kernel reaches RAM
  through the production direct map (`SPAWN_TABLE_PHYSMAP`) and registers
  through `DeviceWindows`, and hands a device an image static's physical
  address through `paging::image_phys`.
