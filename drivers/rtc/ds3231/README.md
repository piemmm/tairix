# `tairix-drv-rtc-ds3231`

Autoloaded user-space real-time-clock driver for the **Maxim DS3231**, an I²C
calendar chip at bus address `0x68`.

## Supported hardware

Device-tree `compatible`: `maxim`, `ds3231`, `maxim`, `ds1307`. The
driver binds only through the discovery match (`BIND_KEYS`); it names no board
and never reaches for an address of its own.

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_IPC_ENDPOINT` | calling the I²C controller its node's link names |
| `CAP_SHM` | mapping the buffer of the channel opened over that link |
| `CAP_IPC_BIND_PRIVILEGED` | binding the RTC service endpoint it serves |

It requests **no** `CAP_MMIO_MAP`: it owns no registers of its own, only a
path to its part. It holds no clock authority either — the machine clock is
set by the one holder of `CAP_TIME_SET` (`userland/system/timed`), which
reads this driver's RTC service endpoint and tags the reading `Firmware`
itself. A compromised RTC driver can lie about the *chip*, but it can neither
assert a provenance it did not earn nor overwrite a network sync.

## One link, one part

Discovery gives this part's node an I²C link naming its controller and its
address. The driver opens a target channel over it, and the controller holds
every message to that one address, so this driver cannot name a neighbour on
the bus. A node linking no one target is left alone: the driver stands down
rather than guessing.

A chip driver whose controller is not yet serving gets a refusal from the
open; `timed`'s existing bounded RTC ladder covers the start-up ordering, so
nothing here retries or spins.

## What it can and cannot vouch for

The status register's oscillator-stop flag is set by the chip whenever the
oscillator has been interrupted — a flat backup cell, a first power-on — and
stays set until something clears it. Until it is cleared the counter is
meaningless, so `read` answers `Ok(None)` rather than reporting whatever the
registers happen to hold, and `set` clears it only *after* a successful write,
so the two agree.

The month register's top bit is a century **carry** the chip toggles when the
year field wraps; it says nothing about which century a freshly powered part is
in, so it is masked off and never read as one.

The `0x00..=0x06` block is the layout the DS1307 defined and the DS3231 kept,
so `maxim,ds1307` binds here too and one decode serves both parts.

The DS3231 carries its own backup-cell input and keeps counting from it, which
is the part's whole purpose, so `battery_backed` is reported `true`. A flat
cell shows up as the oscillator-stop flag rather than as a claim of
persistence.

Judging whether a *decoded* time is a believable wall time is clock policy and
belongs to `timed`. The one exception is the two-digit year, which names one
year in every hundred: it resolves through the class's shared
`resolve_two_digit_year`, against the same fixed window the wall clock
validates every reading against. An instant outside that window is **refused**
on write rather than stored as a year that would read back as another century.

## Testing

Host unit tests drive the chip against `tairix_i2c::mock::MockPart`, the
shared register-file part double — one definition every I²C chip driver's
tests use, so no driver carries a private copy of the scaffold. They cover the
24-hour and 12-hour register layouts, a lost clock integrity, a register block
that is not a calendar, a date the calendar does not have, the write/read
round trip, a failed write leaving the chip unable to vouch, and every year
the plausibility window admits.

**QEMU models no I²C controller** these parts hang off,
so no integration test is possible for this tier: no emulated machine in the
matrix presents an I²C bus with a clock chip on it. The decode, the encode,
and every fail-closed path are host-proven and the live part is an on-metal
acceptance item.

## References

- Maxim DS3231 data sheet, register overview.
- I²C-bus specification and user manual, NXP UM10204.
- `plans/TIMESYNC.md` TS-4 — the staged design this driver lands.
