# `tairix-drv-clock-rpi-firmware`

The Raspberry Pi firmware-clocks driver: the clocks the `VideoCore` firmware
owns, reached only by asking it over the property mailbox. Stability tier:
`experimental`.

It is the one driver of the `raspberrypi,firmware-clocks` node and serves two
roles over its clocks, which touch different clocks and share nothing:

- **The frequency mechanism** (`plans/CPUFREQ.md`). The ARM clock is the
  machine's DVFS seam. The driver takes the mechanism role with
  `cpufreq_bind` and, on a thread of its own, parks in `cpufreq_wait` applying
  the kernel governor's targets. Without it the firmware leaves the cores at
  `arm_freq_min` — 600 MHz on a Pi 4B whose parts are rated at 1.5 GHz.
- **The display's clocks** (`plans/SOUND.md` SND19g). The core clock the
  display engine runs from and the HDMI blocks' state-machine and pixel-path
  clocks are served over `clock-v1` to the nodes that link to them, each a
  floored clock: it runs at the rate the firmware makes for the highest floor
  its holders state, never below the least the firmware accepts, and returns
  to the rate it ran at before once its last holder leaves.

## Supported hardware

Any `raspberrypi,firmware-clocks` node. A board without one leaves the driver
unbound. A linked clock the firmware reports no range for is not served, so a
board whose firmware lacks one still runs its frequency mechanism.

Every range comes from the firmware (its *Get min clock rate* and *Get max
clock rate* tags), never from a board constant, so a board whose
`config.txt` moves one is driven over the range it has. The ARM clock's
targets are asked for at 100 MHz intervals: a finer step spends a firmware
round trip on a change too small to matter.

## What it vouches for

The firmware clamps a request to the clock's range and rounds it to a rate it
can synthesise, so every rate a `clock-v1` reply carries is the firmware's own
answer, never the request echoed. The ARM clock's applied rate is not
reported back to the kernel: the per-CPU estimator measures the live core
clock from the silicon's own counters, a better witness than this driver's
word.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | checked by `register`, as every driver's load gate |
| `CAP_MAILBOX` | reach the `vcmailbox` service, which admits this node's clocks and no others |
| `CAP_CPUFREQ` | take the machine's frequency mechanism role |
| `CAP_IPC_BIND_PRIVILEGED` | the node's `clock-v1` endpoint, under its clock `LinkDuty` |
| `CAP_LOG_EMIT` | the record of every decision |

It holds no `CAP_MMIO_MAP` and no `CAP_IRQ_BIND`: it owns no register window
and no interrupt line.

## Limitations

- **Either role ending ends the driver.** The kernel releases the mechanism
  role and the endpoint with the process, so a replacement can take both.
- **A linked clock cannot be stopped.** The property interface has no off; a
  clock the firmware had not started rests at the least it accepts once its
  last holder leaves (120 MHz for the state-machine clock), or where it runs
  when that least is zero.
- **Not exercisable under QEMU.** QEMU models no `VideoCore`, so the exchanges
  are proven against the mock firmware and the live channel is the on-metal
  acceptance item (`plans/PI.md`).
- **One ARM clock domain.** The Pi's four cores share one ARM clock.

## Events

| Id | Level | Decision |
|---|---|---|
| 24700 | Info | the mechanism applying targets |

A driver's ending is the driver runtime's record (`tairix_drvrt::exit`).

The `clock-v1` endpoint's own are `lib/clock`'s (`docs/src/lib/clock.md`).

## References

- The Raspberry Pi firmware's mailbox property interface (the
  `raspberrypi/firmware` wiki, *Mailbox property interface*): the clock ids
  and the clock-rate tags.
- `plans/CPUFREQ.md`, `plans/SOUND.md` SND19g.
