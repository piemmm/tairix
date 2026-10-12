# `tairix-drv-irq-brcmstb-l2`

Autoloaded user-space driver for the **Broadcom second-level interrupt
controllers**: up to thirty-two device interrupts aggregated onto one line of
the root controller. On the BCM2711 the always-on controller (`aon_intr`)
carries the HDMI blocks' hotplug, CEC and wake-up lines. Stability tier:
`experimental`.

## Supported hardware

- `brcm,bcm2711-l2-intc`, `brcm,l2-intc`, `brcm,hif-spi-l2-intc`,
  `brcm,upg-aux-aon-l2-intc`: the edge-latching layout (status, clear, mask
  status, mask set, mask clear at `0x00`, `0x08`, `0x0C`, `0x10`, `0x14`).
- `brcm,bcm7271-l2-intc`: the level layout (status, mask status, mask set,
  mask clear at `0x00`, `0x04`, `0x08`, `0x0C`; no clear).

It binds only through the discovery match (`BIND_KEYS`) and learns which
layout its node holds from the bind key the node matched.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the controller's registers |
| `CAP_IRQ_BIND` | the controller's own line on the root controller |
| `CAP_IPC_BIND_PRIVILEGED` | the node's endpoint, under its interrupt-line `LinkDuty` |
| `CAP_LOG_EMIT` | the record of every decision |

## The service

The node's `irqline-v1` endpoint: a consumer waits on the line its
`InterruptLine` link names (`plans/SUPPLIERS.md` SL4, `plans/GPIO.md` G1). A
line is unmasked only while its consumer waits; when it fires it is masked and,
in the edge layout, acknowledged, and the wait is answered. An edge arriving
while masked stays latched and fires the moment the next wait unmasks it, so
none is lost; a level line cannot fire again before its consumer has serviced
its device and waited again. A line has one consumer: another live holder of a
link to it is refused `Busy`, and one an ended consumer held is taken over. A
consumer that ends has its lines masked and released.

## Limitations

- **No wake-up configuration.** `brcm,irq-can-wake` is not read; suspend is not
  a state this system enters.
- **Edges are latched, not counted.** Several edges while masked answer one
  wait, as the hardware's one status bit per line allows.

## Testing

Host tests drive the line server over a register model of both layouts: the
bring-up mask and clear, unmask on wait, mask and acknowledge on firing, an
edge latched while masked, a level line that cannot storm, attestation, one
consumer per line, reclaim from an ended consumer, and release on its exit.
Metal acceptance is the HDMI hotplug run (`plans/SOUND.md` SND19q).

## References

- The Linux `irq-brcmstb-l2` driver, for the register layouts.
- `plans/SOUND.md` SND19e — the staged design this driver lands.
