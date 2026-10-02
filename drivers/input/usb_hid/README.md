# `tairix-drv-input-usb-hid` — the USB HID class driver

`plans/HID.md` H5. One instance per HID interface a host-controller driver
publishes, whatever its sub-class or protocol: every keyboard, mouse, touch pad
and touch screen application on the interface is served through `lib/hid`. The
crate is a `lib` (`BIND_KEYS`, `REQUIRED_CAPS`, and the host-testable bring-up)
and the `Run` program `devmgr` autoloads.

## What it does

1. Takes its interface's transport from its node: the URB endpoint, the shared
   buffer, and the interface number its requests address (`HwProperty::UsbInterface`).
2. Reads its interface from the device's configuration descriptor, then the
   report descriptor its HID descriptor states, and parses it.
3. Sets report protocol and reads it back from a boot-subclass device, which
   may ignore the request; sets the idle rate to report only on change; and
   configures the device's features (input mode, surface and button switches,
   contact limits, wheel resolution).
4. A descriptor that does not parse falls back to the boot layout on a boot
   keyboard or boot mouse; any other interface is refused, its reason logged.
   The descriptor is logged in hex at bind, the evidence a capture of an
   unfamiliar device needs.
5. Reads reports with blocking interrupt-IN URBs, so it parks in the kernel
   between reports, and injects each record into the boot seat.

When the interface goes, or keeps faulting, it releases every key, button and
contact the device held before it exits. Exit codes: `80` no driver host, `81`
no transport, `82` bring-up refused, `83` the interface kept faulting.

## Least privilege

`CAP_INPUT_INJECT`, `CAP_SHM`, `CAP_IPC_ENDPOINT`, `CAP_LOG_EMIT`. It holds no
register, DMA or interrupt, and the host-controller driver refuses any control
request of its that reaches past its own interface, so a compromised HID driver
cannot reconfigure the device or reach a sibling interface's driver.

## Supported hardware / limitations

Any HID interface the URB transport serves. Consumer and system controls and
vendor applications are not served, and keyboard LEDs are not driven
(`plans/HID.md` non-goals). The live report path is a metal acceptance item.

## Tests

`cargo test -p tairix-drv-input-usb-hid`: interface discovery, the HID class
requests, and bring-up against a mock interface: report and boot protocol, a
device ignoring `SET_PROTOCOL`, declined optional requests, the boot fallback
and refusals, feature configuration, and a device that goes mid-bring-up. The
decoders are `lib/hid`'s, the transport `lib/usb`'s.
