# `tairix-virtio-input`

`lib/virtio_input` is the arch-neutral, transport-agnostic virtio-input
(keyboard / pointer / touch) device logic the virtio-input driver is built
from: the
virtio-1.1 §5.8 open/poll/decode engine over the bus-agnostic `lib/virtio`
`Transport`. It lives in `lib/*` — not in a driver crate — so **both** the
in-kernel `-M virt` input verticals and the user-space input-driver process
(`tairix-drv-input-virtio-kbd`) compose it without a `drivers/*`→`drivers/*`
dependency (`AGENTS.md` §17.4 / §2.2), exactly as the bus-agnostic xHCI protocol
lives in
[`tairix-usb`](./usb.md) rather than the xHCI driver, and the HID logic lives
in [`tairix-hid`](./hid.md) rather than the USB HID class drivers. The thin
`drivers/input/virtio_input` crate keeps only the §8 `register` entry and the
§18.3 bind table.

## What it provides

- **`VirtioInput`**: the device over a `lib/virtio` `Transport`. `open` runs the
  virtio-1.1 §3.1 initialisation sequence (negotiating only `lib/virtio`'s
  transport features, `VIRTIO_F_VERSION_1` and `VIRTIO_F_ACCESS_PLATFORM` — no
  device-specific features) and pre-posts a pool of device-write event buffers keyed by the
  descriptor head the queue assigns. A single posted buffer is not enough: the
  device fills one buffer per event of a report, so a keypress's `EV_KEY` *and*
  its trailing `EV_SYN` each need a free buffer at once.
- **`poll`** (`tairix_abi::driver::input::Input`): drains the completed events,
  decodes them, and hands each buffer straight back, zeroed, so the pool stays
  full and a completion that wrote nothing surfaces no event. A drain takes at
  most a ring's worth of completions, so a device refilling buffers as fast as
  they are reposted cannot hold it. The wait is interrupt-driven through the
  host's `notify_wait` — never a busy spin — and one that cannot be made at
  all fails the poll `DriverError::DeviceOffline` rather than returning
  nothing to be polled again at once. An empty caller buffer is
  `DriverError::BufferTooSmall`; the engine never panics.
- **`evdev` → `InputEvent` decode**: the wire record is
  `struct virtio_input_event { __le16 type; __le16 code; __le32 value; }`
  (virtio 1.1 §5.8.6) in the Linux `evdev` namespaces, mapped onto the
  platform-neutral `InputEvent`: `EV_KEY` → `Key` (the evdev keycode, `value`
  1 press / 0 release), `EV_REL` `REL_X`/`REL_Y` → `Pointer`, and both
  wheels → `Scroll` in scroll units on the shared axes, which count toward the
  end. `open` writes an `EV_BITS` query for `EV_REL` into the device's config
  space (`select`/`subsel`, virtio 1.1 §5.8.4) and reads the bitmap back: an
  axis whose `_HI_RES` code is offered is read from it alone, in its own 1/120
  units, and the detent twin the device also sends is dropped; any other axis
  is read from `REL_WHEEL`/`REL_HWHEEL` at 120 units a detent. The vertical
  wheel is negated, since `evdev` counts it away from the user; each
  `BTN_GEAR_DOWN`/`BTN_GEAR_UP` press — how QEMU's HID pointers report a
  detent — is one detent, its release nothing. `EV_SYN` frame separators and any unmodelled `type`/`code` are
  consumed but surface no event, so the engine never fabricates a bogus one
  (`AGENTS.md` §2.9 — fail closed, never guess).
- **Multi-touch**: a device whose `EV_ABS` bitmap offers
  `ABS_MT_POSITION_X`, `ABS_MT_POSITION_Y` and `ABS_MT_TRACKING_ID`, each
  position axis with a stated range, is a touch surface, read through
  `poll_reports`. Its slots follow the Linux multi-touch protocol (type B):
  `ABS_MT_SLOT` addresses a slot, a non-negative tracking id begins a contact
  and a negative one lifts it, and each `SYN_REPORT` closes one
  [`TouchFrame`](../abi/input.md) naming every contact still down, its
  position normalised over the axis's stated range and held to it. A slot
  past what a frame carries is not followed. The input properties name the
  surface — `INPUT_PROP_DIRECT`, or no `INPUT_PROP_POINTER`, is a
  touchscreen; `INPUT_PROP_BUTTONPAD` a clickpad; otherwise a touchpad —
  and a stated resolution gives it its physical size. `BTN_LEFT`/`RIGHT`/
  `MIDDLE` ride the frame as its buttons and `MT_TOOL_PALM` marks a palm. On
  `SYN_DROPPED` everything up to the next report is discarded and that report
  lifts every contact, so lost events never leave a finger down.
  `touch_lifted` is the frame a driver injects when its device stops.
- **`VIRTIO_INPUT_DEVICE_ID`**: the virtio device id (18) the driver crate's
  `BIND_KEYS` match key is built from — the single source of truth the device
  logic and the bind table both depend on (`AGENTS.md` §2.2 / §18.3).
- **`VirtioKeyboardConsole`** (`console` module): the keyboard producer half.
  `feed` turns each decoded `evdev`-keycode `Key` edge into the
  `tairix_abi::input::KeyInput` record a driver injects through `key_inject`,
  tracking the held modifiers (each of the eight modifier keys independently,
  collapsing left/right pairs) and the caps-/num-lock toggles and resolving the
  US layout. The `evdev`-keycode→`Key` table is `evdev`-specific, but the
  `Key`→record map is the shared `tairix_keymap::key_input` — the one definition
  the `lib/hid` USB console producer reaches too (`AGENTS.md` §2.2). An unknown
  keycode or non-key event produces no record (fail closed, `AGENTS.md` §2.9).

## Layering and platform-neutrality

`lib/virtio_input` depends only on other `lib/*` crates — `lib/abi` (the
`Input`/`InputEvent`/`BufferClass`/`KeyInput` surface), `lib/virtio` (the
bus-agnostic `Transport`, `SplitQueue`, DMA slabs, and `VirtioHost`), and
`lib/input` + `lib/keymap` (the `Key` vocabulary and the shared `Key`→record
map the console producer uses) — so it satisfies §17.4
and names no board, PCI, or SoC detail (`AGENTS.md` §2.20). It allocates every
device-visible buffer through the `VirtioHost` DMA seam and reaches the device
only through the `Transport` seam, holding no ambient authority (`AGENTS.md`
§4); the same source binds a virtio-input device however it is attached
(MMIO or PCI).

## Test surface

`cargo test -p tairix-virtio-input` exercises, against the in-process
`lib/virtio` `MockTransport` / `MockHost`:

- decode: key press/release, relative pointer (X/Y), both wheels in scroll
  units with their signs, the hi-res code chosen from the device's own bitmap
  per axis (a device answering none read at detents), and the
  discard of `EV_SYN` frame markers / unmapped codes / unmodelled types;
- multi-touch: every contact down framed on each report, normalisation over
  and clamping to the stated range, the surface kind from the properties,
  the size from the resolution, the `SYN_DROPPED` discard-then-lift, slots
  past a frame and repeated ids, buttons and palms, and a device with no
  slotted axes being no touch surface;
- poll-drain: a queued press, press-then-release in order, a frame marker
  surfacing no event, the no-pending-event `Ok(0)`, empty-buffer rejection,
  the per-drain bound, a slot completed without a write, and a wait that
  cannot be made;
- teardown: a drop resets the device before its memory goes, and withholds it
  from a device whose reset does not confirm;
- `console`: `evdev`-keycode resolution (letters, shifted digits, named and
  keypad keys, function keys), caps/num-lock toggling, left/right modifier
  collapsing, and the fail-closed unknown-keycode / non-key / key-repeat cases.

## Stability

Tier: `experimental` (see the crate `README.md`).
