# Input drivers

Input drivers report user-generated events — keyboard, pointer, and
scroll — to the focused session. They implement
[`tairix_abi::driver::input::Input`](../abi/driver_traits.md#input) and
run as user-space drivers. For the **desktop** path, key repeat,
modifier/lock state, keymap translation, and routing to a surface live
above this trait in `userland/gui/wm` and the session layer. For a
**text console**, a keyboard driver instead translates its key events
to console bytes itself and injects them through the `console_input`
syscall (see "Console-input producer" below); key repeat remains a
higher-layer concern either way.

## Class trait

`Input` is a single method:

| Method   | Purpose                                   | Capability gate          |
|----------|-------------------------------------------|--------------------------|
| `poll`   | drain pending events into a caller buffer | `DriverHandle` ownership |

Each `InputEvent` carries a `kind` (`Key`, `Pointer`, or `Scroll`), a
`code` (keycode or axis index), and a `value` (press/release or signed
delta). Per `AGENTS.md` §2.9 the trait never panics: an empty buffer
maps to `DriverError::BufferTooSmall`.

## Shipped drivers

| Driver        | Crate                            | Hardware                         | Status                          |
|---------------|----------------------------------|----------------------------------|---------------------------------|
| ps2           | `tairix-drv-input-ps2`           | Intel 8042 keyboard controller   | host-side tests + QEMU vertical |
| usb_hid       | `tairix-drv-input-usb-hid`       | Every USB HID interface: keyboards, mice, touch pads, touch screens (URB transport class driver) | host-side tests; live path is metal acceptance |
| virtio_input  | `tairix-drv-input-virtio-input`  | virtio-input (keyboard / pointer / touch) | host-side tests + QEMU verticals |

### `tairix-drv-input-ps2`

The PS/2 keyboard driver reads the Intel 8042 keyboard controller that
every x86 PC and QEMU's default `q35`/`i440fx` machines expose: a
status/command register at I/O port `0x64` and a data register at
`0x60`. It is a keyboard driver; the auxiliary (mouse) port is left to
a future pointer driver.

The driver never issues an `inb`/`outb` itself. It reaches the two
ports through the host-supplied `PortIo8` 8-bit port seam, which the
x86_64 architecture port implements (`AGENTS.md` §17.2 / §17.4). It
therefore carries no architecture `cfg` and no ambient authority over
the I/O port space (`AGENTS.md` §4).

`poll` reads the status register and, while the output buffer holds a
keyboard byte, consumes and decodes a scancode-set-1 byte stream into
platform-neutral keycodes: the base make code (`1..=0x7F`) for
unprefixed keys and `0xE000 | make` for `E0`-extended keys, with
`value == 1` for a press and `0` for a release. An `E0` prefix that
arrives at the end of one drain is latched and paired with its code on
the next `poll`. The drain stops at an empty buffer, at an auxiliary
byte (which it does **not** consume), at a full caller buffer, or when
a per-call read budget is exhausted — so a stuck controller can never
make the driver spin (`AGENTS.md` §2.1).

Lifecycle: `register` clears `CAP_DRV_LOAD`; `Ps2Keyboard::new` binds
the driver to the controller ports without performing any I/O; dropping
the `Ps2Keyboard` releases the backend (unload); constructing a fresh
instance reloads. The driver issues the controller no commands, so
unload leaves no state to quiesce.

QEMU integration on a live controller is exercised by
`tests/integration/ps2_input_qemu_x86_64` (`tairix-test-ps2-qemu-x86-64`,
enrolled in `cargo xtask test --qemu`). It boots the production kernel,
loads this driver's signed `.rxe` through `tairix_drvhost::Host`, and
drives it through load → use → unload → reload. The boot hand-off it
needed is the x86_64 8-bit port seam
`tairix_arch_x86_64::pio::X86PortIo8` — the byte-wide sibling of the PCI
bus driver's 32-bit `X86PortIo`, supplying the only in-tree
`PortIo8` implementation (`AGENTS.md` §17.2 / §17.4).

"Use" is **interrupt-driven**. The vertical binds the keyboard line
(ISA IRQ-1 → GSI 1) in the production `tairix_kernel_irq::IrqTable`,
sets the i8042 keyboard-interrupt config bit, masks the legacy 8259
PIC, and unmasks GSI 1 through the published `IoApicController`. It then
makes a keypress deterministic without physical hardware via the i8042
`0xD2` ("write keyboard output buffer") command — injecting a scancode
through the same `PortIo8` backend the driver reads through asserts the
real IRQ-1 line. After `sti` the test waits on `IrqTable::try_wait_step`
until the IO-APIC → LAPIC → IDT → dispatcher → `IrqTable::fire`
round-trip reports `WaitStep::Ready`, then drains the byte through the
driver's `poll`, confirming the decoded press and — after reload — the
matching release. The driver itself stays read-only and polled; the
interrupt only signals *when* a byte is waiting. This shares the
external-IRQ trap glue that `tests/integration/irq_qemu_x86_64`
validates against the PIT.

### `tairix-drv-input-virtio-input`

`tairix-drv-input-virtio-input` is the §8 driver identity — the `register`
entry and the §18.3 `BIND_KEYS` bind table. The reusable open/poll/decode
device logic and the keyboard console producer described below live in the
[`tairix-virtio-input`](../lib/virtio_input.md) library (`lib/virtio_input`),
shared by the in-kernel `-M virt` verticals and the user-space input-driver
process (`tairix-drv-input-virtio-kbd`, below) without a
`drivers/*`→`drivers/*` edge (`AGENTS.md` §17.4 / §2.2 — the virtio analogue of
`lib/hid` ↔ `drivers/input/usb_hid`).

The virtio-input logic implements `Input` over the bus-agnostic virtio
transport from `lib/virtio`, so one source compiles against both the PCI
and MMIO transports (the queue protocol lives once, `AGENTS.md` §2.2).
It is the paravirtualised input device every QEMU machine type can
present (`virtio-keyboard-device` / `virtio-mouse-device` /
`virtio-tablet-device` / `virtio-multitouch-device`) and the input class real
virtio hardware
exposes — the `virt`-board analogue of the x86 PS/2 keyboard.

It consumes the device-to-driver **event queue** (queue 0). The wire
record is `struct virtio_input_event { __le16 type; __le16 code;
__le32 value; }` (virtio 1.1 §5.8.6) in the Linux `evdev` namespaces,
which `poll` maps onto the platform-neutral `InputEvent`: `EV_KEY` →
`Key` (the evdev keycode, `value` 1 press / 0 release), `EV_REL` `REL_X`
/ `REL_Y` → `Pointer`, and both wheels → `Scroll` in scroll units. Which
code each wheel axis is read from is the device's own answer, queried once at
open from its `EV_REL` bitmap in config space: a device offering
`REL_WHEEL_HI_RES` or `REL_HWHEEL_HI_RES` is read from that fine code alone
and its detent twin dropped, and one that does not, or states no bitmap, is
read from `REL_WHEEL`/`REL_HWHEEL` at 120 units a detent. The vertical wheel
is negated because `evdev` counts it away from the user while the shared axis
counts toward the end; the horizontal one already counts toward it. The
`BTN_GEAR_DOWN`/`BTN_GEAR_UP` presses QEMU's HID pointers send are one detent
each. QEMU emulates no horizontal wheel on this device (it drops one
host-side), so that decode is proven by host tests. `EV_SYN` frame
separators and any unmodelled `type`/`code` are consumed but surface no
event, so the driver never fabricates a bogus one (`AGENTS.md` §2.9).

`open` runs the virtio 1.1 §3.1 init sequence (negotiating only `lib/virtio`'s
transport features, `VIRTIO_F_VERSION_1` and `VIRTIO_F_ACCESS_PLATFORM`) and then
**pre-posts a pool of device-write event buffers**, keyed by the
descriptor head the queue assigns. A single posted buffer is not enough:
the device fills one buffer per event of a report, so a keypress's
`EV_KEY` *and* its trailing `EV_SYN` each need a free buffer at once.
`poll` drains every completed event (interrupt-driven through the host's
IRQ waiter — no busy-spin, `AGENTS.md` §2.1), decodes it, and hands each
buffer straight back so the pool stays full. The driver allocates every
device-visible buffer through the host `VirtioHost` DMA seam and reaches
the device only through the `Transport` seam, so it holds no ambient
authority (`AGENTS.md` §4 / §17.4).

It publishes a canonical `BIND_KEYS` table (`AGENTS.md` §18.3): one
entry matching a probed virtio node whose device id is `virtio-input`
(`HwMatchKey::virtio(18)`) at the exact-match priority tier, the single
source of truth its signed-manifest bind table is authored from and the
data `devmgr` (or the in-kernel bootstrap-floor catalogue) resolves a
discovered virtio-input node against. The key carries no transport
detail, so the same driver binds whether the device is attached over
virtio-MMIO or PCI (`AGENTS.md` §2.2 / §17.4).

QEMU integration on a live device is exercised by
`tests/integration/input_virtio_mmio_qemu_aarch64`
(`tairix-test-input-virtio-mmio-qemu-aarch64`, enrolled in `cargo xtask
test --qemu`). It boots the aarch64 `virt` board, builds the
virtio-MMIO transport from the embedded device tree, arms the GICv2 SPI
+ EL1 IRQ path, mints a `KernelVirtioHost`, loads this driver's signed
`.rxe` through `tairix_drvhost::Host`, and drives it through
load → use → unload → reload. "Use" is a **real injected key**: once the
guest logs its event-queue-armed readiness marker, the QEMU runner
(`tools/qemu`) attaches a `virtio-keyboard-device` and sends a key
through the QEMU monitor (`sendkey`); the eventq IRQ fires and the
driver decodes the press and, after reload, the matching release — the
virtio-input analogue of the PS/2 vertical's `0xD2` injection, with the
event originating device-side rather than guest-side.

The riscv64 `virt`-board sibling
`tests/integration/input_virtio_mmio_qemu_riscv64`
(`tairix-test-input-virtio-mmio-qemu-riscv64`, also enrolled in `cargo
xtask test --qemu`) drives the same driver and the same shared
`virtio_input_keypress` key-decode tail over the riscv64 MMIO bring-up
(PLIC source + S-mode trap path), so a single driver source covers the
`input` row of the QEMU matrix on x86_64 (PS/2), aarch64, and riscv64
(`AGENTS.md` §2.2).

The touch path is exercised by `tests/integration/touch_qemu_aarch64`: the
runner attaches a `virtio-multitouch-device` as the board's only pointing
device and taps it over QEMU's QMP control monitor, and the guest passes once
the two taps — the program library's button, then the terminal's row — have
launched the terminal, with the kernel's witness that a touch frame was
delivered (`plans/POINTING.md` PO9).

#### Console-input producer

`lib/virtio_input`'s `VirtioKeyboardConsole` is the keyboard producer half: it
turns the `Key` `InputEvent` edges `poll` decodes (whose `code` is the raw
`evdev` keycode) into the `tairix_abi::input::KeyInput` records a driver injects
through the `key_inject` syscall. It tracks the held modifiers (each of the
eight modifier keys independently, collapsing the left/right pairs) and the
caps-/num-lock toggles, resolves each printable or named key edge into the
`Key` a US keyboard layout produces, and builds the record through the shared
`tairix_keymap::key_input` map — the **one** `Key`→record definition the
`lib/hid` USB console producer reaches too (`AGENTS.md` §2.2). The
`evdev`-keycode→`Key` table is `evdev`-specific (a USB HID keyboard decodes HID
usages into the same `Key` vocabulary), so it lives here; everything is
allocation-free and fail-closed — an unknown keycode or a non-key event
produces no record rather than guessing (`AGENTS.md` §2.9).

#### The autoloaded driver binary (`tairix-drv-input-virtio-kbd`)

`drivers/input/virtio_kbd` (`tairix-drv-input-virtio-kbd`, `src/main.rs`) is the
autoloaded **user-space** virtio-input keyboard driver process — the "drivers in
user space" steady state (`AGENTS.md` §4) on the hardware QEMU `-M virt`
presents (a metal USB keyboard is served by `tairix-drv-input-usb-hid`). It is
a freestanding pure-Rust `tairix-rt` program depending only on `lib/*`
(`lib/virtio`, `lib/virtio_input`, `lib/drvrt`, `lib/rt`, `lib/caps`, `lib/abi`)
so the §17.4 layering holds. `main` builds `RtDriverHost::from_grants_query`
over its kernel-issued grants (coherency `None` — coherent DMA, platform-neutral
§2.20), resolves its single granted register window with
`tairix_abi::driver::sole_register_window` over `RtDriverHost::resources()` (the
one definition shared with the USB keyboard driver, §2.2 / §2.16), maps it
through `mmio_map`, builds the bus-agnostic `MmioTransport`, brings the device up
with `VirtioInput::open_armed`, and pumps `poll_reports`: a touch surface's
frames go to `touch_inject`, and each other event is offered to the shared
pointer mapping first (`PointerInput::from_device_event` — axis deltas, `BTN_*`
edges, and scroll ticks → `pointer_inject`) and then to
`VirtioKeyboardConsole::feed` → `key_inject`. One driver instance is spawned
per discovered virtio-input node (keyboard, mouse and touchscreen alike — the
bind table cannot tell them apart), and each instance's device decides which
producer ever yields a record. A touch device that faults is told to the seat
as a frame with every contact lifted before the driver exits, so nothing it
held stays held. The pump is interrupt-driven: `open_armed` runs
`RtDriverHost::bind_irq` on the granted device line as its *arm* step, strictly
after the eventq is live (`DRIVER_OK`, buffers posted, device kicked), so the
audited `irq_bind` syscall is a truthful "keyboard ready" witness — binding any
earlier advertised readiness while the device could still silently drop a
keystroke (the lost keypress that made the autoload-input vertical flaky).
`poll` parks in the kernel
(`irq_wait` through the host's `notify_wait`) while no event is pending and
acknowledges the device each cycle (`Transport::ack_interrupt`), so an idle
keyboard consumes no CPU — never a yield-poll loop (`AGENTS.md` §2.23). That
wait is deliberately **unbounded** (`u64::MAX`): nothing is outstanding and the
next keystroke may genuinely be hours away, so there is no deadline to apply
and a periodic re-poll would only burn power. It is the opposite case to a
*request* wait (a block transfer), which must always carry its device's
per-request deadline, because there the request's own completion is the only
other event that could end the wait. Every
capability and bound is re-checked kernel-side (`AGENTS.md` §5.4); a bring-up
failure exits with a reserved fail-closed code (`80`/`81`/`82`) and a hard
device fault exits `83` rather than spinning on a broken device. It is a
separate crate from the §8
`tairix-drv-input-virtio-input` identity so it can link `tairix-rt` without
pulling it into the kernel-linked driver shell (`AGENTS.md` §2.2).

### The USB HID class driver (`tairix-drv-input-usb-hid`)

`drivers/input/usb_hid` is the `devmgr`-autoloaded user-space class driver for
every HID interface a host-controller driver publishes, whatever its sub-class
or protocol (`plans/HID.md` H5). Its node grants it the interface's URB
endpoint, its shared buffer and its interface number, and nothing else: no
register, DMA or interrupt (`CAP_INPUT_INJECT`, `CAP_SHM`,
`CAP_IPC_ENDPOINT`, `CAP_LOG_EMIT`). The host controller refuses any control
request of its that reaches past its own interface.

At bind it reads its interface from the configuration descriptor and the
report descriptor its HID descriptor states, parses it with
[`lib/hid`](../lib/hid.md), sets report protocol (reading it back from a
boot-subclass device, which may ignore the request), sets the idle rate to
report on change, and configures the device's features: a digitizer's input
mode and switches, a touch pad's contact limit and pad type, a wheel's
resolution multiplier. A descriptor that does not parse falls back to the boot
layout on a boot keyboard or boot mouse, and refuses any other interface with
its reason logged; the descriptor is logged in hex at bind. Every keyboard,
mouse, touch pad and touch screen application on the interface is then served:
each report is a blocking interrupt-IN URB the host controller answers when
the device reports, so the driver parks between reports, and each record goes
to the boot seat through `key_inject`, `pointer_inject` or `touch_inject`. An
interface that goes, or faults four times running, is let go of first — every
key, button and contact it held released — and the driver exits; a re-plug
autoloads a fresh instance on a fresh node. The live report path is a metal
acceptance item.
