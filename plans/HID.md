# HID — one class driver, one report parser, every transport

Binding under `AGENTS.md`. How a HID device — a keyboard, a mouse, a touchpad,
a touchscreen — reaches the seat: the report-descriptor model every HID
transport shares, the application decoders built on it, and the class drivers
that run them for USB and for I2C (`plans/I2C.md`).

## Ledger

| Id | Item | Status |
|---|---|---|
| H1 | The report-descriptor model (`lib/hid`): every Input, Output and Feature item a field with its report, usages, flags, logical and physical range and unit; collections with their kind, usage and parent; fixed bounds for untrusted input | done |
| H2 | The application decoders over the model: keyboard (array and bitmap key layouts), mouse (buttons, relative X/Y, wheel, AC Pan, resolution multipliers), touchpad and touchscreen (fingers, contact count, scan time, buttons, confidence, surface size, a frame spread over several reports), and the configuration a device needs (input mode, surface and button switches, multipliers) | done |
| H3 | The HID device engine: one device's applications driven from its transport (`HidTransport`: report descriptor, feature reports, input reports) to seat records (key, pointer, touch), every held key, button and contact released when the device goes | done |
| H4 | The USB host controller carries HID as it carries any class: raw interrupt-IN reports sized by the class driver, a control data stage a report descriptor fits, control requests scoped to the node's interface, the interface number on the node, no HID logic in `lib/usb` (`plans/USB.md` U11) | done |
| H5 | The USB HID class driver (`drivers/input/usb_hid`): one per HID interface, binding by class, every application on the interface served | done |
| H6 | The I2C-HID class driver (`drivers/input/i2c_hid`) over the same engine: the HID-over-I2C descriptor, reset and power, the interrupt-driven input register (`plans/I2C.md` I8) | planned |

## Why the class driver parses

A report descriptor is untrusted device input, so it is parsed in the process
with the least authority that can act on it: the class driver, which holds its
interface's transport endpoint and the seat's inject capability and nothing
else. The host controller holds the controller's registers and DMA; a parser
bug there reaches every device on the bus. The class driver also sees every
application an interface declares, so an interface carrying a keyboard and a
touchpad behind report IDs is served whole, which one normalised layout per
interface could never do.

The host controller keeps the standard descriptors (device, configuration,
interface, endpoint, hub) it needs to enumerate and schedule; a class-specific
descriptor is its class driver's.

## The model (H1)

- **Items.** Short items per HID 1.11 §6.2.2; a long item is consumed and
  ignored, as it carries no defined meaning. Global state with a push/pop stack;
  local state cleared at every main item. Usages are 32-bit with their page,
  either an explicit list or a minimum/maximum range; a variable field with
  fewer usages than elements repeats the last.
- **Fields.** Each Input, Output and Feature item is one field: its report
  (`Unprefixed`, or `Prefixed(id)` — never an id of zero standing for none), bit
  offset within the report body, element size and count, flags (constant,
  variable, relative, wrap, non-linear, no preferred state, null state,
  volatile, buffered bytes), usages, logical and physical ranges, unit and unit
  exponent, and its innermost collection. Offsets are kept per report and kind,
  so a re-entered report ID continues its own.
- **Collections.** Kind (physical, application, logical, report, named array,
  usage switch, usage modifier, vendor) and usage, with their parent, so a
  decoder selects by the application a field sits under.
- **Bounds** — defences on untrusted input, not capacities: a descriptor of at
  most 4096 bytes (Linux's `HID_MAX_DESCRIPTOR_SIZE`), at most 256 fields and
  128 collections, 1024 usage entries, a collection depth of 16, a global
  stack of 8, a report of at most 4096 bytes (a touchpad's certification
  feature alone is 256). An input outside a bound, an unbalanced collection, a
  field with no report a decoder could read, or an undemuxable report layout (a
  field placed before the first report ID of a descriptor that declares IDs),
  or a report ID of zero, which HID reserves, is refused whole. The model is built once per device; allocation failure refuses
  the device.

## The decoders (H2)

Each top-level application collection is offered to the decoder for its usage;
one the seat has no channel for is set aside.

| Application | Usage | Decoder |
|---|---|---|
| Keyboard, Keypad | Generic Desktop `0x06`, `0x07` | keyboard: modifiers `0xE0..=0xE7`, key array or key bitmap, both page `0x07` |
| Mouse, Pointer | Generic Desktop `0x02`, `0x01` | mouse: buttons, relative X/Y, wheel `0x38`, AC Pan (Consumer `0x238`), the Resolution Multiplier `0x48` of the collection enclosing each wheel |
| Touch Pad | Digitizers `0x05` | touchpad |
| Touch Screen | Digitizers `0x04` | touchscreen |
| Device Configuration | Digitizers `0x0E` | the Input Mode feature only |

- **Keyboard.** Edge-detects the key set between reports; a report naming
  `ErrorRollOver` (`0x01`) in its array is a phantom state and changes nothing.
  A bitmap (NKRO) layout is the same key set read from bits.
- **Mouse.** Relative axes only: an absolute X/Y under a pointer collection is
  not a mouse and is set aside, as no seat channel places a pointer absolutely
  from a pointing device. Wheel counts are converted to scroll units at the
  multiplier the device confirmed (`plans/POINTING.md` PO2).
- **Touch.** Each Finger logical collection (`0x22`) is one contact slot: Tip
  Switch `0x42` (touching, so a hovering pen in range is not a contact),
  Confidence `0x47` (zero marks a palm), Contact Identifier `0x51`, X and Y
  (absolute, normalised over their logical range). The application's Contact
  Count `0x54` and Buttons complete the frame; a device whose contacts
  outnumber its slots sends the rest in further reports with a zero count, and
  the frame is the union, closed when the first report's count of slots has
  arrived. A frame a new one interrupts is dropped, never delivered short,
  since a contact missing from a frame reads as lifted. The surface's physical size comes from the X/Y physical range, unit
  and exponent; a Pad Type `0x59` feature of zero, or a Button 1 beside the
  fingers, makes a touchpad a clickpad. Contact Count Maximum `0x55` bounds the
  frame; a report naming more contacts than it is refused.
- **Configuration.** Input Mode `0x52` is set to touchpad (3) or touchscreen
  (2); Surface Switch `0x57` and Button Switch `0x58` are set on; each wheel's
  Resolution Multiplier is raised to its maximum, read back, and the answer
  adopted. A refused feature leaves the device as it is, and a touch device left
  in mouse mode is served as the mouse it then is.

## The engine and the transports (H3–H6)

`HidTransport` is what a class driver gives the engine: the report descriptor,
`GET_REPORT`/`SET_REPORT` for features, and the next input report (parking until
one arrives). The engine parses, configures, decodes and answers seat records;
the class driver injects them. A device that goes — unplugged, faulted, or its
driver ending — has every key, button and contact it held released first.

- **USB (H4, H5).** `usb_hid` binds every HID interface by class (`0x03`,
  sub-class none or boot, protocol none, keyboard or mouse). At bind it reads the
  report descriptor with `GET_DESCRIPTOR(Report)` on its interface, sets report
  protocol, reads the protocol back with `GET_PROTOCOL` (a device may ignore the
  request), sets the idle rate to zero (a mouse in report protocol then reports
  only on change), and configures features. A descriptor that does not parse, on
  a boot-subclass interface, falls back to boot protocol and the fixed boot
  layout; on any other interface the device is refused with its reason logged.
  The descriptor is logged in hex at bind, as the evidence a metal capture needs.
  Each interrupt-IN request names the interface's longest input report, the
  transfer Linux's usbhid submits, and the host controller arms no less than one
  service interval's payload, so a report spanning packets arrives whole
  (`plans/USB.md` U11). A transfer longer than one interval's payload once
  faulted behind the Pi 4's transaction translator, so a full-speed device whose
  reports exceed its packet size is part of the live acceptance
  (`plans/USB.md` UM).
  A device that goes, or faults four reads running, is released and its driver
  exits; a refused bring-up logs its reason (events 4244–4248).
- **I2C (H6).** `i2c_hid` reads the HID descriptor at the register the platform
  names (`hid-descr-addr`, or ACPI `_DSM`), resets the device, reads the report
  descriptor, and reads the input register while its interrupt line is asserted
  (`plans/I2C.md`).

## Invariants

- One report parser and one decoder per application, for every transport.
- A descriptor or report is validated whole before anything is applied; a
  malformed report changes no held state.
- Fields are read only from their own report; an unknown report ID is ignored.
- The host controller holds no HID knowledge, and a class driver holds no
  controller authority.

## Non-goals

- Consumer Control, System Control and vendor applications: the seat has no
  channel for media or power keys, so they are set aside.
- Keyboard LED output reports.
- Absolute pointing devices outside a digitizer collection (tablets in mouse
  emulation).
