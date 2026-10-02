# Input events (`abi-v1`)

The desktop is driven by pointer, touch and keyboard input. A device's reports
reach the user-space desktop as a stream of framed records over a
capability-checked kernel input channel; the contracts of those records
live in `lib/abi/src/input.rs` (`tairix_abi::input`).

## The record

[`PointerInput`] is one decoded pointer event. The type makes illegal
states unrepresentable (`AGENTS.md` §2.11):

- `MovedBy { dx, dy }` — the pointer moved by a relative displacement in
  the device's count units (`evdev` orientation: positive x rightward,
  positive y downward).
- `Pressed(button)` / `Released(button)` — a [`PointerButtonCode`]
  (primary / secondary / middle) went down or came up at the current
  pointer position.
- `Scrolled { dx, dy }` — the scroll wheel turned, in **scroll units**
  (the pointer's orientation: positive x toward the logical end, positive y
  toward the user, scrolling downward), acting at the current pointer
  position. The seat owner accelerates a fast turn and forwards it as the
  `WindowEvent::Scrolled` an application receives, in the same units.

## The scroll unit

Every layer counts a scroll in scroll units,
`tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT` (120) to a detent: the
device-level `InputEventKind::Scroll`, this record, the desktop's
`PointerScrolled`, and `WindowEvent::Scrolled`. A wheel that clicks reports
120 a detent and a high-resolution wheel its own fraction, so nothing between
the device and the viewport rounds a slow turn away. It is the evdev hi-res
and Windows `WHEEL_DELTA` convention. A virtio device that offers the
`_HI_RES` codes is read from them alone; a USB mouse's Resolution Multiplier
is raised at enumeration and its counts are converted at the resolution the
mouse confirmed ([HID decode](../lib/hid.md)).

A `WindowEvent::Scrolled` carries the window-local position the turn landed
at and the modifiers held, exactly as a pointer event does, so an application
can qualify a turn (Ctrl to zoom) and route it to the control it landed on.
The seat delivers only turns over a window's client area; one over its frame
reaches no application. Two policies live in the seat alone: a vertical-only
turn made with Shift held arrives sideways, and a turn faster than a
deliberate one is accelerated.

The record is deliberately **screen-independent**: only the seat owner
(the desktop session, which owns the compositor) knows the screen's pixel
extent, so *it* accumulates displacements into the absolute, clamped
on-screen position — an input driver needs no display-geometry authority.

A record is exactly [`PointerInput::WIRE_LEN`] (20) bytes, little-endian:
a `"PIN1"` magic, the two-byte ABI version, a `kind` code, a `button`
code, a reserved half-word, and two 4-byte signed displacements. The
displacement fields carry the reported motion for a move, the signed scroll
units for a scroll, and are zero for a press or release (a pointing device
reports motion separately from clicks, and the seat owner applies a button at
the position its accumulated motion established — the same model as
`lib/input`).

## Fail-closed decoding

[`PointerInput::from_bytes`] validates every field before returning a
value and refuses anything inconsistent rather than guessing
(`AGENTS.md` §5.4 / §19.5): a short buffer, a wrong magic, an
unsupported version, a non-zero reserved field, an undefined `kind`, a
`button` code inconsistent with the kind (a button on a move, or no /
unknown button on a press), or a displacement on a press/release all fail
with the matching [`Errno`]. The decoder is enrolled in the `lib/abi`
fuzz harness (`AGENTS.md` §19.6).

## Relationship to the driver input ABI

This is **not** a duplicate of the device-level
[`driver::input::InputEvent`] (`AGENTS.md` §2.2). That type is what an
input *driver* reports across the [`Input`] driver trait: single-axis
pointer *deltas*, scroll in scroll units, and platform keycodes, one event
per axis or edge. `PointerInput` is the *seat-channel* record a driver
process injects (`pointer_inject`): button keycodes are resolved to the
closed button set, and a wheel turn becomes a `Scrolled` record in the same
units. [`PointerInput::from_device_event`] is
the one shared spelling of that mapping, so the virtio and USB HID driver
processes can never diverge.

## The keyboard record

[`KeyInput`] is the desktop-level counterpart of `PointerInput`: a key
going down or coming up, the [`KeyValue`] it produced — a Unicode
character (`Char`) or a named
non-character key ([`NamedKeyCode`]: Enter, the arrows, F1–F12, …) — and
the [`Modifiers`] (shift / ctrl / alt / meta) held at the time. As with
the pointer, this is the *resolved* event, not the device report:
turning raw keycodes and a keyboard layout into a produced character is
policy above the driver, not a second copy of the data (`AGENTS.md`
§2.2).

A third kind names no key at all: `ModifiersChanged` reports that the set
of held modifiers changed. A modifier produces no character and is no
`NamedKeyCode`, so it can never be typed into a text sink — but it still
has to be *reported*, because a consumer that only ever sees keys cannot
know a modifier is held when something other than a key arrives, a
pointer press most of all. The keyboard drivers emit one per *observable*
change: a key repeat, or letting go of one shift key while the other is
still held, produces nothing.

A record is exactly [`KeyInput::WIRE_LEN`] (20) bytes, little-endian: a
`"KIN1"` magic, the ABI version, a `kind` code (pressed / released /
modifiers-changed), the modifier bitmask, a `key_class` (char / named), a
4-byte codepoint, a 2-byte named-key code, and a reserved half-word.
Exactly one of the two key fields is set for a given class, and a
modifiers-changed record carries an all-zero key field — its one legal
spelling, so a dirty field is refused rather than read as a NUL character.
[`KeyInput::from_bytes`] validates every field — magic, version, reserved,
`kind`, `key_class`, the modifier bits, the named-key code, and that the
codepoint is a real Unicode scalar (an unpaired surrogate is refused) — and
fails closed with the matching [`Errno`] (`AGENTS.md` §5.4 / §19.5). It too
is enrolled in the `lib/abi` fuzz harness (`AGENTS.md` §19.6).

## Touch frames

A touch device reports frames, not pointer motion: what a contact means — a
tap, a scroll, a pinch — is the seat's policy, decided by its gesture
recogniser ([`tairix-touch`](../lib/touch.md)). [`TouchFrame`]
(`tairix_abi::touch`) is one scan of a surface: every contact on it — its
tracking id, whether it is touching or lifting in this frame, whether the
device judges it a palm, and its position normalised to `0..=65535` over the
surface — with the surface's kind (touchpad, clickpad, touchscreen), its
physical size in tenths of a millimetre (zero where unknown) and the physical
buttons held. A contact a frame stops naming has lifted. At most ten contacts
ride in one frame, each named once.

A driver injects a frame whole with `touch_inject` (`CAP_INPUT_INJECT`), so two
devices' contacts never interleave. The kernel validates it and writes its own
stamp over the frame's `source` and `time` fields: the injecting process and
the monotonic instant it arrived. The recogniser follows each surface by that
source and the injector's own device index, and times a tap by arrival, so a
busy desktop reading frames late never mistakes a tap for a rest. The seat's
owner drains frames with `touch_read` (`CAP_INPUT_READ` and the live lease,
like the pointer channel), and the seat's input readiness includes them.

A frame is exactly `TouchFrame::WIRE_LEN` (112) bytes, little-endian: a
`"TCH1"` magic, the ABI version, the device index, the 8-byte source and
time, a surface code, the button bits, the contact count, a reserved zero
byte, the width and height, then ten 8-byte contact slots — id, x, y, a flags
byte (touching, palm) and a zero byte — with every slot past the count zero.
[`TouchFrame::from_bytes`] refuses an unknown surface, button bit or contact
flag, more than ten contacts, a repeated id, a dirty reserved byte or unused
slot, and is enrolled in the `lib/abi` fuzz harness from seeded frames. The C
view carries the magic, size, bound and codes as `TAIRIX_TOUCH_*`.

## Pinch

A pinch reaches an application as `WindowEvent::Pinch`: its
[`PinchPhase`] (begin, update, end, cancel), the window-local place clamped
into the client, the modifiers held, and the scale — the fingers' spread
relative to when the pinch began, in 16.16 fixed point (`PINCH_SCALE_ONE`
unchanged). Because the scale is relative to the start, a zoom is the zoom it
began at times the scale and accumulates no rounding, and a run of updates the
desktop could not deliver at once folds to the newest. Every step of one pinch
goes to the window it began over, as a drag does; a cancel asks the
application to return to where the pinch began. On a touchpad the place is
the pointer; on a touchscreen it is the fingers' centre, which moves as they
pan.

## Where it is consumed

The desktop session backs its
[`InputSource`](../desktop/session.md) seam with two decoders that share
the same `lib/input` `InputEvent` stream: `DeviceInputSource` reads
`PointerInput` records from the kernel pointer channel and touch frames from
the touch channel, through the recogniser, and `KeyboardInputSource` reads
`KeyInput` records from the kernel keyboard channel. The window manager delivers a decoded key event to the
focused window; the taskbar takes no keyboard input.

A `ModifiersChanged` record decodes to `InputEvent::ModifiersChanged` and
always reaches the window-manager router, whatever the pointer is over: it
is seat state, not a key any surface receives. The router keeps the current
set and the session stamps it onto every `WindowEvent::Pointer` it delivers,
so an application can qualify a click by a modifier (a shift-click) without
shadowing state it could never see. In *text* focus the kernel arbiter types
nothing for it — the shared `lib/keymap` encoder yields no bytes — so a held
modifier never reaches a console as input.

[`PointerInput`]: ../../tairix_abi/input/enum.PointerInput.html
[`KeyInput`]: ../../tairix_abi/input/enum.KeyInput.html
[`KeyInput::WIRE_LEN`]: ../../tairix_abi/input/enum.KeyInput.html#associatedconstant.WIRE_LEN
[`KeyInput::from_bytes`]: ../../tairix_abi/input/enum.KeyInput.html#method.from_bytes
[`KeyValue`]: ../../tairix_abi/input/enum.KeyValue.html
[`NamedKeyCode`]: ../../tairix_abi/input/enum.NamedKeyCode.html
[`Modifiers`]: ../../tairix_abi/input/struct.Modifiers.html
[`PointerInput::WIRE_LEN`]: ../../tairix_abi/input/enum.PointerInput.html#associatedconstant.WIRE_LEN
[`PointerInput::from_bytes`]: ../../tairix_abi/input/enum.PointerInput.html#method.from_bytes
[`PointerInput::from_device_event`]: ../../tairix_abi/input/enum.PointerInput.html#method.from_device_event
[`PointerButtonCode`]: ../../tairix_abi/input/enum.PointerButtonCode.html
[`Errno`]: ../../tairix_abi/error/enum.Errno.html
[`driver::input::InputEvent`]: ../../tairix_abi/driver/input/struct.InputEvent.html
[`Input`]: ../../tairix_abi/driver/input/trait.Input.html
[`TouchFrame`]: ../../tairix_abi/touch/struct.TouchFrame.html
[`TouchFrame::from_bytes`]: ../../tairix_abi/touch/struct.TouchFrame.html#method.from_bytes
[`PinchPhase`]: ../../tairix_abi/touch/enum.PinchPhase.html
