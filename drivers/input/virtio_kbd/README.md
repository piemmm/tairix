# `tairix-drv-input-virtio-kbd`

The autoloaded **user-space virtio-input keyboard driver process** — the `Run`
binary `devmgr` spawns when a virtio-input device is discovered (`AGENTS.md`
§18, `plans/PI.md` P10 chunk 5d-2-ii). This is the "drivers in user space"
steady state (`AGENTS.md` §4) on the hardware QEMU `-M virt` actually presents;
a USB keyboard is `drivers/input/usb_hid`'s.

## What it does

`main` (in `src/main.rs`, a freestanding pure-Rust `tairix-rt` program):

1. Builds `tairix_drvrt::RtDriverHost::from_grants_query` over the
   device-resource grants the kernel minted for this process (coherency `None` —
   the kernel carves coherent DMA and the QEMU `virt` virtio interconnect snoops
   the CPU caches, so the binary stays platform-neutral, `AGENTS.md` §2.20).
2. Selects its transport by the grant shape: the four role-tagged
   configuration windows of a virtio-PCI device
   (`tairix_abi::driver::virtio_pci::virtio_pci_windows`), or else the single
   register window of a virtio-MMIO one
   (`tairix_abi::driver::sole_register_window`), each read from
   `RtDriverHost::resources()` — no build-time board constant, and no read of
   PCI configuration space.
3. Builds the bus-agnostic `tairix_virtio::PciTransport` or `MmioTransport`
   over the mapped windows and brings the device online with
   `tairix_virtio_input::VirtioInput::open_armed`, binding the device interrupt
   only once the event queue is live (the host is the `VirtioHost` the device
   carves its event buffers from).
4. Pumps `VirtioInput::poll_reports`, parking on the device's interrupt between
   reports: a key edge becomes a `KeyInput` record, carrying its physical key,
   through `tairix_virtio_input::VirtioKeyboardConsole` and is injected through
   the `key_inject` syscall; pointer and touch reports go through
   `pointer_inject` and `touch_inject`.

Every capability and bound is re-checked kernel-side (`AGENTS.md` §5.4); the
host adds no authority. A bring-up failure exits with a reserved fail-closed
code (`80` no host, `81` no register window, `82` bring-up failed), leaving the
console without a keyboard rather than wedged (`AGENTS.md` §2.9); the spawning
supervisor decides whether to relaunch.

## Why a separate crate

The reusable open/poll/decode device logic and the `evdev` console producer
live in `lib/virtio_input`, and the §8 driver identity (`register` + bind table)
in `drivers/input/virtio_input`. This crate is a *separate* binary so it can
link the userland runtime `tairix-rt` without pulling it into the kernel-linked
`virtio_input` driver, and it depends only on `lib/*` crates (`lib/virtio`,
`lib/virtio_input`, `lib/drvrt`, `lib/rt`, `lib/caps`, `lib/abi`) so the §17.4
layering holds (no `drivers/*`→`drivers/*` edge — the `lib/usb` ↔
`drivers/bus/usb` precedent).

## Supported hardware / limitations

Any virtio-input device (virtio 1.1 §5.8) whose register window and DMA
constraint the kernel granted this process. Each `evdev` keycode is named as
its physical key and typed through the shared US QWERTY layout. The match key
carries no transport detail, so the same driver binds a virtio-input device
attached over PCI or MMIO (`AGENTS.md` §2.2).

## Capabilities

Granted at spawn from its matched node's requested resources: `CAP_MMIO_MAP`
(the register windows), `CAP_MEM_DMA` (the event-buffer DMA region),
`CAP_IRQ_BIND` (the device interrupt it parks on), `CAP_INPUT_INJECT`
(`key_inject`, `pointer_inject`, `touch_inject`) and `CAP_LOG_EMIT`.

## Tests

The decode/console logic is host-tested in `lib/virtio_input`; this crate is the
thin wiring binary (an inert host stub off bare-metal targets, so
`cargo build --workspace` / clippy / fmt cover it). The end-to-end autoload path
is the `-M virt` acceptance vertical (`plans/PI.md` P10 chunk 5d-2-ii).
