# `tairix-hid`

`lib/hid` is the HID protocol every HID class driver runs, whatever carries
its reports: the report-descriptor model, the application decoders over it,
the configuration a device needs, and the device engine that drives them from
a transport to seat records. The USB class driver (`drivers/input/usb_hid`)
and the I2C one share it; the transport is the driver's, and nothing here
knows of one. The design is `plans/HID.md`.

## The model

`ReportDescriptor::parse` reads a report descriptor (HID 1.11 §6.2.2) into
fields and collections. Every Input, Output and Feature item is one `Field`:
its report (`ReportId::Unprefixed`, or `Prefixed` with a non-zero id), its bit
offset within the report (after the id byte when there is one), element size
and count, flags, usages, logical and physical range, unit and exponent, and
the innermost collection it sits in. Offsets run per report and kind, so a
re-entered report continues its own. A usage carries its page; one written
without a page takes the page in force at its main item, as Windows and Linux
read it. Push and Pop save and restore the global state with the report id; a
long item is skipped; of a delimiter set only the first usage is taken.

The bounds are defences on untrusted input, not capacities: a descriptor of at
most 4096 bytes, 256 fields, 128 collections, 1024 usage entries, a collection
depth of 16, a global stack of 8, a report of at most 4096 bytes and an
element of at most 256 bits. A descriptor outside a bound, with an unbalanced
collection, a report id of zero, a malformed usage range or delimiter, or data
a report id cannot demultiplex (a data field outside every id in a descriptor
that declares ids) is refused whole. Allocation is fallible; running out of
memory refuses the device.

## The decoders

Each top-level application is given to the decoder for its usage, and one the
seat has no channel for is set aside. A decoder reads only fields of its own
application and its own reports, validates a report whole before applying
anything, and answers `Decoded::NotMine`, `Applied` or `Malformed`; a
malformed report changes nothing held.

- **Keyboard** (Generic Desktop Keyboard and Keypad): the key set from an
  array or a bitmap, edge-detected between reports and resolved to `KeyInput`
  records by `KeyboardConsole` through `lib/keymap`. The modifier bitmap alone
  reports the modifiers; a key named twice is pressed once; a report naming
  `ErrorRollOver` is a phantom and changes nothing.
- **Mouse** (Mouse and Pointer): three buttons, relative X and Y, the wheel and
  AC Pan in scroll units at the resolution the device confirmed, the remainder
  carried so a fine wheel reports exactly one detent's units per detent. An
  application placing X or Y absolutely is no mouse. Buttons are delivered
  first, then motion, then scroll; every value saturates rather than wraps.
- **Touch** (Touch Pad and Touch Screen): each Finger logical collection is a
  contact slot — tip switch, confidence (zero is a palm), contact identifier,
  absolute X and Y normalised over their logical range — and the application's
  contact count and buttons complete the frame. A frame spread over several
  reports, the later ones counting zero, is delivered once when the first
  report's count of slots has arrived; one a new frame interrupts is dropped,
  as a contact missing from a frame reads as lifted. Contact Count Maximum
  bounds a frame. The surface's size comes from the physical range and its
  centimetre or inch unit; a pad with only a first button, or a Pad Type of
  zero, is a clickpad.

## Configuration

`HidDevice::configure` exchanges feature reports over the driver's
`HidTransport`: Input Mode set to touchpad or touchscreen, the surface and
button switches set on, Contact Count Maximum and Pad Type read and adopted,
and each wheel's Resolution Multiplier raised to its finest setting, read back,
and the answer adopted. A refused feature leaves the device as it is; only a
device that has gone fails the configuration.

## The engine

`HidDevice::new` builds the decoders for a parsed model, or refuses a device
carrying nothing the seat serves. `input` hands one report to every decoder;
`release` lets go of every key, button and contact the device held, so a
device that is unplugged, faults or whose driver ends leaves nothing pressed.
`boot::keyboard` and `boot::mouse` are the boot-protocol layouts as models, so
a device in boot protocol runs through the same decoders.

`transport_error` and `pump_error_limit_reached` are a driver loop's refusal
policy: only the transport endpoint itself having gone (`Errno::NotFound`) is
the device leaving, and every other refusal is a fault the loop rides out
under a saturating consecutive-failure limit.

## Layering

`lib/hid` depends on `lib/abi`, `lib/input` and `lib/keymap` only, touches no
register and holds no DMA. A report descriptor is untrusted device input, so it
is parsed in the class driver, which holds its interface's transport and the
seat's inject capability and nothing else; the host controller carries
reports and never reads them.

## Test surface

`cargo test -p tairix-hid` exercises the model (item, global and local state,
push and pop, page resolution, ranges, delimiters, report ids and every
refusal), each decoder against built descriptors, and the engine and its
configuration exchange against a mock transport.

`tests/fuzz_hid_report.rs` (`cargo xtask fuzz`) mutates real descriptors — boot
layouts, report-id keyboards and mice, a wireless receiver's interfaces, a
high-resolution mouse, a Precision Touchpad, a touchscreen — assembles random
items, and feeds noise, holding: no input panics; an accepted model places
every field inside its own report, after its id; a long item or globals a Push
and Pop enclose change nothing; a malformed report delivers nothing; a button
is never pressed twice or released unheld; a device let go of holds nothing;
configuration fails only when the device has gone; and the console producer
resolves only key edges.

## Stability

Tier: `experimental` (see the crate `README.md`).
