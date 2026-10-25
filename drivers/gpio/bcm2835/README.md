# `tairix-drv-gpio-bcm2835`

Autoloaded user-space driver for the **BCM2835-family GPIO controller**: the
Raspberry Pi 4's BCM2711 block. Stability tier:
`experimental`.

## Supported hardware

- `brcm,bcm2711-gpio`: fifty-eight lines.

It binds only through the discovery match (`BIND_KEYS`).

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the controller's registers |
| `CAP_IRQ_BIND` | the three bank interrupts on the root controller |
| `CAP_IPC_BIND_PRIVILEGED` | the node's interrupt-line endpoint, under its `LinkDuty` |
| `CAP_HW_EMIT` | the header's `tairix,gpio-header` channel node |
| `CAP_LOG_EMIT` | the record of every decision |

The header endpoint is bound restricted-sender requiring `CAP_GPIO`, which
the driver never holds.

## The services

- **`irqline-v1`** to devices the hardware tree links to a line
  (`plans/GPIO.md` G2). The selector is the binding's two cells, the line and
  its trigger (rising, falling, both, high, low). The line is set to input. A
  level line's detection is enabled only while its consumer waits and is
  disabled as it fires. An edge line's is enabled by every wait and disabled
  when an edge fires with no wait posted; that edge answers the next wait at
  once, so a line that keeps firing wakes the driver at most once between
  waits.
- **`gpio-header-v1`** to system services holding `CAP_GPIO`
  (`plans/COOLING.md` CL2), served only where discovery states the node's
  `GpioHeaderLines`. A lent line is listed as free, routed to a peripheral,
  held as an interrupt or driven as an output; a free line is claimed with a
  polarity and an initial state, the level set before the direction. A line
  routed to an alternate function is never lent.

A line has one holder at a time, of either kind. A holder that ends has every
line it held given back: an interrupt's detection off and its event cleared,
an output returned to an input.

## Limitations

- **Pulls are not set.** They are the board's wiring, stated by the
  firmware's `gpio=` lines; nothing here touches the pull registers.
- **Edges are latched, not counted.** Several edges with no wait posted
  answer one wait: the first masks the line until it, as the one status bit
  per line allows.
- **No asynchronous detection.** The asynchronous edge enables are cleared at
  bring-up and never set: every trigger uses the synchronous detectors.

## Testing

Host tests drive the controller over a register model of the block: bring-up
quiescing, the header's listing and the selects it reads, claims and their
refusals, polarity, holder checks, release with an ended holder, edge
latching and an edge that fires unwaited masked until the next wait, a level
that cannot storm, the enables an interrupt reads, a line the block does not
have, one holder per line across both kinds, attestation and a caller the
kernel cannot attribute on either endpoint, an unbound bank, the header's
calls a wake, and reclaim from an ended consumer. QEMU does not model the BCM2711, so metal
acceptance is a fan on a Raspberry Pi 4 (`plans/COOLING.md`).

## References

- BCM2711 ARM Peripherals §5 and BCM2835 ARM Peripherals §6.
- BCM2711 ARM Peripherals §5.1, for the bank-interrupt groups, and the
  `brcm,bcm2835-gpio` binding (taken under BSD-2-Clause), for their order.
