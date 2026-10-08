# PI.md — Booting TAIRiX on a real Raspberry Pi 4 (BCM2711, aarch64)

This is the staged build plan for taking the `kernel/arch/aarch64` port
from "boots on the QEMU `virt` board" to "boots a real Raspberry Pi 4
(BCM2711) into user mode and, ultimately, the desktop / window manager".

`AGENTS.md` is binding — read it, `PLAN.md`, and `plans/WIRING.md` first.
Every rule in this file is binding too. The continuation prompt for fresh
contexts is this plan.

**Note:** `abi-v1` is *not* frozen, despite what `AGENTS.md` / `PLAN.md`
say — the standing task direction supersedes that language. Changing a
`lib/abi` type today is allowed; it requires regenerating the C header
(`cargo xtask c-header --write`), which the drift guard enforces.

## Ledger

| # | Stage | Status |
|---|---|---|
| **P0** | Pi-4 facts of record (no code) | done |
| **P1** | Pi-4 boot stub, linker script and production aarch64 kernel binary | done |
| **P2** | Board-discovered UART console (PL011 and mini-UART) | done |
| **P3** | GIC-400 from the tree, and the Pi RAM map | done |
| **P4** | Generic timer and a live scheduler on the Pi | done |
| **P5** | SMP bring-up on the Pi (PSCI and spin-table) | done |
| **P6** | Spawn `init` into EL0 on the Pi | done |
| **P7** | VideoCore mailbox and framebuffer, accepted on metal | done |
| **P7b** | Framebuffer boot console: video first, UART fallback | done |
| **P7c** | Display power: the firmware framebuffer's own display service, switched off through the firmware's blank request — host-proven; its metal run remains | in progress |
| **P8** | SD-card storage (EMMC2): UHS-I DDR50 / High Speed negotiation, ADMA2 and PIO; UHS-I DDR50 with ADMA2 accepted on metal | done |
| **P9** | Bootable SD image (`tools/mkimage`) | done |
| **P10** | USB-HID input and the desktop on the Pi | in progress |
| **P11** | Login on the consoles | in progress |
| **P12** | On-board gigabit Ethernet (GENET) | in progress |
| **P13** | The legacy DMA engines (`drivers/dma/bcm2835`, `plans/SOUND.md` SND5) — host-proven; its metal run is SND8's first transfer | in progress |

---

## 0. Scope and binding decisions

1. **One board, the Raspberry Pi 4 (BCM2711).** This plan targets the
   Pi 4 / Pi 400 (BCM2711, Cortex-A72, GIC-400) specifically. The Pi 3
   (BCM2837, no GIC) and Pi 5 (BCM2712, RP1 southbridge) are explicitly
   out of scope here; they reuse this work as a later board port. Each
   board difference that surfaces is recorded, never silently assumed.

2. **HAL-first, never `virt`-vs-Pi `cfg` switches (§17.2 / §2.2).** The
   QEMU `virt` board and the Pi 4 are two *boards* of the **same**
   `aarch64` architecture. Their differences — UART model and base, the
   interrupt-controller base, the RAM/MMIO map, the boot protocol, the
   mailbox/framebuffer — are **runtime board data discovered from the
   device tree**, never `cfg(board = …)` forks of the port (that would be
   the §2.2 duplication / §2.3 bloat this charter forbids, and the
   §17.2 burn-down already moved discovery behind `PlatformDiscovery`).
   The single legitimate per-board artefact is the **boot stub + linker
   script + load address** (the `AGENTS.md` §1 "boot stubs" carve-out for
   architecture-required assembly), because that is fixed before any tree
   is parsed.

3. **Discovery is the contract.** Everything the kernel needs to talk to
   Pi 4 hardware — the `/memory` map, the UART (`compatible`,
   `reg`), the GIC-400 (`reg` for GICD/GICC), the timer PPIs, the mailbox,
   the SD host, the USB host — is read from the Pi 4 device tree through
   the shared `lib/fdt` reader and normalised into `tairix_abi::hwtree`
   by `kernel/arch/aarch64::platform::FdtDiscovery` (§18.2). The MMIO
   bases currently hard-coded as `virt` constants
   (`serial::PL011_BASE`, `gic::{GICD_BASE,GICC_BASE}`) must become
   discovered values threaded from the tree, not compile-time constants.

4. **No hard-coded device list standing in for detection (§18.5).** The
   Pi 4 peripherals autoload through the §18.3 `devmgr` match path against
   driver bind tables, exactly as on `virt`. "It's a Pi, just poke the
   known addresses" is forbidden.

5. **Fail closed, no hacks (§2.1 / §2.9 / §5.4).** No
   `unwrap`/`expect`/`panic!` in production paths, no `unsafe` without a
   `// SAFETY:` block plus a test, no retry-until-it-works bring-up, no
   "boots if you squint" milestones. A primitive the Pi genuinely lacks
   (or that is deferred) is declared honestly, mirroring §0.4 of
   `plans/WIRING.md`.

6. **Two proving grounds, both required.** Every stage that *can* be
   proven in emulation lands a `qemu-system-aarch64 -M raspi4b` (or
   `raspi3b` where the Pi-4 model is unavailable) vertical **in addition
   to** the existing `-M virt` verticals — so the board-discovery path is
   exercised in CI without hardware. Stages that can only be proven on
   metal (real mailbox/HDMI, real SD/USB timing) land a documented
   **hardware bring-up checklist** and a UART-log capture as the
   acceptance artefact, since CI has no Pi attached.

7. **Docs + tests are part of every stage (§7 / §13).** Each stage
   updates `docs/src/platform/aarch64.md` (and adds
   `docs/src/install/raspberry_pi.md` when Stage 8 image work lands),
   plus `PLAN.md` and this file, in the same change. Tests are never
   deferred.

8. **One increment per landing.** Land one complete, fully-gated stage,
   update `PLAN.md` + this file, refresh this plan, then
   start the next.

9. **Metal re-verification is guaranteed, not speculative (operator
   commitment, binding).** The operator re-verifies every applicable
   stage on a real Raspberry Pi 4B as the work proceeds and supplies the
   UART/debug-log (or photo) acceptance artefact between chunks. The
   metal-acceptance step *will* be checked every time it applies — it is
   never assumed, skipped, or treated as optional. Therefore design
   decisions are made for the **most correct, properly-designed,
   senior-review-clean** outcome (`AGENTS.md` §2.6 / §23): security and
   correctness are the floor (§2.1 / §5.4 / §23.1), performance is
   first-class (§2.16), and drivers are generic, modular hardware
   interfaces — from PCI/PCIe bridges down to the individual USB-HID
   interface (keyboard, mouse, storage, scanner, printer, …) and other
   PCI devices (storage, serial, parallel/printer ports, …) — never a
   Pi-4-only special case (the work must equally serve `x86_64`,
   `riscv64`, and other boards: §0.2 / §17.2 / §18). Do **not** trade
   design quality for a smaller blast radius on the assumption metal
   verification is optional: it is not. A chunk touching a live,
   metal-confirmed path lands host-tested **plus** a metal checklist —
   never a hack to dodge a check (§2.1).

---

## 1. Baseline — where the aarch64 port stands today

`kernel/arch/aarch64` is at QEMU-`virt` parity with x86_64 (`plans/WIRING.md`
W6/W7/W17): EL1 boot trampoline with an EL2→EL1 drop (`boot.s`), PL011
console (`serial`), GICv2 (`gic`), generic-timer preemption (`preempt`),
stage-1 MMU (`paging`), `svc` syscall entry, `eret` user entry
(`userentry`), context switch, PSCI `CPU_ON` SMP bring-up (`psci` + `smp`),
per-CPU storage (`percpu_hal`), side-channel + memory-tagging profiles,
and FDT → `hwtree` discovery (`platform` + `fdt`). All of it is proven
under `qemu-system-aarch64 -M virt`.

**What is `virt`-specific (the Pi-4 gap):**

| Concern | Today (`virt`) | Pi 4 (BCM2711) |
| --- | --- | --- |
| Production kernel binary | none — aarch64 boots only via per-test bins; `tairix-kernel/build.rs` wires **only** `x86_64` | a real `aarch64-unknown-none` `tairix-kernel` image is required |
| Boot protocol | QEMU `-kernel <elf>`, aarch64 hand-off `x0 = DTB` at EL2/EL1 | Pi firmware (`start4.elf`) loads `kernel8.img` at `0x80000`, enters EL2, `x0 = DTB` (Pi firmware-supplied) |
| Load address / linker | `0x4020_0000` (`aarch64-virt.ld`) | `0x8_0000` (needs an `aarch64-rpi4.ld`) |
| Console UART | PL011 @ `0x0900_0000` (fixed const) | PL011 @ `0xFE20_1000` *or* mini-UART (AUX) @ `0xFE21_5040`, base discovered |
| Interrupt controller | GICv2 @ GICD `0x0800_0000` / GICC `0x0801_0000` (fixed const) | GIC-400 @ GICD `0xFF84_1000` / GICC `0xFF84_2000`, base discovered |
| RAM base | `0x4000_0000` | `0x0` (low 1 GiB; up to 8 GiB with the `>3GiB` window) |
| Display | virtio-gpu / ramfb | VideoCore mailbox framebuffer → `drivers/display/rpi_hvs` (HVS) |
| Storage | virtio-blk-mmio | EMMC2 SD host controller (`drivers/storage`) |
| Input | virtio-keyboard-mmio | USB HID behind the VL805 xHCI (`drivers/bus/usb/{vl805,xhci}` → `drivers/input/usb_hid`) |
| Image builder | `tools/mkimage` emits `images/tairix-aarch64-rpi.img` (P9) | flash + boot the emitted image on metal |

`drivers/display/rpi_hvs` already exists (HVS layer compositor, mock-host
tested) and consumes an `HvsConfig` the boot capability provides; it has
no hardware vertical yet.

---

## 2. Strategy

The work splits into three arcs:

- **Arc A — Boot to a UART prompt on the `raspi4b` model (P1–P3).**
  Pure-software, fully CI-provable under `qemu-system-aarch64 -M raspi4b`.
  This de-risks the boot protocol, the linker/load address, the console,
  and board discovery without any hardware.
- **Arc B — Reach user mode on the Pi (P4–P6).** Interrupt controller,
  timer, MMU, and the live scheduler over discovered Pi bases; spawn an
  init process. Still mostly `raspi4b`-provable, with a metal checklist.
- **Arc C — Real peripherals + bootable image + desktop (P7–P10).**
  Mailbox/framebuffer, SD, USB-HID, the SD-card image, and finally the
  WM/taskbar on the HVS path. These need real hardware to *fully* prove.
- **Arc D — Multi-user login (P11).** Every installed text console sits
  at a `login:` prompt backed by the `/System/Security/Users` database.
  With a display active the video console is the only console — the UART
  then carries the debug log alone, with no session; on a serial-only
  boot the UART is the console.

Land them in order; each stage's "Done when" gate is binding.

---

## 3. Stages

> Status is the ledger's, above. Keep it and the PLAN.md Stage-3b/Stage-8
> entries in sync.

### P0 — Pi-4 facts of record (no code)

Pin down the BCM2711 numbers this plan depends on, in
`docs/src/platform/aarch64.md` under a new "Raspberry Pi 4 (BCM2711)"
section, so later stages cite one authoritative source (§13, no guessing
per §15.7):

- Low-peripheral MMIO base `0xFE00_0000` (the `0x7E00_0000` VC bus alias
  mapped to ARM physical `0xFE00_0000`); PL011 `+0x20_1000`, AUX mini-UART
  `+0x21_5000`, mailbox `+0x00_B880`, EMMC2 `+0x34_0000`.
- GIC-400: GICD `0xFF84_1000`, GICC `0xFF84_2000`.
- Boot: firmware loads `kernel8.img` at `0x8_0000`, AArch64, enters at
  EL2, `x0` = DTB pointer; `config.txt` knobs (`arm_64bit=1`,
  `kernel=kernel8.img`, `enable_uart=1`, `armstub`).
- RAM layout for the 1/2/4/8 GiB SKUs and the `>3GiB` aliasing window.

**Done when:** the section exists, links cleanly (`cargo xtask
docs-check`), and is referenced by P1+. No source code changes.

### P1 — Pi-4 boot stub + linker script + production aarch64 kernel binary

- Add `kernel/arch/aarch64/link/aarch64-rpi4.ld` (load `0x8_0000`),
  alongside the existing `aarch64-virt.ld`. Two linker scripts is the
  §0.2 boot carve-out, not duplication — they differ only in the origin
  address and a comment.
- Generalise `boot.s` so the EL2→EL1 drop + `.bss` clear + stack setup is
  board-independent (it already is, bar the comment); confirm it works
  from EL2 with the Pi register hand-off. If the Pi enters all 4 cores at
  `_start` (firmware default, no `armstub` spin-table), the stub must park
  secondaries (`MPIDR_EL1` affinity ≠ 0 → `wfe` loop) until PSCI/SMP
  bring-up wants them — fail closed, never race (§2.1).
- Teach `kernel/tairix-kernel/build.rs` + source to build as the
  freestanding **aarch64** production kernel (today `is_freestanding()`
  is hard-coded to `x86_64`). The `freestanding` cfg and the
  boot/panic/serial-sink modules must select the aarch64 boot path and
  linker script by `CARGO_CFG_TARGET_ARCH` — this is build glue, the §17.2
  allow-listed place for target conditionals.
- The binary's `kernel_main(dtb)` wires `Aarch64Arch` into `kernel/core`
  (the single §17.1/§17.2 selection point), mirroring the x86_64 `boot`
  module.

**Done when:** `cargo build -p tairix-kernel --target aarch64-unknown-none`
produces a freestanding ELF that links against `aarch64-rpi4.ld`; a host
unit test covers the new `build.rs` arch/linker selection; no `cfg-check`
/ `deps-check` regressions.

**Landed.** `kernel/arch/aarch64/link/aarch64-rpi4.ld` (origin `0x8_0000`)
sits beside `aarch64-virt.ld`; `boot.s` now parks non-boot CPUs
(`MPIDR_EL1` affinity ≠ 0 → `wfe`) before touching the boot stack, so it
serves both `virt` (PSCI-held secondaries) and the Pi (all-core release).
Both EL1 entry trampolines (`boot.s` `.Lin_el1`, `smp.s`) write the known
MMU-off `SCTLR_EL1` (`paging::SCTLR_MMU_OFF`, ARMv8.0 RES1 bits only,
unit-test-pinned) before the first EL1 data access, and
`AddressSpace::switch` installs the whole known `paging::SCTLR_MMU_ON`
(RES1 + M + C + I, after `ic iallu`) rather than OR-ing `M` into the
live register: `SCTLR_EL1` is architecturally UNKNOWN at first EL1 entry
on real silicon (EL2 hand-off and PSCI `CPU_ON` alike), and a carried
UNKNOWN `WXN`/`EE` bit hung the metal Pi 4 at the MMU switch while QEMU
(benign reset values) stayed green.
The same UNKNOWN-reset-state rule holds one level up: the Pi firmware
stub sets only `SCTLR_EL2` and `CPUECTLR_EL1.SMPEN`, so `boot.s`'s EL2
path writes every EL2 control register **whole** with the
unit-test-pinned hand-off values in `tairix_arch_aarch64::el2`
(`HCR_EL2 = RW`, `CNTHCTL_EL2 = EL1PCTEN|EL1PCEN`, `CPTR_EL2 =` RES1,
`MDCR_EL2 = 0`, `VPIDR/VMPIDR` mirrored) — an UNKNOWN `HCR_EL2.TVM`
traps EL1's first `MAIR/TCR/TTBR/SCTLR` write into vector-less EL2,
hanging the metal Pi 4B silently at the MMU switch while QEMU stayed
green.
The boot identity window is bounded to backed memory: gigapages are mapped
only when named by the configured Device mask or the configured
kernel-extent mask (`paging::configure_kernel_gigapages` /
`gigapage_mask_from_extents`, default all — for host tests and the QEMU
integration kernels), and
every other L1 slot is left *invalid* so speculation cannot reach
unbacked bus windows (the metal Pi 4B wedged at the MMU switch exactly
there while QEMU stayed green). The boot path derives the kernel-extent
mask once, pre-MMU, from the kernel image extent, the firmware DTB blob,
and the scan-out surface — the things the kernel addresses *physically*.
It is not widened over discovered RAM: an allocator frame is reached
through the direct physical map in the `TTBR1_EL1` regime
(`plans/OPEN-DEFECTS.md` D56), so a process root carries no mapping of RAM
in the half user code addresses and the window does not grow with the
machine. The switch itself is
real-silicon-honest: the just-written tables are swept to PoC
(`PageTablePool::clean_invalidate_to_poc`, `dc civac` per
`CTR_EL0`-decoded line — MMU-off stores bypass the cache but the walker
reads back cacheable, so firmware cache residue would shadow the
descriptors) and `switch` orders those Device-nGnRnE stores with a
full-system `dsb sy` before enabling translation. The pool's allocation
counter is translation-aware: MMU-off it advances by plain load + store
(LDXR/STXR exclusives never succeed on the BCM2711's MMU-off
Device-nGnRnE accesses, so a `fetch_add` spins forever on metal while
QEMU stays green; MMU-off allocation is pre-SMP boot-CPU-only by
construction) and reverts to `fetch_add` once translation is live. The
permanent park root follows the same boundary: `program_stage1_translation`
returns a private live-translation witness after `SCTLR_EL1` + `isb`, and
only that witness performs the set-once atomic publication, so the MMU-off
activation prefix contains no exclusive retry loop.
With those fixes the metal Pi 4B (8 GB) boots the production pipeline
end to end **through user space**: the
stage-p1 boot line, the kernel-core phase log, PID 1 `init`'s EL0 entry
and banner, and the spawn/wait/exit supervision cycle all render on
both UART0 and the HDMI console (every spawned space's identity window
is derived from the configured Device/kernel-extent gigapage masks,
`paging::configured_identity_gigapages` — P6c-3/P6d — since the former
hard-coded 2 GiB `virt` window dropped the Pi's gigapage-3 UART/GIC
from PID 1's root). The session formerly read end-of-input at its first
prompt (the metal had nothing queued in the PL011 RX FIFO) and exited,
exhausting `init`'s crash-loop budget; the kernel-core
`BlockingConsoleRead` backing (P6e-2) now parks the reader until UART
RX delivers bytes, so the metal session waits at `tairix$ ` for the
user to type.
`kernel/tairix-kernel/build.rs` factors its pure selection logic into
`src/build_support.rs` (host-unit-tested) and emits a build-glue
`kernel_isa` cfg + the per-board linker script — no `cfg(target_arch)` in
the crate body (cfg-check clean). The crate's x86_64 boot pipeline is
gated `kernel_isa="x86_64"`; the new freestanding `boot_aarch64` module +
the aarch64 `kernel_main(dtb)` in `main.rs` construct `Aarch64Arch` (the
§17 selection point), bring up the console, log a boot line, and park
fail-closed. `cargo build -p tairix-kernel --target aarch64-unknown-none`
links a freestanding ELF entered at `0x8_0000`. The discovery-fed
`kernel_core::kernel_main` hand-off (a real memory map / IRQ routing) is
deliberately staged to P2/P3 — fabricating a hardware map would violate
§18.5, and the `-M raspi4b` runtime vertical that proves it cannot pass
until P2's console discovery lands. The `CPACR_EL1.FPEN` enable is now a
single `tairix_arch_aarch64::enable_fp_el1()` helper (§2.2), adopted by
the production binary and the existing aarch64 verticals.

### P2 — Board-discovered UART console (PL011 + mini-UART)

- The fixed `serial::PL011_BASE` constant is gone: the console MMIO base +
  register model now live in a new host-testable
  `tairix_arch_aarch64::console` module (an atomic `(base, model)` pair,
  default = the `virt` PL011 base) that the freestanding `serial` sink
  transmits through on every byte. `console::configure_from_fdt` reads the
  base + model from the device tree, decoding the node's `reg` with its
  parent bus's cell counts and translating it through the ancestor
  buses' `ranges` (the shared `fdt::scan_translated` /
  `fdt::translated_reg` machinery, §2.2) — the real Pi tree's UARTs sit
  under `/soc` with one-cell *bus* `reg` values (`0x7E20_1000`) remapped
  to CPU-physical space (`0xFE20_1000`); an untranslatable node is
  skipped, never poked at its raw bus address (§2.9). The
  `raspi_like_arm` fixture mirrors that real shape (root 2+1 cells,
  `/soc` simple-bus with 1+1 cells and the three BCM2711 `ranges`,
  bus-address parameters). The BCM2835 **AUX mini-UART** is a
  second `ConsoleModel` behind the same `tairix_log::Sink` seam (its own
  `AUX_MU_IO`/`AUX_MU_LSR` register offsets + opposite-sense TX-ready bit),
  selected by the `compatible` string — `brcm,bcm2835-aux-uart` vs
  `arm,pl011`. One console abstraction, two register backends (§2.2).
- `platform::FdtDiscovery` emits a `serial`-class `HwNode` carrying the
  discovered UART `compatible` bind key + its `reg` as a capability-gated
  MMIO resource, so the console base is discovered, not assumed.
- `boot_aarch64::boot` calls `console::configure_from_fdt` from the `x0`
  DTB before its first log line (MMU-off-safe: the `lib/fdt` reader is
  byte-wise, no multi-byte Device-memory load — W17).
- Discovery alone leaves real silicon silent: `uart_init::init_from_fdt`
  runs right after it, muxing GPIO 14/15 to the PL011 (`GPFSEL1` ALT0 +
  pull-none, gated on a discovered `brcm,bcm2711-gpio` node) and
  programming the PL011 line (TRM order, 115200 8N1 + FIFOs from the
  `config.txt`-pinned 48 MHz `init_uart_clock`) — QEMU's powered-up
  PL011 masked the omission; the metal Pi 4B booted with a permanently
  silent UART0 without it. Pure, host-tested register arithmetic; the
  freestanding layer is volatile MMIO only (§2.2).
- The real firmware tree is a regression input: the
  `real_dtb_probe` integration test runs the production discovery walks
  (console, GPIO, mailbox, memory) over the pinned
  `bcm2711-rpi-4-b.dtb` when the firmware cache is present (skips
  honestly when not fetched).

**Done when:** host unit tests cover the mini-UART/PL011 register
encoders and the `compatible`-string console selection + the discovered
`serial` `HwNode` (against the new `tairix_fdt` `raspi_like_arm` fixture);
and the new `tests/integration/uart_console_qemu_aarch64` vertical boots
the `virt` board, **poisons** the console base, then proves
`configure_from_fdt` overwrites it with the base read from the firmware
device tree and that writes reach that base (it prints two lines over the
*discovered* console before the semihosting PASS finisher). All existing
`virt` aarch64 verticals stay green.

**Emulation gap (honest, not faked — §2.1):** the vertical runs on `-M
virt`, **not** a Pi board, because QEMU's `raspi*` models do not model the
Raspberry Pi GPU-firmware DTB hand-off — they enter an ELF `-kernel` with
`x0 = 0` (verified by GDB on `raspi3b`), and QEMU 8.2.2 has no `raspi4b`
at all. The `virt` board *does* pass its generated tree (which carries a
real `arm,pl011` node), so the runtime discover→configure→print path is
CI-proven there against a genuine firmware tree (the canonical `virt` DTB,
dumped + embedded at build time since `-kernel <ELF>` passes no pointer).
The Pi's *specific* console base + the mini-UART register layout are
covered by the host unit tests against the `raspi_like_arm` fixture, and
printing on real Pi PL011 silicon is an on-metal acceptance item for the
Arc C peripheral stages (where the real firmware populates `x0`).

### P3 — GIC-400 from the tree + Pi RAM map

- The GICv2 driver register layout already matches GIC-400; thread the
  GICD/GICC bases from `FdtDiscovery` instead of the `virt` constants
  `gic::{GICD_BASE,GICC_BASE}`. Emit a GIC `HwNode` from discovery.
- Generalise the early memory map: `FdtDiscovery::first_memory_region`
  already reads `/memory`; confirm it yields the Pi's `0x0`-based RAM and
  feed it to `kernel/mem` so the allocator/page tables cover real Pi RAM,
  not the `virt` `0x4000_0000` assumption.

**Done when:** the existing `ipi_smp` / `sched_drive` aarch64 verticals
(or a new `-M raspi4b` analogue) run over **discovered** GIC bases and
Pi RAM, GICv2 IRQs + SGIs deliver, and `cargo xtask cfg-check` confirms no
board constants leaked outside the arch crate.

**Landed.** The fixed `gic::{GICD_BASE,GICC_BASE}` constants are gone:
`gic` now holds the active `(gicd, gicc)` pair as an atomic (default = the
`virt` GICv2 `0x0800_0000`/`0x0801_0000`) that the freestanding
`VolatileGicMmio` reads on every access, with `gic::find_gic` /
`configure_from_fdt` over `lib/fdt` selecting the first GICv2-class
controller (`arm,gic-400`, `arm,cortex-a15-gic`, …) and reading its
first two `reg` regions (GICD, GICC), each decoded with the parent
bus's cell counts and translated through the ancestor buses' `ranges`
(`fdt::translated_reg`) — the real Pi tree's GIC-400 sits under `/soc`
with one-cell bus `reg` values (`0x4004_1000` → `0xFF84_1000`).
`platform::FdtDiscovery` emits an `InterruptController` `HwNode`
carrying the discovered `compatible` bind key + every register window
(`HwDeviceClass::InterruptController` already existed — no ABI change).
The `lib/fdt` `virt_like_arm` / `raspi_like_arm` fixtures carry a GIC
node (virt `arm,cortex-a15-gic` at the root; Pi `arm,gic-400` under
`/soc` with the real four bus-address regions); host tests cover the
GIC discovery, the `HwNode`, and the fail-closed no-GIC path.
`boot_aarch64` parses the `x0` DTB and points the console **and** the
GIC driver at their discovered bases MMU-off, then reads the `/memory`
window once the MMU is on, logging `gic_discovered` / `ram_discovered`
(the live allocator + `kernel_core::kernel_main` hand-off over that map is
deliberately staged to P4/P6 — a hard-coded map would violate §18.5).
**Runtime proof on `-M virt`:** the `ipi_smp_qemu_aarch64` vertical now
**poisons** the GIC base, rediscovers it from the embedded `virt` DTB
before `gic::init`, and asserts it moved to the `virt` GICv2 base, so the
delivered IPI exercises the *discovered* base (`irq_qemu_aarch64` likewise
reads `gic::current()`); both PASS. `cargo xtask cfg-check` stays clean
(no board constant leaked outside the arch crate). The Pi's specific
GIC-400 bases are host-unit-tested + an on-metal item (no `raspi4b` in
QEMU — the same gap as P2).

### P4 — Generic timer + live scheduler on the Pi

- Reuse the W7 live-scheduler wiring (`preempt` + `context` + `mlfq`) over
  the discovered GIC-400 + Pi generic-timer PPI. The Pi's `CNTFRQ_EL0` and
  timer PPIs come from the tree (`timer_ppi` already reads them); confirm
  the Pi's 54 MHz crystal / `CNTFRQ` is honoured rather than the `virt`
  value.

**Done when:** a `-M raspi4b` `sched_drive` vertical drives the live
`Scheduler` ≥ 20 timer ticks + ≥ 1 IPI tick, exactly as the `virt` one
does (`plans/WIRING.md` W7).

**Landed.** The generic-timer counter rate is now a *discovered* board
fact rather than the raw register: `fdt::timer_clock_frequency` reads the
`/timer` node's optional `clock-frequency` override (the standard
`arm,armv?-timer` binding the firmware carries when `CNTFRQ_EL0` is
mis-programmed) and the pure, host-tested `fdt::effective_timer_hz`
selects it over `CNTFRQ_EL0` when present and non-zero, else falls back to
the register (a zero override is treated as absent — never a 0 Hz timer,
§2.9). The freestanding `kernel_arch::timer_frequency_hz(&fdt)` composes
the two; `boot_aarch64` seeds the `Aarch64Arch` clock/preempt interval
from it and logs `timer_hz_from_tree` plus the resolved rate itself as
`timer_hz_hex`. That rate also drives every `kernel_arch::busy_delay_us`
settle the in-kernel bring-up uses. The metal capture read
`timer_hz_from_tree=false timer_hz_hex=0x337_f980` — exactly the Pi 4's
54 MHz crystal — so `CNTFRQ_EL0` is correctly programmed and a
mis-programmed-rate over-wait is **ruled out** as the cause of the P10
multi-second USB bring-up pause: a measurement of the bring-up found
≈14.3 s of real time against ≈356 ms of requested delay at the correct
54 MHz, so the counter is sound and the seconds were code-side.
Two diagnostics localise it: `SerialSink` prefixes every line with a
monotonic `CNTPCT_EL0`-derived `[<secs>.<millis>]` stamp (`kernel_arch::uptime_ms`,
so a capture reads the real wall time between any two lines), and `build.rs`
emits `KERNEL_BUILD_ID` (git short hash + `+dirty` + `SOURCE_DATE_EPOCH`-aware
build epoch, §19.3), logged as `build_id` on the `4097` line so a capture
proves which build is running. That stamp is regenerated on **every** build:
`build.rs` declares no narrow `rerun-if-changed` and always re-runs (a metal
reflash therefore always shows a fresh epoch / current `+dirty` hash), because
the old narrow git-only rerun inputs left the id stale through a dirty-tree
edit so a reflash reported an old id for new code (`SOURCE_DATE_EPOCH` still
pins it for a reproducible build, §19.3; the embedded `init`/driver fixtures
are re-validated by the same always-run, their `build-std` relink gated on an
actual `Run.ld` change). The timestamped capture (with `build_id`
confirming the current image) was **decisive** and corrected the earlier
un-timestamped guess: the caps-readiness wait (`4108`→`4109`) is only
~0.35 s — the `wait_for_caps_ready` *elapsed-wall-time* bound
(`CAPS_READY_BUDGET_US`≈256 ms via `Delay::now_us`, retained as a §2.16
defence) works, and `4109 polls_hex=0x100` is 256 *fast* reads (the BCM2711
master-abort returns the `dead_dead` poison in ~1.3 ms, not the ~54 ms first
inferred) — so the caps loop is **not** the pause. The ~14 s is almost
entirely inside `BrcmPcieRc::bring_up`: ~11.2 s between the RC-register-window
map (`4105 phys_base=fd50_0000`) and `4101 link trained`, with no log line
between, while bring_up's coded delays total only ~hundreds of ms and its
reads target the RC's own (fast) register block. The `4117` per-phase split
(`BringUpTiming` from the `Delay` clock, host-tested) then pinned the ~11 s
to the reset phase, then the reset sub-spans pinned it to the **first access
to the MISC register block** (`0x4xxx`). Early experiments that powered the SerDes or read link status before
cycling the bridge reset stalled identically — *every* early MISC
access master-aborts — refuting the SerDes-IDDQ theory. The
real gate is the controller reset — the BCM2711 holds the core off until the
always-accessible RGR1 bridge `sw_init` reset (`0x9210`) is cycled, which is
why the **same** `MISC_PCIE_STATUS` read costs ~8 µs in the config phase (the
reset having run by then) yet ~10.8 s before it. The BCM2711 PCIe bring-up sequence
never touches a MISC register before cycling the bridge reset.
**Fixed (host-proven; confirmed gone on metal):**
`BrcmPcieRc::reset_controller` cycles **only** the always-accessible RGR1
bridge `sw_init` reset (`0x9210`) — bringing the core and its MISC block
online — then lets the core and MISC block settle; the
`4117` capture confirmed `reset_swinit_us`/`reset_settle_us` collapse to
microseconds and the ~14 s pause is gone. The gentlest no-touch-probe
bring-up does **not** re-assert a fundamental reset or toggle the SerDes
`IDDQ` (either could drop the VL805 firmware the previous boot stage
loaded over the power-on link); `PERST#` is left as the handoff left it
and `train_link` deasserts it (the single `PERST#`-deassert edge that
re-triggers any `VideoCore` VL805 firmware reload
(see the firmware item below). (The Pi UART is the SoC's
own PL011/mini-UART — no path to the PCIe/VL805 — so logging cannot perturb
the controller.) `timer_clock_frequency` matches the
timer node through the shared `Fdt::nodes` early-returning walk (the same
byte-safe traversal `gic::configure_from_fdt` uses, §2.2) — **not** the
whole-tree `Fdt::property`/`walk` scan, which faults under the verticals'
MMU-off boot when the compiler widens the byte reads; reaching only the
matched node's own properties keeps discovery safe MMU-off. **Runtime
proof on `-M virt`:** the `sched_drive_qemu_aarch64` vertical now derives
the tick interval from `timer_frequency_hz(&fdt)` over the embedded `virt`
DTB and **poisons** the GIC base, rediscovering it (`configure_from_fdt`)
before `gic::init`, so the ≥ 20 timer ticks + ≥ 1 IPI that drive the live
`Scheduler` run over the *discovered* GIC base and frequency. The `virt`
tree omits `clock-frequency`, so the runtime path exercises the register
fallback while the override branch is host-unit-tested; honouring the Pi's
real 54 MHz crystal is an on-metal item (no `-M raspi4b` in QEMU — the
same gap as P2/P3). `cargo xtask cfg-check` stays clean.

### P5 — SMP bring-up on the Pi (PSCI vs spin-table)

- The board's start mechanism is discovered, never assumed: PSCI over
  the `/psci`-declared conduit when the node exists, else the
  Devicetree spin-table release the Pi 4's stock firmware declares
  (`enable-method = "spin-table"` + per-cpu `cpu-release-addr`), else
  fail closed to a single-CPU boot.

**Landed — both mechanisms.** The PSCI conduit is a *discovered* board
fact end to end. `fdt::psci_method` rides the shared `Fdt::nodes`
early-return walk (matching the `/psci` node by an `arm,psci`
`compatible` prefix and reading `method` from that node only), the same
byte-safe traversal `gic::configure_from_fdt` /
`fdt::timer_clock_frequency` use (§2.2) — safe on the MMU-off bring-up
path where a full-tree scan faults. `boot_aarch64` reads the mechanism
from the `x0` DTB — the `/psci` conduit when declared, else each
`/cpus/cpu@*` node's spin-table release word
(`tairix_fdt::CpuNode::spin_table_release`, dense-aligned by
`cpu_topology::align_release_addrs`) — installs it via
`Aarch64Arch::with_secondary_start`, and logs `smp_start_method`
(`psci`/`spin_table`/`none`); a tree declaring neither leaves the
mechanism unset, so the `SecondaryBringup` HAL fails closed
(`SmpError::NotReady`) rather than assuming one. **Runtime proof on
`-M virt`:** `ipi_smp_qemu_aarch64` *discovers* the conduit from the
embedded `virt` tree, asserts it is the board's `hvc`, fails closed
otherwise, and starts the secondary core + delivers a directed SGI over
*that* discovered conduit. Host tests cover the conduit read from both
the `virt` (`hvc`) and `raspi` (`smc`) fixtures, the fail-closed
no-`/psci` path, and the spin-table discovery (positive, wrong-method,
zero/malformed-address). The **spin-table release path is the metal
Pi 4 production path** (its stock firmware declares no `/psci`, which
left every secondary `smp_secondary_entry_not_installed`-dead before
this landed): `smp::start_secondary_spintable` writes the argument-free
`_start_secondary_spintable_aarch64` trampoline address to both the
firmware `cpu-release-addr` word and the kernel's own
`SECONDARY_KERNEL_RELEASE` word (the `_start` park loop now polls it —
formerly an unreleasable `wfe` loop), publishes the released core's
masked affinity in `SECONDARY_KERNEL_RELEASE_TARGET`, sweeps all three
to PoC, and `sev`s;
the trampoline drops from EL2 via the shared `_el2_establish_and_drop`
(factored out of `_start`, §2.2), recovers its dense id from the
published affinity table (`smp::register_secondary_affinities`, swept
to PoC by `prepare_secondary_bringup`), and joins the common PSCI
trampoline path; an undescribed affinity parks fail-closed. The
**production multi-core bring-up rides the discovered mechanism end to
end**: `boot_aarch64` sizes every per-CPU backing from the validated
`/cpus` dense map, `kernel_core::kernel_main` starts each secondary
after `BootCompleted` (audited
`SecondaryCpuStarted`/`StartFailed`/`Online`), and each core adopts the
boot translation and joins the shared kernel dispatch loop — proven by
the `-smp 4` `kernel-arch-boot-aarch64` vertical (see
`docs/src/platform/aarch64.md`). No `-M raspi4b` in the pinned QEMU, so
the end-to-end spin-table release is an on-metal acceptance item (the
same gap as P2/P3/P4); `cargo xtask cfg-check` stays clean.

Secondaries are brought up **serialised behind a per-core online
acknowledgement barrier** (`kernel_core::init::start_secondaries` +
`smp::SecondaryDispatch::mark_online`): the boot CPU releases one
secondary, waits (bounded via `monotonic_ns`, fail-loud `no_online_ack`)
for it to publish the online edge from `run_secondary` — set only after
that core has adopted the kernel translation regime and armed its per-CPU
interrupt state — then releases the next, and only returns to spawn PID 1
once all have checked in. This is a correctness requirement on real
hardware: the last-released core must finish bring-up before the boot CPU
mutates shared kernel state, or it faults mid-adopt (a cache/coherency
hazard cacheless QEMU never shows, seen on metal as the highest dense id
deterministically never coming online — present in the topology, zero
context switches).

Secondaries are **also** serialised against each other by a **per-core
release gate** on `SECONDARY_KERNEL_RELEASE_TARGET` (a `sev` wakes every
parked core at once — the kernel release word is shared and a firmware
spin-table `sev` likewise wakes every core still in the stub — so the
release must name exactly one core): `start_secondary_spintable`
publishes the affinity of the single core being released (swept to PoC
before the `sev`), and a woken core proceeds only when that target equals
its own masked affinity — the rest re-park. The gate is enforced at
**both** convergence points, against the one shared
`smp::release_gate_open` predicate: the `_start` park loop **and** the
`_start_secondary_spintable_aarch64` trampoline (the kernel never assumes
the firmware `cpu-release-addr` channel is per-core). The gate predicate
and its edge cases are host-tested (`smp_tests`); the parked-core polling
is assembly.

**Root cause (metal): a secondary's UNKNOWN power-on caches were never
invalidated before its data cache was enabled.** The cpu-labelled
beacons localised the death to `adopt_boot_translation` (core printed
`reached rust entry`/`fp enabled` then stopped before `mmu adopted`),
and the per-core state dump proved the inputs were identical and correct
across cores (same boot root, same MMU-off `SCTLR_EL1`, stacks differing
only by the 64 KiB stride). The failure was a **race**, not bad state:
deterministic-then-intermittent (~50/50) on the highest/last-released
core. On AArch64 a core's cache contents are architecturally UNKNOWN at
power-on; a freshly-released secondary can hold stale lines over the very
physical addresses TAIRiX now uses for the boot page tables and that
core's stack. TAIRiX enabled `SCTLR_EL1.C` on the secondary
(`program_stage1_translation`) **without first invalidating its local
D-cache**, so the first cacheable access after the MMU came on — the
translation-table walk or a stack access — intermittently read a stale
cache line shadowing DRAM and the core faulted with no vectors installed.
The boot CPU never hit this because firmware hands it clean caches. (The
earlier serialisation-gate and stack-mapping-coherency theories were both
disproven: core 3 entered Rust cleanly with a correct, PoC-coherent
`.bss`/low-RAM stack, and the boot CPU only spin-waits during bring-up.)

**Fix:** two complementary defences make the boot-root hand-off coherent
across the MMU-off/cacheable boundary. (1) The boot CPU publishes the
park root (`publish_park_root`) with a cacheable store but a secondary
reads it MMU-off, non-cacheably from DRAM, in `adopt_boot_translation`;
publication therefore cleans the park-root word to the point of coherency
(`dc civac`), or the store lingers in the boot CPU's write-back cache,
every secondary reads a stale zero, and each parks fail-closed with "no
boot root" (the observed on-metal symptom: all secondaries report "no
boot root" and the later release gets `no_online_ack`). (2)
`adopt_boot_translation` invalidates the calling core's local data cache
to the Level of Coherence (`paging`'s set/way
`_invalidate_local_dcache_to_poc`, `dc isw`) **before**
`program_stage1_translation` enables the MMU+caches. It is *invalidate*,
never clean: the stale lines hold no live data this core produced (every
prior access was MMU-off Device-nGnRnE, never cached), and cleaning would
write their garbage back over the live tables. Both are secondary-path
concerns — the boot CPU's `AddressSpace::switch` is otherwise unaffected —
so neither perturbs the single-CPU QEMU boot and both are safe on QEMU's
`cortex-a72` model. The end-to-end release remains on-metal-only (no
`-M raspi4b` in the pinned QEMU); acceptance is all four
`SecondaryCpuOnline` lines with core 3 scheduling across repeated boots.

### P6 — Spawn `init` into EL0 on the Pi

The user-mode milestone is the first time the *production* kernel reaches
EL0 on any arch (today only per-test fixtures do, via `spawn_and_enter`),
and the standing direction is to do it *properly*: `init` is a real Rust
program that parses a startup config and writes its first line to the
**console** — the detected framebuffer if any, else the first discovered
UART. That console line needs a real syscall (there was none), and
`init` "starting the user in a shell" needs a userland process-spawn
syscall (there is none). So P6 is staged into chunks, each landed green
on its own before the next.

- **P6a — `console_write` `abi-v1` syscall `[x]`.** New syscall number 11
  + `CAP_CONSOLE_WRITE` (id 18), the `SyscallSpec` row, the
  `kernel/syscall` dispatch arm + recomputed `SYSCALL_TABLE_HASH`, the
  `kernel/core` `ConsoleWrite` seam (boot installs the device — framebuffer
  else first UART — defaulting to a fail-closed `NULL_CONSOLE` →
  `NotImplemented`) + the copy-in handler, the `tairix_sys_console_write`
  C stub, and the regenerated C header. `SYSCALL_NAME_MAX` bumped 12→13
  to fit the name. All host tests + `abi-check` + `c-header` green.
- **P6b — `tairix-init` becomes a real program `[x]`.** The `tairix-init`
  package now builds the `init` bundle's `Run` entry-point binary
  (`src/run.rs`, `AGENTS.md` §16.5) as a **pure-Rust** program. TAIRiX is
  Rust-only (`AGENTS.md` §1), so it links the new pure-Rust userland runtime
  `lib/rt` (`tairix-rt`) — **never** the C ABI (`crt0` + `abi-sys`), which
  exists solely for non-Rust programs (`AGENTS.md` §16.4). `tairix-rt`
  provides `_start`, the §19.2 stack canary, the panic handler, and idiomatic
  syscall wrappers; `tairix_rt::entry!` names the program's safe `main() ->
  i32`, which parses the compiled-in startup config and writes its first
  banner line through the P6a `console_write` syscall, the runtime routing the
  return through `exit`. Both `tairix-rt` and the C ABI reach the kernel
  through the **one** shared trap primitive `lib/abi-trap` (`tairix-abi-trap`,
  the §1 syscall/svc/ecall carve-out), so the trap assembly is not duplicated
  (`AGENTS.md` §2.2). The **startup config** (`src/startup.rs`) is a tiny,
  allocation-free, host-tested text format with two required directives —
  `console` and `session <absolute-path>` — that fails closed on an
  unknown/duplicated directive, a wrong/missing argument, a non-absolute
  path, an over-long config, or an omitted directive (`AGENTS.md` §2.9,
  §19.5). The program links **only** the runtime and its own parser — never
  the orchestrator library, whose `alloc`+crypto chain would be §2.3 bloat —
  so the parser lives beside the binary and the shipped image carries no
  crypto and **no `unsafe`** (verified: the linked aarch64 PIE exports
  `_start`/`__tairix_rt_main`/`rust_rt_start` and the mangled
  `tairix_rt::console_write`, with zero `tairix_sys_*` and zero crypto symbols).
  A self-contained `build.rs` sets the `freestanding` cfg from `target_os`
  only (no `target_arch`, so `cfg-check` stays clean; no dependency on the
  tests harness, so §17.4 layering stays clean); `Run.ld` mirrors the proven
  PIE link layout — the one shared `lib/rt/Run.ld` the userland runtimes
  link. Host DoD green (3 new
  `tairix-rt` tests + 10 startup-config tests + the freestanding aarch64 build
  via `build-std=core,alloc,compiler_builtins`). The end-to-end EL0 spawn of
  this binary is P6c.
- **P6c — production boot reaches EL0 `[x]`.** Wire the freestanding
  aarch64 `tairix-kernel` boot path through `kernel/{mem,ipc,sec,syscall}`
  over the discovered `/memory` map, install the console (discovered
  framebuffer else first UART) via `with_console`, embed the `init` rxe,
  and spawn PID 1 into EL0 via the `userentry` `eret` path. Add a `-M virt`
  vertical asserting the EL0 transition + the `init` banner on the
  discovered UART. This is the largest P6 step — it stands up the aarch64
  production runtime *and* the OS's first production-boot→EL0 spawn (no arch
  reaches user mode from `kernel_main` today; `kernel_core::kernel_main`
  halts after `BootCompleted`) — so it is landed in sub-increments, each
  green over the whole project on its own:
  - **P6c-1 — discovered `/memory` → `BootMemoryMap` `[x]`.** The aarch64
    boot path turns the firmware-discovered RAM window + the linker
    `__kernel_end` into the canonical two-region physical map the live
    allocator hand-off consumes (reserve `[ram_base, __kernel_end)`,
    page-align the rest usable), the riscv64 boot pipeline's
    `build_memory_map` analogue. The bounds/overflow arithmetic lives in a
    host-tested `tairix_kernel::mem_map` module (8 unit tests; gated to the
    aarch64 build and `cargo test` so it is never dead code, §2.3) because
    the bare-metal `boot_aarch64` cannot be host-compiled. The boot audit
    line now records `mem_map_built` / `mem_map_status` /
    `usable_bytes_hex` / `reserved_bytes_hex`, failing closed to a status
    string (never a panic, §2.9) on an absent or malformed window.
  - **P6c-2 — MMU + `kernel_main` hand-off `[x]`.** `boot_aarch64`
    discovers the console + GIC bases MMU-off (early-return walks),
    derives the identity map's Device gigapage mask from them
    (`paging::identity_device_mask` — discovered MMIO gigapages Device,
    the kernel image's own gigapages Normal/executable; `virt`: GiB 0
    Device, Pi 4: GiB 3 Device with the kernel at `0x8_0000` in a Normal
    GiB 0), installs it via `paging::configure_device_gigapages`, then
    enables the stage-1 identity MMU
    (`AddressSpace::new_identity_gigapages` over a static boot
    `PageTablePool`, 512×1 GiB, then `switch`) and installs the EL1
    vectors *before* any further work, so the `kernel_core`
    allocator/scheduler atomics run on Normal memory and the full-tree
    `first_memory_region` FDT walk is MMU-on-safe (the §watch-out
    hazard). The boot audit line records `device_gigapages_hex`
    (`virt`: `0x1`; Pi 4: `0x8`). It adds the local `Aarch64BinArch`
    `KernelArch` wrapper (orphan-rule sibling of the x86_64 `BinArch` /
    riscv64 `RiscvBinArch`), a slot-based aarch64 `production_dispatch` +
    `DISPATCH_SLOT` (the shared arch-neutral frame-read / errno-encode /
    slot-forward logic factored into a host-tested `dispatch_core` module,
    §2.2), a `UartConsole` `ConsoleWrite` over the discovered UART
    (`serial::write_console_bytes`), and hands a validated `BootInfo`
    (`.with_console(&UART_CONSOLE)`) to `kernel_core::kernel_main` — so the
    aarch64 production kernel reaches `BootCompleted` like x86_64/riscv64,
    or parks fail-closed (§2.9) on an unsound hand-off. `kernel/core` grew
    a `BootInfo.console` field + `with_console` builder (default
    fail-closed `NULL_CONSOLE`) threaded into `KernelDispatchHook::new`. The
    `kernel_arch_boot_aarch64` vertical now boots the *production* pipeline
    to `AuditEvent::BootCompleted` on `-M virt` (embedded `virt` DTB; QEMU
    passes no `x0`). `boot.rs::main` passes `&SERIAL_SINK` as both sinks.
  - **P6c-3 — embed `init` rxe + spawn PID 1 into EL0 `[x]`.** The
    `tairix-kernel` build script compiles the pure-Rust `tairix-init-run`
    `Run` binary PIE (the one shared `lib/rt/Run.ld`) and converts the linked
    ELF to an
    embedded `rxe` blob with `tairix_itest_harness::elf2rxe` (stamped with
    the kernel's `SYSCALL_TABLE_HASH`, biased to 64 GiB) — the same path the
    cc3/spawn fixtures use (§2.2; host-only build glue, TAIRiX stays
    Rust-only §1). `kernel/core` gained an arch-neutral PID-1 spawn seam:
    `BootInfo.init: Option<&dyn InitSpawn>` + `with_init`, invoked by
    `kernel_main` after `BootCompleted`; the object-safe `InitSpawnCtx`
    (`frames`/`audit`/`admit_init`) lets the arch seam build the image
    (`spawn_image` — the new authorise+build+`ProcessSpawned`-audit half of
    `spawn_and_enter`, no enter) while the core registers PID 1 with the
    scheduler + capability table and dispatches it. The aarch64
    `init_spawn` seam builds an identity user address space whose window
    is derived from the configured Device/RAM gigapage masks
    (`paging::configured_identity_gigapages` — 2 GiB on `virt`, 4 GiB on
    the Pi 4, whose UART/GIC live in gigapage 3; the 64 GiB bias avoids
    the gigapage collision), parses the embedded `rxe`, and
    boxes the `userentry` `eret` as the scheduler task body; the body runs
    under `step` so the per-CPU `current_task` is set when `init`'s first
    `svc` traps back. PID 1 runs as uid 0 with `{CAP_CONSOLE_WRITE}`. The
    `spawn_init_qemu_aarch64` `-M virt` vertical asserts the EL0 transition:
    `ProcessSpawned` (4030) → `SyscallInvoked` (5000, `init`'s audited
    `exit`) → semihosting PASS. **Locking fix:** the production
    `KernelDispatchHook` now snapshots the caller's caps and drops the read
    guard before dispatch, so the caps-mutating handlers (`exit`,
    `cap_delegate`, `cap_revoke` — all take `caps.write()`) no longer
    self-deadlock the writer-preference `RwLock` (a latent bug, since no
    arch reached EL0 through the real hook before).
  - **P6c-3 follow-up — registry-storable address space; the `init` banner
    prints `[x]`.** An arch `AddressSpace<P>` is `!Sync` (owns a `&'static
    mut` root + a non-`Sync` page-table source), so it could not be stored
    in the `Send+Sync`, lock-shared `AddressSpaceRegistry`, and PID 1's
    `console_write` user-copy resolved no address space and failed closed
    with `BadAddress`. Added `AddressSpace::freeze()` → `FrozenAddressSpace`
    in `kernel/mem`: a `Send+Sync` POD snapshot walking every live page
    through `translate` into a `BTreeMap<Page,(Frame,MapFlags)>`, so it
    answers the copy path's permission checks identically to the live space.
    `InitSpawnCtx::admit_init` now also takes the boxed frozen view + boxed
    `DirectPhysMap`; `KernelInitSpawner` registers them under
    `SecTaskId(task_id)` in `&state.aspaces` (fail-closed on a duplicate
    id), and the aarch64 `init_spawn` seam freezes `space` after
    `spawn_image` and passes both. `init`'s `run.rs` now gates its `exit` on
    a full-length `console_write` (parks fail-closed otherwise, §2.9), so
    the `spawn_init_qemu_aarch64` vertical's PASS (keyed on the audited
    `exit` 5000 — `console_write` is `audit:false`) now genuinely proves the
    banner reached the console (`-M virt`, verified green).
- **P6d — userland process-spawn syscall `[x]`.** Add the `abi-v1`
  spawn syscall (gated by `CAP_PROC_SPAWN`, already reserved at id 17) +
  an embedded-program registry so `init` can launch a separate process.
  Per the standing direction this is being done *properly* — a real
  concurrent process (its own isolated address space, scheduled
  independently), not an `exec`-style hand-off — which requires real
  kernel-thread EL0↔EL0 context switching the kernel does not have yet.
  So P6d is itself staged in **`plans/SPAWN.md`** (SP0 design; SP1
  kernel-thread task runtime wiring the existing `ContextSwitch` HAL into
  the live scheduler; SP2 resumable EL0 tasks that timeshare a CPU; SP3
  the `spawn` syscall #12 + embedded-program registry; SP4 `init` launches
  the `session` process — overlapping P6e). Each `SP`-stage lands green
  over the whole project on its own. **SP0, SP1, SP2a, SP2b, and SP2c are
  landed, so SP2 is complete on aarch64:** the SP0 design note is
  `docs/src/architecture/multitasking.md`, and the `kernel/core::kthread`
  runtime (`spawn_kthread`, the `Yielder`, the per-task kernel stack) is
  host-tested and proven on `-M virt` by
  `tests/integration/kthread_switch_qemu_{aarch64,riscv64,x86_64}` — two
  kthreads ping-pong through the real `ContextSwitch::switch`, now a
  production scheduling path on every arch. (The x86_64 sibling was the
  first on-metal first-resume into a real Rust trampoline and surfaced a
  latent `TaskCtx::prepare` rdi-slot + stack-alignment bug, now fixed.)
  SP2a added the arch-neutral EL0-reschedule machinery, and **SP2b makes
  PID 1 reach EL0 as a resumable user kthread**: `KernelArch` exposes a
  `ContextSwitch` (`type Cs` + `context_switch()`), the aarch64 port gained
  `paging::activate_user_root` for the per-task `pre_resume` hook,
  `InitSpawnCtx::admit_init` admits PID 1 via `spawn_user_kthread`, and the
  `KernelDispatchHook` producer maps `yield`/`exit` to a `Reschedule`
  outcome (the handlers no longer drive the scheduler directly). The
  production `spawn_init_qemu_aarch64` vertical reaches EL0 through that
  full path. **SP2c then proves two EL0 user tasks timeshare one CPU**:
  the new `tests/integration/spawn_el0_timeshare_qemu_aarch64` vertical
  builds two hardware-isolated EL0 address spaces from the pure-Rust
  `tairix-test-el0-yielder` fixture (it links the new `tairix_rt::yield_now`
  wrapper), admits each as a resumable user kthread via `spawn_user_kthread`,
  and drains the cooperative `step` loop while a dispatch callback maps each
  task's `yield`/`exit` to `reschedule_current` — verified green on `-M
  virt`. **SP3 (the `spawn` syscall #12 + embedded-program registry) is
  staged SP3a/SP3b; SP3a is landed:** the `abi-v1` `spawn` syscall #12
  (`CAP_PROC_SPAWN`, audited) is wired end to end — `lib/abi` row + frozen
  tests, the `tairix_sys_spawn` C stub + regenerated header, the
  `kernel/syscall` dispatch arm + recomputed `SYSCALL_TABLE_HASH` — plus
  the `kernel/core` path-keyed `ProgramRegistry` and the fail-closed
  `ProcessSpawn`/`SpawnCtx` seam (default `NULL_PROCESS_SPAWN` →
  `NotImplemented`, mirroring `NULL_CONSOLE`). The `spawn` handler
  copies-in the path, resolves it, and admits a **Ready** resumable user
  kthread through `SpawnCtx::admit_process` (host-proven by a `ProcessSpawn`
  double + 8 host tests). **SP3b and SP4 are now landed too, so P6d is
  complete:** the real aarch64 `ProcessSpawn` producer
  (`kernel/tairix-kernel/src/spawn_producer.rs`) builds each child a fresh,
  hardware-isolated identity address space (window mask-derived, as PID
  1's) whose page tables come from
  the kernel's live `FrameAllocator` through a boot-cached `kernel/mem`
  `FrameTableSource` (§24.1 — no fixed reserve, capacity scales with RAM;
  without switching the spawning caller's `TTBR0_EL1`), drives the audited
  `spawn_image` + `admit_process`, and is
  installed via `BootInfo::with_spawn`; the kernel `build.rs` now embeds both
  `init` and the `Shell` session program through one `elf2rxe` helper. PID 1
  `init` (granted `CAP_PROC_SPAWN`) spawns `config.session()`
  (`/System/Commands/elsh.app/Run`) through `tairix_rt::spawn` and keeps running; the
  `tests/integration/spawn_session_qemu_aarch64` vertical proves both
  processes run on `-M virt` (PASS on two `ProcessSpawned` + three audited
  syscalls — the session's gated banner+exit is necessarily last).
- **P6e — real shell REPL + session supervision `[x]`.** The `session`
  program `init` launches is currently a banner+exit `Run` stub in the
  `Shell` bundle; P6e wires the existing `tairix-elsh` interpreter library
  into it (a real REPL) and has `init` supervise the session across its
  lifetime (restart, reap). **Design correction (binding, AGENTS.md §20):**
  the shell must do its text I/O over its **inherited standard streams
  (fd 0 `stdin` / fd 1 `stdout` / fd 2 `stderr` / fd 3 `stdinfo`)**, *not*
  over the kernel-discovered console via `console_read`/`console_write`.
  Binding the REPL to the discovered console hard-codes "whichever console
  the kernel found" into the shell — ambient authority (§4) and hidden
  device coupling (§17.3/§17.4). Reading fd 0 / writing fd 1 makes the same
  `tairix-elsh` binary "just work" whether started on a UART, a
  framebuffer console, a network socket, or a WM terminal surface, with
  **zero** shell-side changes — only the *backing* of its descriptors
  differs. The gap this exposes: `abi-v1`'s startup vector
  (`lib/abi/src/process.rs`) carries args/env/canary but **no descriptor
  table** — there is no notion of inherited streams yet. So P6e is staged
  so that P6e-1/P6e-2 build the device *backing* and P6e-3a adds the
  stream layer the shell actually binds to:
  - **P6e-1 — `console_read` `abi-v1` syscall + kernel seam `[x]`.** The
    input counterpart of P6a's `console_write`, and — together with it —
    reframed as the bootstrap **device backing** the stream layer attaches
    to fd 0/1, *not* the shell's interface (AGENTS.md §20). Syscall **#13**
    (`SyscallNumber::CONSOLE_READ`) gated by the new
    `CapabilityId::CONSOLE_READ` (**id 19**), appended to the `lib/abi`
    source of truth (table row, regenerated C header, recomputed
    `SYSCALL_TABLE_HASH`). A `ConsoleRead` seam in `kernel/core::console`
    (default `NULL_CONSOLE_READ`, fail-closed `NotImplemented`, installed
    via `BootInfo::with_console_read`) and a `console_read` handler that
    reads into a bounded (`CONSOLE_READ_MAX`) kernel staging buffer and
    `copy_out`s to the caller (short/zero reads valid, defensive clamp,
    `BadAddress` on a faulting/unregistered caller). `lib/rt::console_read`
    + `lib/abi-sys::tairix_sys_console_read` wrappers. Host-proven by 7
    `kernel/core` tests + the rt/abi-sys/abi drift+marshalling tests; the
    dispatcher reachability/fuzz/proptest doubles gained the new arm. **No
    device read is wired yet** — the aarch64 serial has no RX primitive, so
    `console_read` fails closed everywhere until P6e-2.
  - **P6e-2 — UART RX device + wiring `[x]`.** A non-blocking
    console-input read primitive was added to the aarch64 serial path:
    `ConsoleModel::rx_ready` decodes each model's receive-status bit
    (PL011 `UARTFR.RXFE` set = FIFO empty; mini-UART `AUX_MU_LSR_REG`
    bit 0 set = data ready), reusing the existing data/status offsets
    since the RX registers coincide with TX on both models;
    `serial::getchar`/`read_console_bytes` drain the RX FIFO into the
    caller's buffer and stop at the first absent byte — no busy-wait
    (§2.1), so an empty read is a valid zero-length short read. The
    zero-sized `UartConsole` now implements `ConsoleRead` (`Ok(0)` inert
    on host), and `boot_aarch64::enter_kernel_core` installs it through
    `BootInfo::with_console_read(&UART_CONSOLE)` beside the existing
    `.with_console`; kernel-core's init pipeline wraps whatever device the
    boot path installed in `BlockingConsoleRead`
    (`kernel/core/src/console.rs`), which parks an empty-handed
    `stream_read` caller on the scheduler (`reschedule_current`, the
    `wait`-syscall poll-and-park loop) and re-polls on redispatch — the
    backing owns blocking (§20), so user space never sees a spurious
    end-of-input. This completes the **bootstrap backing** — it feeds
    fd 0's backing object (P6e-3a), it is **not** called directly by the
    shell. The receive-bit decoders are host-unit-tested (2 new
    `console` tests + 1 `aarch64::arch_wrapper` adapter test); the
    freestanding aarch64 kernel builds clean.
  - **P6e-3a — standard-stream ABI + fd table `[x]`.** The two console
    syscalls were evolved **in place** (§2.13) into fd-keyed stream ops:
    `stream_write(fd, buf, len)` (#11) and `stream_read(fd, buf, len)`
    (#13), arg_count 3 with a leading `U32 fd`, appended-row-stable
    capabilities (`CAP_CONSOLE_WRITE`/`CAP_CONSOLE_READ` kept as the coarse
    "may use a console-backed stream" gate). `lib/abi/src/process.rs` gained
    the per-process descriptor model — `STDIN`/`STDOUT`/`STDERR`/`STDINFO`,
    `STD_STREAM_COUNT`, `StreamMode{Closed,Read,Write}`, and `DescriptorTable`
    (`closed()`/`standard()`/`mode()`) — established at spawn and held per
    task in `AddressSpaceRegistry` (new `set_streams`/`streams`, cleared on
    `withdraw`). The `stream_write`/`stream_read` handlers resolve `fd`
    against the caller's table **before** any state and fail closed with
    `NotFound` unless the direction matches; both production admit paths
    (`admit_init`, `admit_process`) install `DescriptorTable::standard()`, so
    a process's fd 0 reads / fd 1/2/3 write the discovered console the boot
    path installed (the P6e-1/P6e-2 UART backing **reused behind the
    stream**, not exposed). `lib/rt` exposes the standard streams over fd
    0/1/2/3 through its `io` trait layer (`console_*` removed); `lib/abi-sys`
    exports `tairix_sys_stream_write`/`tairix_sys_stream_read`; `init` + the
    `Shell` session write their banner via `tairix_rt::io::Stdout`. C header regenerated
    (`TAIRIX_SYS_STREAM_WRITE`/`_READ`) and `SYSCALL_TABLE_HASH` recomputed
    (`1cfbad…`); the abi-check + c-header drift guards are green. Proven
    host-side (the `lib/abi` descriptor-table tests, the `aspace` stream-map
    tests, the 11 reworked + 3 new `kernel/core` handler gate tests, the
    rt/abi-sys fd-marshalling tests) and on `-M virt`: the
    `spawn_session_qemu_aarch64` vertical now proves a spawned child writes
    fd 1 over the discovered-UART backing (the shell banner lands on fd 1).
    Whole-project gate (fmt / `cargo xtask ci` / `fuzz --secs 5`
    / `soak.sh both` / `cargo xtask test --qemu`) **green on this host**.
    Real UART **RX** over fd 0 on silicon remains an on-metal item (no
    deterministic `-M virt` serial-RX injection, consistent with P6e-2).
  - **P6e-3b — shell REPL over its streams + `init` supervision `[x]`.**
    Wire `tairix-elsh` to read fd 0 / write fd 1 (and emit `stdinfo` on
    fd 3 per §20) through the `lib/rt` standard-stream wrappers, with
    `init` supervising the session (restart, reap). The shell contains
    **no** reference to `console_*` or to any device. Staged into the REPL
    itself (P6e-3b-i) and `init` supervision (P6e-3b-ii) — **both landed**.
    - **P6e-3b-i — shell REPL over fd 0/1/2/3 `[x]`.** The `Shell` bundle's
      `Run` binary (`userland/shell/elsh/src/run.rs`) no longer prints a
      banner and exits: it runs the sibling `tairix-elsh` interpreter as a
      read-eval-print loop (the new `repl` lib module) over its **inherited
      standard streams** (`AGENTS.md` §20). `repl::run` reads command lines
      from fd 0 (`tairix_rt::io::Stdin`, reassembling lines across reads,
      stripping CRLF, capping a line at 4 KiB and discarding an over-length
      line), runs each through `Shell::run_line`, writes the prompt + output
      to fd 1/2 through the `RtConsole` seam, and emits one `omission`
      `StdInfoRecord` on fd 3 when a line is dropped (§20.1). It binds to fd
      0/1/2/3 only — **no** `console_*` or device reference (ambient authority
      §4 / hidden coupling §17.3/§17.4). A zero-length read is end of input
      (clean exit); *blocking* is the stream backing's job (§20), and the
      kernel-core `BlockingConsoleRead` backing provides it (P6e-2), so an
      interactive session sits at its prompt until input arrives. The
      `RtProcessHost` launches commands via `spawn` + reaps via `wait`
      (args/env, signals, `cd`, and — since `plans/SPAWN.md` SP10 —
      pipelines and redirections all run end to end; only the `{var}`
      dynamic descriptors, fd ≥ 10, stay refused closed).
      `lib/abi` gained a tested `Errno::from_i32` decoder
      (single source of truth, §2.2; no C-header/hash impact — a method, not
      an ABI type change); the shared `lib/rt` read primitive surfaces a
      negative `-errno` as its typed `Errno` (never as a zero-length read that
      would read as end of input) and clamps the count to `buf.len()`
      (defence in depth, §5.4). Host-proven (6 new `repl` tests over scripted
      stdin/stdinfo + `Console`/`ProcessHost` fixtures; the `lib/rt` stream
      primitive tests; the `Errno::from_i32` round-trip test) and freestanding-built on
      all three bare-metal targets; the `spawn_session_qemu_aarch64` vertical
      proves the interactive loop (the session blocks at its prompt and the
      runner types a scripted `exit\n` at the guest's serial input). Docs:
      `docs/src/userland/shell.md`.
    - **P6e-3b-ii — `init` session supervision `[x]`.** PID 1 `init` no
      longer spawns-and-forgets the session: `userland/system/init/src/run.rs`
      now runs a fail-closed **supervise loop** — `spawn` the session, `wait`
      on exactly that child (blocking until it exits, reaping it), then
      relaunch it. The loop is bounded by a small `SESSION_SPAWN_BUDGET`
      crash-loop guard: a session that blocks on input runs for PID 1's whole
      life and never approaches it, but one that exits instantly (no input
      backing) stops the loop at `EXIT_SESSION_EXHAUSTED` rather than
      busy-spinning on `spawn` (`AGENTS.md` §2.1). A refused `spawn` is
      fail-loud but never boot-fatal (§2.24): the refusal is reported on
      `stderr` and only that entry's slot abandoned, while a failed `wait`
      still ends the run (`EXIT_WAIT_FAILED`, §2.9). The
      userland + kernel-bookkeeping pieces were already wired — the production
      aarch64 pipeline wires the `KernelProcessWait` producer
      (`kernel_core::run_phases`), the `spawn` admit path's `register_child`,
      and the `exit` handler's `record_exit`, and `admit_init`'s drive loop
      re-dispatches the parked `init` after the session exits — but the
      supervise loop exposed a latent **aarch64 arch defect** that hung the
      vertical (see the errata below). This changes `init`'s audited-syscall
      sequence, so the `-M virt` vertical assertions were reworked:
      `spawn_session_qemu_aarch64` now keys PASS on **three** `ProcessSpawned`
      (init + two session launches — the second launch proves the first was
      reaped and relaunched) + **four** audited syscalls (`init`'s `spawn`,
      `init`'s `wait`, the session's `exit`, `init`'s second `spawn`); the
      sibling `spawn_init_qemu_aarch64` still PASSes (its witness is now
      `init`'s first audited syscall, the `spawn`, instead of an `exit`) and
      its doc was updated. The session now **blocks** on fd 0 (the kernel-core
      `BlockingConsoleRead` backing) instead of exiting at end-of-input, and
      the runner gained deterministic `-M virt` serial-RX injection
      (`SerialInjection`: pipe QEMU stdin, type a scripted line once the
      guest prints its prompt marker), so the vertical exercises the full
      interactive cycle: prompt → injected `exit\n` → reap → relaunch → the
      second session blocks at its prompt. The session's capability set is
      `{CAP_CONSOLE_WRITE, CAP_CONSOLE_READ}` on every port (§2.2); ports
      with no console-read backing (x86_64, riscv64) keep failing closed at
      `NULL_CONSOLE_READ`, so their session verticals still witness the
      EOF-exit supervision path. Docs: `docs/src/userland/init.md` ("Session
      supervision").
    - **Errata — aarch64 exception return-state save/restore `[x]`.** The
      P6e-3b-ii supervise loop hung the `spawn_session_qemu_aarch64` vertical
      (`Outcome::Timeout`): `init`'s `wait` reaped the session and returned,
      but `init` never reached its relaunch `spawn`. Root cause was a latent
      defect in the aarch64 EL1 exception trampoline (`kernel/arch/aarch64/
      src/vectors.s`): it saved only `x0..x30`, **not** `ELR_EL1`/`SPSR_EL1`/
      `SP_EL0`, relying on the live system registers across `eret`. That holds
      only when a handler returns directly — but a parked `wait`/`yield`
      (SP2) suspends the task **mid-handler** and switches to another task,
      whose own trap/`eret` clobbers those registers, so the resuming
      exception `eret`ed `init` to the session's PC/stack. (The SP2c
      `spawn_el0_timeshare` vertical masked it: two *identical* programs at
      identical VAs resume "correctly" at the wrong-but-equal PC.) Fix: the
      common trampoline now saves `ELR_EL1`/`SPSR_EL1`/`SP_EL0` into an
      enlarged 288-byte per-exception frame (GP-register offsets unchanged —
      the `[u64; SAVED_GPRS]` syscall view is intact) and writes them back
      before `eret`, making every exception's resume self-contained across a
      cooperative context switch. `spawn_session_qemu_aarch64` now PASSes
      (3 `ProcessSpawned` + 4 audited syscalls). The riscv64/x86_64 trap
      vectors carry the same latent pattern but have no EL0 spawn/wait
      timeshare path wired yet, so it is unreachable there today and is
      folded into those ports' user-mode bring-up follow-ons. Docs:
      `docs/src/platform/aarch64.md` (Interrupts).
    - **Prerequisite — `lib/rt` `mem_map`-backed `#[global_allocator]`
      `[x]`.** The `tairix-elsh` interpreter is `no_std + alloc`, but the
      freestanding userland runtime had no heap, so the shell could not link
      it. `lib/rt` now registers a `#[global_allocator]`
      (`lib/rt/src/heap.rs`): a free-span allocator over a fixed-base virtual
      arena that grows by `mem_map(FIXED)` and shrinks by `mem_unmap`,
      first-fit with alignment-padding return + neighbour coalescing, real
      free, deterministic-OOM-to-null (`AGENTS.md` §4/§2.9), no re-zero on
      free (the kernel already zeroes on map/free, §2.16). The pure free-span
      bookkeeping is host-unit-tested over a fake pager; the aarch64 `-M virt`
      vertical `tests/integration/heap_qemu_aarch64` proves it end to end — a
      pure-Rust EL0 fixture (`tests/integration/heap_program`) Box-allocates,
      grows a `Vec` across pages, reallocates after freeing, verifies every
      value, and exits 0 (PASS), with the allocator-issued `mem_map`/
      `mem_unmap` `svc`s routed through the live `MemMap` producer.
      **Verified green under QEMU on `-M virt`.** Design note:
      `docs/src/architecture/memory.md` §7d. This unblocks the REPL; the REPL
      itself + `init` supervision (which also needs a process-wait syscall)
      remain.
    - **Prerequisite — `wait` process-wait syscall (SP6) `[x]`.** Both the
      shell's foreground job control and `init` supervising the session
      (reap, restart) need a way to block on and reap a child — `spawn` was
      spawn-and-forget. **SP6 is COMPLETE** (`plans/SPAWN.md` SP6): SP6a
      landed the `abi-v1` surface (`SyscallNumber::WAIT` #16 + `WAIT_PID_ANY`, the
      `wait(I32 pid, UserPtr status) -> U64` row, unprivileged + audited),
      the `tairix_sys_wait` C stub + regenerated header, the `tairix_rt::wait`
      wrapper, the `kernel/syscall` dispatch arm + doubles, and the
      fail-closed `kernel/core::procwait::ProcessWait` seam + handler. **SP6b
      (this session)** landed the scheduler-side producer: the `ProcessWait`
      trait gained default-no-op `register_child`/`record_exit` hooks (so the
      null default + test doubles stay inert and no `new()` churn), the real
      `KernelProcessWait<A>` owns a `SpinLock<ProcessTable>` and blocks a
      waiting parent by cooperatively parking it via `reschedule_current(…,
      Yield)` until a child is reapable (fail-closed `NotImplemented` if no
      user kthread is published — never a busy-spin), `exit` records the code,
      the `spawn` admit path registers the parent→child link, and `run_phases`
      installs the producer via the hook's new `with_process_wait`. The
      aarch64 `-M virt` vertical `tests/integration/wait_qemu_aarch64` (+ the
      two-role `tests/integration/wait_program` fixture) proves a parent reaps
      a child that exited with a known code and reads it back, exiting 0 —
      **verified green under QEMU on `-M virt`**. This unblocks the REPL +
      `init` supervision.

**Done when:** under `-M raspi4b`, the kernel reaches `init` in EL0 and
`init` emits its first line on the console (framebuffer if present, else
the discovered UART), then starts the user's shell; a vertical asserts the
EL0 transition + the `init` banner. (This is the "boot into user mode"
milestone.)

**Landed (proven on `-M virt`).** All of P6a–P6e are `[x]`: the production
aarch64 kernel reaches EL0, PID 1 `init` writes its banner over its
inherited `stdout`, launches the `Shell` session through the `spawn`
syscall, and now **supervises** it (`spawn`→`wait`/reap→relaunch). The EL0
transition + banner are proven by `spawn_init_qemu_aarch64` and the
supervision by `spawn_session_qemu_aarch64`, both on `-M virt` — QEMU 8.2.2
has no `raspi4b` and `raspi*` performs no DTB hand-off (the standing P2
gap), so the `-M raspi4b` form of this gate is an on-metal acceptance item.

**P6 follow-on — SP5 `mem_map`/`mem_unmap` (runtime anonymous memory).**
A spawned process otherwise has only its fixed spawn-time image, so it
cannot obtain a heap; SP5 (`plans/SPAWN.md`) adds the `mmap`-style
anonymous map/unmap pair a future `lib/rt` `malloc`/`free` layers over.
**SP5-0 (design note) and SP5a (the `abi-v1` surface + fail-closed seam)
are landed:** `SyscallNumber::MEM_MAP` (#14) / `MEM_UNMAP` (#15), the
`MapFlags` type (with `FIXED`), the appended `Errno::OutOfMemory` (#20),
the `tairix_sys_mem_map`/`tairix_sys_mem_unmap` C stubs + regenerated header,
the dispatcher arms, and `kernel/core`'s `MemMap` seam (`NULL_MEM_MAP` /
`with_mem_map`, unprivileged + unaudited, fail-closed `NotImplemented`).
**SP5b-1 is also landed:** the reusable, host-proven `kernel/mem::anon`
live-address-space producer (`map_anonymous`/`unmap_anonymous` — zero on
map/free, W^X `RW|USER`, deterministic OOM, fail-closed all-or-nothing
reclaim, per-page TLB flush). **SP5b-2 is also landed (SP5 complete):** the
aarch64 `-M virt` EL0 vertical `tests/integration/mem_map_qemu_aarch64`
wires the producer through the `kernel/core` `MemMap` seam — it builds one
isolated EL0 space with `spawn_image`, **retains** it live behind a `MemMap`
producer over `map_anonymous`/`unmap_anonymous`, admits the program as a
resumable user kthread, and routes the program's `mem_map`/`mem_unmap`
`svc`s through it; the pure-Rust EL0 fixture
`tests/integration/mem_map_program` (linking the new
`tairix_rt::mem_map`/`mem_unmap` wrappers) maps a region (FIXED),
writes+verifies a pattern, unmaps it, then faults on use — the fault handler
reports PASS, **verified green under QEMU on `-M virt`**. The **riscv64
sibling `tests/integration/mem_map_qemu_riscv64` is now landed too**: it
reuses the same pure-Rust `mem_map_program` fixture and the same
`kernel/mem::anon` producer over an Sv39 U-mode space, but drops into the
program through `spawn_image` + a direct `EnterUser::enter_user` (a single
task that only direct-returns from its `ecall`s, so the riscv64
cooperative-switch trap-save path stays off the critical path) and reports
the use-after-unmap page fault as PASS on `-M virt` (ids 4284–4287, **verified
green on this host**). The x86_64 sibling + production per-task live-space
retention still follow.

**P6 follow-on — kthread kernel-stack guard page.** The deep-`wait`-handler
overrun that silently corrupted the next task's snapshot (P6e-3b-ii) is now
defended: `kernel/core::kthread::BoxStack` carries a poison-filled guard
page immediately *below* the usable stack and `dispatch_step` verifies its
canary on every switch-back, failing the task closed on an overrun rather
than letting it reach the heap neighbour (`AGENTS.md` §4 / §2.9 / §2.17,
host-proven; the same emulation `kernel/mem`'s slab guard documents). **This
is the real, non-deferred defence — not the old 64 KiB limit bump.** The
*deployment* form, which turns the overrun into an immediate hardware fault
instead of a next-reschedule detection, is now **landed `[x]`** (G1–G3c):
The deployment form — turning the overrun into an immediate hardware fault
instead of a next-reschedule detection — is **landed on every port `[x]`**,
and not by splitting a live block.

A kthread kernel stack is a run of pages in the **shared kernel remap
window** (`kernel/mem::KernelVirtMap`), laid out `[guard slot | usable
stack]` with the guard slot reserved and never mapped. The window's
sub-hierarchy is installed by every translation root, so the guard is absent
in all of them at once: no root refines a block for it, none carries a
per-task unmap, and a stack overrun faults synchronously under whichever
root the task runs. The tier is architecture-neutral
(`kernel/core::kstack`), installed from the one boot site that also installs
the heap's growth source, and sized as a share of the window derived from
discovered RAM (§24.1). Every kthread stack — PID 1's, a `thread_create`'s,
a deferred load's — comes from `kstack::alloc_kernel_stack`; a build with no
remap window keeps the poison-canary `BoxStack`, fail closed, never an
unguarded stack.

The earlier staged design refined the coarse identity block covering a
guard page in each task's own root, over a boot-carved physical arena. That
was a break-before-make violation
on a live translation regime — a granule change the architecture leaves
undefined, bounded by a whole-regime invalidation but not removed by it —
and it had to be repeated per root, including for blocks an arena chained
after a root was already live. The window form removes the violation rather
than bounding it, so the split primitive, the arena, and the physical carve
are all deleted (`plans/OPEN-DEFECTS.md` D81, D82).

The fault form is proven on all three ports by
`tests/integration/stack_overrun_qemu_{aarch64,riscv64,x86_64}`: a kthread
admitted on a tier-drawn stack through the production
`spawn_kthread_with_stack` writes the highest byte of its guard slot — the
first byte a contiguous downward overrun crosses — and the unmapped slot
raises a synchronous fault *while the kthread runs*, which each port's fault
handler confirms by cause and faulting address. Each vertical first checks
the stack really came from the window, so a silent degrade to the
software-canary fallback fails the test rather than passing it. Docs:
`docs/src/platform/{aarch64,riscv64,x86_64}.md`.

### X — x86_64 concurrent user mode: timeshare → spawn → wait (P6 cross-port follow-on) `[x]`

**x86_64 now reaches a full concurrent, multi-process user mode** to match
aarch64 (SP2c EL0 timeshare, SP3b/SP4 `spawn`, SP6 `wait`): X1–X4 and the X4
follow-on are all `[x]`, so PID 1 `init` spawns the session, reaps it, and
relaunches it under the live scheduler on x86_64 (`spawn_session_qemu_x86_64`,
3/4). The **riscv64** timeshare sibling is the remaining cross-port follow-on,
a *separate, larger* one deferred behind this arc (see the end of this
section): its `trap.s` runs the handler on the interrupted **user** `sp` with no
`sscratch` kernel-stack swap, so it needs a trap-entry redesign before a
cooperative mid-handler park can work at all.

The X1–X4 chunks (below) were staged lowest-risk-first because the x86_64
machinery already existed: ring-3 entry (`tairix_arch_x86_64::userentry`), the
`mem_map` producer path (`mem_map_qemu_x86_64`), a `syscall`/`sysret` stub that
already switches to a kernel stack, an `X86_64Arch: SchedulerArch`, and an
x86_64 `ContextSwitchHal`.

**Binding design findings** (from the x86_64 `syscall` stub,
`kernel/arch/x86_64/src/syscall_entry.rs::syscall_entry_stub`):

- The stub loads the kernel stack from the **per-CPU** `SyscallTls.kernel_rsp0`
  (`gs:0`) and saves the user `%rsp` into the **per-CPU** `user_rsp_save`
  (`gs:8`); the saved user RIP (`%rcx`) and RFLAGS (`%r11`) are **pushed onto
  the kernel stack** (already frame-resident, so they survive a park).
- Unlike aarch64 — where an EL1 trap implicitly reuses the running kthread's
  `SP_EL1`, so each user kthread's syscall lands on its own kernel stack with
  no extra work — x86_64 must **explicitly** point `kernel_rsp0` at the
  **current** user-kthread's own kernel stack on each resume, or two tasks'
  syscall handlers collide on one stack (a correctness *and* isolation defect,
  §4).
- The per-CPU `user_rsp_save` (`gs:8`) is the x86_64 analogue of the aarch64
  `ELR_EL1`/`SPSR_EL1`/`SP_EL0` errata (4c780bc): a task parked **mid-handler**
  by a cooperative `yield`/`wait` (SP2) has its saved user `%rsp` overwritten
  by another task's syscall before it resumes, so the durable save must move
  onto the **per-task kernel-stack frame** (where `%rcx`/`%r11` already live).
  This is a real structural fix — never a limit bump or a "works for one task"
  shortcut (§2.17 / §2.1).

**Security / correctness / performance invariants (all chunks).** Every
syscall stays capability-checked **kernel-side** in `kernel/syscall` (§5.4);
none of this adds authority. Task isolation is enforced by distinct top-level
page tables (a fresh PML4 per space, §4) reactivated through CR3 on resume.
The fixes are structural, fail-closed (§2.9), and carry no `unsafe` without a
`// SAFETY:` block + a test (§2.10). The per-resume CR3 + `kernel_rsp0` reload
is the minimal switch cost the aarch64 sibling already pays; no allocation or
copy is added on the syscall hot path (§2.16).

- **X1 — x86_64 single resumable user-kthread `[x]`.** A single ring-3 task
  is admitted as a resumable user kthread and cooperatively parks/resumes under
  the live scheduler on x86_64. Two primitives, the siblings of the aarch64
  `activate_user_root`: `tairix_arch_x86_64::paging::activate_user_root(root_phys)`
  reloads CR3 (free `mov cr3`, host no-op; the load flushes non-global TLB
  entries so no `invlpg`), and `syscall_entry::set_kernel_rsp0(cpu, top)`
  repoints only the per-CPU `SyscallTls.kernel_rsp0` field (no MSR rewrite, no
  `user_rsp_save` touch) after the same fail-closed `validate_kernel_rsp0`
  stack-pivot check. The arch-neutral `kernel/core::kthread` `pre_resume` hook
  now takes the task's own kernel-stack top (`PreResume = FnMut(u64)`; the
  dispatcher passes `stack.top()`) — closing the gap aarch64 fills implicitly
  via `SP_EL1` (§2.4, not interface creep). The aarch64 hooks ignore the arg.
  Proven by `tests/integration/spawn_el0_resume_qemu_x86_64`: boots the
  production pipeline, builds one isolated ring-3 space via `spawn_image`,
  admits it via `spawn_user_kthread` whose `pre_resume` reloads CR3 +
  `set_kernel_rsp0`, drives `Scheduler::step`, and PASSes once the task yielded
  its full count and exited (dispatch maps `yield`/`exit` to
  `reschedule_current`). The durable user-`%rsp` `gs:8` hazard is **not**
  exercised by one task; its structural fix lands with its two-task exerciser
  in X2. Host tests cover `set_kernel_rsp0`'s validation; docs in
  `docs/src/platform/x86_64.md` ("Resumable ring-3 user kthread").

- **X2 — x86_64 return-state survives a concurrent park + two-task EL0
  timeshare `[x]`.** Two x86_64 tasks timeshare one CPU as resumable user
  kthreads, proven by `tests/integration/spawn_el0_timeshare_qemu_x86_64` (the
  SP2c sibling: two hardware-isolated ring-3 spaces — two PML4s, one shared
  frame pool, §4 — each admitted as a resumable user kthread whose `pre_resume`
  reloads its CR3 + `kernel_rsp0`, driven by the cooperative `step` loop
  mapping each `yield`/`exit` to `reschedule_current`; PASS once both yielded
  their full count and exited). It required **two** independent structural
  fixes, both shipped here (a one-task X1 run exposes neither):
  - **(1) Durable user-`%rsp` save on the per-task kernel frame.**
    `syscall_entry_stub` now `pushq %gs:8`s the just-stashed user `%rsp` onto
    *this task's* kernel-stack frame (beside the frame-resident `%rcx`/`%r11`)
    and restores it with a single `popq %rsp`. The user-`%rsp` slot doubles as
    the System V alignment pad, so the frame size — hence alignment — is
    unchanged (no hot-path cost). `gs:8` is now a transient temp held only
    between the entry `swapgs` and the first kernel-stack push, before any
    cooperative switch can occur, so a task parked mid-handler no longer has
    its saved user `%rsp` clobbered by a *different* task's syscall through the
    shared per-CPU slot. The x86_64 analogue of the aarch64
    `ELR_EL1`/`SPSR_EL1`/`SP_EL0` errata (4c780bc); structural, never a limit
    bump (§2.17).
  - **(2) `swapgs` balance across a cooperative mid-handler park (the blocker,
    not anticipated by the original X2 text).** Fix (1) is necessary but **not
    sufficient**: the two-task vertical (and the *original* pre-(1) stub)
    double-faults identically — a `v=08` #DF with `rsp=0` at
    `syscall_entry_stub`, CR2=-8 — because the per-CPU GS-swap state is left
    unbalanced across a park. The kernel's convention outside the stub's
    swapgs window is current GS = user value, `KERNEL_GS_BASE` = kernel TLS
    (`enter_user` relies on it). When task A's `syscall` runs the entry
    `swapgs` then parks mid-handler via `reschedule_current`, the dispatcher
    enters task B through `enter_user`/`iretq` (no `swapgs`), so B runs ring-3
    with kernel GS still active; B's first `syscall` `swapgs` flips GS the
    wrong way → `movq %gs:0,%rsp` reads address 0 → push faults → #PF on a
    null stack → #DF. X1 never exposes it because the same task always does the
    matching exit `swapgs`. Fix: a HAL cooperative-park hook pair on
    `tairix_arch_api::ContextSwitch` — `enter_cooperative_park` /
    `leave_cooperative_park`, default no-op (aarch64/riscv64 need nothing) —
    that `kernel/core`'s kthread runtime calls in `suspend_thunk_syscall`
    around the suspend switch (the user-kthread mid-handler park path; a
    kernel kthread's `suspend_thunk_body` skips the bracket). x86_64 implements
    them as a `swapgs` back to the between-handler convention immediately before
    the park and back into the stub-window convention immediately after resume;
    both are on the *task's* control flow and pair exactly, and the first
    trampoline→`enter_user` entry never goes through the syscall thunk, so it
    correctly does no swapgs. Structural, fail-closed, no limit bump (§2.17),
    capability checks unchanged (§5.4), per-PML4 isolation intact (§4). No ABI
    change. Stub rustdoc + the `SyscallTls` (transient-`gs:8`) docs updated;
    the host stub-layout test still pins the 16-byte two-word layout. Docs in
    `docs/src/platform/x86_64.md` + `docs/src/architecture/multitasking.md`.

- **X3a — x86_64 PID 1 (`init`) reaches ring 3 (production path) `[x]`.** The
  prerequisite for the x86_64 `spawn` producer: the **production**
  `tairix_kernel::boot` pipeline now spawns PID 1 into ring 3 through the real
  `kernel_main` + `InitSpawn` path (not a test-driven ad-hoc scheduler like
  X1/X2), the cross-port sibling of the aarch64 P6c-3 milestone. Three pieces,
  all wired into `BootInfo`:
  - `x86_64::init_spawn::X86_64InitSpawn` (`with_init`): builds `init`'s ring-3
    image through the audited `spawn_image` and admits it as a resumable user
    kthread (`admit_init`); `pre_resume` reloads CR3 (`activate_user_root`) +
    repoints the entry stack (`set_kernel_rsp0`); `BoxStack` kernel stack
    (software canary — the hardware guard-page form is aarch64-only).
  - `serial_sink::Com1Console` (`with_console`): the COM1 `ConsoleWrite` stream
    backing, so `init`'s fd-1 banner lands (§20); the x86_64 `UartConsole`
    sibling.
  - `boot::try_boot` enables `IA32_EFER.NXE` (production W^X step, §19.2).
  - **Key invariant:** the seam switches CR3 to the fresh space to build the
    image, and the x86_64 page-table walk dereferences tables by their **low
    physical address**, so the space must identity-map all of RAM (not the
    32 MiB `new_identity_first_32mib` window the X1/X2 verticals use). The new
    `paging::AddressSpace::new_identity_first_gib` (shared `new_identity`
    helper) maps 4 GiB, mirroring the boot trampoline (covers RAM + the LAPIC).
    Embedded program rxes now build for x86_64 too (`build.rs` generalised over
    a per-target link recipe). Proven by `tests/integration/spawn_init_qemu_x86_64`
    (PASS on `ProcessSpawned` + an audited `SyscallInvoked`). **No ABI change.**

- **X3b — x86_64 `spawn` concurrent producer `[x]`.** The real x86_64
  `ProcessSpawn` producer — `kernel/tairix-kernel/src/x86_64/spawn_producer.rs`,
  the cross-port sibling of the aarch64 `spawn_producer.rs` — is wired through
  `BootInfo::with_spawn` (in `boot::try_boot`, beside the X3a `with_init` seam)
  with the shared embedded `spawn_layout::PROGRAM_REGISTRY` (the `Shell` `rxe` `build.rs`
  already bakes for x86_64). On `init`'s `CAP_PROC_SPAWN`-gated `spawn` for
  `/System/Commands/elsh.app/Run`, it draws the child's page tables from the kernel's
  live `FrameAllocator` through a boot-cached `kernel/mem` `FrameTableSource`
  (§24.1 — no fixed `.bss` reserve, capacity scales with RAM, fail-closed
  `NoSpace` only on genuine OOM), builds a 4 GiB-identity child PML4 with
  `new_identity_first_gib`, drives the audited `spawn_image` + `admit_process`
  (the child gets only `{CAP_CONSOLE_WRITE, CAP_CONSOLE_READ}`, no ambient
  authority), and admits
  it **Ready** — returning the PID without entering it (a true concurrent spawn).
  **Key decision:** unlike the X3a PID-1 seam (which switches `CR3` to build the
  image), the producer runs under PID 1's own `CR3` — whose
  `new_identity_first_gib` map covers the low 4 GiB identity (existing-table
  physical derefs + the allocator's page-table and image frames) **and** the
  higher-half kernel window (new-table static pointers + the `DirectPhysMap`) —
  so it builds the child's tables **without switching `CR3`**, never moving the
  running parent out from under itself, exactly as the aarch64 producer builds
  through its identity window (§2.2). The child's own `CR3` is reloaded by its
  `pre_resume` hook (CR3 + `set_kernel_rsp0`); its kernel stack is a software-
  canary `BoxStack` (the hardware guard-page fault-form is aarch64-only —
  riscv64/x86_64 `Pending`). Proven by
  `tests/integration/spawn_session_qemu_x86_64` (enrolled in
  `tools/xtask/src/commands/qemu_tests.rs`): PASS on two `ProcessSpawned`
  (PID 1 + the session) and two audited `SyscallInvoked` — the second necessarily
  the session's `exit`, since `init`'s `wait` only completes after the session is
  reaped, proving the session actually *ran* in its own ring-3 space.
  `init`'s `wait`→reap→relaunch supervision cycle is **not** asserted here — it is
  the x86_64 `wait` validation (X4). **No ABI change.**

- **X4 — x86_64 `wait` sibling `[x]`.** The `KernelProcessWait` producer is
  already installed on every production pipeline by `kernel/core`'s `run_phases`
  (so `register_child` on the spawn-admit path and `record_exit` in `exit`
  fire), so X4 added the proving vertical:
  `tests/integration/wait_qemu_x86_64` (enrolled in
  `tools/xtask/src/commands/qemu_tests.rs`), the cross-port sibling of
  `wait_qemu_aarch64`. It boots the production `tairix-kernel` pipeline (GDT
  ring-3 selectors / TSS / `syscall` entry), and on `BootCompleted` builds a
  **parent** and a **child** hardware-isolated ring-3 space (two PML4s, one
  shared frame pool) from the cross-arch `wait_program` fixture (built PIE in
  both roles + converted to `rxe`), installs a `KernelProcessWait<X86_64Arch>`,
  registers the link, and drives the cooperative `step` loop. It admits the
  **parent first**, so the parent's `wait` runs while the child is still
  registered-but-unexited and the producer **parks** it (`Reap::Blocked` →
  `reschedule_current`); the child then runs, exits, and the parent is
  re-dispatched to reap it and copy the reaped code out to its `status` pointer
  — exercising the resume-after-cooperative-park return-state path on the x86_64
  trap, then exiting 0. PASS verified under QEMU. The
  resume-after-park path the X4 note flagged is therefore **proven sound on
  x86_64** (X1/X2's durable user-`%rsp` save + `swapgs` balance cover it). **No
  ABI change.**

- **X4 follow-on — x86_64 `init` supervision cycle (relaunch-`spawn`) `[x]`.**
  `spawn_session_qemu_x86_64` now asserts the **full** `wait`→reap→relaunch
  supervision cycle (**3** `ProcessSpawned` / **4** audited syscalls, the
  cross-port equal of the aarch64 sibling): PID 1 `init` spawns the session,
  `wait`s, the session exits, `init`'s `wait` reaps it and returns to ring 3,
  and `init`'s relaunch `spawn` builds a third process — proven green under QEMU.

  **Root cause (was a frame-allocator-vs-kernel-image overlap, not a trap-state
  bug).** The x86_64 `boot::build_memory_map` built the `BootMemoryMap` straight
  from the UEFI map, where `bootmemory::from_uefi` (correctly) classifies
  `EfiLoaderCode`/`EfiLoaderData`/`EfiBootServicesCode`/`Data`/
  `EfiConventionalMemory` as `Usable` — but the loader places *this* kernel
  into that memory, and **nothing reserved the running kernel image** (unlike aarch64
  P6c-1's `[ram_base, __kernel_end)`). By the 2nd (relaunch) `spawn`, the low
  usable RAM consumed by boot + PID 1 + the 1st session pushed the allocator
  cursor across 1 MiB into the kernel image; `spawn`'s `build_process_image`
  zero-fill / page-table writes (through the higher-half direct map) corrupted
  live `.text` (the derail target was `0xffffffff80120000` = physical
  `0x120000`, the kernel image), producing the wild CPL=0 execution.

  **Fix (structural, with regression tests).** `kernel/arch/x86_64/linker.ld`
  now emits a `__kernel_phys_end` physical symbol (end of `.bss`, incl. the bump
  heap), `BootMemoryMap::reserve_range` clips a physical range out of every
  `Usable` region (preserving the allocator's no-overlap invariant; leaving the
  range an implicit reserved gap), and `build_memory_map` reserves
  `[__boot_phys_start, __kernel_phys_end)`. Host-tested in
  `kernel/mem/src/bootinfo.rs` (split/truncate/skip/zero-width + an
  allocator-never-hands-out-a-reserved-frame contract test); proven end to end
  by the strengthened `spawn_session_qemu_x86_64` (3/4). Docs:
  `docs/src/platform/x86_64.md` ("Reserving the kernel image out of usable
  RAM").

**Done when (per chunk):** the chunk's QEMU vertical PASSes under `cargo xtask
test --qemu` **and** the whole-project gate (§5) is green; docs + host tests
land in the same change (§7 / §13).

**riscv64 concurrent user mode `[x]`.** The riscv64 spawn/wait
timeshare sibling of the x86_64 X-series, landed lowest-risk-first, one
fully-gated chunk per landing. All of RV1–RV-X4 are done: the riscv64 port
now brings up concurrent, multi-process user mode (resumable user kthreads,
two-task timeshare, runtime `spawn`, and blocking `wait`/reap), reaching
parity with the aarch64 and x86_64 ports.

- **RV1 — `trap.s` per-task kernel stack + frame-resident return state
  `[x]`.** The prerequisite trap-entry redesign. The vector now swaps `sp`
  with `sscratch` on entry (port invariant: `sscratch` = the running user
  task's **trap anchor** while in U-mode, 0 while in S-mode; a nested S-mode
  trap lands `sp == 0` and is recovered onto the interrupted kernel `sp`), so
  the handler never runs on the interrupted **user** `sp` (which a
  cooperative `ContextSwitch::switch` taken mid-handler would wrongly persist).
  The anchor is a 16-byte kernel-only region at the top of the task's
  kernel-stack window carrying the running hart's kernel `tp`, which the
  from-U prologue reloads — after spilling the user's `tp` into the frame —
  before anything reads it: `tp` doubles as this port's per-hart identity
  anchor *and* as the psABI thread pointer U-mode writes freely, so leaving it
  alone would let a task name another hart and steer the kernel onto that
  core's per-CPU state. It also makes the thread pointer per-task, the
  platform contract thread-local storage needs;
  `tests/integration/tp_isolation_qemu_riscv64` is the adversarial witness.
  The vector saves `sepc`/`sstatus`/the interrupted `sp`/the interrupted `tp`
  into a 256-byte `trap::TrapFrame` (GP-register offsets unchanged, so the
  `[u64; …]` syscall view is intact) and reloads them before `sret`, picking
  the U-mode vs S-mode return path from the saved `sstatus.SPP`; the syscall
  path advances the
  **saved** `frame.sepc`. `userentry::enter_user` arms `sscratch` before its
  first `sret`; `init_traps` zeroes it at boot. This is the riscv64 sibling of
  the aarch64 `ELR_EL1`/`SPSR_EL1`/`SP_EL0` return-state errata. Host-proven
  (`trap_layout_tests.rs` parses every `.equ` out of `trap.s` and pins it
  against the `TrapFrame` field or Rust constant it addresses) and
  every line of the redesigned vector is exercised by the existing riscv64
  matrix: U-mode `ecall`s/faults (`mem_map`/`spawn_program`/`abi_sys`/
  `memory_isolation`) drive the from-U swap + U-return path, and S-mode
  timer/IPI traps (`sched_drive`/`ipi_smp`/`timer_preempt`) drive the nested-S
  recovery + S-return path — all green under QEMU. No ABI/C-header impact
  (`TrapFrame` is internal to the arch crate). Doc:
  `docs/src/platform/riscv64.md` ("Per-task kernel stack + frame-resident
  return state").
- **RV-X1 — single resumable user-kthread `[x]`.** The riscv64 sibling of
  x86_64 X1 / aarch64 SP2b. `tairix_arch_riscv64::paging::activate_user_root(
  root_phys)` is the per-task `pre_resume` reactivation primitive: it
  reprograms `satp` (`satp_sv39(root_phys)` + `sfence.vma`) on a hart whose
  paging is already on — a free function over the raw `u64` root (so the hook
  stays `Send`), lighter than `AddressSpace::switch`, with a bare-metal arm and
  an inert host arm presenting one `unsafe` API. The
  `tests/integration/spawn_el0_resume_qemu_riscv64` vertical (reusing the
  arch-neutral `el0_yielder` fixture) reads the timer rate from the firmware
  tree, builds one isolated Sv39 U-mode space via `kernel_core::spawn_image`,
  admits it as a **resumable user kthread** via `spawn_user_kthread` (its
  `pre_resume` hook calls `activate_user_root`; the handed kernel-stack top is
  unused on riscv64 — `sscratch` is armed by `userentry::enter_user` and
  preserved across a park by RV1, with per-task `sscratch` repointing deferred
  to RV-X2), and drives the cooperative `Scheduler::step` loop while the
  dispatch callback maps each `yield`/`exit` `ecall` to `reschedule_current`.
  PASS once the task yielded its full count and exited — the first chunk that
  *exercises* RV1's mid-handler-park safety on a user task. Doc:
  `docs/src/platform/riscv64.md` ("Resumable U-mode user kthread").
- **RV-X2 — two-task EL0 timeshare `[x]`** (SP2c sibling).
  `tests/integration/spawn_el0_timeshare_qemu_riscv64` proves **two**
  hardware-isolated U-mode tasks timeshare one hart as resumable user kthreads
  on `-M virt`: two Sv39 spaces (two `PageTablePool`s + a shared frame pool, §4)
  built from the one `el0_yielder` `rxe` via `kernel_core::spawn_image`, each
  admitted via `spawn_user_kthread`, driven by the cooperative `Scheduler::step`
  loop with the dispatch callback mapping each `yield`/`exit` `ecall` to
  `reschedule_current`. **No new structural code was needed** (the vertical
  only): unlike x86_64's per-CPU `set_kernel_rsp0`, riscv64 `sscratch` is
  per-task hardware state — `userentry::enter_user` arms it on first entry and
  the RV1 trap vector re-arms it from each task's own kernel-stack frame on
  every U-return (`trap.s`: `sscratch = sp + TRAP_FRAME_SIZE`), so each
  `pre_resume` hook only reactivates its `satp` root and ignores the
  kernel-stack-top argument (the predicted per-task `sscratch` repointing is
  unnecessary, as aarch64 SP2c needed nothing over SP2b). Doc:
  `docs/src/platform/riscv64.md` ("Two-task U-mode timeshare (RV-X2)").
- **RV-X3 — `spawn` concurrent producer `[x]`** (SP3b/SP4 sibling).
  `tests/integration/spawn_session_qemu_riscv64` proves a parent U-mode
  task's `CAP_PROC_SPAWN`-gated `spawn` builds a fresh, hardware-isolated
  Sv39 child and admits it **Ready** concurrently on `-M virt`. The
  `spawn_session_program` fixture is one source in two roles (parent
  `spawn`s the session then yields; child/session yields then exits,
  `AGENTS.md` §2.2). The mini-kernel admits the parent as a resumable user
  kthread; its `spawn` `ecall` is routed by the dispatch callback to a
  riscv64 `ProcessSpawn` producer (the cross-port equal of
  `Aarch64ProcessSpawn` / the x86_64 producer) that builds the child its
  own Sv39 space over a separate `PageTablePool` (data frames from the same
  monotonic pool, never aliasing, §4) **through the parent's identity
  window without switching the running parent's `satp`**, admits it Ready
  via `spawn_user_kthread`, and returns its PID — the parent keeps running
  (a true concurrent spawn). The child's own root is installed by its
  `pre_resume` hook (`activate_user_root`) on first resume. PASS once the
  producer built the child and both tasks yielded their full count and
  exited (two `ProcessSpawned`). Doc: `docs/src/platform/riscv64.md`
  ("Runtime `spawn` concurrent producer (RV-X3)").
- **RV-X4 — `wait` `[x]`** (SP6 sibling): the riscv64 cross-port equal of
  `wait_qemu_aarch64` / `_x86_64`. `tests/integration/wait_qemu_riscv64`
  proves a parent U-mode task `wait`s on its spawned child, parks until the
  child exits, reaps it, and reads back its code on `-M virt`. It reuses the
  arch-neutral `wait_program` two-role fixture (child exits with a
  build-pinned code; parent `wait`s + verifies), builds a child + parent as
  isolated Sv39 spaces (the RV-X3 mini-kernel shape) via
  `kernel_core::spawn_image`, installs the shared
  `kernel_core::KernelProcessWait<RiscvArch>` producer, registers the
  parent→child link, and drives the cooperative `step` loop: the child
  `exit`s, the parent's `wait` parks (`reschedule_current`, no busy-spin §2.1)
  then reaps it, the kernel copies the reaped code out to the parent's
  `status` through the retained frozen parent space (`copy_out`), and the
  parent verifies it and exits 0 (PASS, ids 4332-4334). The first riscv64
  exerciser of the resume-after-cooperative-park return-state path on a
  *user* task (RV1's per-task kernel stack + frame-resident return state).
  No ABI change. Doc: `docs/src/platform/riscv64.md` ("`wait`: blocking reap
  of a child (RV-X4)").

**riscv64 production boot path (RV-P series).** The riscv64 spawn/wait
arc above proved the concurrent-user-mode *mechanism* in test mini-kernels;
the RV-P series brings the **production `tairix-kernel` binary** up on
riscv64, mirroring the aarch64 P-stage arc.

- **RV-P1 — production boot to `BootCompleted` `[x]`.** The production
  `tairix-kernel` binary now boots the QEMU `virt` / SiFive board
  (`riscv64gc-unknown-none-elf`, linked with the arch port's
  `riscv64-virt.ld`) to `AuditEvent::BootCompleted`. The boot pipeline is
  the new `tairix_kernel::boot_riscv64` (`RiscvBinArch` `KernelArch`
  adapter, `build_boot_memory_map`, `try_boot`, `boot`): it parses the
  OpenSBI-handed device tree for the RAM window + `timebase-frequency`,
  builds the two-region `BootMemoryMap` (`[ram_base, __kernel_end)`
  reserved, the page-aligned remainder usable), and hands a validated
  `kernel_core::BootInfo` to `kernel_core::kernel_main` with `satp = 0`
  (the `virt` board's atomics are well-defined MMU-off, so no Sv39 bring-up
  is needed to reach `BootCompleted`). This pipeline is the **single**
  riscv64 boot orchestration (§2.2): the `tests/integration/riscv64_boot`
  wrapper re-exports it and only adds the test-side firmware-map/DTB
  observers before delegating, so every riscv64 QEMU vertical
  (`kernel_arch_boot_riscv64`, the virtio/framebuffer/input bins) runs the
  production code. Proven by `kernel_arch_boot_riscv64` (`id=4004 kernel
  boot completed` → SiFive PASS). **No ABI change** (the `lib/abi` types,
  syscall table, and C header are untouched). Doc:
  `docs/src/platform/riscv64.md` ("Kernel boot pipeline").
- **RV-P2 — Sv39 MMU enable + trap vector + syscall dispatch `[x]`.** The
  production `boot_riscv64::boot` now runs **paged**:
  `enable_mmu_and_vectors` identity-maps the whole low Sv39 window
  (`[0, 512 GiB)`, 1 GiB leaves over a `.bss` `PageTablePool`), writes
  `satp`, and points `stvec` at the S-mode trap vector via the new
  `trap::install_trap_vector` (the vector-only half factored out of
  `init_traps`, so the boot installs the vector **without** enabling
  asynchronous interrupts — `sie`/`sstatus.SIE` stay clear). The
  production `ecall` dispatch callback `riscv64::dispatch::production_dispatch`
  (the riscv64 sibling of `x86_64::dispatch`/`aarch64::dispatch` over the shared
  `dispatch_core`) is installed before any user thread can run; a pool that
  cannot satisfy the identity map fails closed. Because the map is identity
  (physical == virtual) and full-window, every board address — kernel
  image, DTB, PLIC, MMIO, the device-bring-up DMA carves — keeps its
  address under translation, so every riscv64 vertical runs under the paged
  boot. Proven by `kernel_arch_boot_riscv64` (`mmu_enabled=true
  dispatch_installed=true` → `id=4004` → SiFive PASS) and the
  virtio-blk/net + framebuffer verticals (device bring-up MMU-on). **No ABI
  change.** Doc: `docs/src/platform/riscv64.md` ("Kernel boot pipeline").
- **RV-P3 — user-mode drop + kthread-spawning seam `[x]`.** The riscv64
  `InitSpawn`/`ProcessSpawn` production seams (`riscv64::init_spawn` /
  `riscv64::spawn_producer`, the aarch64 `init_spawn`/`spawn_producer`
  analogue) are installed by `boot_riscv64::try_boot` via
  `BootInfo::with_init`/`with_spawn`, alongside the SBI-console
  `with_console` backing (`RiscvUartConsole` over the new verbatim
  `serial::write_console_bytes`). After `BootCompleted`, `kernel_main`
  drops PID 1 `init` into U-mode (its own Sv39 root, the shared 4 GiB
  `spawn_producer::identity_gigapages()` window plus the direct physical
  map, a window-backed hardware-guarded kernel stack), `init`
  writes its banner through `stream_write` and
  issues the `CAP_PROC_SPAWN`-gated `spawn` for `/System/Commands/elsh.app/Run`; the
  producer builds the session a fresh, hardware-isolated space from the
  allocator-backed `FrameTableSource` (no fixed reserve, §24.1) and admits
  it Ready. The kernel `build.rs` now also builds the embedded `init`/`Shell`
  `rxe` blobs for the riscv64 target. Proven by `spawn_init_qemu_riscv64`
  (`id=4030` PID 1 → the `TAIRiX <version>: …` machine-summary banner → `id=4030`
  Shell → `id=5000 sc=spawn` → SiFive PASS). **No ABI change.** Doc:
  `docs/src/platform/riscv64.md` ("PID 1 into user mode").

### P7 — VideoCore mailbox + framebuffer (metal)

**Landed — the host-provable protocol half.** The BCM2711 mailbox
property-channel client lives in the shared `lib/vcmailbox` crate
(§2.2 — the P7b framebuffer boot console speaks the same protocol; doc:
`docs/src/drivers/display.md`, "Firmware framebuffer discovery"):

- `FramebufferRequest::encode` frames the framebuffer request (set
  physical/virtual size, depth 32, pixel order from the
  `DisplayFormat`, allocate at page alignment, get pitch);
  `decode_framebuffer_response` validates the in-place answer
  fail-closed (header code, per-tag response bits/lengths, exact
  geometry echoes, pitch/size consistency); `bus_to_arm_physical`
  strips the 2-bit VC alias and rejects a zero, unaligned, or
  out-of-aperture buffer. The decoded `FirmwareFramebuffer` yields the
  `ScanoutConfig` (`ScanoutConfig::from_firmware`, plus the bus alias)
  `RpiHvs::open` consumes; the crate also carries the display-size
  query (`query_display_size`) the P7b boot console probes with.
- The doorbell is behind the `MailboxTransport` seam: `MmioMailbox`
  drives the register block over two capability-gated `RegisterWindow`s
  with a budget-bounded poll (`DEFAULT_POLL_BUDGET`), failing closed
  with `Timeout` — never an unbounded spin. A property completion naming
  another buffer (a dead predecessor's, answered late) is drained and
  counted rather than taken for this exchange's reply.
- Host tests cover framing (framebuffer + display-size), every
  fail-closed decode path, the alias↔aperture translation in both
  directions (`bus_to_arm_physical` / `arm_physical_to_bus`), and the
  doorbell transport in `lib/vcmailbox` (against its shared
  `mock::MockFirmware`, exported behind the `mock-firmware` feature),
  plus the wiring fail-closed paths and the full chain in `rpi_hvs`:
  mock firmware → `wiring::open_with_transport` → `ScanoutConfig` →
  `RpiHvs::open` → `present` into the discovered surface.
- **No QEMU vertical, deliberately.** `virt` RAM begins at
  `0x4000_0000` — outside the BCM2711 30-bit VideoCore aperture — so
  the driver's (correct) aperture validation can never pass there, and
  §0.4 forbids a Pi-board QEMU vertical. The emulation artefact is the
  host-side full-chain test; the real scan-out is the metal item below.

**Landed — the metal wiring.**

- `FdtDiscovery` discovers the mailbox node (`brcm,bcm2835-mbox`) and
  emits it into `tairix_abi::hwtree` through the generic Stage 4.HW
  walk (`kernel/arch/aarch64::platform`): the doorbell window as a
  capability-gated MMIO resource (base/length read from the tree, never
  a `const`) plus the one per-device augmentation — a `HwResource::dma`
  request for a one-page property-buffer carve bounded by the 30-bit
  VideoCore aperture (the `lib/fdt` `raspi_like_arm` fixture carries
  the node). The QEMU `virt` tree has no mailbox, so its hardware tree
  simply omits the node (§18.4) and the `-M virt` verticals are
  untouched.
- `drivers/display/rpi_hvs::wiring` is the driver-host bring-up seam:
  `open_discovered` checks `CAP_MMIO_MAP`, maps the discovered doorbell
  + the host's property-buffer carve, translates the carve to a bus
  address (`arm_physical_to_bus`), rings `MmioMailbox`, and delegates
  to `open_with_transport`, which assembles the full `HvsConfig`
  (firmware scan-out + the host's `HvsRegions`: DLIST RAM, control
  window, plane carves) and calls `RpiHvs::open`.

**Accepted on metal.** A real Pi 4B drives the VideoCore mailbox exchange,
maps the firmware framebuffer, and scans the HVS surface out to HDMI; the
operator's photo + UART log is the recorded acceptance artefact.

**Done when:** the mailbox property protocol has host unit tests
(request/response framing, bus↔physical translation, fail-closed on a bad
aperture) — done; `rpi_hvs` consumes a discovered `HvsConfig` — done
(hardware-tree mailbox node + `wiring::open_discovered`); a metal bring-up
scans the firmware framebuffer out to HDMI — done (operator metal
acceptance).

### P7b — Framebuffer boot console: video first, UART fallback

Console output (boot log and every later phase) defaults to the
**attached display**; the UART is the last resort when no video output
exists. Doc: `docs/src/platform/aarch64.md`, "Framebuffer boot
console".

**Landed — the code-complete console.**

- `kernel/arch/aarch64::video`: `find_mailbox` discovers the
  `brcm,bcm2835-mbox` doorbell with the shared early-returning
  `fdt::scan_translated` walk; `bring_up` (over the `lib/vcmailbox`
  `MailboxTransport` seam) queries the display's EDID-derived native
  size (`0×0` = no display → UART keeps the console) and allocates a
  32-bit surface at exactly that size — the whole panel, since the
  generated `config.txt` disables the firmware's overscan margins (P9).
  `TextConsole` is a full
  `xterm-256color` terminal: shell output is fed through the one shared
  streaming parser (`tairix_vt::Parser`, §2.2 — no second escape parser,
  depended `default-features = false` so only its allocation-free parser
  view links into the allocator-free QEMU bins), each `Op` mutates the
  retained cell grid, and the dirtied cells are repainted onto the
  scan-out surface once per write — SGR 16/256/truecolour, bold/reverse,
  cursor positioning, erase, scroll region, and explicit scroll. Glyphs
  are the shared `tairix_font` Inconsolata coverage atlas (§2.2, generated
  by `cargo xtask font-atlas`) drawn at its authored 8×16 cell, one atlas
  pixel per screen pixel, so a 1080p panel is a 240×67 grid of crisp cells
  rather than a magnified 120×33 one. Reaching the bottom margin
  scrolls the retained cell grid
  up one line (a real terminal scroll), not a ring wrap; the pixels are
  repainted once per write, never copied per scrolled line — the per-line
  framebuffer copy made a large listing burst monopolise the CPU for
  seconds on metal, starving the buffered serial drain. Program-output
  newline processing is applied inside that retained-grid batch, so one
  `stream_write` remains one repaint and one cache clean rather than being
  fragmented at every line feed; scheduling relies on preemption between
  syscalls, not an output-path yield.
- Bring-up runs in the **pre-MMU** phase of
  `tairix-kernel::boot_aarch64` (caches off ⇒ the property exchange is
  DMA-coherent with no maintenance; the state cell is written by the
  single-threaded boot CPU — no atomic RMW MMU-off). Post-MMU rendering
  serialises on a DAIF-masking `IrqSafeSpinLock` over the port's one masking
  primitive (`irqmask::DaifIrqControl`) and cleans the touched
  scanlines (`dc cvac` + `dsb`) so the firmware scan-out sees them. The
  doorbell base joins the Device-gigapage mask inputs; the boot audit
  line carries `video_console=true/false`.
- `serial::ConsoleWriter` (log sink) and `serial::write_console_bytes`
  (the `stream_write` fd 1/2 backing) render to the screen when
  `video::is_active`, else fall back to the UART; console input stays
  on the UART. Everything fails closed to the UART (§2.9): no mailbox
  node (QEMU `virt` — the UART verticals are unchanged), detached
  display, or any rejected firmware answer.
- Host tests: fixture mailbox discovery (translated `0xFE00_B880`),
  mailboxless-tree fallback, mock-firmware bring-up (native mode,
  detached display, inconsistent answer), the geometry scale policy and
  fail-closed surface validation, and the terminal renderer (glyph
  rows in the default colours, SGR colour interpreted-not-printed,
  256-colour/truecolour, reverse video, control-byte drop, `?` fallback,
  backspace/`\r`, absolute cursor positioning, erase-in-line, upward
  scroll on reaching the bottom, explicit `SU`, dirty bands).

**Accepted on metal.** The boot log renders on the attached HDMI display
on a real Pi 4 (`video_console=true`), and a detached-display boot proves
the UART fallback (`video_console=false`); the operator's photo + UART
logs are the recorded acceptance artefacts.

**Done when:** the boot log renders on the attached display on a real
Pi 4 with the UART fallback proven by the detached-display boot — done
(operator metal acceptance); everything host-provable is landed and
tested — done.

### P7c — Display power: the firmware framebuffer switched off

The boot display's scan-out surface is the firmware's, and so is its output,
so the screensaver's energy saving reaches the panel through the firmware.

What it guarantees:

- **Bound by discovery.** The aarch64 port publishes the boot display node
  with the firmware framebuffer's own binding, `brcm,bcm2708-fb`
  (`tairix_vcmailbox::FIRMWARE_FRAMEBUFFER_COMPATIBLE`), ahead of
  `simple-framebuffer`, only when the surface came from the VideoCore
  firmware. `drivers/display/rpi_fb` binds it at priority 20 over the generic
  framebuffer service's 10, so a board without it still gets a display.
- **The power switch.** `FirmwareDisplay` answers `Display::set_power` with
  the firmware's blank request (tag `0x0004_0002`, `encode_blank_screen` /
  `decode_blank_screen_response`, validated fail-closed like every property
  answer) over the mailbox service (`CAP_MAILBOX`); its pixels take the
  generic `tairix_display::Framebuffer` path under the one service loop
  (`plans/DISPLAY.md` D9).
- **Host-proven** against `mock::MockFirmware`, which models the blank state.

What remains: the metal run — a Pi 4B whose desktop's display-off wait runs
out blanks its HDMI output, and the first input lights it again, with the
operator's photo and UART log as the acceptance artefact.

### P8 — SD-card storage (EMMC2)

**Depends on `PLAN.md` Stage 4.HW** (bind table + `devmgr` + the drvhost
`.rxe` process-spawn path) — all landed: the aarch64 walk emits a
`brcm,bcm2711-emmc2` node (Storage class, translated MMIO window) from a
Pi-shaped tree with no per-device code, and `devmgr` binds the driver
against its `compatible` string (§18.3).

**Landed — the storage path is a discovery-matched bootstrap floor
(§18.6).** Both block drivers now publish a canonical `pub const
BIND_KEYS` (`tairix_drv_storage_emmc2` → compatible
`brcm,bcm2711-emmc2`; `tairix_drv_storage_virtio_blk` → virtio device id
2) — the single §18.3 source the signed manifest's bind table is authored
from. As the storage path that must be up before the signed driver store
is reachable, both are registered in the kernel binary's
`driver_catalog::IN_KERNEL_DRIVERS` floor registry (`build.rs` bakes each
an Ed25519-signed `InKernel` manifest carrying that same `BIND_KEYS`), so
the kernel binds the root block device because a discovered node matched a
driver's bind table — through the one shared `lib/devmatch` policy
`devmgr` uses (§2.2) — never a hand-wired probe (§18.5). Host-proven
(`driver_catalog` storage-node binding + signed-gate admission; the two
crates' bind-table tests). The boot path that *resolves* the discovered
root block node against this floor is landed (`tairix_kernel::root_storage`,
the `4135` `ROOT_STORAGE_AUTOLOAD` bind gate — Chunk B-2 below); bringing
the bound driver up and mounting the volume is the rest of Chunk B-2.

**The block driver.** `drivers/storage/emmc2` (`tairix-drv-storage-emmc2`)
implements `tairix_abi::driver::block::Block` for the BCM2711's SDHCI 3.00
host. Its design and test surface are `drivers/storage/emmc2/README.md` and
`docs/src/drivers/block.md`; the load-bearing facts:

- The state machine is written against the `SdhciHost` seam (registers,
  completion park, timed wait, DMA areas); metal drives it over `IrqSdhci`
  through `wiring::open_discovered`, host tests over `mock::MockSdhci`. No
  QEMU vertical, deliberately — QEMU models no Pi EMMC2 (§0.4).
- **Full speed is UHS-I DDR50 at 1.8 V (50 MB/s).** Clocks are divided from
  the firmware's EMMC2 base clock (`FirmwareClock::Emmc2`), and bring-up
  negotiates down UHS-I → High Speed → Default Speed, verifying each rung
  with a read of block 0. UHS-I needs the `Board`'s `CardSupply`: on the Pi 4
  the `vqmmc`/`vmmc` rails are firmware-expander GPIO lines resolved from the
  device tree (`tairix_arch_aarch64::sd_supply`, `tairix_fdt::supply`) and
  driven over the VideoCore mailbox (`tairix_vcmailbox::set_gpio_state`)
  during the bring-up alone. SDR50 needs tuning on this controller and the
  driver performs none, so DDR50 is the ceiling, as under Linux.
- **ADMA2 moves 256 KiB per command** through staging carved inside the
  node's `/emmc2bus` DMA window and addressed through it; bring-up keeps DMA
  only after a DMA read of block 0 matches the PIO one, else serves the card
  over the data port. Auto-`CMD23` where the SCR offers it; R1 and post-write
  `CMD13` errors fail the transfer; `Sensitive` staging is zeroed.
- The root-unlock bring-up logs the negotiated link (`root-unlock: emmc2
  link`: mode, clock, base clock, signalling, `CMD23`, DMA, fallbacks). The
  debug image also traces every step of it on the UART, each line flushed as
  written (`storage-trace`), so a stalled bring-up shows the step it stopped
  at.

**Metal acceptance.** A Pi 4B brings its card up at UHS-I DDR50, 50 MHz,
1.8 V, with `CMD23` and ADMA2 and no fallback, and mounts `/System` over the
link. Its firmware answers `SET_GPIO_STATE` with a zero per-tag code word, so
the supply switch is judged by the status word alone, as Linux's expander
driver judges it; and it reports no EMMC2 clock, so the host divides from its
capabilities' 100 MHz.

**Done when:** host unit tests cover identification, the speed ladder and its
fallbacks, and both transfer paths against the mock, and the metal boot mounts
from a real card over the negotiated link — both done.

### P9 — Bootable SD image (`tools/mkimage`)

The image builder is landed and the emitted image boots a real Pi 4 into
user mode (operator metal acceptance).

- `tools/mkimage` (`tairix-mkimage`, lib + bin) authors
  `images/tairix-aarch64-rpi.img` in pure Rust (§12 — no
  `parted`/`mkfs` shell-outs): an MBR (three 1 MiB-aligned primaries back
  to back: `0x0C` FAT32 boot @ LBA 2048 and `0x7F` ARXFS root, 64 MiB
  each, around the `0x7E` read-only `/System` sized to its content), with
  every partition laid down by the **real** in-tree drivers
  (`Fat32::format` / `ARXFS::format` — author and consumer share one
  on-disk definition, §2.2), mirroring the
  `tests/integration/{fat32,arxfs}_image` fixture pattern.
- Boot partition: the verified firmware blobs (the `disable-bt` overlay
  planted at its firmware-fixed `overlays/` path), a generated
  `config.txt` (`arm_64bit=1`, `kernel=kernel8.img`, `enable_uart=1`,
  `dtoverlay=disable-bt`, `disable_overscan=1` so the firmware's default
  per-edge overscan margins do not inset the scan-out surface every display
  surface sizes itself from, `init_uart_baud=115200` from the architecture
  port's shared console-rate constant;
  `armstub=armstub8.bin` only when the optional stub is staged), and
  `kernel8.img` — the P1 release ELF flattened by `mkimage`'s fail-closed
  converter (`elfflat`: ELF64/LE/`ET_EXEC`/aarch64 only, `PT_LOAD` layout
  must start *and* enter at `0x8_0000`, overlap/size-bound checks).
- Firmware blobs stay uncommitted third-party inputs (§19.3):
  `tools/mkimage/firmware.lock` pins the upstream HTTPS `source`
  directory plus name + byte length + SHA-256 of `start4.elf` /
  `fixup4.dat` / `bcm2711-rpi-4-b.dtb` / `overlays/disable-bt.dtbo`
  at upstream release `1.20260521`
  (provenance + licence documented in the manifest); verification fails
  closed on any mismatch. `cargo xtask image` fetches any blob missing
  from its `target/pi-firmware/` cache from the pinned source and gates
  every download on the manifest checksums, so the image build is one
  step; an operator-staged `--firmware` dir is only verified, never
  written. `armstub8.bin` is optional and unpinned — no official binary
  exists and the boot stub parks secondaries itself; it joins the
  manifest when SMP-on-metal needs it.
- Root partition: an encrypted ARXFS volume (no plaintext mode) carrying
  the §16 skeleton (`/System` + its twelve subdirectories incl.
  `Security/{Keys,Policy}`, `/Users`, `/Apps`, `/Storage`); the §11
  databases/users are the installer's first-boot job. The volume key is
  **passphrase-derived** (§11): the build provisions a per-volume arxfs
  `UnlockDescriptor` (random salt + PBKDF2 cost), derives the key from the
  profile's `passphrase_for` (`INSTALLER_PASSPHRASE` — **blank** — for the
  installer; `DEBUG_PASSPHRASE` — `root` — for the never-shipped debug
  image), provisions the root under it, and plants the plaintext descriptor
  on the FAT boot partition as `root.unlock` (the LUKS-header analogue the
  bootstrap reads before mounting). The bootstrap tries the **blank**
  passphrase silently first, so the installer image unlocks with **no
  prompt** and boots straight into the §11 installer; only a non-blank
  passphrase (debug `root`, or a production operator-chosen one) draws the
  `Root passphrase:` prompt. The derived key is written to the sibling
  `…-rpi.rootkey` file (0600) for host mounting — never inside the image,
  and re-derivable from `root.unlock` + the profile passphrase. A shippable
  user root is unlocked by an operator-chosen passphrase the installer sets,
  never a blank default.
- Entry points: `cargo xtask image --target aarch64-rpi` and the
  delegating `cargo xtask build --target aarch64-rpi` (`--headless`
  accepted; the image content is identical until installable GUI userland
  ships). A staged firmware dir may come from `--firmware` or
  `$TAIRIX_PI_FIRMWARE`; otherwise the pinned blobs are fetched
  automatically. The standalone `tairix-mkimage rpi` CLI mirrors the
  same flags (with `--firmware` required — no network I/O in mkimage).
- Host tests: ELF→flat layout + every refusal, manifest parse/verify
  fail-closed (incl. the committed manifest), boot/root partition
  round-trips re-mounted through the real drivers, the `root.unlock`
  descriptor planted on FAT re-deriving the exact volume key, a
  wrong-passphrase mount refusal (no separate oracle, §5.4), and
  full-image assembly with both partitions mounted from their MBR offsets.
  The MBR table is encoded through the shared scheme-neutral `lib/partition`
  layer — the one definition the kernel root-mount reader parses back, so
  author and reader cannot drift (§2.2); its MBR/GPT encode/parse tests
  live in that crate. Docs: `docs/src/install/raspberry_pi.md`.

**Accepted on metal.** The emitted `.img` boots a real Pi 4 into user mode
per the flashing/first-boot doc; the operator's UART log is the recorded
acceptance artefact (the P7/P8 metal items ride the same boot).

**Done when:** `cargo xtask build --target aarch64-rpi` (and `--headless`)
produces a flashable `.img` — done; `docs/src/install/raspberry_pi.md`
documents flashing + first boot — done; the image boots P6 (user mode) on
real hardware per a recorded checklist — done (operator metal acceptance).

### P10 — USB-HID input + desktop on the Pi

- Bring up the Pi 4 USB host — the VL805 xHCI behind the BCM2711 PCIe root
  complex, serving the USB-A ports — far enough to enumerate USB-HID
  keyboards and mice, so the WM input router has real events. The DWC2 OTG
  port follows only if a use needs it.
- Run `userland/gui/{wm,taskbar,session}` on the HVS path: the headless
  build stays first-class (§17.3), and the graphical session is the
  launchable option `userland/session/login` offers when the display +
  input drivers loaded.

| Item | What it is | Status |
|---|---|---|
| 5a | Each chain driver owns its canonical `BIND_KEYS`; `HwMatchKey` matches PCI and USB classes with vendor/product wildcards | done |
| 5b | Bus drivers describe what they enumerate as child nodes: `PciBus::describe_function` (the full 24-bit class), `UsbDevice::describe_device` (the interface class) | done |
| 5c | One match policy, `lib/devmatch`, for the in-kernel floor and `devmgr` (5c-i); every in-kernel driver admitted through the signed-manifest gate `KernelDriverLoader::admit`, its manifest signed at build against the kernel's embedded trust anchor (5c-ii) | done |
| 5d-0 | A driver reaches its device only through owner-checked per-task grants: the grant table (5d-0-ii (a)), the guarded `MmioWindowMap` (b), the retained live address space and its producers (b′), non-`FIXED` `mem_map` placement and `dma_alloc` (c) | done |
| 5d-1 | `lib/drvrt`'s `RtDriverHost`: the driver host over those grants | done |
| 5d-2 | User-space drivers by discovery: `resource_grants` (5d-2-i); grants minted at driver spawn (5d-2-ii (a)), `devmgr`-driven spawn through the signed gate (b-1), `lib/usb` (b-2-i), enumeration over the shared `Delay` (b-2-ii), the autoloaded driver binaries, the store scan and reader, and the `-M virt` autoload vertical (b-2-iii) | done |
| 5e | The flip (D5d, B5): the in-kernel keyboard scaffold deleted, the floor storage only | done |
| D1 | The runtime hardware-inventory store (`hwtree_store`) | done |
| D2a | Block-device sharing (`shared_block`); the floor disk held for the system's life by the driver-store service (D2a-2) | done |
| D2b | The read-only `/System` file service (D2b-1, `system_files`), reached over `ipc_call` (D2b-2) through the driver-store server (D2b-2c, `driver_store_server`, `tairix_abi::driver_store`) | done |
| D3 | The VideoCore mailbox in user space: `tairix_abi::mailbox_ipc`, the server side of `ipc_call` (`call_recv`, `call_reply`), `drivers/bus/mailbox/vcmailbox` | done |
| D4 | The flashable image ships the signed bus and USB class bundles (`image_drivers`); node removal (`hw_remove_node`) and its `devmgr` unload reaction (`plans/USB.md` U1) | done |
| D5b | The kernel assigns a published node its identity (D5b.2a); `pcie_brcm` as an autoloaded user-space bus driver (D5b.2b) | done |
| D5c | `vl805` reloads the firmware, then publishes `usb,xhci` | done |
| D5d | The flip (5e, B5) | done |
| B1 | The three-partition split and the read-only `/System` mount | done |
| B2 | The `/System` store autoloaded before the unlock | done |
| B3 | USB devices enumerated into the hardware tree, by the user-space `xhci` driver | done |
| B4 | The EMMC2 root | done |
| B5 | The flip (5e, D5d) | done |
| GUI | `userland/gui/{wm,taskbar,session}` on the HVS path | planned |

The `D` items are design D, the user-space driver chain the hardware tree
sequences; the `B` items are design B, the pre-unlock store (below).

**Input — done.** USB input comes up entirely in user space, as a chain of
signed driver bundles in the read-only `/System` volume's `/System/Drivers/`
store, autoloaded before the encrypted root is unlocked (design B, below).
Each is its own crate with its device logic co-located as a `lib` target
(§2.22):

1. `drivers/bus/pcie_brcm` binds the discovered `brcm,bcm2711-pcie` node,
   trains the link and publishes the VL805's PCI function;
2. `drivers/bus/mailbox/vcmailbox` serves the VideoCore mailbox, its
   property layout shared through `lib/vcmailbox`;
3. `drivers/bus/usb/vl805` reloads the VL805 firmware and only then
   publishes the `usb,xhci` node, so firmware-before-bring-up holds by
   construction;
4. `drivers/bus/usb/xhci` brings the controller up over the shared `lib/usb`
   engine and publishes each device it enumerates;
5. `drivers/input/usb_hid` serves each HID interface.

The kernel's bootstrap floor is storage only — virtio-blk and EMMC2 (§18.6).
The chain's live acceptance (attach → keystroke, detach → `usb_hid` unloads
while the controller stays up, re-attach → autoloads again, and cold boot
with the keyboard unplugged then plugged in) is `plans/USB.md` UM.
**Metal-pending (`plans/OPEN-DEFECTS.md` D167):** a driver restart that
recovers its predecessor's quarantined DMA — `vcmailbox`'s
firmware-revision probe, which rests on the VideoCore answering property
requests in posting order, the VL805's `HCRST` before `UsbDevice::start`,
and GENET's `DMA_DISABLED` wait — each observed, after killing the driver
mid-traffic, as the successor's `DMA_QUARANTINE_RELEASED` record carrying
non-zero `bytes` (every bring-up records one, so the record alone proves
nothing).

**Remaining for P10 (GUI):** run `userland/gui/{wm,taskbar,session}` on
the HVS path so `userland/session/login` offers the launchable graphical session
when the display + input drivers are present (the headless build stays
first-class, §17.3).

#### What the BCM2711 PCIe bring-up must do

Each fact below was established on metal and is pinned by a
`drivers/bus/pcie_brcm` or `lib/pci` test; QEMU models no Pi PCIe (§0.4).

- **Touch no MISC register before the reset, and reset gently.** Until the
  always-accessible RGR1 bridge `sw_init` (`0x9210`) is released, every
  MISC-block access stalls for seconds and returns the root complex's
  master-abort poison `0xdead_dead` (not TAIRiX's all-ones "no device"
  sentinel). The bring-up releases only `sw_init`: it re-asserts no
  fundamental reset and leaves the SerDes `IDDQ` alone, so no firmware the
  previous boot stage left resident is dropped, and `train_link` makes the
  one `PERST#`-deassert edge
  (`reset_releases_sw_init_without_re_asserting_a_fundamental_reset`,
  `bring_up_releases_sw_init_before_touching_misc_and_skips_the_serdes_toggle`).
- **Program the root port as a bridge.** Its bus numbers (primary 0,
  secondary 1, subordinate equal to the secondary — there is no on-board
  switch), so configuration is forwarded; its Memory Base/Limit over the
  discovered outbound PCIe range, which must lie below 4 GiB and fails
  closed otherwise, so reads of the VL805's BAR are forwarded; and Memory
  Space + Bus Master in its Command register **last**, after link-up, since
  the root complex latches them only against a live link
  (`program_bridge_bus_numbers`, `program_bridge_mem_window`,
  `program_bridge_command`;
  `bring_up_names_the_downstream_bus_so_config_is_forwarded`,
  `bring_up_opens_the_bridge_memory_window_so_bar_reads_are_forwarded`,
  `bring_up_enables_memory_space_and_bus_master_on_the_bridge`).
- **Forward configuration only to device 0 of the secondary bus.** The root
  port is a single-device link: a forwarded configuration read nothing
  answers times out into a CPU external abort, so `lib/pci`'s windowed
  `mech_brcm` resolves every other downstream target to "no device" without
  touching the controller
  (`phantom_downstream_targets_are_no_device_without_an_index_write`).
- **The outbound window register packs the limit above the base.**
  `MISC_CPU_2_PCIE_MEM_WIN0_BASE_LIMIT` (`0x4070`) holds the limit in bits
  `[31:20]` and the base in `[15:4]`; transposed, the window is inverted and
  every BAR read master-aborts
  (`outbound_window_decodes_a_non_empty_range_covering_the_cpu_window`).
- **The inbound side.** An `RC_BAR2` the firmware configured is kept, since
  VideoCore's firmware load assumes it (`entry_inbound_window` reads the
  state `start4.elf` left;
  `bring_up_preserves_a_firmware_configured_inbound_window`).
  `MISC_CTRL.SCB0_SIZE` is sized to the DMA region: left at its reset value,
  the inbound decoder silently drops DMA past a small window while every
  configuration and outbound access still works (`encode_scb_size`, `0x11`
  for the Pi's 4 GiB viewport;
  `encode_scb_size_sizes_the_inbound_scb_window_to_the_region`).
- **Two address spaces.** A DMA buffer's device-visible address is its PCIe
  address: the Pi 4's inbound viewport maps PCIe
  `[0x4_0000_0000, 0x6_0000_0000)` onto RAM `[0, 0x2_0000_0000)`, and each
  side is bounded in its own space (`kernel/core::devres::translate_device_addr`).
  A BAR the reset left unassigned is placed in the outbound window by
  `PciBus::assign_bar`
  (`assign_bar_places_an_unassigned_64bit_bar_in_the_window`), and
  `lib/drvrt`'s `RtDriverHost` translates an outbound `BusWindow` BAR to the
  CPU window it maps.

#### What the VL805 and its xHCI need

- **Firmware.** The link bring-up's `PERST#` drops the VideoCore-loaded
  firmware on EEPROM-less boards, so `vl805` reloads it with one
  `NOTIFY_XHCI_RESET` (tag `0x30058`, `dev_addr` `0x10_0000`) once the BAR is
  based. The reply can take longer than the 1 s Linux allows, so the
  mailbox driver waits up to `REPLY_WINDOW_NS` (4 s). The reload is
  best-effort: the controller's own capability block at `Xhci::open` is the
  readiness gate, never the vendor version register at configuration
  `0x50`. A `GET_FIRMWARE_REVISION` probe (`vl805::probe_firmware_revision`)
  tells a dead mailbox path from VideoCore dropping only the xHCI tag. The
  property buffer sizes each tag's value buffer to the larger of request and
  response, and `find_tag` fails an oversized reply closed.
- **Controller start (`lib/usb`).** Only stale write-1-to-clear status is
  cleared before `HCRST`, and `CNR` is enforced after it
  (`open_resets_a_halted_controller_with_pre_reset_cnr_and_hse`). The
  scratchpad buffers `HCSPARAMS2` names — 31 pages on the VL805 — are
  reserved and `DCBAA[0]` pointed at them, without which no command
  completes (`start_reserves_scratchpad_and_programs_dcbaa0`). `PORTSC.PP`
  is asserted on a port-power-controlled controller before connects are
  debounced (`bring_up_connects_a_port_only_after_power`).
- **Enumeration (`lib/usb`).** `SET_PROTOCOL(boot)` and the interrupt-IN
  ring go only to a HID interface: a hub STALLs the request, halting the EP0
  it still needs, and an armed hub status pipe interleaves its events with
  the hub's EP0 `GET_STATUS` transfers
  (`enumerating_a_hub_leaves_ep0_usable_for_the_hub_descriptor`,
  `enumerating_a_hub_does_not_arm_its_interrupt_endpoint`). A device behind
  a hub is addressed on its own slot with the Route String and, at full or
  low speed behind a high-speed hub, the TT; the hub's slot is first marked
  a hub with its port count and TT think time, or its split transactions are
  never scheduled
  (`enumerate_downstream_hid_addresses_a_full_speed_keyboard_through_the_hub`,
  `addressing_a_downstream_keyboard_marks_the_parent_hub_as_a_hub`). The
  interrupt endpoint is taken from its descriptor — DCI, `wMaxPacketSize`
  and `bInterval` — with a non-zero Max ESIT Payload, or the periodic
  scheduler reserves nothing for it
  (`downstream_keyboard_is_serviced_on_its_descriptor_reported_endpoint`,
  `the_downstream_interrupt_endpoint_carries_a_nonzero_max_esit_payload`).
  A failed attach records its stage, the raw completion code of the last
  event it saw, and why the wait rejected it (`EnumStage`,
  `last_completion_code`, `last_reject_reason`, `last_event_type`).
- **DMA ordering.** The BCM2711's PCIe is not I/O-coherent. User-space DMA
  is Normal Non-Cacheable from `dma_alloc`, and `lib/usb` orders it with
  `lib/dma-barrier`: `dma_wmb` before the controller start and each
  doorbell, `dma_rmb` between an event's cycle bit and its body.

#### Pre-unlock signed driver store (design B)

**The constraint.** On metal the USB keyboard types the encrypted-root
unlock passphrase, so its drivers cannot be autoloaded *from* the encrypted
root: that volume is not mounted until the passphrase is entered.

**The decision (operator-approved): a dedicated read-only, signed `/System`
volume reachable before unlock.** Drivers keep their §16.2 home
(`/System/Drivers/`), not the FAT boot partition. Security holds without
encrypting `/System`: **every bundle is Ed25519-signed and verified against
the kernel's embedded trust anchor at load (§18.6)**, so a tampered
read-only store fails closed, and `/System` holds no secrets. The encrypted
root keeps only the secret-bearing user data (`/Users`, app/user installs,
`/Storage`).

**On-disk layout (the §16/§11 image; `tools/mkimage`):** three MBR
partitions — (1) FAT boot (firmware, kernel, `root.unlock`); (2) **`/System`
— read-only `ARXFS`, unencrypted, the signed-bundle store**; (3) the data
root — encrypted `ARXFS` carrying `/Users`/`/Apps`/`/Storage` and
`/System/Security/Users`. The installer (§11) authors the same split, and
expert mode may not collapse it.

**Boot sequencing (no ambient authority, fail closed §5.4):**
1. the storage bootstrap floor (virtio-blk or EMMC2) binds the root block
   device;
2. the unlock kthread mounts the read-only `/System` volume and autoloads
   its signed store against the discovered tree (`devmgr` match → signed
   gate → user-space spawn); the bus drivers publish what they enumerate,
   and each published node autoloads in turn, bringing the keyboard up;
3. it unlocks the data root and installs the users database. The blank
   passphrase is tried silently first, so the installer image unlocks with
   no prompt (§11); only a non-blank passphrase draws the keyboard-served
   `Root passphrase:` prompt.

What the finished parts guarantee:

- **B1 — the split.** `lib/partition` carries the `ARXFSSystem` role (MBR type
  `0x7e`, GPT `TYPE_GUID_ARXFS_SYSTEM`). `ARXFS::open_read_only` fails every
  mutator closed. The `/System` volume is keyed by the non-secret
  `SYSTEM_VOLUME_KEY`, its integrity resting on the per-bundle signatures.
  `root_mount::with_system_volume` mounts it read-only only once its
  `Drivers` store is found (`SYSTEM_VOLUME_MOUNTED` 4140, each decline
  `SYSTEM_VOLUME_UNAVAILABLE` 4141) and never aborts the boot.
- **B2 — pre-unlock autoload.** The store is scanned relative to the volume's
  own root, at `SYSTEM_VOLUME_STORE_PATH` (`/Drivers`). `tools/xtask`'s
  `image_drivers` installs the signed bus and USB class bundles there;
  `autoload_caps` carries `CAP_HW_EMIT` and `CAP_MAILBOX` for them, each
  driver still bounded by its manifest's intersection. Proven on `-M virt`
  by `autoload_input_qemu_aarch64` (discover → signed gate → spawn →
  `key_inject`, all before the unlock).
- **B4 — the EMMC2 root.** The unlock kthread dispatches on the floor block
  driver bound — virtio-blk or EMMC2 — into one shared `finish_unlock`
  tail. EMMC2 is admitted through the signed load gate and maps only its
  node's sole register window. Its two bring-up facts: the controller reset
  clears SD Bus Power, so the card rail is powered (3.3 V) before clocking;
  and the controller right-aligns the CRC-stripped R2 response, so
  `CSD_STRUCTURE` is `RESP3[23:22]`
  (`a_stalled_or_unpowered_controller_fails_closed_at_the_first_command`,
  `structure_bits_above_the_field_are_not_read_as_v2`).
- **The hand-off to login.** Login waits on the unlock rather than racing
  its prompt (P11). `unlock_root_disk_interactively`'s `on_resolved`
  releases console 0 on every outcome and wakes any type-ahead
  (`the_console_is_released_on_every_unlock_outcome`). Neither the silent
  blank-passphrase probe nor login's pending `users_db_read` poll logs as an
  error: a wrong passphrase is `ROOT_UNLOCK_KEY_REJECTED` (4142, Debug)
  while a structural refusal stays `ROOT_MOUNT_REJECTED` (4134, Error), and
  an audited handler's `WouldBlock` and `NotFound` are
  `SYSCALL_HANDLER_WOULD_BLOCK` and `SYSCALL_HANDLER_NOT_FOUND` (5005 and
  5006, Debug), never the 5004 rejection.

**Done when:** on real hardware the desktop composites through `rpi_hvs`,
the taskbar renders, and a USB keyboard/mouse drives the WM; a recorded
demo (photo + UART log) is the acceptance artefact. Headless `-M raspi4b`
CI stays green throughout.

### P11 — Login on the consoles

Every *text* console (screen, UART) that reaches user mode sits at a
`login:` prompt; an authenticated user's **shell of choice** is started as
their session. The video console and the UART console are **separate
session contexts** (separate stream backings, separate login instances), so
two users — or the same user twice — can be logged in concurrently.

**Landed — the credential foundation (host-proven):**

- `lib/crypto`: PBKDF2-HMAC-SHA256 (`pbkdf2_sha256` / `pbkdf2_sha256_verify`,
  published vectors, `ct_eq` comparison) — the password derivation the user
  database stores.
- `lib/users` (`tairix-users`): the `/System/Security/Users` `users-v1`
  format — full §5.1 account identity (username, uid/gids, display name,
  home, shell of choice, `CAP_*` grant ceiling, `active`/`locked` state,
  salted PBKDF2 record at a bounded per-record cost), fail-closed bounded
  parser (64 KiB / 512-byte lines / 512 records, unique usernames + uids),
  exact-round-trip serialiser, and `authenticate` with one indistinguishable
  refusal + a dummy derivation at the database's highest cost for unknown /
  locked accounts (§19.1). Fuzz harness `fuzz_users` enrolled in
  `cargo xtask fuzz`. Docs: `docs/src/lib/users.md`.
- `userland/session/login::auth::UsersAuthenticator`: the production
  `Authenticator` seam over a parsed `UsersDb`; every refusal is the same
  `Errno::PermissionDenied`. Login's `Uid`/`Gid` now come from `lib/users`.
- `tools/mkimage` image profiles: `cargo xtask image --target aarch64-rpi
  [--profile debug|installer]` emits
  `images/tairix-aarch64-rpi-<profile>.img`. The **debug** image seeds
  `/System/Security/Users` with the `root`/`root` test account (per-build
  random salt, default cost, explicit admin cap ceiling); the **installer**
  image seeds none (the §11 installer authors it on first boot). Proven by
  mkimage host tests mounting the built root and authenticating.
- **Root-volume read path at boot** (former increment 1):
  `tairix_kernel_core::users::load_users_db` reads
  `/System/Security/Users` off the mounted root volume's
  `FilesystemRead` + `FilesystemSecurity` driver through the VFS's
  §5.3-checked per-inode delegation — the root mount carries the volume's
  driver (`MountTable::back_root`, exactly once), the file is bounded
  against the format's 64 KiB maximum *before* reading, and the bytes go
  through the fail-closed `tairix-users` parser. The read runs under the
  kernel bootstrap identity (`uid 0`, **no** capabilities — a
  capability-gated or unreadable record refuses, §5.1/§5.4); every
  outcome is audited (`USERS_DB_LOADED` 4040 / `USERS_DB_REJECTED` 4041)
  and any refusal leaves **no** database. Proven by kernel/core unit
  tests (every refusal), the `arxfs_image` users-root fixture round
  trip through the real driver, and the `users_db_qemu_aarch64` `-M
  virt` vertical (virtio-blk MMIO → arxfs mount → loader →
  authenticate). The Pi's metal root mount (P8/P9) and the
  volume-key hand-off to the loader on metal ride the P8/P9 metal
  items.
- **The login `Run` binary + the `users_db_read` delivery seam**
  (former increment 1): the login service ships at
  `/System/Services/login.app/Run` (`userland/session/login/src/run.rs`, a
  `tairix-rt` program) and PID 1 `init`'s `session` directive points at
  it. The kernel-held database is delivered through the new `abi-v1`
  syscall **`users_db_read`** (no. 19, gated on the new
  **`CAP_USERS_READ`** (21), audited): the kernel serves the exact
  `users-v1` text from the `kernel/core::users::UsersDbSource` seam
  (installed via `with_users_db`; fail-closed `NotImplemented` unwired /
  `NotFound` with no database / `BufferTooSmall` rather than truncate),
  and login re-parses it with the same fail-closed `tairix-users`
  parser. With no database (installer image, no root volume) login wires
  a deny-all authenticator — the prompt stays up and every attempt is
  refused (§5.4.5). Design B spawns `login` **before** the in-kernel unlock
  kthread mounts the encrypted root, and the kthread prompts for the
  passphrase on the **same** console, so the `users_db_read` seam is a
  **three-state** machine (`LateUsersDb`) and `tairix_login::supervise`
  acts on it **before every round**: while the unlock is still running the
  read returns `WouldBlock` and `login` *waits without prompting* (yielding
  the CPU) so the unlock owns the console and the two prompts never collide;
  a delivered database wires `UsersAuthenticator`; and once the unlock
  resolves with no database (installer image, or it gave up) the read
  returns `NotImplemented` and the fail-closed `DenyAll` prompt runs. The
  kernel pairs `LateUsersDb::resolve` with opening the console-0 input gate
  through one `release_console0_to_login` helper (§2.2), so a `login` parked
  on the pending read always makes progress. This fixes the two metal
  P8/B4 login failures (`root`/`root` refused after unlock; the premature
  `Username:` prompt). Regressions: kernel-core
  `the_late_users_db_is_pending_until_the_unlock_resolves` /
  `a_resolved_empty_late_users_db_fails_closed_not_implemented` /
  `an_installed_database_wins_over_a_later_resolve`, and login
  `login_waits_while_pending_then_authenticates_once_installed` /
  `an_absent_database_prompts_deny_all_without_waiting` /
  `the_users_database_is_reloaded_before_each_round`.
  The `SessionLauncher` spawns the authenticated
  record's **shell of choice** via `spawn`/`wait`; the embedded-program
  registry now carries **per-program capability grants + argument
  vectors** (`EmbeddedProgram.caps`/`.args`, all three arch producers —
  login holds the console pair + `CAP_PROC_SPAWN` + `CAP_USERS_READ`,
  the shell only the console pair). Proven by kernel/core +
  kernel/syscall + login unit tests and the reworked
  `spawn_session_qemu_{aarch64,x86_64}` verticals (init supervises
  login; the aarch64 vertical holds an ordered scripted dialogue over
  the runner's multi-step serial script — `root` → `Password: ` →
  refused password → `Login incorrect` → second `Username: ` → a
  513-byte over-bound line → fail-closed exit → reap → relaunch — and
  the runner fails the run if the guest exits before every scripted
  prompt appeared, so a login crashing per keystroke cannot pass on
  relaunch event counts alone).
  Login's **entire prompt/credential input path is allocation-free** —
  the userland heap's production `mem_map` producer is staged
  (`plans/SPAWN.md` SP5b), so any allocation there would abort the
  process (the original per-keystroke `Vec::push` did exactly that on
  metal: every typed character killed login and `init`'s relaunch
  re-printed `Username: `). `tairix_login::Prompt` fills caller stack
  buffers (`INPUT_LINE_MAX` = 512), `Credentials` borrows `&str`, and
  `Login::run` zeroes the password buffer after every attempt; only the
  authenticate path (which parses a delivered database) still needs
  SP5b and the P8 root mount.
- **Beacon + bring-up debug removal** (former increment 2): the
  boot-progress beacons (`boot_aarch64`/`serial`/`video`) and the
  serial bring-up mirror in `kernel/arch/aarch64/src/serial.rs` are
  deleted. The **boot-log** path routes by build profile
  (`serial::ConsoleWriter`): a **release build** is video-first with the
  UART as the fallback (`AGENTS.md` §10), while a **debug build**
  (`cfg(debug_assertions)`) routes the whole log/debug stream to the
  **UART instead** — even while a login session owns the UART — so a
  serial capture of a development boot carries the full diagnostic
  stream while the screen stays clear for the user-facing session; with
  no UART discovered the bounded transmit drops the bytes and the
  screen is never the debug log's sink. Because the single freestanding
  kernel cannot read which image it was planted in, the routing is tied
  to the **image profile** by building the kernel in the matching Cargo
  profile (`tools/xtask` `kernel_build_profile`): `--profile debug`
  compiles a `dev` (`debug_assertions`-on) kernel that logs to the UART,
  `--profile installer` compiles a `--release` kernel that logs on
  screen. The earlier defect was building both images from one
  `--release` kernel, so `debug_assertions` was always off and the debug
  log never reached the UART.
- **Separate console contexts — LANDED** (former increment 1, minus
  echo). The video console and the UART are independent stream backings
  with their own login sessions:
  - `tairix_abi::DescriptorTable` records, per standard descriptor, the
    installed-console index backing it (`standard_on(console)`;
    `standard()` = console 0). `spawn` (now 3-arg) takes a `console`
    selector — `CONSOLE_INHERIT` (all-ones sentinel) copies the
    caller's own table (login's shell stays on login's console), any
    other value names a validated installed-console index and fails
    closed with `NotFound` otherwise. New `abi-v1` syscall
    **`console_count`** (no. 20, `CAP_CONSOLE_WRITE`, unaudited)
    reports the installed-list length. C view regenerated
    (`tairix_sys_spawn` 3-arg, `tairix_sys_console_count`,
    `TAIRIX_CONSOLE_INHERIT`).
  - kernel-core holds a `'static [ConsoleDevice]` list
    (`BootInfo::with_consoles` → `KernelSyscallHandlers::with_consoles`;
    empty fail-closed default). `stream_write`/`stream_read` resolve
    the descriptor's direction first, then its console index against
    the list (missing console → `NotImplemented`); the init pipeline
    wraps every listed read half in `BlockingConsoleRead`.
    `KernelSpawnCtx` carries the spawner-resolved table to admit.
  - aarch64 installs `[VideoConsole, UartConsole]` when the P7b
    framebuffer console is active, else `[UartConsole]`.
    `serial::write_console_bytes` is now UART-only (the UART console's
    write half, its own login); `VideoConsole` writes through
    `video::write_bytes` and reads from the **keyboard seam** — a
    directly attached USB-HID / PS/2 keyboard once the P10 input wiring
    lands; until then every poll reports "no input pending" and the
    reader parks at its prompt rather than borrowing the UART's bytes.
    x86_64 (COM1) and riscv64 (SBI) list single write-only consoles
    with fail-closed `NULL_CONSOLE_READ` read halves — behaviour
    unchanged.
  - PID 1 `init` supervises **one login per discovered console**
    (`userland/system/init/src/supervisor.rs`, host-tested over the
    `Sessions` seam): `console_count` → `spawn_at(session, console)`
    fan-out, wait-any reaping, relaunch on the exited session's own
    console within a per-console `SESSION_SPAWN_BUDGET`, exhaustion /
    spawn / wait failures and a zero-console system fail closed
    (`EXIT_NO_CONSOLES` 74). The bootstrap slot table is a fixed
    8-entry stack array until the userland heap (SP5b) lets it size
    from the count.
  - Proven by kernel-core unit tests (per-descriptor console routing
    both directions, console_count, spawn explicit/invalid/inherit
    attachment), lib/rt + abi-sys marshalling tests, and the init
    supervisor host tests; the `-M virt` verticals ride the unchanged
    single-UART list.
- **Stream-layer echo + echo control — LANDED.** Terminal local echo is
  the kernel's read line-discipline behaviour, not a per-program job
  (§2.2): `ConsoleDevice` carries a per-console `echo` flag (default
  on), and `stream_read` writes the bytes it consumes back to the same
  console's write half, rendering a bare CR/LF as CR-LF, so a typed
  username is visible. The **discipline-control contract** is the
  `abi-v1` syscall **`stream_input_mode`** (no. 21, `CAP_CONSOLE_READ`,
  unaudited): `stream_input_mode(fd, mode)` selects the resolved input
  console's discipline — cooked (echo on, the default), secret (echo
  off, activity indicator on — a password read), or raw (echo off,
  nothing drawn — a full-screen program's read); the reserved `0` and
  unknown modes fail closed. `login` selects secret around the password
  read and restores cooked after, so a credential is never rendered, and
  fails the read closed if the mode cannot be selected (`AGENTS.md`
  §5.4); `top`/`man` select raw so neither echo nor marker paints over
  their displays. First-party wrapper `tairix_rt::set_input_mode`; C
  stub `tairix_sys_stream_input_mode` (header regenerated). Proven by
  kernel-core tests (echo to the write half + CR/LF translation, secret
  and raw suppressing echo, fail-closed on the reserved/unknown mode and
  a non-read fd), console.rs `echo_bytes` unit tests, and lib/rt +
  abi-sys marshalling tests.
- **Read-line editing (erase rub-out) — LANDED.** The read line
  discipline edits the line, not just echoes it. The erase vocabulary is
  one shared `lib/vt` definition (`control::is_line_erase` — Backspace
  `BS` or Delete `DEL` — plus the Delete key's `CSI 3 ~` escape sequence
  via the `line::EraseSeq` recogniser held across split reads, and the
  `ERASE_ECHO` `BS SP BS` rub-out, §2.2), so the kernel echo and the
  reader's buffer can never disagree on what erased. Kernel **echo** half
  (`ConsoleDevice::echo_bytes`): an erase rubs out the previous character
  instead of painting stray control glyphs, bounded by a per-console line
  state (`EchoLine` — column + held sequence prefix, reset on CR/LF and on
  every `set_input_mode` change) so an erase at the start of the input line
  never walks back over the prompt; the state persists across the many
  per-byte `stream_read` drains one logical input line spans. Reader
  **buffer** half (`tairix_vt::line::LineEditor`, a host-tested
  allocation-free editor shared by the root-unlock passphrase prompt,
  login's prompt reads, and the shell REPL's line reader): CR/LF completes
  the line, an erase pops the last byte (zeroed on removal, §4) and is
  never stored, any other byte appends or fails closed `TooLong`;
  `login::run::read_line_raw` drives it. Proven by the `lib/vt` control +
  `line` tests and the `console.rs` erase tests (rub-out, BS-as-erase,
  Delete-sequence rub-out incl. split reads, no-op at line start,
  persistence across calls, CR/LF reset, `set_input_mode` reset). Docs:
  `docs/src/architecture/syscalls.md`, `docs/src/lib/vt.md`,
  `docs/src/userland/login.md`.
- **Secret-entry feedback (`[input active...]`) — LANDED.** Every
  echo-suppressed (password) prompt shows the shared activity marker: the
  pure `tairix_vt::secret::SecretIndicator` state machine (show on first
  typed character, dots cycling `.`/`..`/`...` on a one-second cadence).
  The animation is bounded: it runs for at least three seconds
  (`SECRET_ANIMATE_NS`) after the most recent keystroke and then freezes
  (marker stays, dots stop), a later keystroke restarts it; on Enter the
  marker is replaced in place with `[input complete]`, and erasing back to
  empty (or aborting the read) removes an in-progress marker while a
  completed one is left on screen. Hosted per console as the kernel
  `SecretFeedback` (`kernel/core` console). `set_input_mode(Secret)` arms
  it (login's password read — the raw mode never does, so a full-screen
  program's keystrokes draw nothing), the root-unlock kthread arms its
  own instance
  around the passphrase prompt, and the blocking console readers
  (`BlockingConsoleRead`, `KthreadConsoleRead`) feed it the consumed bytes
  and drive its one-shot animation deadline through the `CONSOLE_WAITQ`
  timed park (`rearm_timed_wakeup` / `wait_now_ns`) — armed only while the
  dots are moving, so a prompt with nothing typed (and a frozen marker)
  takes no timer wake-ups (tickless, §17.1). Only the typed-character
  *count* is tracked; no secret byte is stored or rendered. The aarch64
  framebuffer text console honours Backspace (cursor back one column, no
  glyph), so the marker's rub-outs and dot redraws render correctly on the
  HDMI console, not as `?` fallback glyphs. Proven by `tairix_vt::secret`
  tests and the `console.rs` feedback tests (inert until armed, marker
  show, freeze after the window, resume/extend on typing, `[input
  complete]` on Enter, erase/abort removal, secret-mode arm / cooked- and
  raw-mode disarm) plus the `video.rs` backspace tests.
- **Keyboard input for the video console — kernel-side delivery seam
  LANDED.** The video console's read half is now a kernel-side type-ahead
  queue a keyboard-input driver feeds, not the inert `Ok(0)` poll a
  display-with-no-keyboard returned. New `abi-v1` syscall
  **`console_input`** (no. 22, gated on new **`CAP_INPUT_INJECT`** (22),
  unaudited): a driver that has decoded a directly attached keyboard
  pushes the decoded console bytes into a target installed-console index;
  the kernel copies them in (capability- and bounds-checked, §5.4),
  enqueues them on that console's `tairix_kernel_core::ConsoleInputQueue`
  (a bounded type-ahead ring that is both the console's `ConsoleRead`
  half — drained by a video-login `stream_read`, waking a reader parked
  in `BlockingConsoleRead` — and its `ConsoleInput` half), and zeroes
  each byte as the consumer drains it (a typed password transits it,
  §4 / §23.1). `ConsoleDevice` gained an `input` half (default
  `NULL_CONSOLE_INPUT`, fail-closed; preserved across the init
  `BlockingConsoleRead` rebuild); aarch64's `VIDEO_ONLY_CONSOLES[0]`
  is backed by the shared `VIDEO_KEYBOARD` queue, so the
  video login takes input only from its own keyboard, never the serial
  line (with a display active the UART is not installed as a console at
  all). First-party wrapper `tairix_rt::console_input`; C stub
  `tairix_sys_console_input` (header regenerated). Proven by kernel-core
  queue unit tests (FIFO drain, short read, ring wrap, overflow short
  push) + the `console_input` handler tests (push→read round trip;
  fail-closed for an unknown console and for a non-injectable UART
  console) + lib/rt / abi-sys marshalling tests.
- **Keyboard input for the video console — host-side producer LANDED.**
  The producer that turns decoded key events into the bytes
  `console_input` injects is now host-proven. The shared terminal key
  map is the new `lib/keymap` crate (`tairix-keymap`): `encode_key(Key,
  Modifiers, &mut [u8])` writes the console (tty) bytes one key press
  sends — a printable char (UTF-8, with the `Ctrl` C0-control and `Alt`
  meta-prefix arithmetic), `Enter`→CR, `Backspace`→DEL, `Tab`, `Escape`,
  the arrows (`ESC [ A`..`D`), and the editing/nav/function keys via the
  canonical `lib/vt` `SS3`/`CSI … ~` tables (no second escape definition,
  §2.2). It is `no_std`, allocation-free (writes a caller buffer, so it
  works before the SP5b userland heap), and fail-closed (`MAX_KEY_BYTES`
  bounds it; an unmappable key emits nothing). `drivers/input/usb_hid`
  gained a `console` module: the US HID-usage→`tairix_input::Key` table
  (letters/digits/shifted symbols/named keys/keypad), a stateful
  `KeyboardConsole` tracking the modifier bits + caps/num lock, and
  `pump_once` — the driver loop that polls the keyboard, feeds each event
  through `KeyboardConsole::feed` + `encode_key`, and injects the bytes
  through a `ConsoleSink` (on metal a `console_input` call against the
  video console's index; host tests use a recording sink). The
  HID-usage→`Key` half is HID-specific (a `ps2` keyboard resolves
  scancode set 1 into the same vocabulary, reusing `lib/keymap`); the
  `Key`→bytes half is the one shared map (§2.2). Proven by 13 `lib/keymap`
  unit tests + 13 usb_hid `console` tests (layout, ctrl/alt, caps/num
  lock, named/arrow/function sequences, fail-closed cases, and the full
  `BootKeyboard`→keymap→sink "hi" chain). Docs:
  `docs/src/lib/keymap.md`, `docs/src/drivers/input.md`.

**Remaining (next increments, in order):**

1. **Keyboard input for the video console — VL805/xHCI metal delivery.**
   The kernel delivery seam (`console_input` + `ConsoleInputQueue`) and
   the host-side producer (`lib/keymap` + the usb_hid `console` module
   above) are landed; what remains is the **metal** path that feeds the
   producer real reports: the USB-HID-over-xHCI **VL805** wiring (the P10
   alternative track) so the driver's `pump_once` loop runs against a
   real keyboard and injects into the video console. QEMU models no Pi
   USB, so this is a metal checklist (`AGENTS.md` §20 / §0.4; the UART
   stays its own session).
2. **Configurable log policy** — the log output/direction (which
   consoles/sinks receive log lines), rotation, and on-storage age
   limits become administrator-settable configuration under
   `/System/Settings` (§16.2), replacing the compiled-in routing;
   requires the persistent `/System/Logs` store (§19.4) before rotation
   and age limits are meaningful. The debug-build dual echo stays a
   debug-only exception.
3. **Login over a real database in production.** The passphrase-derived
   root-unlock **primitive is landed**: `drivers/filesystem/arxfs`'s
   `unlock` module (`UnlockDescriptor` — PBKDF2-HMAC-SHA256 over a
   per-volume random salt + bounded iteration count, fail-closed
   encode/decode, `derive_volume_key`) turns an operator passphrase into
   the volume's `VolumeKey` (`AGENTS.md` §11). It is the LUKS-style
   indirection above the always-encrypted volume; the plaintext
   descriptor rides beside the volume (the FAT boot partition on a Pi
   image). A wrong passphrase derives the wrong key and `ARXFS::open`
   refuses it (`PermissionDenied`) — no separate oracle. Host-proven:
   the `unlock` unit tests plus an end-to-end arxfs test that formats a
   volume under a passphrase-derived key and re-mounts it (wrong
   passphrase refused). Docs: `docs/src/filesystem/arxfs-spec.md` §7
   (incl. the §19.9 TPM/secure-boot future hand-off, which seals the key
   to a measured boot and falls back to the passphrase).

   **Image authoring is landed** (`tools/mkimage`): `build_rpi_image`
   provisions a per-volume `UnlockDescriptor` (random salt +
   `UNLOCK_DEFAULT_ITERATIONS`) and derives the volume key from the
   profile's `passphrase_for` — `INSTALLER_PASSPHRASE` (**blank**, the
   installer image re-provisioned at install time) or `DEBUG_PASSPHRASE`
   (`root`, the never-shipped debug image). The passphrase is not a
   `build_rpi_image` argument: deriving it from the profile makes it
   impossible to provision an image under a passphrase that disagrees with
   the prompt (§2.2). It provisions the encrypted root under the derived
   key and plants the plaintext descriptor on the FAT boot partition as
   `root.unlock` (`fatboot::ROOT_UNLOCK_NAME`). The boot path tries the
   blank passphrase silently, so the installer unlocks with no prompt; the
   derived key is also emitted to `.rootkey` for host mounting and is
   re-derivable from `root.unlock` + the profile passphrase. Host-proven by
   the mkimage tests (the on-FAT descriptor re-derives the exact key per
   profile; a wrong passphrase is refused with no separate oracle, §5.4).
   Docs: `docs/src/install/raspberry_pi.md`.

   **Still staged** (each its own increment; the chain end to end is
   gated on these):
   - **Installer-authored production root.** The §11 installer first-boot
     flow provisions the *user's* encrypted root under their **chosen**
     passphrase and writes its descriptor — a real, operator-set
     passphrase, never the blank `mkimage` default. (The blank-passphrase
     `mkimage` images above are the development/first-boot artefacts only.)
   - **Boot mount.** The production boot reads the descriptor from the
     boot partition, prompts for the passphrase on the console, derives
     the key, mounts the discovered root volume (EMMC2 on metal /
     virtio-blk on `virt`), runs the kernel-side read, and installs the
     held text into the production dispatch hook.
     - **Kernel-neutral install seam — LANDED.** The architecture-neutral
       half of this — the §17.4 boundary between the install *policy* and
       the board storage bring-up that *produces* the mounted driver — is
       wired: `tairix_kernel_core::users::load_users_db_source` reads
       `/System/Security/Users` off a mounted root FS driver (sharing the
       §5.3-checked read, the fail-closed parse, and the `USERS_DB_*`
       audit with `load_users_db`, §2.2) and returns a
       `HeldUsersDbSource` owning the canonical `users-v1` text (zeroed on
       drop §4, redacted `Debug`). A boot path `Box::leak`s the holder and
       installs it through the new `BootInfo::with_users_db`;
       `kernel_main` threads it into the `KernelDispatchHook` so
       `users_db_read` serves it. The default stays the fail-closed
       `NULL_USERS_DB` (login refuses every attempt) until a boot path
       calls `with_users_db`. Host-proven (kernel/core: 3
       `load_users_db_source` cases + the `BootInfo::with_users_db`
       builder). No `lib/abi`/C-header change (`BootInfo` is a kernel/core
       handover type, not FFI).
     - **Chunk A — the arch-neutral unlock + mount + load composition —
       LANDED (host-proven).** `tairix_kernel::root_mount::unlock_root_and_load_users`
       (`kernel/tairix-kernel/src/root_mount.rs`) is the one composition —
       in the `Layer::Tooling` bin crate, the only layer permitted to name
       both the `arxfs` driver and `kernel/core` (§17.4) — that turns the
       three artefacts a boot path recovers off storage into the served
       `users-v1` database: the plaintext `root.unlock` descriptor, the
       typed passphrase, and the encrypted root `Block` device. It decodes
       the descriptor fail-closed (`UnlockDescriptor::decode`, §5.4.3),
       derives the volume key from the passphrase (PBKDF2-HMAC-SHA256) in a
       `Zeroizing` wrapper (§4 — wiped on drop, the audited `zeroize` crate,
       no hand-rolled primitive), mounts the encrypted root (`ARXFS::open`
       — a wrong passphrase refused with `PermissionDenied`, no plaintext
       fallback, no separate oracle, §4 / §5.4), then runs
       `load_users_db_source`. Every refusal is audited (`4133`
       `ROOT_MOUNT_UNLOCKED` / `4134` `ROOT_MOUNT_REJECTED`; no secret ever
       logged, §19.4) and yields no database (§5.4.5). Host-proven by 4
       tests (the correct passphrase unlocks the volume and the served text
       parses + authenticates the planted `root`/`root`; a wrong passphrase,
       a tampered descriptor, and a non-arxfs volume each refused
       fail-closed). The shared `tairix_test_arxfs_image` fixture gained
       `build_users_root_image_with_key` + `VecBlock::from_bytes` so the
       test authors a real volume under a passphrase-derived key through the
       one on-disk-layout source of truth (§2.2). No `lib/abi`/C-header
       change.
     - **Chunk B-1 — the FAT `root.unlock` reader — LANDED (host-proven).**
       `tairix_kernel::root_mount::read_root_unlock_descriptor` reads the
       plaintext `root.unlock` key-derivation descriptor off the FAT boot
       partition through the **same** real FAT32 driver `tools/mkimage`
       authored it with — one on-disk definition for writer and reader
       (§2.2). The file name is now the shared
       `tairix_drv_fs_arxfs::ROOT_UNLOCK_NAME` constant (hoisted from
       `tools/mkimage::fatboot`, which re-exports it; §2.2 / §2.14). The
       descriptor is a fixed-length record, so the read is strictly bounded
       and fail-closed (§5.4 / §24.4): the entry's size is checked to be
       **exactly** `UNLOCK_DESCRIPTOR_LEN` *before* a byte is read (both a
       truncated and an over-long file refused), and the bytes are still
       re-validated by `UnlockDescriptor::decode` inside
       `unlock_root_and_load_users` (§5.4.3). Host-proven by 6 reader tests
       (read-back + decode, end-to-end feed into `unlock_root_and_load_users`,
       missing → `NotFound`, truncated/over-long → `OutOfRange`, unformatted
       partition refused) over a FAT volume authored through the real FAT32
       driver. No `lib/abi`/C-header change.
     - **Chunk B-2 boot-path entry composition — LANDED (host-proven).**
       `tairix_kernel::root_mount::mount_root_and_load_users` is the single
       boot-path entry that threads Chunk B-1 into Chunk A: given the two
       brought-up `Block` devices (FAT boot partition + encrypted root) and
       the typed passphrase it reads the `root.unlock` descriptor
       (`read_root_unlock_descriptor`) and, on success, calls
       `unlock_root_and_load_users` — so the boot path neither re-threads the
       descriptor buffer nor reconciles two error taxonomies itself (§2.2). A
       descriptor that cannot be read off the boot partition is audited
       (`4134`) and returned as the new `RootMountError::DescriptorRead`
       (cause `descriptor_unreadable`); the encrypted root is never touched
       and no database is served (§2.9 / §5.4.5). Host-proven by 3 tests
       (end-to-end success authenticating the planted `root`/`root`; a
       missing descriptor refused with no unlock; a wrong passphrase refused
       fail-closed). No `lib/abi`/C-header change.
     - **Chunk B-2 single-disk partition split — LANDED (host-proven).**
       `tairix_kernel::root_mount::mount_root_disk_and_load_users` is the
       entry for the common case: **one** whole-disk `Block` device. It
       parses the partition table through the shared, scheme-neutral
       `lib/partition` layer — MBR encode (the image author) + fail-closed
       MBR/GPT parse (the boot reader), the one on-disk definition
       `tools/mkimage` writes (§2.2), so it reads a Pi MBR card **and** a
       UEFI x86_64 GPT disk with no board `cfg` (§2.20). It locates the FAT
       boot and `ARXFS` root partitions **by role** (not by index), opens a
       bounds-checked `PartitionBlock` window onto each **in sequence** (one
       device, two windows, via the new `impl Block for &mut B` forwarding in
       `lib/abi`), reads the descriptor off the boot window, then mounts the
       root window. A malformed/forged table, a missing FAT boot or `ARXFS`
       root partition, or an out-of-range extent is audited (`4134`) and
       returned (`RootMountError::PartitionTable`/`NoBootPartition`/
       `NoRootPartition`/`PartitionWindow`); no database is served (§2.9 /
       §5.4). Host-proven by 3 tests (whole-disk MBR split → unlock → load
       authenticating the planted account; no-table and no-root-partition
       refusals) plus the `lib/partition` MBR/GPT parse + fuzz suite. No
       `lib/abi` ABI change (the `&mut B` `Block` impl is a forwarding impl,
       not a new method — no C-header regen).
     - **Chunk B-2 root-storage bind gate — LANDED (host-proven).**
       `tairix_kernel::root_storage` resolves which **discovered**
       hardware-tree node carries the bootstrap root block device, and which
       floor block driver binds it, through the **same** shared `lib/devmatch`
       policy the user-space `devmgr` autoloader uses — applied against the
       in-kernel bootstrap-floor catalogue (`driver_catalog`, the
       `provides_root_block` entries: virtio-blk + EMMC2). The kernel binds a
       block driver because that driver's signed bind table matched a
       discovered node's identity, never because it *hunted* for a disk
       (§18.3 / §18.5 / §18.6). A streaming `RootBlockSelection`
       resolves each node off the discovery sink with no whole-tree buffer
       (§2.16). It is **resolution only** — it mounts nothing — so the
       metal-confirmed boot is unaffected (§2.17). Fail closed (§2.9):
       no block device leaves the root unbound (informational, §18.4); a
       directly-described device (EMMC2 `compatible`) binds straight from the
       tree while a bus-probed one (virtio-blk device id) binds only once the
       bus driver attaches the probed child (§18.2); more than one distinct
       block device fails closed as ambiguous (root disambiguation needs an
       explicit boot descriptor, not a guess). Wired into the aarch64 boot
       path (`aarch64::boot::audit_root_storage_binding`, post-MMU before the
       core hand-off) and audited `4135` `ROOT_STORAGE_AUTOLOAD`. Host-proven
       (8 `root_storage` tests + the `driver_catalog` block-flag test);
       aarch64 kernel builds freestanding. No `lib/abi`/C-header change.
     - **Chunk B-2 late-bound users-db cell — LANDED (host-proven).**
       `tairix_kernel_core::LateUsersDb` (`kernel/core/src/users.rs`) is the
       set-once `UsersDbSource` the post-boot unlock step publishes the
       mounted database into. The encrypted root is unlocked only **after**
       the console keyboard is live (the operator types the passphrase, §11),
       which is past where `BootInfo::with_users_db` is consumed — so the
       dispatch hook holds a `&'static LateUsersDb` from boot and reads
       `text()` on every `users_db_read`, and the unlock step installs the
       loaded database into the same cell once it exists. It is a
       `OnceCell<HeldUsersDbSource>` that fails closed (`Errno::NotImplemented`,
       like `NULL_USERS_DB`) until `install` succeeds (§5.4.5), is **immutable
       after the first install** (set-once; a later install returns
       `UsersDbAlreadyInstalled`, so no post-unlock path can swap the live
       credential database, §5.4), zeroes the rejected duplicate's and its own
       credential bytes (§4), and exposes **no syscall surface** (`install` is
       internal kernel code, not an ABI method). Host-proven by 3 `users_tests`
       (fails-closed-until-installed; serves-installed-text + re-parse +
       authenticate; set-once-refuses-replacement). No `lib/abi`/C-header
       change.
     - **Chunk B-2 interactive unlock policy — LANDED (host-proven).**
       `tairix_kernel::root_mount::unlock_root_disk_interactively` is the
       device-independent prompt + retry + install policy the in-kernel
       unlock kthread runs once the board has brought up the root block
       device and the console keyboard is live. Generic over the `Block`
       disk and taking the console halves as the object-safe
       `tairix_kernel_core::{ConsoleWrite, ConsoleRead}` seams, it names no
       arch or device type (§17.4). Each attempt prompts
       `ARXFS passphrase:`, reads one line into a zeroized,
       fixed-length on-stack buffer
       (`MAX_PASSPHRASE_LEN`; the secret never reaches the heap, a log, or
       memory beyond the attempt, §4 / §19.4; Backspace edits, an over-long
       line is a refused attempt not a truncated secret, §5.4.3), and runs
       `mount_root_disk_and_load_users`. Success publishes the loaded
       database into the set-once `LateUsersDb` (`4136`) and collapses the
       prompt line in place to just the filesystem label `ARXFS` (CR + label
       + erase-to-end-of-line) followed by a blank line so the login
       `Username:` is separated; a wrong
       passphrase (`Mount(PermissionDenied)`) is audited (`4137`),
       rate-limited by a minimum three-second timed park
       (`unlock_service::park_for_ns`, injected as a `delay` seam so host
       tests pass a no-op; never a busy-wait, §2.1/§2.23), reported
       (`Incorrect passphrase`), and prompted **again — indefinitely**: the
       root holds the only user data and login is refused until it mounts, so
       there is nothing to advance to without the correct passphrase. Only a
       structural failure or a console read fault gives up (`4138`), leaving
       the cell empty so every login is refused until reboot (§2.9 / §5.4.5).
       Host-proven by `root_mount` tests over a mock console + the same
       MBR + encrypted-`ARXFS` disk fixture `tools/mkimage` writes (§2.2). No
       `lib/abi`/C-header change.
     - **Chunk B-2 `&'static LateUsersDb` dispatch-hook wiring — LANDED
       (whole gate green).** The shared set-once cell
       `tairix_kernel::root_mount::LATE_USERS_DB` is the one definition the
       dispatch hook reads and the unlock kthread installs into; the aarch64
       boot path hands it to the hook via `BootInfo::with_users_db`, so
       `users_db_read` reads that cell on every call. Until an install it
       fails every read closed, identical to the previous `NULL_USERS_DB`
       default, so login still refuses every attempt until a root is mounted
       (§5.4.5) and the metal-confirmed boot is unaffected (§2.17). No
       `lib/abi`/C-header change.
     - **Chunk B-2 root-mount->login QEMU vertical — LANDED (whole gate
       green, incl. both Pi images).** A new `-M virt`
       `tairix-test-root-unlock-login-qemu-aarch64` vertical reuses the
       virtio-blk-mmio bring-up, then drives the production
       `unlock_root_disk_interactively` over a planted **whole-disk**
       encrypted-root image (MBR + FAT boot carrying `root.unlock` + a
       passphrase-derived encrypted `ARXFS` root): it types the passphrase
       at the prompt over a scripted console, mounts the root, installs the
       database into a `LateUsersDb`, and proves the planted account
       authenticates while a wrong password is refused. The disk is the
       shared `tairix-test-encrypted-root-image` fixture (authored by the
       real in-tree FAT32/`ARXFS` drivers + `mbr::encode`), which the
       `root_mount` host tests also split, so the in-memory split tests and
       the live (emulated) board exercise one on-disk layout (§2.2). No
       `lib/abi`/C-header change.
     - **INCREMENT (1) — production aarch64 device-IRQ subsystem — LANDED
       (host-proven + `-M virt` vertical).** The prerequisite that lets a
       kthread block on a device interrupt is complete:
       `kernel/tairix-kernel::aarch64::gic_irq::GicIrqController` bridges the
       arch `GicController` to `tairix_kernel_irq::IrqController`,
       `Aarch64BinArch::irq_routing`/`install_irq_dispatch` wire the GICv2
       routing + the EL1-vector `set_device_irq_dispatch` seam into the
       kernel/core `irq` phase (additive, no SPI bound until INCREMENT (2), so
       the metal boot is unaffected, §2.17); `kernel/arch/aarch64::fdt::
       gic_device_intid` decodes a device node's `interrupts` triple into a
       GICv2 INTID (SPI=number+`MIN_SPI_INTID`, no board constant, §2.20); and
       `tairix_kernel_core::KthreadIrqWaiter` is the cooperative
       `tairix_kernel_irq::IrqWaiter` an in-kernel service kthread drives
       (yields via the `YieldHandle`, mirrors `SyscallIrqWaiter`). Proven by
       `tests/integration/irq_kthread_qemu_aarch64`: a discovered RTC SPI wakes
       a `spawn_kthread` service parked in `block_until_ready` under the live
       eevdf scheduler, with the post-fire mask-before-wake check. Host tests:
       `KthreadIrqWaiter` (5), `gic_device_intid`/`gic_intid_from_cells` (9).
       No `lib/abi`/C-header change.
     - **INCREMENT (2) — Chunk B-2 the live in-kernel unlock kthread —
       LANDED (host-proven + the whole `-M virt` QEMU matrix green, both Pi
       images built).** `kernel/tairix-kernel::unlock_service` admits an
       in-kernel root-unlock scheduler kthread at the aarch64 init seam (in
       `init_spawn`, before `admit_init` diverges) that brings the bootstrap virtio-blk root device up over the
       INCREMENT (1) device-IRQ path and runs the device-independent
       `unlock_root_disk_interactively` policy. What landed:
       - `unlock_service` (top-level module, host-tested on the CI host): the
         **arch-neutral, device-independent core every port shares** (§2.2 /
         §17.2 — it names no architecture). It holds the post-MMU boot stash
         `record_boot`/`take_boot` (`RootBlockBinding` + the firmware DTB
         pointer + the discovered `&[HwNode]`, filled by
         `boot::audit_root_storage_binding`), the **console-0 ownership gate**
         (`Console0Gate`/`CONSOLE0_GATE`/`GatedConsoleRead`) that resolves the
         two-readers-of-one-queue concurrency note (item 5: the primary
         console's `login` reads through a `GatedConsoleRead` that withholds
         input — kernel-core `BlockingConsoleRead` parks it — until the unlock
         kthread opens the gate, while the kthread reads the raw device; the
         gate opens on every completion/fail-closed path so `login` is never
         left latched out; wired into both aarch64 console lists
         `arch_wrapper`), and the shared, host-tested helpers a future
         x86_64 / riscv64 port reuses verbatim rather than copying: the
         `UNLOCK_SERVICE`/`UNLOCK_TASK` ids + `note`, the `service_caps`
         (`MMIO_MAP`+`MEM_DMA`+`DRV_LOAD`) / `loader_caps`
         (`DRV_LOAD`+`DRV_KERNEL`) builders, the cooperative
         `KthreadConsoleRead` passphrase reader, and the `AutoloadHook`
         `MountedRootHook` over the abstract `DriverProcessSpawn` seam (so it
         names no concrete producer).
       - `aarch64::root_unlock` (`#[cfg(freestanding)]`, in the aarch64 boot
         subtree beside `boot`/`init_spawn`): the **arch-specific bring-up
         only** — `spawn_if_present` + `run_unlock` (over the arch-neutral
         helpers above) and the port's `MmioFloorPort` half: its bookkeeping
         `PageTablePool` and window bases, the `DeviceWindows` register map,
         the composite interrupt controller, and the `wfi` fallback park. The
         virtio-blk disk is
         brought up by the shared `floor_mmio::bring_up_virtio_mmio` off the
         boot-discovered DTB — `virtio_mmio_bus_from_dtb` → `MmioMap` +
         `KernelMmioMapper` over a throwaway bookkeeping address space →
         `provision_virtio_mmio` → the slot's SPI bound on the core-published
         `IrqTable` and armed → `DmaPool` + `KernelVirtioHost` — and opened by
         `finish_virtio_unlock` (the signed `drvhost::Host::load` gate →
         `VirtioBlk::open` → `finish_unlock`). EMMC2 (the Pi SD host) shares
         the register map, the line binding, and the DMA window.
       - **Interrupt-driven boot + task-parking device wait (load-bearing).**
         The production aarch64 boot brings the GICv2 up for delivery and runs
         interrupt-driven: `gic_irq::install_device_irq_dispatch` (kernel-core
         `irq` phase) calls `gic::init()` (enable distributor + CPU interface;
         additive — no line delivers until a driver routes its own, §2.17).
         The unlock kthread binds the device SPI on the core-published
         `IrqTable`; the shared waiter (`tairix_kernel_core::IrqParkWaiter`)
         re-arms the line (`gic_irq::CompositeIrqController::rearm`; unmask is
         the arch op the kernel `IrqController` trait omits) and **parks the
         calling task off the run queue** (register on `IRQ_WAITQ` → re-check
         `IrqTable::ready_for` → park; woken by the ISR's `irq_wake`), so a
         steady-state filesystem wait from a syscall context never halts the
         CPU and the dispatch loop (and `pump_tx` console drain) keeps running
         for the whole device wait. A context that cannot be scheduler-parked
         (the boot kthread) takes `root_unlock::wfi_fallback_park` — the
         canonical race-free `exceptions::{mask_irq, wait_for_interrupt,
         enable_irq}` park (mask → re-check → `wfi` → unmask) — which starves
         nothing at boot because everything else is parked waiting on it.
         The consuming `block_until_ready` loop clears the per-line ready
         flag on every wake, so the wait re-parks instead of degenerating
         into a budget-bounded busy-poll.
         `KthreadConsoleRead` keeps the cooperative `CooperativeYield` for the
         IRQ-less, polled passphrase read.
       - **virtio-MMIO device-interrupt ACK (load-bearing).** `tairix_virtio::
         Transport` gained a default-no-op `ack_interrupt`; `MmioTransport`
         overrides it to read `InterruptStatus` and write `InterruptACK`
         (virtio 1.1 §4.2.2), and `virtio_blk::run_request` calls it after
         `poll_used`. Without it the device holds its line asserted after a
         completion, so the next re-arm re-delivers a stale edge and the
         driver mis-pairs back-to-back reads — the read corruption that blocked
         the live mount. The default no-op keeps MSI-X PCI / the mock
         unchanged (3 host tests).
       - **Unlock audit on the audit channel (§19.4).** The unlock service's
         mount / install / give-up decisions route through the boot audit sink:
         `InitSpawnCtx::static_audit()` (default `None`; production
         `KernelInitSpawner` returns the `'static` audit) is threaded through
         `spawn_if_present` → `run_unlock` → `note()` + the capability-derive,
         MMIO map, virtio host, signed load gate, and
         `unlock_root_disk_interactively` (fallback `SERIAL_SINK`).
       - `ConsoleRead` dropped its blanket `Sync` supertrait; `Sync` is now
         required at the shared-`'static` storage sites (`ConsoleDevice.read`,
         `BlockingConsoleRead`), so the single-kthread `!Sync` cooperative
         reader is admissible.
       - **Bootstrap-floor virtio-MMIO enumeration** (`root_storage::
         observe_virtio_mmio_block_devices`, driven from
         `boot::audit_root_storage_binding`): the raw `virtio,mmio` firmware
         node carries only its `compatible` string, which binds no block
         driver, so the boot path probes each slot's `DeviceID` through the
         MMIO bus driver (`virtio_mmio_bus_from_dtb`) and folds a *probed*
         `HwMatchKey::virtio(2)` child node (the genuine probed identity,
         §18.2/§18.5 — never fabricated) into the same `RootBlockSelection`.
         This is what makes the production `virt` boot actually bind its
         virtio-blk root and admit the unlock kthread; the Pi tree has no
         `virtio,mmio` node, so it is a no-op there and metal-neutral
         (§2.17). The reused `aarch64::root_unlock` device-id constant is
         deduped to the canonical
         `tairix_drv_storage_virtio_blk::VIRTIO_BLK_DEVICE_ID` (§2.2).
         Host-tested (7 `root_storage` cases over a fake `Bus`).
       - The **admission vertical** `tairix-test-root-unlock-admission-qemu-
         aarch64` boots the production pipeline (`boot_aarch64::boot`) on
         `-M virt` with the planted `tairix-test-encrypted-root-image`
         virtio-blk disk: the enumeration binds the root, `spawn_if_present`
         admits the kthread, and it brings the device up over the device-IRQ
         path, reads the typed passphrase, mounts the encrypted root, and
         installs the database. PASS keys on the
         `unlock_service::USERS_DB_INSTALLED_MESSAGE` (a shared `pub const`,
         §2.2) observed on the audit sink — the *kthread-admission* witness
         (distinct from `root_unlock_login`, which drives the policy directly).
         Whole `-M virt` QEMU matrix green. Driving the per-console `login` to
         authenticate end to end additionally needs the userland heap to parse
         the served DB (SP5b, below), so it is out of this vertical's scope; the
         DB content authenticating `root`/`root` is proven by
         `root_unlock_login`.
       - No `lib/abi`/C-header change (the enumeration adds a new trait-free
         seam; the device-IRQ + console wiring is internal).
       - **EMMC2 root bring-up — wired (host-tested at the driver level).**
         The unlock kthread dispatches on the bound floor block driver
         (`run_unlock` → `virtio_blk_unlock` / `emmc2_unlock`); the EMMC2
         arm maps the matched node's sole SDHCI register window under
         `CAP_MMIO_MAP` through a minimal in-kernel MMIO-only `Emmc2Host`,
         admits `tairix-drv-storage-emmc2` through the signed §8 gate, and
         feeds the opened `Block` to the shared `finish_unlock` tail
         virtio-blk also uses (§2.2). See design B's **B4** above.
       - **Remaining (metal-gated, §0.4 — `raspi4b` cannot model EMMC2):**
         (a) the §0.9 metal UART log of the EMMC2 `/System` + encrypted-root
         mount; (b) the §0.9 metal UART-typed-login acceptance.
   - **Login parse.** The userland `login` parses the served text, which
     needs the production `mem_map` producer (`plans/SPAWN.md` SP5b).
   - The `-M virt` `tairix-test-root-unlock-login-qemu-aarch64` vertical
     proves the unlock + install + `root`/`root` authentication end to end
     over the passphrase path (landed, above); the live UART-typed login on
     the booted system rides the kthread flip + the §0.9 metal checkpoint.

**Done when:** a metal Pi 4 and the `virt` verticals sit at `login:` on
every text console, `root`/`root` logs in on a debug image and gets the
record's shell, a second login on the other console works concurrently,
an installer image refuses every login until the installer has authored
users, and no beacon/debug output remains in production boots.

### P12 — On-board gigabit Ethernet (GENET)

**Landed — the host-provable driver and the shipped bundle**
(`plans/NETWORK.md` N14): `drivers/network/genet` drives the BCM2711's
`brcm,bcm2711-genet-v5` UniMAC and its external BCM54213PE RGMII PHY,
serving the `netchan-v1` device-channel contract to `netstack` like any
other NIC. It is planted as a signed `/System/Drivers/network/genet/Run`
bundle by `build_image_driver_bundles`, so `devmgr` autoloads it against the
discovered node; the flashable image also ships a `network.conf` selecting
DHCPv4 + IPv6 SLAAC on it (`plans/DHCP.md` D5), so a booted board
self-configures with nothing for the operator to edit.

**On-metal run 1 found the document unplanted where its reader looks.** The
board autoloaded the driver, published its `netchan`, and `netstack` bound
the interface — then nothing: no `INTERFACE_CONFIG_APPLIED`, no
`DHCP_LEASE_ACQUIRED`. The shipped `network.conf` was written to the
encrypted root's `/System/Settings`, while the only reader — `devmgr`,
pre-unlock over the read-only `/System` store endpoint — resolves the
volume-relative path the ABI names, so the default was read by nothing and
the machine managed no interface. Both sides now derive that path from
`SystemConfigFile::volume_path` (`plans/DHCP.md` D5).

Reviewing the driver for what else stood between a bound interface and a
lease found a second, independent blocker: bring-up programmed **no receive
destination-address filter**, and the UniMAC drops every frame that matches
no enabled filter slot, so the NIC would have received nothing at all. It now
enables broadcast + its own unicast address before enabling the receiver, and
the stack programs the group addresses it needs into the remaining 15 slots
(`plans/NETWORK.md` N14, N14d), so IPv6 neighbour discovery reaches the NIC
too.

Every base, IRQ, DMA aperture, and even the MAC address is a discovered
value: the register window and both INTIDs come from the node's `reg` and
`interrupts`, the DMA reach from the parent `/scb` bus's `dma-ranges`, and
the link-layer address from the firmware-published
`mac-address` / `local-mac-address` binding — no `const PI_*` anywhere. Two
discovery defects were fixed on the way: the hardware tree emitted the raw
device-tree `interrupts` cell rather than the GICv2 INTID, and a
DMA-mastering node on a `dma-ranges` bus requested no DMA constraint.

**No QEMU form exists.** QEMU models no GENET (`-device help` carries no
such device) and its `raspi*` machines hand the kernel no device tree, so
this stage has no emulated vertical by construction (§0.4 — never fake a
passing vertical). The driver's coverage is its register-level model suite.

**Landed since — offloads (`plans/NETWORK.md` N18a).** The driver advertises
`RX_CSUM_VALIDATED | TX_CSUM_TCP | TX_CSUM_UDP | TX_SEGMENT_TCP`: the receive
checksum engine reports per descriptor (no receive status block, so the frame
layout is untouched), the transmit status block directs the transmit engine,
and the driver splits a segmentation super-frame itself because the silicon
has no segmentation engine. The split is device-independent arithmetic in
`lib/net::txoffload`, host-tested there and against the register model here.

**Remaining — the on-metal acceptance item.** Flash the emitted image to a
Pi 4B with a cable in the RJ45 socket and confirm, from the operator's UART
log: `devmgr` autoloads the bundle against the GENET node, `devmgr` logs
`NETWORK_IFCONFIG_DELIVERED` for `wan`, `netstack` logs `DRIVER_BOUND` then
`DHCP_LEASE_ACQUIRED`, the board answers a ping at its leased address from
another host on the LAN, and the address matches the MAC printed on the board
(so the firmware-published address was used, not an invented one). Then
unplug and re-plug the cable and confirm the link event re-resolves without a
driver restart.

**Done when:** the checklist above is recorded against a real Pi 4B.

### P13 — The legacy DMA engines

`drivers/dma/bcm2835` binds the discovered `brcm,bcm2835-dma` node, serves
its `dmaengine-v1` endpoint and ships as `/System/Drivers/dma/bcm2835/Run`.
It is host-proven against a register-level model of the engines
(`docs/src/drivers/dma.md`); QEMU runs no cyclic DREQ-paced chain, so the
rest is metal.

**Metal checklist**, run with SND8's first consumer:

1. The driver comes up serving channels `0x7f5` and declares the node
   quiesced; the firmware's channels 1 and 3 are never touched, and the
   firmware's own display and audio keep working.
2. A cyclic transfer to a peripheral FIFO raises one interrupt per period on
   its channel's line, including channels 7–10 on their shared lines.
3. A consumer killed mid-stream has its channel stopped and released, and
   the next instance of its driver opens the same line.

**Done when:** the checklist above is recorded against a real Pi 4B.

---

## 4. Cross-cutting requirements (apply to every stage)

- **Discovery, not constants.** Any new MMIO base lands as a discovered
  `hwtree` resource, never a fresh `const PI_*_BASE`. `cargo xtask
  cfg-check` must stay clean; the grandfather list stays empty.
- **Capabilities + audit.** Every new device handle (UART, GIC, mailbox,
  framebuffer, SD, USB) is reached through a capability the matched
  `hwtree` node requested; every match/load/skip is logged with a stable
  event id (§5.4 / §18.3).
- **Honest emulation gaps.** Where `raspi4b` cannot model a peripheral
  (HVS, real HDMI, real USB timing, the GENET MAC), say so in the stage and
  carry the metal checklist; never fake a passing vertical (§2.1 /
  `WIRING.md` §0.4).
- **`virt` stays green.** None of this regresses the QEMU `virt` board:
  the board difference is discovered data, so the same code serves both.

## 5. Definition of done (per stage, run over the whole project)

```
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all && cargo fmt --all --check
cargo xtask ci            # clippy -D warnings, deps-check, cfg-check, test matrix,
                          # docs-check, deny, c-header drift, proptest/fuzz --quick,
                          # model-check, spec-review, abi-check, and the image gate
                          # (both aarch64-rpi profiles built end-to-end)
cargo xtask fuzz --secs 5
tools/ci/soak.sh both --secs 20   # developer machine (that's us): max 20 s;
                                  # the unbounded 24 h soak is the CI host's job
```

The QEMU verticals are **not** in the host-only `cargo xtask ci` gate; run
the enrolled matrix separately — and add a `-M raspi4b` invocation path to
`tools/qemu/src/aarch64.rs` so the new board verticals run:

```
cargo xtask test --qemu
```

A single Pi-board QEMU bin can be iterated directly once the `raspi4b`
machine support lands in the runner:

```
cargo build -p <pkg> --target aarch64-unknown-none
cargo run -q -p tairix-qemu --bin tairix-qemu-run -- \
    --kernel target/aarch64-unknown-none/debug/<bin> --arch aarch64 \
    --board raspi4b --timeout-secs 60
```

Stages that can only be proven on metal (P7–P10) additionally record a
hardware bring-up checklist + a UART-log / photo acceptance artefact under
the stage, since CI has no Pi attached.

## On-metal acceptance: receive-path cost (`plans/NETWORK.md` N17)

Two items need a live Pi 4 and cannot be settled in QEMU, which models no
GENET.

1. **The idle-CPU measurement.** The reported symptom was ~15% of a core
   consumed while idle on a LAN. N17a removes the interrupt storm that
   caused it (the driver never masked its DMA-done sources while the stack
   was woken, and the kernel re-arms the line at every park, so the
   condition re-latched immediately). Confirm on metal that an idle Pi 4 on
   a busy segment now costs approximately nothing, and that
   `stats:net/<iface>/rx.filtered` climbs while `rx.packets` does not — that
   is the pre-filter shedding foreign broadcast.
2. **The offload measurement.** The open question — whether the 64-byte
   receive status block can coexist with the `RBUF_ALIGN_2B` pad — turned
   out to be moot: the receive checksum offload does not need the status
   block, only `RBUF_CHK_CTRL = RBUF_RXCHK_EN` and bit 15 of the completed
   descriptor, so the receive-buffer layout is unchanged. Receive and
   transmit checksum offload and driver-side TCP segmentation are landed
   (`plans/NETWORK.md` N18a). What metal still owes is the *measurement*:
   confirm a bulk TCP transfer reaches gigabit line rate at a lower CPU
   cost than before, that `ss`/`sysinfo` show no rise in checksum errors
   or retransmissions, and that a peer on the LAN accepts every segment
   the driver split (a wrong per-segment checksum would show as loss, not
   as corruption).
