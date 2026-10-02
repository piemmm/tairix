# `tairix-hid`

The HID protocol every HID class driver runs, whatever its transport
(`plans/HID.md`): the report-descriptor model, the keyboard, mouse, touchpad
and touchscreen decoders over it, the configuration a device needs, and the
device engine that drives them from a transport to seat records. It names no
device, bus or board, so the USB and I2C class drivers share it from `lib/*`.

See `docs/src/lib/hid.md`.

## Public surface

- `ReportDescriptor::parse` — the model: every Input, Output and Feature item
  a `Field` (its report, bit offset, element size and count, flags, usages,
  logical and physical range, unit), every `Collection` with its kind, usage
  and parent. A descriptor outside the fixed bounds is refused whole.
- `HidDevice` — one interface's applications: `new` keeps those the seat has
  a channel for, `configure` exchanges feature reports over a `HidTransport`,
  `input` decodes one report onto a `SeatSink`, and `release` lets go of every
  key, button and contact held.
- `KeyboardDecoder`, `MouseDecoder`, `TouchDecoder` — the per-application
  decoders a `HidDevice` composes.
- `boot::keyboard`, `boot::mouse` — the boot-protocol report layouts, as
  models.
- `KeyboardConsole` — key usages resolved to `KeyInput` records through
  `lib/keymap`.
- `transport_error`, `pump_error_limit_reached` — a driver loop's refusal
  policy.

## Dependencies

`lib/abi`, `lib/input`, `lib/keymap`.

## Stability

Tier: `experimental`.

## Tests

`cargo test -p tairix-hid`: the model, each decoder and the engine against
built descriptors and a mock transport. `tests/fuzz_hid_report.rs`
(`cargo xtask fuzz`) holds the parser, the decoders and the configuration
exchange to their invariants over mutated real descriptors, random items and
noise.
