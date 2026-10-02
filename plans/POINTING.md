# POINTING — scroll axes and resolution, touch, gestures and pinch

Binding under `AGENTS.md`. How a wheel turn and a touch reach an application:
the one scroll unit every layer carries, the devices that feed both axes and
fine resolution, what a scroll tells the window it lands on, and the touch
path — contacts, the seat's gesture recogniser, and the pinch an application
zooms by.

## Ledger

| Id | Item | Status |
|---|---|---|
| PO1 | One scroll unit, 1/120 of a detent, from the device decode to the application (`tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT`) | done |
| PO2 | Horizontal and high-resolution wheels: virtio `REL_HWHEEL` and the `_HI_RES` pair chosen from the device's own event bitmap; HID AC Pan and the Resolution Multiplier; report deltas kept at their full width | done |
| PO3 | A scroll lands at a place: `WindowEvent::Scrolled` carries the window-local position and the modifiers held; Shift turns a wheel sideways in the seat; a scroll is a gesture to the lone-Ctrl locate (D446) | done |
| PO4 | Zoom by the wheel: Ctrl and the wheel zoom Paint and the viewer, anchored on the pointer | done |
| PO5 | Touch frames: `TouchFrame` (`tairix_abi::touch`) and each seat's touch channel, `touch_inject` and `touch_read`, every frame stamped with the injector it came from and the instant it arrived | done |
| PO6 | The seat's gesture recogniser (`lib/touch`): a touchpad's pointer motion, two-finger scroll on both axes, pinch, taps, tap-and-drag and clickpad buttons, palms set aside; a touchscreen's direct pointer and two-finger gestures; the desktop and the greeter both driven by it; the Settings Trackpad pane | done |
| PO7 | Pinch to the application: `WindowEvent::Pinch` with its phase, the scale since it began, its place and the modifiers, held by the window it began over; continuous zoom in Paint and the viewer | done |
| PO8 | virtio-multitouch: absolute ranges and properties from config space, MT protocol B framed by `SYN_REPORT` | done |
| PO9 | Guest proof: the harness's virtio-multitouch device and QMP control monitor, and the touch vertical — two taps launch the terminal on a desktop whose only pointing device is the touchscreen | done |
| PO10 | USB touchpads and touchscreens: the HID report model, the digitizer decoders, and the one USB HID class driver (`plans/HID.md` H1–H5) | done |
| PO11 | HID over I2C on device-tree machines: the rebuilt I2C protocol, supplier links, the GPIO line interrupt, the BSC's repeated START, and the I2C-HID class driver (`plans/I2C.md` I1–I4, I8; `plans/SUPPLIERS.md`; `plans/GPIO.md` G1–G2; `plans/HID.md` H6) | planned |
| PO12 | The DesignWare I2C controller with Intel LPSS, on x86_64's published PCI functions (`plans/I2C.md` I5–I6; `plans/FINISH-x86_64.md` S6) | planned |
| PO13 | HID over I2C from ACPI: the namespace bus driver, `PNP0C50` with its `_DSM` and `_CRS`, and the PCH GPIO controller (`plans/ACPI.md` A1–A4; `plans/GPIO.md` G3; `plans/I2C.md` I7) | planned |

## The scroll unit

A scroll is counted in **scroll units**, `SCROLL_UNITS_PER_DETENT` (120) to a
detent, at every layer: the device-level `InputEventKind::Scroll`, the seat
channel's `PointerInput::Scrolled`, `lib/input`'s `PointerScrolled`, and
`WindowEvent::Scrolled`. It is the evdev hi-res and Windows `WHEEL_DELTA`
convention, so a wheel that clicks reports 120 a detent and a fine wheel its
own fraction, and nothing between the device and the viewport rounds a slow
turn away. Positive `y` is toward the user (scrolling toward the end), positive
`x` toward the logical end, on every device.

## Wheels

- **virtio-input.** The driver reads the device's `EV_REL` bitmap from config
  space at open. A device that offers `REL_WHEEL_HI_RES` (or
  `REL_HWHEEL_HI_RES`) is read from that code alone on its axis and the
  low-resolution twin it also sends is ignored; otherwise the detent code is
  read at 120 a detent. A device that answers no bitmap is read at detent
  resolution. QEMU's `BTN_GEAR_*` presses stay one detent each.
- **USB HID.** `lib/hid`'s mouse decoder locates AC Pan (Consumer `0x238`)
  beside the wheel, and the Resolution Multiplier feature field of the
  collection enclosing each wheel. When `usb_hid` brings the interface up, the
  device engine sets each multiplier to its maximum with `SET_REPORT(Feature)`
  and reads it back; the multiplier the device confirmed divides that axis's
  counts, with the remainder carried, so a 16-step wheel reports exactly 120
  per detent. A device that refuses keeps a multiplier of one. Motion is read
  at the width the report descriptor declares, so a high-resolution sensor's
  fast flick is never clamped to eight bits.
- QEMU emulates no horizontal wheel on any device TAIRiX drives (its virtio
  and USB mice drop it host-side), so the horizontal decode is proven by host
  tests. The vertical wheel is proven on the guest by the Settings vertical,
  which scrolls the category strip to its end with QEMU's wheel
  (`PointerAction::Wheel`) and checks the frame that follows.

## A scroll at a place

`WindowEvent::Scrolled { window_id, x, y, dx, dy, modifiers }`: the
window-local position the pointer was at and the modifiers held, exactly as a
pointer event states them. A wheel over a window's frame is the frame's and
reaches no application. The window client's `scroll_input_events` feeds a
control the position first and the scroll after it, and the document host
states the modifiers before both, as it does for a press.

The seat owns two policies, so no application keeps its own copy:

- **Shift turns a wheel sideways.** A vertical-only wheel turn made with Shift
  held arrives horizontal. The modifiers still say Shift, so an application
  that wants a Shift-wheel of its own can tell.
- **Acceleration** is measured in scroll units: a turn faster than a
  deliberate detent rate is multiplied in proportion, up to a ceiling, and a
  fine wheel's small steps accelerate exactly as its detents would.

A scroll counts as a gesture to the lone-Ctrl locate, as a button does, so
Ctrl and the wheel never show the rings on release (D446).

## Zoom by the wheel

Ctrl and the wheel zoom an application that zooms — Paint and the viewer — one
rung of its zoom ladder for every detent's worth of units, the remainder
carried and dropped when the turn reverses, anchored so the picture point
under the pointer stays under it.

## Touch

Touch reaches the seat as frames, not as pointer motion, because what a
contact means is policy the seat owns: whether a tap clicks, which way two
fingers scroll, how far a fingertip moves the pointer.

- **The frame.** `TouchFrame` is one device frame: every contact touching or
  lifting in it — a tracking id stable while the finger is down, whether it is
  touching, whether the device trusts it as a finger rather than a palm, and
  its position normalised to 0..=65535 over the device's own range — with the
  surface's kind (a touchpad or a screen; a clickpad's surface is its button),
  its physical extent, and the physical buttons held. It is one fixed-size
  record, so two devices' contacts never interleave.
- **The channel.** A driver injects frames with `touch_inject`
  (`CAP_INPUT_INJECT`); the kernel validates each, stamps it with the injecting
  process and its monotonic arrival over whatever the driver wrote, and queues
  it on its seat's touch channel (64 frames), the oldest dropped when full, as
  the pointer channel's are. Only the seat's live lease owner drains it, with
  `touch_read` (`CAP_INPUT_READ`), and the seat's input readiness includes it,
  so every seat owner — the desktop and the greeter — drains it on every wake.
  Both staging copies are wiped, as a key's are: a touchscreen's contacts can
  spell what was typed on an on-screen keyboard.
- **The recogniser** (`lib/touch`) keeps a state machine per (injector,
  device) — at most eight, the least recently fed let go, releasing what it
  held — fed frames timed by their arrival, and answers pointer and gesture
  events; between frames it needs waking only at the one deadline it states,
  after every queued frame is fed. On a touchpad one finger moves the pointer
  at a gain rising from 4 to 16 pixels a millimetre with finger speed, times
  the user's speed, the motion held back while the touch could still be a tap
  so a tap clicks where the pointer was; two fingers moving together scroll,
  both axes, 25 units a millimetre, keeping to the axis they began on unless
  they began diagonally, and two moving apart or together pinch, whichever the
  centre's travel or the spread's change shows first. A brief tap (180 ms,
  1.3 mm) of one, two or three fingers is a primary, secondary or middle click
  where tapping is on; a one-finger tap's press is held 180 ms, so a touch that
  follows drags and a quick second tap double-clicks. A clickpad's press is the
  button its fingers count, a touchpad's own buttons are device buttons mapped
  through the seat's button order, and while any is held the moving finger
  drives the pointer and no gesture begins. A contact the device marks as a
  palm takes part in nothing until it lifts. On a touchscreen one finger puts
  the pointer where it lands and presses once it moves past 1.5 mm or rests
  100 ms, so a second finger landing with it begins the two-finger gesture at
  the fingers' centre instead of a click; a second finger during a press ends
  the press first. Content follows the fingers there, 10 units a millimetre. A
  touchscreen that states no size covers the screen, measured by the desktop's
  density.
- **Pinch to the application.** `WindowEvent::Pinch` carries the phase
  (begin, update, end, cancel), the scale since the pinch began in 16.16
  fixed point, the window-local place and the modifiers. A pinch belongs to
  the window it began over until it ends, as a drag does; a scale is relative
  to the start, so a zoom is `start × scale` and accumulates no rounding.
  Paint's and the viewer's zooms are continuous, the ladder kept for the
  stepping commands.
- **The seat owners.** The desktop session's input source drains the touch
  channel beside the pointer's into one stream: a touchpad moves the pointer a
  mouse moves, a touchscreen places it, and each source's press and release
  reaches the desktop as it happens, so a press whose driver died before
  releasing it (D508) is undone by the next release of that button from any
  device. The window manager holds a pinch for the window it began over and
  delivers every step as `AppPinch`; a run of updates folds latest-wins in the
  shell and in the hold-back. The greeter runs the same recogniser at its
  default settings. The touch settings are the desktop document's
  `touchpad.tap`, `touchpad.natural_scroll` and `touchpad.speed`, set in the
  Settings Trackpad pane.
- **Devices.** virtio-multitouch (QEMU, a screen), USB HID digitizers in
  Precision Touchpad input mode, and I2C-HID — the bus most laptop touchpads
  and touchscreens sit on — each normalise their contacts into frames, and
  send a frame with every contact lifted when their device goes away. The HID
  ones share one report model and decoder (`plans/HID.md`).
- **Guest proof.** `tools/qemu` attaches QEMU's virtio-multitouch device and
  taps it over the QMP control monitor (`PointerAction::Tap`), the human
  monitor having no touch command. The touch vertical
  (`tests/integration/touch_qemu_aarch64`) taps the Library button and then
  the terminal's row on a board with no mouse, and passes on the kernel's
  touch-delivery witness and the terminal's launch: the device, its driver,
  the seat, the recogniser and the bar, end to end. Two-finger scroll and
  pinch are proven from the recogniser through the session, the window
  manager and the window channel to Paint and the viewer by host tests; the
  audit trail can attest a launch, but not what an application did with a
  pinch.

## Invariants

- One scroll unit at every layer; no layer converts it to or from whole
  detents.
- A decoder reads exactly one of a hi-res code and its detent twin per axis.
- A scroll reaches only the application whose client area is under the
  pointer, with the position and modifiers stated.
- The Shift sideways turn and the acceleration have one definition each, in
  the seat.
- What a touch means has one definition, the recogniser, for every seat
  owner; a frame's source and time are the kernel's, never the driver's.
- A pinch's scale is relative to its start, and every step of one pinch goes
  to one window.
