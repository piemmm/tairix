# FINISH-x86_64 — an x86_64 image that boots to the desktop

Binding under `AGENTS.md`. The staged plan for taking the x86_64 port from
"production kernel, text login and shell in QEMU" to "a disk image that boots
from UEFI firmware to the graphical desktop in QEMU", on exactly the boot and
display path a real UEFI PC uses. A physical target — the Surface Go 2 — is the
next stage (§5) and is out of scope until this plan is done.

Read first: `plans/ARCHSUPPORT.md` (x86_64 product parity; this plan delivers
its A5 and builds on the landed parts of its A2 and A4, the production storage
floor and `devmgr` autoload over the PCI tree; A2's registry deletion waits on
F4 and a riscv64 image, and A4's virtio pointer is F6 and its i8042 node S4),
`plans/BOOTLOADER.md` (the Rust UEFI loader
and GPT image this plan builds on — B3/B4 are owned there, not here),
`plans/DISPLAY.md` (seats, the display service, the boot-display publication),
`plans/NEW-DESKTOP-LOGIN.md` (the text-vs-graphical decision and the greeter),
and `plans/FIX-DISPLAY-ACCELERATION.md` (the display ABI the scan-out feeds).

## Ledger

| Id | Item | Status |
|---|---|---|
| F1 | x86_64 write-combining memory type (IA32_PAT) | planned |
| F2 | One shared boot framebuffer console and console/seat layout for every port | done: delivered by `plans/FIX-DISPLAY-ACCELERATION.md` F2 |
| F3 | The UEFI loader hands the kernel a GOP framebuffer (`plans/BOOTLOADER.md` B3) | blocked: the §4 measurement decision, `plans/BOOTLOADER.md` B6 |
| F4 | GPT/ESP whole-disk image, `cargo xtask image --target x86_64`, `run --target x86_64` (`plans/BOOTLOADER.md` B4) | blocked: F3 |
| F5 | x86_64 boot display: GOP scan-out → boot console, display node, seat registry | blocked: F1, F3 |
| F6 | Display service and pointer input autoload on x86_64 | blocked: F4, F5 |
| F7 | Graphical session verticals: greeter by default, then the revealed desktop | blocked: F6 |
| F8 | Platform page, README matrix, and `plans/ARCHSUPPORT.md` A5 closed | blocked: F7 |

## 1. What exists, and what is missing

The production x86_64 boot (`kernel/tairix-kernel/src/x86_64/boot.rs`)
already reaches a text session in QEMU from a planted whole-disk image: ACPI
and PCI discovery, the root mount over virtio-blk-PCI with MSI-X (an encrypted
root unlocked at the console), the read-only `/System` store, the users
database, the interrupt-driven COM1 console, `login`, the shell, and `devmgr`
autoloading signed drivers into their own processes (virtio input, virtio-net,
the sound cards, the xHCI host controller, the CMOS clock). It runs on one CPU
(`plans/ARCHSUPPORT.md` A10). `tools/qemu` already renders windowed
interactive x86_64 sessions with virtio keyboard, mouse and multitouch over
PCI.

Missing, in the order the chain needs it:

- **No framebuffer reaches the kernel.** QEMU's `-kernel` PVH boot hands over
  none, and the multiboot2 framebuffer tag the kernel can parse needs a boot
  loader to produce it.
- **Write-combining is refused.** `kernel/arch/x86_64/src/paging.rs` returns
  `MapError::Unsupported` for `PageFlags::WRITE_COMBINE`, because the port
  never programs the PAT. A display aperture is `FramebufferMemory::WriteCombine`,
  so the display service could not map the screen even if one were published.
- **No boot framebuffer console.** The shared console and console/seat layout
  (F2) run on x86_64: COM1 is the console with a seat, and a discovered
  virtio-gpu hosts the framebuffer console
  (`plans/FIX-DISPLAY-ACCELERATION.md` K). With no framebuffer tag, nothing
  shows the console before a display driver hosts it.
- **No image and no `run`.** `cargo xtask image` and `run` accept only
  `aarch64-rpi`.

## 2. Binding decisions

- **One boot display path, the real one.** The boot's screen is the UEFI
  **GOP** framebuffer, set up by OVMF, handed over by the first-party loader in
  the multiboot2 framebuffer tag — the same chain a physical UEFI machine runs.
  A discovered display engine
  that hosts the console — virtio-gpu in QEMU
  (`plans/FIX-DISPLAY-ACCELERATION.md` C) — takes the screen over from it, as
  the HVS does from the Pi's firmware framebuffer. Rejected alternatives:
  ramfb over fw_cfg under `-kernel` (on x86 fw_cfg is found only through the
  ACPI namespace or a blind fixed-port probe, and the path would not exist on
  real hardware), and a bochs-display PCI driver (QEMU/VirtualBox only).
- **PVH stays the fast test path, and a boot without a framebuffer is
  first-class.** Verticals that need no display keep booting with `-kernel`.
  A boot with no framebuffer tag selects the COM1-only console layout, and
  login proceeds in text; that is the headless configuration, not a fallback.
- **OVMF is the pinned QEMU build's own `edk2-x86_64-code.fd`**, installed by
  `tools/ci/install-qemu.sh` from GPG-verified QEMU source. It is not fetched,
  vendored, or pinned separately. Each run takes a fresh writable copy of the
  variable store, so no state leaks between runs.
- **Firmware in the loop must be deterministic; if it isn't, that is a
  defect.** The harness booted through OVMF and GRUB until it moved to PVH
  (`tools/qemu/src/x86_64.rs`). The recorded reason was that OVMF's video path
  crashed nondeterministically, and no root cause was ever given. The OVMF
  verticals here are run repeatedly under the loaded matrix, and any
  nondeterminism is diagnosed to its cause — ours, OVMF's, or QEMU's — before
  the item is done. Firmware boot is a separate `tools/qemu` boot kind beside
  PVH. The existing test that PVH argv carries no firmware stays, because it
  still describes the PVH kind exactly.
- **The kernel trusts the framebuffer tag no more than any other boot input.**
  The scan-out is admitted only as 32-bpp direct RGB, with base, pitch and
  extent free of overflow, the pitch covering a row, and the aperture outside
  every usable RAM range of the memory map. Anything else is refused and the
  boot proceeds headless.
- **The display is reached only through discovery.** The kernel publishes the
  admitted scan-out as the boot-display hardware-tree node
  (`boot_display::observe_boot_display`). The display service autoloads
  against it through `devmgr` and receives one write-combining framebuffer
  grant, exactly as on aarch64. Nothing below the display service names a
  display device.
- **One console layout, aarch64's.** With a scan-out admitted, the
  framebuffer console is the only console and COM1 is the log line with no
  session; without one, COM1 is the console. The boot log is routed by build
  profile as on aarch64: a release build renders it on the admitted screen, a
  debug build keeps all of it on COM1.
- **Shared first, then wired.** The console renderer and the console/seat
  layout are hoisted into shared homes (F2) before x86_64 uses them. A second
  copy for x86_64 is the duplication the charter forbids.

## 3. Increments

### F1 — x86_64 write-combining

The port programs `IA32_PAT` (MSR `0x277`) at CPU setup, identically on the
BSP and on every AP before that CPU maps anything. The Intel SDM requires the
PAT to agree across processors. Entries 0–3 keep their power-on values
(WB, WT, UC-, UC), so the existing PWT/PCD encodings keep their meaning.
Entry 4 becomes WC. `PageFlags::WRITE_COMBINE` then encodes as PAT index 4:
bit 7 on a 4 KiB leaf, bit 12 on a 2 MiB or 1 GiB leaf. Every x86_64
leaf-encoding path (kernel and user address spaces, the MMIO mapper) accepts
it, and the flag decode reads it back. CPUID.01H:EDX.PAT absent refuses
write-combining (`Unsupported`) rather than silently mapping UC.

The aperture must not be aliased with a conflicting memory type, which the
SDM leaves undefined; a write-combining page aliased to a cacheable one is the
case it singles out. Today it would be: the direct physical map (a 0–4 GiB
floor, widened to the top of RAM) maps every range it spans, non-RAM
included, through PAT entry 0, and it is the kernel root's one mapping of
physical memory. An aperture in its span therefore already has a
write-back-typed alias, cacheable wherever the firmware's MTRRs leave the
range write-back. F1 removes that alias rather than relying on the MTRRs, so
the kernel's one mapping of the aperture is the write-combining one.

Tests: host tests of the PAT value and of the leaf encode/decode round trip at
every page size, and the x86_64 conformance vertical mapping a write-combining
page outside RAM. `vesa_display_qemu_x86_64` is not that evidence: its
scan-out is a `ramfb` surface in guest RAM, which the direct map already maps
write-back, and the vesa driver maps it through `MmioMapper::map_window`,
which takes no memory type.

riscv64 refuses `WRITE_COMBINE` the same way (Svpbmt `NC` is the encoding). Its
one display, virtio-gpu, scans out of buffers in ordinary RAM and maps no
aperture, so the refusal is honest fail-closed behaviour; its encoding lands
with a riscv64 boot display, not here.

### F2 — one boot framebuffer console, one console/seat layout

The arch-neutral framebuffer console lives in `lib/fbcon`: the `port` half (a
port's `ConsolePort`: its direct-map lookup, whether it cleans its writes to
memory, and the interrupt-control type the render lock masks with) and the
`hosted` half (the surface slot and its placement, the render lock, attach,
the verbatim and program-output write paths, the grid and extent queries, the
surface disposition, adoption onto kernel-owned memory, the fatal paint
modes, and `paint`). Its grids are storage the kernel supplies, so the crate
links no allocator. A port supplies only where the surface comes from and its
`ConsolePort`: aarch64 cleans to the point of coherency, while x86_64 and
riscv64 share one coherent port. The slot is written once before the MMU is
on, where an atomic read-modify-write is UNPREDICTABLE, and locked only after.

`plans/OPEN-DEFECTS.md` D152 is fixed there for every port: a fatal report
never waits on the render lock.

`kernel/tairix-kernel`'s one `console_layout` module holds the console-list
and seat-registry statics and the choice of console 0 for every port, the one
the root-unlock prompt reads, and keeps the invariant that the seat's text
sink is the console listed at index 0.

### F3 — the loader hands over a GOP framebuffer

Delivered by `plans/BOOTLOADER.md` B3. What this plan needs from it is
recorded there: the GOP mode choice, the framebuffer tag, and the firmware
entropy seed (the second source `plans/OPEN-DEFECTS.md` D166 needs). B3
verifies the kernel's signature under B6's policy, so F3 also waits on B6.
The kernel's multiboot2 entry has not run on a live boot since the harness
moved to PVH; the multiboot2 parse `kernel/arch/x86_64/src/bootinfo.rs`
dispatches to (`lib/multiboot2`) is host-tested and fuzzed only. B3's OVMF
vertical is its first live boot on the current tree.

### F4 — the whole-disk image, `image` and `run`

Delivered by `plans/BOOTLOADER.md` B4 (GPT encode in `lib/partition`, the ESP
carrying the loader, the kernel and — for an encrypted root — `root.unlock`,
then the `/System` store and the root). The ESP holds no other file;
everything else is on the `/System` volume.

The composition `build_rpi_image` performs above its boot partition (users
and groups databases, home, attestation key, machine id, library catalogue,
the `/System` and root partitions) is hoisted into one function both image
builders call. Only the boot partition and the partition-table scheme differ
(`tairix_syshelp::assemble_disk` writes MBR today).

This plan owns the xtask surface:

- `cargo xtask image --target x86_64 [--profile debug|installer]` writes
  `images/tairix-x86_64-<profile>.img`, a raw GPT disk that UEFI firmware
  boots from a USB stick as written. The hybrid BIOS/UEFI
  `images/tairix-x86_64.iso` the charter names is the same layout with
  `plans/BOOTLOADER.md` B5's BIOS half added, and is not needed for QEMU or a
  UEFI-only machine.
- `cargo xtask run --target x86_64` boots that image windowed: OVMF, the disk
  as virtio-blk-PCI, the default VGA device for GOP, virtio keyboard, mouse and
  multitouch, and virtio-net on user networking pinned to the MAC the shipped
  `network.conf` binds. No `-kernel` appears in the command line.

The run spec is a pure, host-tested function, as `run_session_spec` is for
aarch64.

### F5 — the x86_64 boot display

The x86_64 boot reads the multiboot2 framebuffer tag and admits it into a
`BootScanout` under the §2 checks, with `FramebufferMemory::WriteCombine`. It
maps the aperture write-combining and attaches the shared boot console (F2).
The boot publishes the boot-display node, and chooses its console list, seat
registry and root-unlock console through the shared layout (§2): the
framebuffer console when a scan-out was admitted, COM1 otherwise.

The kernel console yields the surface to the display service through the same
reclaim and purge disposition aarch64 uses, so no two writers share a scan-out.

Vertical: an OVMF boot whose serial transcript witnesses the boot-display node
and whose host-side screendump shows console text.

### F6 — display service and pointer on x86_64

The x86_64 image's driver set gains the framebuffer display service and the
virtio-input driver, both built for x86_64 by the existing `image_drivers`
pipeline. `devmgr` autoloads the display service against the boot-display
node and the input driver against each virtio-input PCI function, keyboard,
pointer and touchscreen alike. The input driver is transport-neutral and
already binds over PCI.

Vertical: the display service's first present, plus a pointer event delivered
to the input-focus arbiter over virtio-PCI.

### F7 — the graphical session

Two x86_64 siblings of existing aarch64 verticals, each a thin bin over the
shared scenario code with no second scenario implementation:

- `greeter_default_qemu_x86_64`: an unconfigured machine that can draw boots
  to the greeter.
- A desktop-reveal vertical: log in, reach `desktop fully revealed on screen`,
  and deliver a pointer event to the desktop.

Both boot through OVMF from the F4 image. The x86_64 verticals that need no
display keep PVH, so firmware start-up cost is paid only where it is under
test.

### F8 — docs and closure

- `docs/src/platform/x86_64.md` gains the boot-display, PAT and GOP sections.
- A new `docs/src/install/x86_64.md`, listed in `docs/src/SUMMARY.md`,
  describes `image` and `run`.
- The README rows for framebuffer/display, graphical login and bootable image
  move to their real x86_64 state.
- `plans/ARCHSUPPORT.md` A5 is marked done.

## 4. Open decisions

- **Loader measurement without a TPM.** `plans/BOOTLOADER.md` states the rule
  two ways: its §0 has the loader measure the kernel into the TPM "where
  present", while its §3 invariant says it "never launches an image it could
  not verify and measure". OVMF in QEMU has no TPM unless an external `swtpm`
  is attached, so under the §3 reading F3 cannot boot. Either the harness
  attaches a software TPM, or the policy becomes §0's: "always verify; measure
  when a TPM is present, and record its absence". This is a security-policy
  choice for the user, made before B3 lands, and `plans/BOOTLOADER.md` then
  states the one rule chosen.

## 5. Next stage: Surface Go 2

Out of scope until F8 is done, except S4 and S6, which the touchpad and
touchscreen work needs first and so may land before F8 (`plans/POINTING.md`
PO12 binds on S6's published PCI functions, PO13 on S4's namespace driver).
Nor do two security items wait for F8: `plans/ARCHSUPPORT.md` A8 (KPTI,
speculation barriers), whose blocker has cleared so `PLAN.md` orders it ahead
of other stage work, and `plans/OPEN-DEFECTS.md` D953, which S5 cites. What the
tree already shows the physical machine needs beyond this plan; the exact parts
are confirmed by S0.

| Id | Item | Status |
|---|---|---|
| S0 | Hardware survey of the target: CPU, storage controller, display, input, radios, and the ACPI namespace | planned |
| S1 | Secure Boot: sign the loader for the machine's key database, or document the disable step (a decision for the user) | planned |
| S2 | xHCI bound by its PCI class over the PCI tree: the x86_64 boot publishes each xHCI function, `drivers/bus/usb/xhci` binds class `0x0C0330` from any vendor at a lower priority than an exact-part driver, and `audio_qemu_x86_64`'s USB-speaker run proves it over `qemu-xhci` | done |
| S3 | A driver for the machine's internal storage: no NVMe or AHCI driver exists, and the one SDHCI host driver (`drivers/storage/emmc2`) binds only the Pi 4's device-tree node and speaks the SD-card protocol, with no eMMC command set | planned |
| S4 | The ACPI namespace (`plans/ACPI.md` A1–A8): the AML bus driver, power-off (`plans/ARCHSUPPORT.md` A7), the power button, an i8042 (`PNP0303`) node and a driver process over the existing `drivers/input/ps2` decoder, and ACPI-enumerated I2C/HID devices; the battery, which no plan owns yet | planned |
| S5 | Real-silicon hardening and robustness: `plans/OPEN-DEFECTS.md` D953 (declared speculation barriers no entry path runs), D744 (taking over a firmware-translating IOMMU turns translation off), D85 (spurious LAPIC interrupt), D3 (hard-lockup watchdog); production SMP (`plans/ARCHSUPPORT.md` A10) | planned |
| S6 | Every PCI function the ECAM walk finds published into the hardware tree with its BARs, its interrupt and its DMA-stream grants. Today the boot publishes virtio functions and the xHCI and HD Audio classes only, a class function with its register BAR alone and none without an MSI or MSI-X route (INTx needs `plans/ACPI.md` A5), each interrupt-driven one spending a vector from the single pool `plans/OPEN-DEFECTS.md` D742 describes. Bus mastering already follows ownership, and every function carries its identity (`plans/IOMMU.md` IOM7, IOM9) | in progress |
