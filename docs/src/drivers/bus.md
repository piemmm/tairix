# Bus drivers

TAIRiX bus drivers enumerate the devices attached to a transport
(PCI, MMIO, virtio) and surface them to the userland driver host as
`BusDevice` records. They implement the single class trait
[`tairix_abi::driver::bus::Bus`] and nothing else — every other type
is `pub(crate)` per `AGENTS.md` §8.

## Stage-4 drivers in this class

| Crate                    | Platform              | Status   |
| ------------------------ | --------------------- | -------- |
| `lib/pci`                | x86_64 (PIO) / PCIe ECAM / BCM2711 windowed | Shipped (library) |
| `drivers/bus/pcie_brcm`  | Pi 4 (BCM2711 RC)     | User-space bus-driver crate (link bring-up engine + `Run` bin; host-proven); metal pending |
| `drivers/bus/mmio`       | aarch64 / riscv64     | Shipped  |
| `drivers/bus/virtio`     | cross-arch            | Stage 4.D |
| `drivers/bus/usb/xhci`   | generic xHCI host (Pi 4 VL805) | P10 protocol layers + enumeration (host-proven) |
| `drivers/bus/usb/vl805`  | Pi 4 (VL805 device)   | User-space bus-driver crate: firmware-reload policy + `Run` bin (reload firmware → emit `usb,xhci` node B; host-proven); metal pending |

## Capability model

All bus drivers ship as user-space `.rxe` modules
(`DriverKind::UserSpace`). The driver host requires the universal
`CAP_DRV_LOAD` grant at `register` time; enumeration through `Bus`
inherits that gate through the issued `DriverHandle`
(`AGENTS.md` §5.4 / §8).

Bus drivers **never** read or write hardware until the host first
calls into the `Bus` trait — `register` itself is a pure capability
check that issues the per-driver marker handle.

## BAR / MMIO mapping

Both drivers **discover** the device-side memory windows (PCI BARs
or DT `reg` ranges) but **do not** map them. The actual mapping
request is routed through the driver host's memory capability by the
upper driver (Stage 4.D virtio-blk / virtio-net). This is the
direct consequence of `AGENTS.md` §4 ("memory isolation is enforced
by hardware") — the bus driver is not in the trust path for memory
mapping.

## PCI configuration-access library — `lib/pci`

### Configuration access

The enumeration, capability-walk, BAR-sizing, and window/MSI-X
hand-off core is parameterised over the `ConfigSpace` trait, so it is
independent of how configuration space is reached. Two access
mechanisms implement that trait; the caller picks one at construction
(`mechanism_one` / `mechanism_ecam` / `mechanism_brcm`).

#### Mechanism #1 — legacy I/O ports (x86_64)

PCI Local Bus 3.0 §3.2.2.3.2:

| Port  | Purpose                       |
| ----- | ----------------------------- |
| `0xCF8` | 32-bit configuration address |
| `0xCFC` | 32-bit configuration data    |

The crate splits the `in`/`out` instructions behind the
`tairix_abi::PortIo` seam so the in-crate unit tests can exercise the
bridge against a recording mock without touching real I/O ports; the
only real `PortIo` implementation lives in the x86_64 architecture
port. `ConfigAddress::to_cf8` is the single defensive gate — an
out-of-range address reads the `0xFFFF_FFFF` "no device" sentinel
rather than reaching a port.

#### ECAM — memory-mapped PCIe configuration (cross-arch)

PCI Express Base 3.0 §7.2.2 maps configuration space flat into MMIO:
each `(bus, device, function)` owns a 4 KiB block, so a configuration
dword is a naturally-aligned access at the computed byte offset
(`ConfigAddress::ecam_offset`):

```text
 bits 27..20: bus       (one 1 MiB block per bus)
 bits 19..15: device    (one 32 KiB block per device)
 bits 14..12: function  (one  4 KiB block per function)
 bits 11..0 : register byte offset
```

`EcamConfigSpace` reads and writes through a kernel-mapped
`tairix_abi::RegisterWindow` over the host bridge's configuration
region — obtained from the MMIO-map facility after a `CAP_MMIO_MAP`
check, so the driver never synthesises a pointer (`AGENTS.md` §4).
An access past the window's length, or a malformed address, resolves
to the same `0xFFFF_FFFF` sentinel, so an enumeration walk that runs
off the mapped buses fails closed rather than reading out of bounds
(`AGENTS.md` §5.4). Flat ECAM is the path any PCIe host bridge with a
contiguous MMCONFIG region uses; it carries no target-conditional
`cfg` (`AGENTS.md` §17.2).

#### BCM2711 windowed configuration access (Raspberry Pi 4)

The Raspberry Pi 4's BCM2711 root complex does **not** map
configuration space flat. Its own root-bus header (`bus 0`, `devfn 0`)
is read directly at the controller base, but a downstream function
(`bus >= 1`, e.g. the VL805 xHCI at `01:00.0`) is reached through an
index/data window pair inside the controller's own register block:
the function's `(bus << 20) | (devfn << 12)` block address is written
to the `EXT_CFG_INDEX` register (`0x9000`), then the dword is accessed
through the 4 KiB `EXT_CFG_DATA` window (`0x8000`) at the register
byte offset. `BrcmConfigSpace` implements `ConfigSpace` with exactly
this windowing — the *only* BCM2711-specific knowledge; the
enumeration, BAR-sizing, and capability walk above it are unchanged.
`mechanism_brcm(window, secondary_bus)` builds the bus over it. An
access that lands outside the mapped window, or any function but `00.0`
on the root bus, resolves to the same `0xFFFF_FFFF` sentinel
(`AGENTS.md` §5.4). The link behind the bridge must be **up** before any
downstream access — the `drivers/bus/pcie_brcm` root-complex bring-up
(below) guarantees that before handing its register window here.

The BCM2711 root port is a **single-device** link, so the accessor
forwards a configuration transaction only to `device 0` on
`secondary_bus` (the bus number the bring-up programmed into the bridge
bus-number register) and resolves every other downstream target to the
`0xFFFF_FFFF` sentinel *without* issuing a transaction. This is not just
hygiene: once the root port forwards downstream, a config read to a
non-existent target forwards a TLP that nothing answers, and the
completion timeout becomes a CPU external abort — a flat 256-bus walk
over forwarded config would wedge the boot CPU. The gate resolves a
non-zero slot on a non-root bus to the sentinel without forwarding.

### Enumeration walk

- 256 buses × 32 devices × 8 functions.
- Function 0 must be present before higher functions of the same
  device are probed; the multifunction bit is checked at offset
  0x0C, bit 7.
- `vendor == 0xFFFF` is the "slot empty" sentinel and is skipped.

### Capability list walk

Triggered when status bit 4 is set; the walker follows the linked
list rooted at offset 0x34 with a hard upper bound of 64 entries to
defend against circular `next` pointers (returns
`DriverError::DeviceFault`). MSI (`cap_id = 0x05`) and MSI-X
(`cap_id = 0x11`) are decoded structurally; every other capability
ID is reported opaquely so the host can audit it without re-walking
configuration space.

### BAR walker

Reads each BAR slot of a type-0 header, advances by two slots for
64-bit memory BARs, and runs the standard FFFFFFFF/read-back/restore
probe to compute the window size. Non-type-0 headers
(PCI-to-PCI bridge, CardBus) yield `DriverError::Unsupported`; they
are out of scope for Stage 4.

### Acceptance: exact q35 device list

`tests::q35_enumeration_matches_exact_device_list` asserts the
following list against a mock-host fixture reproducing QEMU's `q35`
default PCI tree:

| BDF      | vendor:device | class  | role              |
| -------- | ------------- | ------ | ----------------- |
| 00:00.0  | 8086:29C0     | 0x0600 | Host bridge       |
| 00:03.0  | 1AF4:1041     | 0x0200 | virtio-net-pci    |
| 00:1f.0  | 8086:2918     | 0x0601 | LPC bridge (mf)   |
| 00:1f.2  | 8086:2922     | 0x0106 | AHCI SATA         |
| 00:1f.3  | 8086:2930     | 0x0C05 | SMBus             |

The same enumeration core is exercised by the `Bus::enumerate`
implementation that the driver host wires up after `register`.

### Acceptance: VL805 over ECAM

`tests::ecam_enumeration_finds_root_port_and_vl805` and
`tests::ecam_capability_walk_decodes_vl805_msix` lay a flat ECAM
region (a PCIe root-port bridge at 00:00.0 and the Pi 4's VL805 xHCI
`1106:3483` at 01:00.0, with absent slots reading the all-ones
sentinel as real hardware master-aborts) and drive the same
enumeration and capability-walk core over `EcamConfigSpace`,
asserting both devices are listed and the VL805's MSI-X capability
decodes. The BAR *size* probe depends on hardware read-only BAR bits
and is covered by the mechanism-#1 fixtures, not the plain-memory
ECAM backing.

## BCM2711 PCIe root-complex bring-up — `drivers/bus/pcie_brcm`

The Pi 4's VL805 xHCI sits behind the BCM2711 PCIe root complex, which
ships out of reset with its link **down**. Before the windowed
configuration access above can reach the VL805, the root complex must
be brought up. The `drivers/bus/pcie_brcm` driver crate's `lib` target
performs that bring-up over the BCM2711 root-complex registers. (The
bring-up engine is co-located in that driver crate, not a `lib/*`
device-support crate: PCIe root-complex bring-up sits above the §18.6
bootstrap floor, so it has no charter-legal non-driver consumer for the
§2.20 carve-out, `AGENTS.md` §2.22.)

### Seams

The `BrcmPcieRc` state machine is written against two seams so it is
proven host-side (`AGENTS.md` §2.2):

- `RegisterBlock` — the shared register seam from `tairix_abi`,
  implemented for the kernel-minted `RegisterWindow` on metal and a
  register-level mock in tests.
- `Delay` — a microsecond busy-delay for the bring-up's hard timing
  requirements (SerDes settle, the 100 ms post-`PERST#` link-training
  window), supplied by the kernel composition on metal and a no-op in
  tests.

### Bring-up sequence

Release the controller's bridge reset **before** touching any MISC
register: the BCM2711 holds the controller core off at OS entry, and a
MISC-block (`0x4xxx`) access does not complete until the always-accessible
RGR1 bridge `sw_init` reset (`0x9210`) is released — touching MISC first
master-aborts on the SoC bus completion timeout (~10.8 s on metal),
which is what the multi-second bring-up pause turned out to be. So,
following the BCM2711 PCIe bring-up sequence: release the bridge `sw_init`
reset, bringing the core and its MISC block online, then let it settle. This is
the gentlest **no-touch-probe** bring-up: the previous boot stage
(`start4.elf`) hands off with the bridge `sw_init` reset **and** `PERST#`
already asserted and the VL805 firmware loaded over the power-on link, so
the driver does **not** re-assert a fundamental reset or toggle the SerDes
`IDDQ` — either of which could drop that resident firmware — and the
link-up step deasserts the already-asserted `PERST#`, producing the single
deassert edge. On the Pi 4 the VL805's xHCI firmware is loaded by the
bootloader EEPROM (via VideoCore), and on such a board VideoCore (re)loads
the blob on the **`PERST#` deassert edge** — the only edge the bring-up
drives, after which no runtime VL805 reload is issued.
So this driver produces that single deassert edge (rather than a fresh
fundamental reset), and the keyboard composition deliberately does **not**
issue a runtime `NOTIFY_XHCI_RESET` reload if the VL805's firmware version
(config `0x50`) stays `0` after the link trains — issuing a redundant reload
can be destructive on Pi firmware. Then program
`MISC_CTRL` (SCB access, UR config reads, 128-byte burst, RCB modes);
program the inbound (PCIe→system-memory) viewport `RC_BAR2` from the
discovered `dma-ranges` (the size encoded by `encode_ibar_size`, the
size rounded up to a power of two); disable the unused `RC_BAR1` /
`RC_BAR3` inbound windows; confirm the root-port role (fail closed
with `DeviceFault` otherwise); advertise ASPM L0s+L1 and present the
root complex as a PCI-PCI bridge; program the bridge bus-number register
(primary 0, secondary/subordinate = the single downstream bus) so the
port forwards configuration to the directly-attached VL805; program the
bridge Memory Base/Limit window (config offset `0x20`, covering the
outbound PCIe range) so the port forwards *memory* transactions to the
VL805's BAR — the BCM2711 ships that register empty, so without it BAR
reads master-abort to the `0xdead_dead` poison even though config reads
succeed (the bridge-window assignment a full PCI enumerator performs, which
the windowed `mech_brcm` accessor does not); program the outbound (CPU→PCIe)
MMIO window from the discovered `ranges`. Finally deassert `PERST#` and
poll `MISC_PCIE_STATUS` for data-link-active + phy-link-up, bounded by
`DEFAULT_LINK_POLLS` (100 ms), and confirm the link with a fail-closed
`link_up()` (`DeviceFault` otherwise). **Only then** enable Memory Space
+ Bus Master in the bridge's *own* Command register (config offset
`0x04`) — the standard PCI-PCI bridge enable a full enumerator performs,
which the windowed `mech_brcm` accessor
does not. This is issued *after* the link is up, because a PCI-PCI bridge
is enabled only once the link trains: the integrated RC latches
Memory Space Enable against a live link, so an earlier write (with
`PERST#` still asserted) does not stick — the metal `4110` symptom that
read the bridge command back as `0x0000` and left the VL805 BAR
master-aborting to `0xdead_dead`. All windows are device-tree-discovered, never compiled-in
(`AGENTS.md` §18.1).

`entry_inbound_window` exposes the inbound (PCIe→system-memory) viewport
registers (`RC_BAR1_LO`, `RC_BAR2_LO`/`HI`, `RC_BAR3_LO`) **as the previous
boot stage left them**, captured read-only and fail-closed during `bring_up`
before `RC_BAR2` is reprogrammed. On the Pi 4 the boot firmware's VL805
handoff depends on that inbound DMA window, so the capture both drives the
"don't reprogram a firmware-configured window" decision in `bring_up` and
lets a metal run compare it with the known-good
`IB MEM 0x0..0x1ffffffff -> 0x4_0000_0000` (`AGENTS.md` §15.7). The
post-bring-up window read-backs that once logged the trained register block
were removed: on real BCM2711 silicon reading those MISC registers after the
link trains stalls for seconds while the bring-up holds the CPU,
and with the link confirmed up they added no functional value
(`AGENTS.md` §2.14 / §2.16).

### Composition

The crate owns its discovered-node parsing and its autonomous floor
entry, beside the link-training engine they feed (`AGENTS.md` §2.2 /
§2.21): `wiring::pcie_bringup_from_node` reads the controller register
window plus the inbound/outbound address windows off the discovered
`brcm,bcm2711-pcie` `HwNode` into a `PcieBringup` (failing closed with a
`BringupError` naming the first missing resource — never an invented
window, `AGENTS.md` §18.5), and `wiring::bring_up_from_node` is the §18.6
autonomous bootstrap-floor entry that maps the window under `CAP_MMIO_MAP`
and trains the link over it (`DriverError::NotFound` on an incomplete
node). `wiring::open_discovered` is the lower seam they share with a
caller that already holds the windows. The caller then recovers the
window (`into_regs`) and builds `mechanism_brcm(window)` to enumerate the
VL805. The crate performs only the PCIe link bring-up and so never depends
on another driver crate (`AGENTS.md` §17.4); the VL805 firmware reload is
the separate `drivers/bus/usb/vl805` device crate's job and the xHCI
bring-up the separate `drivers/bus/usb/xhci` crate's. Both windows the
`PcieWindows` carries are device-tree-discovered: the inbound aperture
from the node's `dma-ranges` (an `HwResource::dma_translated` carrying
the CPU-reachability top, extent, the inbound PCIe-space base, and whether
the functions behind it snoop, which the published function's grant carries
on unchanged) and
the outbound MMIO window from its `ranges` (an `HwResource::bus_window`
carrying the CPU base, size, and far-side PCIe base —
`kernel/arch/aarch64::fdt::{dma_ranges_aperture,outbound_mmio_window}`,
`AGENTS.md` §18.1).

The whole chain runs in **user space**, decoupled by the hardware tree —
no driver names another (`AGENTS.md` §17.4 / §4). The kernel boot walk
seeds the discovered `brcm,bcm2711-pcie` root complex and VideoCore mailbox
nodes, and `devmgr` autoloads each signed `/System/Drivers/` bundle against
its node: the `pcie_brcm` bus driver maps its register window and trains the
link (`open_discovered`), assigns the VL805 BAR, and publishes the VL805 PCI
function through `hw_emit_node` (carrying the BAR + DMA grants); the `vl805`
device driver binds that, reloads the controller firmware over the VideoCore
mailbox, and publishes the controller as a `usb,xhci` node forwarding those
grants; the xHCI host-controller driver binds *that*, maps the BAR, carves
DMA, brings the controller up, enumerates the attached devices, and publishes
one node per interface; and the `usb_hid` class driver binds each HID
interface node and injects its records into the seat.
Each driver receives only the grants its matched node requested (`AGENTS.md`
§18.3), reached through its rt-backed `DriverHost`. The engines are
host-tested up to the controller hand-off, where the inert mock register
window faults — the metal boundary; QEMU models no Pi PCIe link timing or
USB, so the live enumerate→emit→autoload chain is the metal acceptance item
(`plans/PI.md` P10 D5d, `AGENTS.md` §0.9).

## MMIO driver — `drivers/bus/mmio`

### DTB iterator

The boot DTB is parsed once through `tairix_fdt::Fdt`. This is the
single shared device-tree parser in the workspace (`AGENTS.md`
§2.2): the architecture ports' platform discovery, the QEMU
verticals, and this driver all walk the `virt` tree through it. It
validates the FDT header, bounds-checks every read, and never
panics. The MMIO driver walks every node with `Fdt::nodes`, filters
on `compatible = "virtio,mmio"` (`Node::is_compatible`), reads
`reg = <base length>` (`Node::property` + `Property::read_be_u64`),
then probes the four-register identifier window through the volatile
reader.

### Volatile read seam

The only `unsafe` block in the crate sits inside
`VolatileMmioRead::read32` and is bounds-checked against the
`base_phys + len` window the constructor recorded. The trait
(`MmioRead`) is the in-crate test substitution point.

### Acceptance: exact virt slot list

`tests::virt_enumeration_matches_exact_device_list` asserts the
following list against a four-slot `virt`-style DTB plus a fake
register window in which two slots are populated:

| Slot base    | DeviceID | role           |
| ------------ | -------- | -------------- |
| 0x0A00_0000  | 1        | virtio-net     |
| 0x0A00_0200  | 2        | virtio-blk     |

The two trailing slots have `DeviceID == 0` and are skipped — the
same behaviour `virtio-mmio.c` in QEMU exhibits for unattached
transports.

## xHCI driver — `drivers/bus/usb/xhci`

The Pi 4 reaches its USB-A ports through a VL805 PCIe xHCI controller
(`plans/PI.md` P10). The bus-agnostic xHCI protocol layers and the
multi-device enumeration engine live in the `lib/usb`
(`tairix-usb`) crate — the USB analogue of `lib/virtio` — so this driver
and an arch-neutral user-space keyboard driver can both build on the same
engine without depending on each other (`AGENTS.md` §17.4). This driver
crate adds the §18.3 `BIND_KEYS` bind table and, host-tested in its `lib`
target, the controller bring-up, the per-interface URB service, the table of
published interface nodes and their transports, and the controller's fault
domain, which its `Run` binary composes with the live kernel calls. QEMU
models no Pi USB timing, so the host suite is the emulation artefact and
metal acceptance stays a checklist. The protocol behaviour described
below is implemented in `lib/usb` (see `docs/src/lib/usb.md`).

### Register seam and bring-up

Every controller access goes through the shared `RegisterBlock` seam —
implemented for the kernel-minted `RegisterWindow` on metal, and for a
register-level mock in tests. `Xhci::open` validates the capability block
(`CAPLENGTH`/`HCIVERSION` plausibility, non-zero
`MaxSlots`/`MaxPorts`/`DBOFF`/`RTSOFF` — the absent-controller
all-ones read fails here), halts a running controller, then issues the
self-clearing Host Controller Reset. Before asserting `HCRST`, it clears
only the stale write-1-to-clear `USBSTS` latches TAIRiX has observed on
firmware handoff (`HSE|PCD`); `Controller Not Ready` is enforced after
that reset, not treated as an unrecoverable pre-reset state. A halted
controller handed over with stale `CNR|HSE|PCD` may need those latches
cleared before the reset completes, while a post-reset `CNR` still fails
closed. Every wait is poll-budget-bounded and fails closed with
`DeviceFault` (`AGENTS.md` §2.1); the controller is left halted.
The same logic is exposed through `Xhci::open_diagnostic`, which keeps
the fail-closed `DriverError` but also names the refused stage
(`capability`, `halted_before_reset`, `reset_self_clear`, or
`controller_ready_after_reset`) and carries the last readable
`USBCMD`/`USBSTS` snapshot. The Pi keyboard bring-up logs those fields on a
metal `4101` open failure so the next capture identifies the exact stuck
reset condition instead of collapsing every timeout to a bare `device_fault`.
`Xhci::start` then programs the DMA structures and runs it: `CONFIG`
(all reported slots enabled), `DCBAAP`, `CRCR` (consumer cycle state
1), interrupter 0's single-entry event ring segment table over
`RTSOFF` (`ERSTSZ`/`ERSTBA`/`ERDP`), and Run/Stop — refusing any
address that is zero or not 64-byte aligned (`DmaProgram` plausibility,
§6.1, fail closed). `Xhci::ack_event` advances `ERDP` (clearing Event
Handler Busy) after each consumed event. `PORTSC` reads decode through
`PortStatus` with 1-based port bounds checks, `Xhci::begin_port_reset`
starts the §4.19.5 port reset with the write-1-to-clear bits masked so no
pending change bit is consumed by accident (its completion is awaited a
layer up, where the clock and the controller interrupt live) and
`Xhci::clear_port_reset_change` consumes the reset's own latches, and
doorbell rings validate both the index (≤ `MaxSlots`) and the §5.6 target
rules.

### TRB rings

`trb` defines the 16-byte TRB plus a fail-closed `TrbType` subset (an
unknown type is `OutOfRange`, never a guess) and the **complete**
`CompletionCode` set of xHCI 1.2 table 6-90 — a code the decoder cannot
name is a code no diagnostic can report, which is how the Pi 4's
`Context State Error` on Address Device read as a driver decode failure;
only the values the specification reserves or leaves vendor-defined still
fail closed. `indicates_device_unreachable` names the codes that read as a
hot-removal and `indicates_state_disagreement` those where the controller
*rejected* the command on its own slot/port state, which a fresh slot
re-drives. It also carries the on-ring little-endian byte conversion and
the transfer-event field decoders (slot ID, endpoint ID, transfer
residual). `ring` carries the §4.9 state machines and
holds **no memory**: `ProducerRing::push` returns a `PushOutcome` —
the cycle-stamped TRB, its slot and device-visible address, and (on a
wrap) the re-cycled Link TRB to publish *after* the data TRB — so the
owner of the device-shared memory performs every write and the
cycle/wrap/full logic is host-proven. The ring refuses caller-set
cycle bits and caller Link TRBs and fails closed (`Busy`) when full;
`EventRingCursor` consumes only TRBs whose cycle bit matches its
expectation, inverting it on each wrap, and holds no borrow of the
segment (the controller keeps writing it) — it is handed the single
entry at `dequeue_offset` afresh on each call. Only that entry is ever
examined, so a poll reads 16 bytes rather than the whole segment out of
non-cacheable DMA memory; on a device streaming reports that difference
dominates the driver's steady-state CPU cost.

### Device enumeration and the interrupt report path

`device` is the multi-device enumeration engine. All device-shared
bytes live in a growable bank of DMA chunks behind the crate's
`DmaBank` seam — the `SlabBank` over the host's owned `DmaSlab`
allocations in production, a plain shared buffer in tests. The
engine's first chunk holds the 64-byte-aligned shared `Layout` (DCBAA,
ERST, command ring, event segment, input context, the idle control
binding the EP0 cursor rests on while no slot is active, the scratchpad),
sized exactly to the controller's reported geometry and refused if its
base is misaligned or the bank cannot supply it. Every concurrently
served device's region (output context, EP0 ring and control data
buffer, interrupt-IN / bulk transfer rings, report buffers, and bulk
staging) and every addressed hub's status-change ring + report live in
chunks grown on attach and released on detach, so the served-device count
is bounded only by the controller's reported slots and genuine memory
exhaustion — never a compile-time budget.

`UsbDevice::start` first declares the controller quiesced to its DMA bank
(`DmaBank::device_quiesced`) — an `Xhci` exists only once `open` has halted
and reset it, so rings a dead predecessor left may leave the kernel's DMA
quarantine — then zeroes the shared chunk, publishes the ERST entry and the
rings' Link TRBs, and starts the controller through `Xhci::start`. A start
that fails once the controller may be running resets it before the chunk goes,
and keeps the chunk if the controller will not reset; a `UsbDevice` resets its
controller the same way whenever it is dropped. Every slot the engine gives up
— a detached device or hub, a failed attach, or a device with no interface the
engine serves (refused `Unsupported` before anything is configured, never
reported attached) — is disabled, and its chunks go back to the bank only once
the controller confirms Disable Slot: an unconfirmed slot keeps its DCBAA entry
and has its chunks withheld (`DmaBank::withhold`), the local bookkeeping is
still freed so a re-plug enumerates, and a failed attach is not re-driven onto
it. A teardown untracks the slot's endpoints before issuing Disable Slot, so a
completion landing during the wait is drained rather than re-arming the slot. A
confirmed controller reset (`reset_and_reenumerate`) releases every chunk so
withheld; a reset that fails releases none.
`UsbDevice::attach_root_port(port)` then brings the device on a root-hub
port to the configured state (§4.3): port reset when the port is not
yet enabled — awaited to completion exactly as a downstream hub port's is
(`await_root_port_reset_complete`: `PORTSC` re-read at 20 ms intervals
parked on the controller's interrupt, bounded at 800 ms, requiring the
reset done **and** the port enabled, then the reset's own `PRC`/`PEC`
latches consumed and the `TRSTRCY` recovery interval settled before the
device is addressed; skipping that settle is what had the Pi 4's VL805
reject Address Device with a Context State Error) — Enable Slot
(validating the returned slot ID), Address
Device (input control context `A0 | A1`, slot context, EP0 context
with the speed-derived **worst-case** max packet size), an 8-byte
`GET_DESCRIPTOR(device)` prefix read ending at `bMaxPacketSize0` —
one packet at the smallest legal EP0 size, so it completes whatever
size the device really uses — whose validated value (low speed 8,
full speed 8/16/32/64, high speed 64, SuperSpeed exponent 9; anything
else is `BadMagic`) drives an **Evaluate Context** (§4.6.7) whenever
it differs from the assumed size (a full-speed wireless receiver's
8-byte EP0 otherwise terminates every longer EP0 IN read short at its
first packet — the metal `DeviceFault`), then the full 18-byte
`GET_DESCRIPTOR(device)` (decoded fail-closed — a forged length, type,
or zero-configuration descriptor is `BadMagic`),
`GET_DESCRIPTOR(configuration)` in two steps (the 9-byte header for
`wTotalLength`, then exactly that many bytes — never an over-long
request a buggy device might mishandle; the interface descriptors'
class triples and `bConfigurationValue` / `bInterfaceNumber` drive the
steps below — never assumed), and `SET_CONFIGURATION`. A device serving a
mass-storage interface whose `iSerialNumber` is non-zero has its serial number
read in between: the LANGID table, then the string in the first language
listed (never an assumed one), each descriptor header first and then at
exactly the `bLength` it claims. Any fault on that read — a STALL, a timeout,
a transaction error — or a malformed answer only leaves the device without a
serial. A transaction fault on an address or descriptor read re-drives the
device after a port reset, on a fresh slot and a rebuilt EP0 ring, which the
aborted read's TDs would otherwise be replayed from. Every control transfer
that does not complete has EP0 taken back before its error returns (Reset
Endpoint for a halt, Stop Endpoint for a timeout, then Set TR Dequeue onto a
rebuilt ring).

Every interface with an interrupt-IN endpoint or a bulk pair is configured
(Configure Endpoint) and served, whatever its class; the engine sends no class
request. A HID interface's protocol, idle rate, report descriptor and features
are its class driver's (`drivers/input/usb_hid`, `plans/HID.md`), sent over its
own URB transport inside its interface's `UrbScope`.

The interrupt-IN endpoint is armed only once the class driver's first report
request names the longest report it expects: to that, or to one service
interval's payload if that is longer, and no further. A full/low-speed
endpoint behind the high-speed hub's transaction translator faults with a
Split Transaction Error when a transfer outruns the interval budget the TT
scheduled, retracting the interface — the metal keyboard behind the Pi 4 hub.
Each report is delivered as the device sent it.

The endpoint is not primed during enumeration. A hub
reports interface class `0x09`, not HID, and is served as a hub alone: no
interface its configuration claims gets a device entry, which would alias the
region its hub entry holds. Its interrupt status-change endpoint is captured
during enumeration but armed only once the hub is installed and marked:
arming it inline would make the hub deliver asynchronous status-change
reports that interleave with — and fault — the EP0 hub-class transfers that
follow (a transfer event whose interrupt-TRB pointer is not in the control
wait's watch list → `REJECT_ADDRESS_MISMATCH`, then a wedged ring; the metal
symptom was the hub's per-port `GET_STATUS` reads returning the all-ones
sentinel). A hub reporting no such endpoint is left unwatched, never watched
on an endpoint an earlier hub reported.

Devices *downstream* of an enumerated hub (every external device on the
Pi 4B hangs off the onboard `2109:3431` VIA hub) are each reached on
their **own xHCI slot** — the `bring_up` walk attaches every connected
downstream port, so a keyboard and
a storage stick are served together. `UsbDevice::attach_downstream_device
(down_port, speed)` keeps the hub addressed on its slot and gives the
new device the EP0 ring, output context, and control data buffer of a freshly
claimed device region (`control`/`address_device`/`next_report` follow the
active slot through `ep0_ring_off`/`output_ctx_off`/`ctrl_data_off`), then
Enable Slot + Address Device
with a slot context carrying the **Route String** (the hub's downstream
port, §8.9) and — for a full/low-speed device behind the high-speed hub
— the **transaction-translator** Hub Slot ID and Port Number (§6.2.2),
so the controller splits its transactions through the hub's TT. The
post-Address sequence (descriptors → Configure Endpoint →
`SET_CONFIGURATION` → ready for request-driven report arming) is the shared
`finish_enumeration`, identical to a root-port device —
only the topology in the slot context differs. A failed attach restores
the hub as the active control context and releases the claimed slot, so
one port's broken device never costs the other ports their service. The
caller owns the wall-clock settle windows (the `Delay` seam): the walk
powers the port, resets it (`SET_FEATURE(PORT_RESET)`), then **polls the
reset to completion** (`await_hub_port_reset_complete`: `GET_STATUS` at
20 ms parked intervals, bounded at 800 ms — a slow external hub
legitimately takes hundreds of milliseconds — requiring reset-signalling
done and the port enabled, then the `TRSTRCY` settle) before addressing at
the speed the final status reports. The root-port tier runs the same
protocol step over `PORTSC` off the same three figures
(`PORT_RESET_POLL_US` / `PORT_RESET_POLLS` / `PORT_RESET_SETTLE_US`,
defined once), so a fix to the reset handling cannot land on one tier and
miss the other. A failed attach snapshots its diagnostics
(`UsbDevice::last_attach_fault`: port, error, stage, completion/
event-type/reject, final `wPortStatus`) before the latch drain overwrites
the live state, so the HCD's hot-plug failure warning names the failing
step.

Before addressing anything behind it, the bring-up walk first
**marks the hub as a hub** in its own slot context
(`configure_hub_slot`): it reads the hub descriptor (`bNbrPorts` and the
`wHubCharacteristics` TT Think Time, `read_hub_topology` — requested at
the full base-descriptor size production stacks send, only the bytes
delivered decoded, by `HubDescriptor::decode` alone, and retried a bounded
three attempts when the hub answers with a refusal STALL or a reply that is
not a hub descriptor;
a truncated 8-byte read is an exchange no mainstream host issues, and a
real Realtek RTS5411 answered it with garbage on a successful transfer,
refusing the whole tier behind it), copies the
controller's live output slot context, sets the **Hub** bit, **Number of
Ports**, and **TT Think Time** (single-TT, so the Multi-TT bit stays
clear), and issues a Configure Endpoint over the hub's slot that names
only the slot context (Add flag `A0`). The descriptor type follows the
hub's own protocol speed: a **SuperSpeed hub** serves only the fixed
12-byte `0x2A` SS hub descriptor (USB 3.2 §10.15.2.1) and STALLs the
USB 2.0 `0x29` request — the metal defect where the same enclosure
enumerated on a USB 2.0 root port but its whole tier was refused on the
SuperSpeed one. An SS hub has no transaction translator (its slot's TT
Think Time is programmed zero), is told its tier depth with the
mandatory `SET_HUB_DEPTH` request (USB 3.2 §10.16.2.7) right after its
slot is configured, carries only SuperSpeed devices on its downstream
ports (the USB 2.0 `wPortStatus` speed bits are reserved there, so the
attach never decodes them), and latches the SS change set
(warm-reset/link-state/config-error in place of enable/suspend), which
the per-port latch drain clears through the SS `CLEAR_FEATURE`
selectors. Without the hub marking the controller never
schedules the split transactions a full/low-speed device behind the hub
needs, so the keyboard is addressed (Address Device succeeds) but its
interrupt-IN endpoint never completes and it delivers no report — the
metal symptom where the keyboard enumerated but typing produced nothing
(xHCI §6.2.2).

The interrupt-IN endpoint context also carries a non-zero **Max ESIT
Payload** (`ep_ctx_dwords`, §6.2.3.8 dword 4 bits 16:31: the bytes one
service interval moves, `PeriodicShape::payload`). The xHCI periodic scheduler reserves no
bus bandwidth for a periodic endpoint whose Max ESIT Payload is zero
(§4.14.2), so the controller would service it never — fatal precisely
for a full/low-speed interrupt endpoint behind the hub's TT, where the
scheduler must budget the split transactions. With the hub marked but
the payload left zero, Address Device and Configure Endpoint both
succeed, yet the keyboard delivers no report and the poll loop spins
with zero events — the metal symptom where the addressed keyboard never
typed. A control endpoint (Interval `0`) leaves the field reserved-zero.

The interrupt-IN endpoint itself is **read from the configuration
descriptor, never assumed** (`InterfaceInfo::decode_all`). The driver
walks past each default-alternate interface descriptor to its first
interrupt-IN endpoint and takes its Device Context Index
(`2 × endpoint_number + 1`), `wMaxPacketSize`, and `bInterval`;
`finish_enumeration` then configures *that* DCI per served interface, and
`next_report` doorbells and drains it for each waiting URB.
`interrupt_interval` encodes the endpoint-context Interval from the
descriptor's `bInterval` and the device speed (high/SuperSpeed
`bInterval − 1`; full/low-speed frames → the `fls(bInterval × 8) − 1`
microframe exponent, clamped 3..=10, xHCI Table 6-12); its Max Burst Size
and Max ESIT Payload come from the high-speed transaction bits of
`wMaxPacketSize` or the `SuperSpeed` endpoint companion. A `wMaxPacketSize`
past the speed's interrupt maximum (8, 64 or 1024 bytes) is held to it in the
endpoint context, so the controller refuses a longer packet and a transfer
never outruns its buffer. Every endpoint is polled at its own interval. Hard-coding the
endpoint as endpoint 1 (DCI 3) left the controller polling — and the
doorbell ringing — the wrong endpoint for a keyboard whose interrupt-IN
endpoint sat elsewhere, so it scheduled the real endpoint never: the
keyboard was addressed (`4128`) with the hub marked and a non-zero Max
ESIT Payload, yet typing produced nothing and the poll loop spun with
zero events. An interface with no interrupt-IN endpoint and no bulk pair is
not served.

Control transfers carry the SETUP payload as immediate data, set
Interrupt-on-Short-Packet on the IN data stage, and watch only the
addresses of their own in-flight TRBs: a completion for a TRB never
issued, an undecodable completion code, an unexpected event type, or a
stalled request is a `DeviceFault`, and every wait is bounded by the
engine's poll budget (`AGENTS.md` §2.1 / §2.9). A slot's data stages move
through the control data buffer in its own region, and IN data is copied out
before any other context is activated. A timed-out transfer stays armed, so a
device that answers one late writes only its own region, never the data of
another device's or a hub's transfer (a port status it could otherwise spoof).

`UsbDevice::next_report` serves a class driver's interrupt-IN URB: its first
request fixes the length the endpoint is armed to, and nothing is armed
before it. Each completion's claim is validated end to end (slot, endpoint
ID, completion code, TRB address inside the interrupt ring, residual within
the armed length — §5.4) and the report queued as the device sent it; the
oldest queued report is delivered, or the URB is held until one arrives. The
crate's tests prove the chain against the register-level mock plus an
in-memory ring model, and the fail-closed paths (forged residual, empty port,
double enumeration, undersized or misaligned DMA region).

### Controller recovery keeps the devices that come back

A controller that latches `USBSTS.HSE` or `HCHalted` raises no further
interrupt until it is reset (xHCI §4.24.1); on the Pi 4 the VL805 does so
during a downstream hot-removal teardown. The HCD recovers it the way the
Linux USB core's `usb_reset_and_verify_device` does: **a controller reset does
not remove the controller's children.**

- Each recovery attempt runs `UsbDevice::reset_and_reenumerate` with every
  interface node still published.
- After a successful reset each node is matched by identity
  (`interfaces::Interfaces::reconcile` over `UsbDevice::device_identity`) to
  the device-table index now serving its device, wherever the walk placed it:
  a device's bus position — root port and Route String — plus its descriptor
  identity (vendor, product, `bcdDevice`, device class triple, and the served
  interface's number and class triple) and, for a storage device, serial
  number (`DeviceIdentity::recognises`). The walk
  re-enumerates from index 0 in port order, so a device an earlier unplug left
  behind a hole, or one hot-plugged out of port order, comes back at another
  index and keeps its node, id, buffer, and bound class driver there. A node
  whose device is found nowhere is retracted, and a served device no node
  claims is published; no two nodes serve one index. Two storage devices of
  one model that traded places are told apart by their serial numbers, and a
  storage device without one is never recognised across the reset, so its
  node is retracted and republished; a serial that no longer reads counts as
  another device — a driver reload, never a driver bound to the wrong one.
- A held URB is answered only once its device's fate is known: the reissuable
  `WouldBlock` where the device came back (the reset discarded whatever
  transfer it had armed, and the class driver submits again), `NotFound` where
  its node was retracted. Told to reissue before the reset, a class driver
  would submit again during it, and that submission could reach a device that
  replaced its own.
- A reset that fails leaves the nodes published while the controller's grace
  window (`domain::ControllerHealth`) runs. Nothing is driven through the
  controller meanwhile — the device table a failed reset leaves behind is not
  trusted: a report poll is held for the next attempt, any other transfer is
  answered `WouldBlock` so its class driver's own recovery paces the retry, and
  the window's one-shot retries the reset. If the window elapses first the
  subtree fails closed for good: every interface node is retracted, so the
  device manager unloads their drivers, and the HCD exits with the reason
  logged, handing the controller's memory to the kernel to quarantine.
- Keeping the node is the only way a device survives the reset with its
  driver, because node ids are never reissued: a retracted node's driver is
  unloaded for good, and the device manager holds nothing across a reset.
- Every node is published with a shared buffer created for it, which no other
  node ever carries: removing a node retires the regions it conferred, so the
  kernel refuses to confer them again, and a new node's driver finds nothing of
  the previous device. The HCD releases its own mapping of a retracted node's
  buffer; a driver still mapping it keeps it alive until it exits. The call
  endpoint is reused. Removal revokes the previous driver's grant to it before
  `hw_remove_node` returns, and a node is published on the endpoint only after
  every call still queued there has been answered `NotFound`, so nothing the
  previous driver posted reaches the new device (`plans/USB.md` U10).

## VL805 USB bus driver — `drivers/bus/usb/vl805`

The VL805 firmware (re)load is the one thing specific to that *device*,
so it is its own driver — separate from, and not intertwined with, the
generic PCIe root-complex driver (`drivers/bus/pcie_brcm`, which trains the
link) and the generic xHCI host engine (`lib/usb`, which brings the
controller up and enumerates devices). A different board may need the PCIe
driver without USB at all, or an xHCI controller that needs no firmware
reload; keeping the three separate is the correct modular shape
(`AGENTS.md` §2.2 / §8 / §17.4).

The firmware policy and the controller-node wiring live **in the driver
crate** `drivers/bus/usb/vl805`, as a host-testable `lib` target
(`src/lib.rs` + `src/wiring.rs`) that the crate's freestanding `Run` binary
(`src/main.rs`, which links the userland runtime `tairix-rt`) links. The
logic is co-located here, not in a `lib/*` device-support crate: a VL805
USB driver sits above the §18.6 bootstrap floor and so has no charter-legal
non-driver consumer for the §2.20 carve-out (`AGENTS.md` §2.22). Putting it
in a `lib` target keeps it host-unit-tested without a kernel and the binary
crosses no `drivers/*`→`drivers/*` edge (`AGENTS.md` §17.4 / §2.2).

On a Pi 4 without the SPI EEPROM (rev 1.4+), the VL805 carries no
resident firmware: the `VideoCore` loads it at power-on and a PCIe
`PERST#` drops it, so only `VideoCore` can reload it over a
`NOTIFY_XHCI_RESET` firmware-property request. The driver may know the
VL805/BCM2711 — but it reaches the firmware mailbox **only** through the
board-neutral `tairix_abi::driver::mailbox::MailboxChannel` seam, never a
doorbell address or a `kernel/*` dependency (`AGENTS.md` §17.4). Its
public surface is the §8 `register` entry, the §18.3 `BIND_KEYS` bind
table (exact PCI `1106:3483`, ranked above the generic class-wildcard
xHCI driver), two firmware-policy functions composed over a
`MailboxChannel` —

- `probe_firmware_revision` — a benign firmware-revision liveness read
  that separates a broken mailbox path from `VideoCore` dropping the
  reset tag (`AGENTS.md` §15.7), and
- `reload_firmware` — the `NOTIFY_XHCI_RESET` reload, fail-closed: an
  unverified firmware ack is treated as a failure, never a success
  (`AGENTS.md` §5.4) —

and the `wiring` composition the bin runs: `build_xhci_node` publishes
the controller as `node B`, an `usb,xhci` hardware-tree node (the shared
`tairix_usb::XHCI_COMPATIBLE` identity) **forwarding** the register BAR +
DMA grants the bin received on the VL805 PCI node (`node A`), and
`reload_firmware_and_publish` reloads the firmware then emits node B —
so firmware-before-bring-up holds by construction (node B does not exist
until the reload runs). The bin holds only `CAP_MAILBOX` + `CAP_HW_EMIT`:
it forwards the BAR/DMA grants without ever mapping them (`AGENTS.md` §4
— least privilege), and the host-controller driver `drivers/bus/usb/xhci`
binds node B (its `BIND_KEYS`) to bring the controller up.

The property-message *layout* lives once in `lib/vcmailbox`
(`encode_xhci_reset` / `decode_xhci_reset_response` and the
firmware-revision pair); the VL805 driver only sequences the policy, never
re-deriving the layout (`AGENTS.md` §2.2). The mailbox *mechanism* (the
discovered doorbell window, the DMA-aliased property buffer, the cache
coherency) lives behind the `MailboxChannel`: the user-space bin reaches
the autoloaded `drivers/bus/mailbox/vcmailbox` service over the kernel
call surface (the `ipc_call` endpoint, gated by `CAP_MAILBOX`). At bring-up
the service exchanges one firmware-revision probe before it serves: the
firmware answers property requests one at a time in posting order, so that
answer proves any request a dead predecessor left in flight is finished and
its property buffer may leave the kernel's DMA quarantine. An exchange drains
a property completion naming another buffer — that predecessor's, answered
late — rather than taking it for its own. A request of its own the firmware
leaves unanswered keeps the buffer the firmware's: the next exchange first
waits for that reply and, until it lands, neither stages nor posts anything.
The service's buffer is owned by a `DmaMailbox`, which withholds it rather
than freeing it when dropped with such a request outstanding, so no exit path
of the service can return memory the firmware may still write. QEMU
models no `VideoCore`, so
the policy is host-proven (in the driver crate's `lib` target) against the
protocol-faithful `lib/vcmailbox` mock firmware and the reload-and-publish
wiring against `DriverHost`
doubles; the live reload → publish chain is the on-metal acceptance item
(`plans/PI.md` P10).

## Register-window hand-off

Enumeration only *names* a device; before a virtio transport can
drive it, the device's register block has to be mapped into the
driver's address space. A bus driver never synthesises that pointer
itself — doing so would be ambient authority, which `AGENTS.md` §4
forbids. Instead the kernel is the sole minter of a register window.

### The seam

`lib/abi` defines two types and one trait:

- `RegisterWindow` — a capability-checked, kernel-mapped MMIO window.
  Its only constructor (`from_mapping`) is `unsafe` and is called
  *only* by the kernel after it has validated the mapping, so safe
  code can never fabricate one. Every accessor (`read_u32` /
  `write_u32` / …) is bounds- and alignment-checked and returns
  `WindowError` rather than touching memory out of range.
- `MmioMapper` — the kernel-side MMIO-map facility the bus driver
  calls: `map_window(phys_base, len) -> Result<RegisterWindow,
  MmioMapError>`.
- `MmioMapError` — `CapabilityMissing` / `InvalidRegion` /
  `Unsupported`, each with an `as_driver_error()` mapping.

The host hands the bus driver an `&dyn MmioMapper` through
`DriverHost::mmio_mapper()` (default `None`). The kernel's concrete
mapper is `KernelMmioMapper` in `kernel/virtio` (the kernel crate,
because it links `kernel/{mem,sec}`, which a driver may not —
`AGENTS.md` §17.4); it wraps `kernel/mem::MmioMap` and routes every
request through the capability gate `kernel/sec::map_mmio`.

### Capability flow

```text
bus driver                     kernel (KernelMmioMapper)
----------                     --------------------------
resolve (phys_base, len)
   PCI : Pci::map_bar_window(bdf, bar_index, mapper)
   MMIO: Mmio::map_slot_window(base, mapper)
        │
        └── mapper.map_window(phys_base, len) ──► kernel/sec::map_mmio
                                                    1. check CAP_MMIO_MAP
                                                       │ no  → MmioMapDenied (audit 1041)
                                                       │       Err(CapabilityMissing)
                                                       │ yes
                                                    2. MmioMap::map  (NO_CACHE, guard pages)
                                                    3. emit MmioMapped (audit 1040)
        ◄────────────── RegisterWindow ─────────────┘
   hand to PciTransport / MmioTransport (lib/virtio)
```

The kernel maps the device's *own* physical frames with caching
disabled (`MapFlags::NO_CACHE`) and brackets the window with guard
pages, so a driver that walks off the end of a register block faults
instead of poking a neighbouring device (`AGENTS.md` §4). The grant
and every refusal are recorded in the audit log (events `1040`
`MmioMapped` / `1041` `MmioMapDenied`; see
`architecture/security.md`).

The PCI hand-off resolves the requested memory BAR (refusing I/O-port
BARs, which are reached through port I/O, and unused BARs); the MMIO
hand-off reads the `<base, length>` pair from the matching
`virtio,mmio` device-tree node. Neither path can run without the
caller holding `CAP_MMIO_MAP`.

### virtio-1.x configuration windows

A modern virtio-PCI device does not expose its register blocks as
whole BARs; instead it publishes each configuration structure as a
vendor-specific capability (`cap_id = 0x09`) carrying a
`(cfg_type, bar, offset, length)` tuple (virtio 1.x §4.1.4). The PCI
capability walker decodes these into `Capability::Virtio` /
`Capability::VirtioNotify` records, and
`Pci::virtio_window_region(bdf, cfg_type)` resolves a requested
`cfg_type` — common (`1`), notify (`2`), ISR (`3`), or device (`4`) — to
its `bar.base + offset` physical address and `length`, and
`VirtioPciBus::map_virtio_window` maps exactly that span through the
`CAP_MMIO_MAP`-gated `MmioMapper`. The `bar_offset + length` span is
bounds-checked against the resolved BAR size before the mapping request,
so a malformed capability fails closed (`OutOfRange`) rather than
mapping past the device's window.
`Pci::virtio_notify_off_multiplier(bdf)` returns the notification
scale from the notify capability. The four windows plus the
multiplier are exactly what `PciTransport::new` consumes, so a
boot-time PCI walk hands a working modern-virtio transport to the
driver host without ever synthesising a pointer.

### Ring-0 virtio-PCI walk

These hand-offs are `pub(crate)` on the concrete `Pci` type, because a
driver crate's only public surface is `register` (`AGENTS.md` §8). Ring
0 therefore reaches them through a frozen ABI seam rather than the
concrete type: `Pci<C>` implements
`tairix_abi::driver::virtio_pci::VirtioPciBus` (a supertrait of `Bus`),
whose `virtio_window_region` / `notify_off_multiplier` methods forward to
the inherent ones and whose `map_virtio_window` default composes the
resolve with the mapper. The default is inherited rather than
re-implemented, so the seam ring 0 calls is the only such path.

The kernel's `provision_virtio_pci(bus, bdf, mapper, build)` (in
`kernel/virtio/src/virtio_pci_walk.rs`) takes a `&dyn VirtioPciBus` and
the function its caller's bound node was published for, maps that
function's four windows through the `CAP_MMIO_MAP`-gated `MmioMapper`,
reads the notify multiplier, and assembles a `PciTransportWindows`
(which lives in `lib/virtio`). It walks no bus: the function driven is the
node bound, never the first of its kind the bus happens to hold. It does not name a concrete transport
itself: the caller passes `build` — in production `PciTransport::new`
with the entry it routed — so `kernel/virtio` depends only on `lib/*` and
never on the `drivers/bus/virtio` crate (`AGENTS.md` §17.4:
`kernel/* → lib/*`, never a driver). Ring 0 thus names no concrete
`drivers/bus/*` type and holds no ambient authority — the capability
check lives in the mapper, and every failure is a typed
`VirtioPciWalkError` rather than a panic (`AGENTS.md` §2.9).
The `cfg_type` discriminants live once, in `tairix_abi`, and the driver
uses them directly rather than through an alias of its own.

### Ring-0 virtio-MMIO walk

The `virt`-platform path mirrors the PCI walk one level down. A
virtio-MMIO device is a single register block whose `<base, length>`
pair the MMIO bus driver reads from its `virtio,mmio` device-tree node;
`Mmio::map_slot_window(base, mapper)` maps exactly that block through
the `CAP_MMIO_MAP`-gated `MmioMapper`. As with PCI, this hand-off is
`pub(crate)` on the concrete `Mmio` type, so ring 0 reaches it through
a frozen ABI seam: `Mmio<'_, T>` implements
`tairix_abi::driver::virtio_mmio::VirtioMmioBus` (a supertrait of
`Bus`), whose `map_slot_window` forwards to the inherent one. The
kernel's `provision_virtio_mmio(bus, device_id, slot, mapper, build)` (in
`kernel/virtio/src/virtio_mmio_walk.rs`) takes a `&dyn
VirtioMmioBus`, enumerates the bus into a bounded table, takes the slot
at position `slot`, refusing it unless its `DeviceID` is the requested
virtio device type (the bare type over MMIO, not the PCI `0x1040 + type`
encoding; `first_virtio_slot` finds the first of a type), maps
its single window, and hands it to `build` (in production
`MmioTransport::new`). As with the PCI walk, `kernel/virtio` names no
concrete transport type, so it depends only on `lib/*` and never on the
`drivers/bus/virtio` crate (`AGENTS.md` §17.4). Ring 0 holds no ambient
authority; every failure is a typed `VirtioMmioWalkError` rather than a
panic (`AGENTS.md` §2.9).

### Boot wiring

`provision_virtio_pci` yields the transport its `build` closure
constructs. The in-kernel floor disk's bring-up
(`kernel/tairix-kernel/src/x86_64/floor.rs`) claims its node for the
kernel, so no process is admitted as its driver, provisions it through
the kernel's one owner of PCI configuration space
(`kernel/tairix-kernel/src/pci_host.rs`), routes its MSI-X interrupt (see
below), hands the function over as a bus master — through its domain when
a unit translates it, at once otherwise — and hands the transport to the
signed virtio-blk driver over a per-driver DMA pool. User-space virtio
drivers are granted their register windows, routed interrupt line and DMA
constraint by the boot probe and never reach configuration space at all.

### Topology and isolation

`PciTopology::topology(confinement, external)`, implemented by every
mechanism, walks every function once — alternative-routing (ARI) functions
past function 7 included — and returns a `tairix_pci::topology::Topology`:
each function's identity, header, PCI Express port type, ACS registers and
address translation, page request, PASID and SR-IOV state, whether it sits
below an external-facing port (a hot-plug capable slot, or one the platform
names through `external`), the bridge forwarding to each bus — proven to
form a tree, or the walk is refused `DeviceFault` — and each function's
requester-id aliases and isolation group (`docs/src/security/iommu.md`).
A virtual function's requester id joins its physical function's group.
With `Confinement::Confine` it first turns on the isolating ACS controls
each function offers, turns off ATS, page requests, PASID and virtual
functions, and reads back what stayed as it was. The legacy capability list and the extended
list from offset `0x100` are each walked by one bounded iterator; the
extended space is reached through ECAM and the BCM2711 window only, so
mechanism #1 reports no extended capability. Every kernel PCI observer
reads this one walk, so a bus is scanned once however many observers run
and however many functions it holds. Over that walk,
`PciTopology::decoded_windows` answers the PCI addresses the segment decodes
as memory: each root-bus function's memory BARs (the host bridge's aside),
sized with its decoding off, and each memory window a root-bus bridge
forwards, below which every other function's BARs lie.

### MSI-X interrupt routing

Enumeration and window mapping bring a device's registers online; the
device also needs an interrupt line. A modern virtio-PCI function
delivers interrupts through MSI-X (PCI Local Bus 3.0 §6.8.2): one
message-signalled vector per table entry held in a memory BAR, plus a
per-function enable bit in configuration space. Routing the line means
programming an entry with the message the platform interrupt controller
minted, unmasking that entry, and enabling MSI-X on the function.

`Pci::route_msix(bdf, entry, message, mapper)` does exactly that: it
locates the function's MSI-X capability (decoded by the capability
walk), bounds-checks `entry` against the table size, resolves the table
BAR, maps the addressed 16-byte entry through the given `MmioMapper`,
masks the entry, writes the message address/data, unmasks it — an
unmasked entry's message is never changed — then sets the MSI-X Enable bit
and clears the function mask in the capability's Message Control register.
A table that lives in an I/O-port BAR is refused (`Unsupported`); an
entry index beyond the table or an entry that overruns its BAR fails
closed (`OutOfRange`); a caller without `CAP_MMIO_MAP` is denied
(`PermissionDenied`, propagated from the mapper). The driver never
synthesises a pointer. `Pci::mask_msix(bdf, masked)` sets or clears that
function mask alone, read back, so a function the kernel must stop raising
its messages raises none until it is routed again.

The `MsiMessage` (address + data) is **opaque** to the bus driver: only
the architecture layer knows how to address its interrupt controller.
On x86, `tairix_arch_x86_64::irq::msi_message(vector, destination)`
encodes the local-APIC message format (physical destination, fixed
delivery, edge trigger; Intel SDM Vol 3A §11.11) — the `0xFEE`-prefixed
address selecting the destination CPU and the data carrying the chosen
external vector (`0x30..=0xFE`); once interrupt remapping is on, the
message names the function's remapping entry instead
(`docs/src/security/iommu.md`). A GIC or PLIC port would build a
different pair; the bus driver copies whichever it is given verbatim.

As with the virtio-window hand-off, ring 0 reaches `route_msix` through
a frozen ABI seam rather than the concrete type: `Pci<C>` implements
`tairix_abi::driver::msix::MsixBus` (a supertrait of `Bus`), so the
boot path can route a device's interrupt through a single `&dyn
MsixBus` without naming a concrete `drivers/bus/*` type
(`AGENTS.md` §8). A route that fails leaves the function undiscovered,
rather than granting a line that never delivers.

Where no controller takes messages — an FDT board's GICv2 or PLIC — the
kernel grants a function its INTx line instead. The pin is swizzled to the
root bus (`pin' = (pin − 1 + device) mod 4 + 1` at each bridge,
`Topology::intx_at_root`), looked up in the host's `interrupt-map` under
its mask, and decoded by the port's controller; the grant is an
`HwResource::irq` with no MSI-X entry, so the driver's transport signals
on the pin and acknowledges by reading its ISR status. A pin is raised
only while an owner is bound to its line (`PciBus::set_intx`, and
[Shared lines](../security/irq.md#shared-lines)).

Routing turns memory decoding on, since the table lives in a BAR, and
leaves bus mastering as it was: an MSI is an upstream memory write, so
the function delivers only once its owner makes it a bus master
([Bus mastering](#bus-mastering-follows-ownership)).

### Generic-PCI BAR hand-off (the xHCI / VL805 path)

A non-virtio PCI device — the Raspberry Pi 4's VL805 `PCIe` xHCI USB
host controller (`plans/PI.md` P10) — exposes its registers as a whole
BAR, not the virtio capability tuples, and drives DMA without MSI-X.
Its driver needs a smaller surface than `VirtioPciBus`: map one BAR,
and turn on bus mastering. `tairix_abi::driver::pci::PciBus` (a
supertrait of `Bus`) is that seam:

- `map_bar_window(bdf, bar_index, mapper)` resolves the memory BAR's
  probed base/length and maps it through the same `CAP_MMIO_MAP`-gated
  `MmioMapper` (refusing I/O-port and unused BARs). The resolved base is
  the address held in the BAR — a *PCIe-bus* address; turning it into a
  CPU mapping is the host bridge's job, so a bridge-aware `MmioMapper`
  (`lib/drvrt`'s `RtDriverHost`, which applies an outbound `BusWindow`'s
  bus→CPU translation) does it, not this architecture-neutral walk;
- `enable_memory_space(bdf)` turns on decoding of the function's
  memory BARs, and `set_bus_master(bdf, master)` lets it issue upstream
  memory requests or stops it (PCI Local Bus 3.0 §6.2.2). Each changes
  only its own command bit and writes nothing when the bit already holds
  the asked value.
- `assign_bar(bdf, bar_index, window_base, window_size)` assigns the
  BAR a base when firmware left it **unassigned** (address bits zero),
  placing it at the lowest size-aligned PCIe-bus address inside the host
  bridge's outbound window and returning that base. Firmware normally
  programs BARs, but resetting and re-enumerating a root complex (the
  BCM2711 PCIe bring-up) leaves a downstream function unassigned, so a
  map would target physical address 0; assigning resources from the
  bridge's window is the PCI core's job. It probes the
  BAR's size/type, writes both dwords for a 64-bit BAR (control bits
  preserved), and leaves an already-based BAR untouched (a no-op under
  QEMU or a firmware-assigned BAR). It fails closed (`OutOfRange`) if the
  size-aligned placement does not fit the window, and refuses I/O-port
  BARs (`Unsupported`).
- `read_config(bdf, offset)` reads a configuration-space dword back
  (the byte `offset` taken to its dword), a read-only diagnostic that
  touches no state. It confirms a prior write took effect — a
  just-assigned BAR, an enabled command register, a programmed bridge
  window — so a metal capture can tell a configuration write that did
  not stick apart from a device that does not decode despite correct
  programming (`AGENTS.md` §15.7). It reaches both the root-port bridge
  (bus 0) and a downstream function through the same windowed accessor.

`Pci<C>` implements `PciBus` by forwarding to the inherent
`map_bar_window` / `enable_memory_space` / `set_bus_master` /
`assign_bar` / `read_config`. A device-class driver reaches the bus only
through `&dyn PciBus`, never naming the concrete `lib/pci` crate
(`AGENTS.md` §17.4).

### Bus mastering follows ownership

No routing or mapping helper makes a function a bus master
(`plans/IOMMU.md` IOM7). The owner of a function's configuration space
sets Bus Master Enable when it hands the function over and clears it
when it takes the function back. `Pci::set_bus_master` first clears a PCI
Express function's Enable No Snoop, leaving it stopped where the bit will
not clear: a No Snoop request reaches memory past the caches its buffers
were scrubbed and kept coherent through.

- **The x86_64 kernel** owns every function its boot probe enumerates.
  Before any translation unit is taken over, the probe stops mastering
  every virtio function, and every function behind a unit except a
  bridge or one firmware keeps a reserved window for. Afterwards a
  translated function masters once its owner's domain is attached, and
  stops before that domain is destroyed. An untranslated one masters
  from its owner's first carve until its owner ends.
- **The BCM2711 root complex driver** (`drivers/bus/pcie_brcm`) owns
  the VL805's configuration space. It makes the function a bus master
  as it publishes it, and stops it again if the publish is refused. The
  root port's own Bus Master Enable forwards for the whole subtree and
  stays on for the bridge's life.

The xHCI host-controller driver consumes it in
`tairix_drv_bus_usb::bringup`. The board bus drivers assign the
controller's BAR and publish the enumerated controller as the
`usb,xhci` node carrying the BAR + DMA + IRQ grants
(`drivers/bus/pcie_brcm` trains the link and enumerates the VL805;
`drivers/bus/usb/vl805` reloads its firmware and emits the node). The
autoloaded HCD derives those grants (`derive_controller_resources`),
maps the register BAR, and builds the growable `SlabBank` over its
host's DMA seam with the discovered inbound-DMA aperture top — every
chunk the engine grows (the shared structures now, each device's
region as it attaches) is verified at allocation time to lie wholly
**below** the aperture the bridge lets devices reach (fail-closed
`OutOfRange`, `AGENTS.md` §5.4) — and then brings the controller up
through `Xhci::open` + `UsbDevice::start` + `UsbDevice::bring_up`
(`bring_up_controller_diagnostic`, whose phase breadcrumb reports a
map / open / start / enumerate failure distinctly — the `enumerate`
phase now only ever names a fault of the **controller**, because a device
that will not enumerate is a counted skip and never fails the walk). Before
its first chunk, `UsbDevice::start` narrows the bank, and through it every
later carve of the host, to the controller's own reach — 32 address bits
for a controller without `HCCPARAMS1.AC64` — so the kernel places the
chunks where the controller reaches them rather than the bank refusing them
after the fact. A
skipped port is warned with the failing port's own snapshot (port,
enumeration stage, completion / event-type / reject) rather than the live
breadcrumb, which after a multi-port walk describes whichever port ran
last; the serve loop then owes it exactly one deferred re-attach off a
one-shot (`SkippedPortRetry` → `UsbDevice::retry_skipped_ports`), so a
device that merely lost the boot race comes up without a re-plug while a
genuinely broken one can never loop.
QEMU models no Pi USB timing (`AGENTS.md` §0.4), so the host tests prove
the composition and its fail-closed paths up to the controller hand-off;
the live controller bring-up is the on-metal acceptance item.

### Child-node emission into the hardware tree

A bus that enumerates downstream devices is responsible for growing the
hardware tree at runtime (`AGENTS.md` §18.1 / §18.3): each device it
finds becomes a child `HwNode` carrying the match keys a driver's signed
bind table resolves against, so a device behind the bus autoloads its
driver as match **data** rather than by a hand-wired composition module
(`AGENTS.md` §2.2 / §18.5). `PciBus::describe_function(bdf)` is that
seam: it reads the function's `vendor:device` and its **full 24-bit class
code** `(base_class << 16) | (sub_class << 8) | prog_if` — the prog-if
kept so an xHCI host (`0x0C_03_30`) is told apart from the older
OHCI/UHCI/EHCI USB host classes that share `0x0C_03`, exactly what the
generic xHCI driver's wildcard bind key needs — and returns an `HwNode`
carrying a single `HwMatchKey::pci`. The node's `HwDeviceClass` is derived
from the PCI base class (serial-bus and bridge → `Bus`); driver binding is
decided by the match key, not the class. An absent function (the all-ones
vendor sentinel) fails closed with `NotFound`, never a fabricated node
(`AGENTS.md` §2.9). The node's **identity is kernel-assigned on publish**:
`describe_function` returns it with placeholder id/parent, and the
`hw_emit_node` syscall stamps an id no node has held before in this boot and
the emitter's own matched node as parent, so a bus driver can neither forge
its tree position nor collide with an id (`AGENTS.md` §4 / §5.4 / §18.1).
No resource capabilities are attached here either — those are minted at
the load gate. This is the PCI half of `plans/PI.md` Stage 4.HW item 5b.

The user-space BCM2711 PCIe bus driver (`drivers/bus/pcie_brcm`) drives
this seam end to end: it binds the discovered `brcm,bcm2711-pcie` node,
trains the link, locates the VL805 with the shared
`tairix_pci::find_function_by_class` scan, assigns/enables/maps its BAR
with `tairix_pci::assign_and_map_bar` (the one primitive the xHCI driver
also uses, `AGENTS.md` §2.2), resolves the BAR to its CPU-physical address
with `tairix_pci::bus_to_cpu_phys`, and publishes the controller as an
xHCI `HwNode` carrying that BAR (an `Mmio` window inside the bridge's
outbound `BusWindow` grant, so the kernel's grant-coverage check admits
it), a DMA constraint, and the MSI vector it routed the function to,
exactly as `MsiAllocation::resource` names it: that is the grant
`msi_alloc` minted, and a line re-described as a wired pin is refused. The
composition lives — and is host-tested against a mock bus — in the driver
crate's own `lib` target (`wiring::emit_vl805_node` /
`wiring::publish_usb_function`), so the driver binary is a thin
freestanding stub.

The USB host driver does the same one level down, for the HID device it
enumerates behind the controller (`plans/PI.md` Stage 4.HW item 5b-ii).
A USB device's class lives on its *interface*, not its device
descriptor (whose `bDeviceClass` is `0` for an HID device), so
the enumeration (`UsbDevice::bring_up` /
`UsbDevice::attach_root_port`) reads the configuration descriptor at its
exact advertised `wTotalLength` during bring-up (a 9-byte header read
first, then precisely that many bytes) and parses **every**
default-alternate interface descriptor (`InterfaceInfo::decode_all`,
walking the concatenated descriptors by each `bLength`, fail-closed on
a truncated, mistyped, or
interface-less reply). A composite device — a wireless keyboard+mouse
receiver carrying a boot-keyboard *and* a boot-mouse interface — gets one
device-table entry and one emitted node **per served interface**, the
siblings sharing the device's slot and EP0. The discovered
`bConfigurationValue` and each `bInterfaceNumber` drive `SET_CONFIGURATION`
and the interface node's `HwProperty::UsbInterface` — neither is assumed to
be `1` / `0` any more — and the 24-bit interface class
`(bInterfaceClass << 16) | (bInterfaceSubClass << 8) | bInterfaceProtocol`
(an HID boot keyboard is `0x03_01_01`, a boot mouse `0x03_01_02`) is
captured for emission. `UsbDevice::describe_device(parent_id, node_id)`
then returns an `HwNode` of class `Input`, parented at the controller's
node, carrying one `HwMatchKey::usb` of the device's `vid:pid` and that
captured interface class — never a fabricated one (`AGENTS.md` §18.5) —
so the class-wildcard `BIND_KEYS` of `usb_hid` and `usb_msd` resolve
against it exactly as `devmgr` will. The node also states the interface's
number (`HwProperty::UsbInterface`), the `wIndex` its class driver's requests
must name. The node's `HwNode::address` is
the device's bus position (its root port above its Route String), which a
controller reset keeps, so the sibling interface nodes of one
composite device carry the same non-zero address and an inventory
consumer (`lsusb`) attributes them to a single physical device —
purely descriptive, never part of bind matching. It fails closed with
`NotFound` before a device has been enumerated.

Together with the bus-driver `BIND_KEYS` (`plans/PI.md` P10 5a), the
`devmgr` autoload wiring (5c) is the data-driven path the whole chain runs
on: every link is an autoloaded user-space driver.

## Constructing the real-hardware bus

The x86_64 boot pipeline reaches PCI through `tairix_pci::mechanism_ecam(window)`
where the firmware's MCFG describes an ECAM window, and otherwise through
`tairix_pci::mechanism_one(pio)`: configuration **mechanism #1**, the
`0xCF8` address word / `0xCFC` data word port pair (PCI Local Bus 3.0
§3.2.2.3.2). Both return `impl VirtioPciBus + MsixBus + PciBus`; every
trait has `Bus` as a supertrait, and the concrete `Pci` type stays
crate-private (`AGENTS.md` §8). Neither constructor carries a
`cfg(target_arch …)` gate: the `pio` argument is a `tairix_abi::PortIo`
backend, so the only `in`/`out` instructions live inside the architecture
port that supplies it (`tairix_arch_x86_64::pio::x86_port_io()`).
Construction performs no I/O.

On aarch64 and riscv64 the kernel takes every operational
`pci-host-ecam-generic` host the device tree describes
(`kernel/tairix-kernel/src/pci_fdt.rs`, over `tairix_fdt::pci`), its
functions' streams read from the host's `iommu-map` against the tree's
translation topology (`kernel/tairix-kernel/src/iommu_fdt.rs`,
[DMA translation](../security/iommu.md#topology-in-the-hardware-tree)): it maps
the configuration region, then numbers the buses depth first and places
every BAR and bridge window inside the windows the host's `ranges`
forward and the port can reach — sized bottom-up, placed largest
alignment first, decode enabled only for a space whose BARs all fit
(`PciResources::assign`) — unless `linux,pci-probe-only` keeps firmware's.
Every BAR, on every port, resolves only through the host's apertures and
never over RAM or the registers of the devices the platform itself is made
of (on x86_64: the local APICs and the message window, each IO-APIC, each
translation unit, each ECAM region), so a device cannot aim a grant or a
kernel write at either by what it reads back. A window the device tree
describes over RAM or over its host's configuration region is not used. On
riscv64 a host or translation unit whose registers lie above the 4 GiB every
process root maps is refused (`plans/OPEN-DEFECTS.md` D717). Both paths
then share one probe.

Ring 0 keeps the result as its one owner of that segment's configuration
space (`tairix_kernel::pci_host::PciHost`). Every kernel access goes
through it one at a time, because mechanism #1 reaches every function
through one machine-wide pair of ports and a command register changes by
a read and a write; no process may be granted those ports. The Pi 4's
VL805 is reached through the BCM2711 windowed mechanism's `PciBus` view
(see above).

## Shared types

There is no copy-paste between the two drivers; the shared pieces of
code are the FDT parser (`lib/util::dtb`), the `RegisterWindow` /
`MmioMapper` register-window seam (`lib/abi`), and the `PortIo`
port-I/O seam (`lib/abi`), all of which live below the drivers because
more than one crate needs them (`AGENTS.md` §2.3). The `PortIo` seam
crossed into `lib/abi` once a second caller materialised — the x86_64
architecture port that implements it (`AGENTS.md` §17.2 / §17.4) — so
the PCI driver no longer carries the `in`/`out` instructions or a
target gate. PCI and MMIO still each keep their own
configuration-access abstraction (`ConfigSpace` / the MMIO slot
reader) inside their crate because no second caller for those has
materialised.
