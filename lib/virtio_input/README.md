# `tairix-virtio-input`

Arch-neutral, transport-agnostic virtio-input (keyboard / pointer / touch)
device logic: the virtio-1.1 §5.8 open/poll/decode engine over the bus-agnostic
`lib/virtio` `Transport`. It lives in `lib/*` so both the in-kernel `-M virt`
input verticals and the user-space input-driver process compose it without a
`drivers/*`→`drivers/*` dependency (`AGENTS.md` §17.4 / §2.2 — the virtio
analogue of `lib/hid` ↔ `drivers/input/usb_hid`). The thin
`drivers/input/virtio_input` crate keeps only the §8 `register` entry and the
§18.3 bind table built from `VIRTIO_INPUT_DEVICE_ID`.

See `docs/src/drivers/input.md` for the full description and test surface.

## Public surface

- `VirtioInput` — the device over a `lib/virtio` `Transport`: `open` (the
  virtio-1.1 §3.1 init sequence + event-buffer pool) and `poll`
  (`tairix_abi::driver::input::Input`, interrupt-driven drain, never a busy
  spin: a wait that cannot be made fails the poll `DeviceOffline` rather than
  returning nothing to be polled again). A drain takes at most a ring's worth
  of completions, and each slot is zeroed before it is reposted, so a
  completion that wrote nothing surfaces no event. Dropping it resets the
  device, and withholds the event pool from a device whose reset does not
  confirm.
- `poll_reports` / `Report` — the drain a touch device is read through: the
  same events, plus one `tairix_abi::touch::TouchFrame` per `SYN_REPORT` of
  a device that reports slotted contacts (the multi-touch protocol, type B).
  `touch_lifted` is the frame its driver injects when the device stops, so
  no contact stays held.
- `VIRTIO_INPUT_DEVICE_ID` — the virtio device id (18) the driver crate's
  `BIND_KEYS` match key is built from (the single source of truth, §2.2).
- `key_edge` — the keyboard producer half: names each decoded
  `evdev`-keycode `Key` `InputEvent` edge's physical key through one table (the
  key's HID usage) and answers the `tairix_abi::input::KeyEdge` a driver
  injects through `key_inject`; the seat reads what it types. An unknown
  keycode, a key's own repeat and a non-key event are no edge. Allocation-free
  and fail-closed (`AGENTS.md` §2.9).

## Dependencies

`lib/abi`, `lib/virtio` — both `lib/*` (§17.4). Names
no board, PCI, or SoC detail (`AGENTS.md` §2.20); the bus-agnostic `Transport`
abstracts the transport, so the same source binds a virtio-input device however
it is attached.

## Stability

Tier: `experimental`. The open/poll/decode surface is still evolving alongside
the `plans/PI.md` 5d-2-ii user-space input-driver bring-up; `abi-v1` types it
exchanges are governed by `lib/abi`.

## Tests

`cargo test -p tairix-virtio-input` — decode, multi-touch framing,
poll-drain, and teardown unit tests against the in-process `lib/virtio` `MockTransport` / `MockHost`, plus the
`console` producer's keycode-table, physical-key, modifier and lock tests
(`AGENTS.md` §7).
