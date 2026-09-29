# `tairix-drv-storage-emmc2` — Raspberry Pi 4 EMMC2 SD-host driver

`plans/PI.md` P8 deliverable. Implements `tairix_abi::driver::block::Block`
for the Raspberry Pi 4 (BCM2711) EMMC2 controller, an Arasan / SDHCI 3.00
SD host, at the fastest bus timing the controller, the card and the board
drive.

**Stability tier:** `experimental`. Host-tested against a register-level
model of the controller and card; the Pi 4 runs it as the storage bootstrap
floor (`crate::aarch64::root_unlock::emmc2_unlock` in the kernel).

## Bus speed

`Emmc2::open` resets the controller and reads what it can do — the SDHCI
version, capabilities and maximum-current registers (`host::HostCaps`). Every
SD clock is divided (`host::sd_clock`) from the base clock actually feeding
the controller: the platform's figure when the `Board` gives one (on a Pi 4,
the firmware's EMMC2 clock), else the capabilities register's; with neither,
bring-up refuses rather than guess.

Negotiation walks down a ladder, each rung verified by reading block 0 at its
own timing before it is accepted:

| Rung | Signalling | Mode chosen | Bus rate |
|------|------------|-------------|----------|
| UHS-I | 1.8 V | DDR50, else untuned SDR50, else SDR25 | 50 / 50 / 25 MB/s |
| High Speed | 3.3 V | High Speed, else Default Speed | 25 / 12.5 MB/s |
| Default Speed | 3.3 V | Default Speed | 12.5 MB/s |

UHS-I is tried only when the controller signals it and the `Board` supplies a
`CardSupply` that can both switch the card's I/O rail and cycle its power: a
card that has switched to 1.8 V comes back to 3.3 V only by losing power, so
the switch is attempted only where a failure can be undone. Bring-up selects
3.3 V on the supply before the first command, as a card powers up; a supply
that refuses even that is dropped, and the card runs at 3.3 V
(`BringUpStage::InitialSignalling` in `Link::fallback`). On the BCM2711
the capabilities offer DDR50 and SDR50-with-tuning, so a UHS-I card runs
DDR50 — the mode Linux runs on this controller. Sampling-clock tuning is not
performed, so a mode that needs it is never chosen.

The UHS-I sequence is the SD and SDHCI specifications': `ACMD41` with S18R;
on S18A, `CMD11`, the card holds `DAT0` low, the SD clock stops, the board
switches the rail and the host sets 1.8 V signalling, the clock stays gated
10 ms, restarts, and the card releases `DAT0`. Then `CMD2`/`CMD3`/`CMD9`/
`CMD7`/`CMD16`, 25 MHz, `ACMD51` (SCR), `ACMD6` (4-bit), `CMD6` to query the
card's modes and switch to the chosen one (and its current limit, where the
host's 3.3 V maximum current and the card both allow more than 200 mA), the
host's UHS mode select, High Speed timing and clock, and the verify read.

A failure in a step the rung added steps down one rung; if the card had left
3.3 V it is power-cycled first (rail off, I/O rail to 3.3 V, 10 ms off, on,
10 ms ramp). A card that never answers identification is power-cycled once
and retried. What made the bring-up settle for less is kept in
`Link::fallback`, and `Emmc2::link` reports the result, which the kernel logs
as the `root-unlock: emmc2 link` record.

`ACMD41` is polled every 10 ms for at most 100 rounds — the SD specification's
one second — through `SdhciHost::delay_us`, a timed park, never back to back.

Only high-capacity, block-addressed (SDHC/SDXC, CSD structure v2) cards are
supported; a byte-addressed, pre-v2, or CSD-v1 card is refused
(`DriverError::Unsupported`) rather than mis-addressed. The CSD is decoded
from the R2 response as the controller presents it: CRC stripped and
right-aligned, so `CSD_STRUCTURE` is at `RESP3[23:22]` and `C_SIZE` at
`RESP1[29:8]`.

## Transfers

The fast path is 32-bit ADMA2 through two host-granted DMA areas
(`SdhciHost::dma_region`): a `DMA_DATA_BYTES` data staging area moving
`DMA_STAGE_BLOCKS` (256 KiB) per command, and a `DMA_TABLE_BYTES` descriptor
table of 64 KiB descriptors over it (`adma::encode_table`). A longer transfer
is split per window. Before each command the engine synchronizes the table and
the data range (`SdhciHost::sync_dma`) and issues `dma_wmb`; after a read it
issues `dma_rmb` and synchronizes again before copying out. The kernel host's
staging lies inside the node's DMA window and is addressed as the
controller's bus sees it.

ADMA2 is kept only once bring-up has read block 0 by it into staging filled
with the inverse of what the data port read there, and the two agree; a
descriptor, bus address or cache maintenance that lands the data anywhere
else leaves the inverse behind, and the driver stays on the data port
(`Link::dma_fallback`).

The fallback is programmed I/O through the buffer data port, one command per
65535 blocks. Every path shares one command/transfer-mode encoding
(`data_command`). A multi-block command announces its length with Auto-`CMD23`
when the SCR says the card takes it, and is stopped by Auto-`CMD12` when it
does not. A data command whose own R1 reports an error fails, and every write
is followed by `CMD13`, which collects the errors the card reports only after
programming and waits out its programming busy.

A `BufferClass::Sensitive` transfer's staging copy is zeroed, and pushed out
to memory, before `read_blocks_with_class` / `write_blocks_with_class`
returns, whatever the outcome.

## Recovery

A failed transfer is recovered by the SDHCI error-interrupt sequence: the
command and data lines are reset, which halts the ADMA2 engine, and a
multi-block transfer is aborted with `CMD12`, its busy awaited. A controller
whose line reset never confirms is sent no abort and may still be mastering
the staging, so every later DMA transfer is refused and dropping the driver
withholds the staging for the kernel's DMA quarantine.

Only an answered abort proves the card back in `tran`. Otherwise the next data
command first asks with `CMD13`: a transfer still open is aborted and a
programming card's busy awaited on an R1b `CMD13`, each asked again, for at
most `CARD_STATE_ROUNDS` rounds; anything else fails closed and asks again
next time.

## Trace

The engine reports what it does through `SdhciHost::trace`, which the metal
host forwards to `CompletionWait::trace` (`trace::Trace`):

- each bring-up step as it starts, and each rung it attempts;
- the controller's version, capability and maximum-current registers;
- every SD clock it programs;
- each command and its first response word;
- the card's OCR, SCR and switch status;
- the DMA staging, and the first byte a failed DMA self-check differed at;
- every wait that ended without what it wanted, with the register value it
  last saw and how many completion parks it took.

A host that records nothing keeps the default no-op. The kernel's debug image
prints the trace (the `storage-trace` feature).

## Layered seam

The state machine is written against `SdhciHost`: register access, the
completion park, the timed wait, and the DMA areas. Metal drives it over
`IrqSdhci` — a capability-gated `RegisterWindow`, a `CompletionWait` (which
is also the `Delay`) parking on the controller's GIC line, and the two
`DmaSlab`s — built by `wiring::open_discovered`. The `Board` (base clock,
`CardSupply`) is borrowed for the bring-up alone. Host tests drive the engine
over `mock::MockSdhci`, a model of the controller, the card and the board's
supplies that asserts the SD clock never exceeds what the card's state allows
and moves data only when the host's timing matches the card's.

## Supported hardware

| Device                | Board   | Status                              |
|-----------------------|---------|-------------------------------------|
| `brcm,bcm2711-emmc2`  | Pi 4    | UHS-I DDR50, High Speed, ADMA2 + PIO (host-tested); metal pending |

The aarch64 `FdtDiscovery` emits the `brcm,bcm2711-emmc2` node (Storage
class, translated register window, and the DMA window of its `/emmc2bus`
`dma-ranges`). The driver's `BIND_KEYS` match it; as part of the storage
bootstrap floor the kernel binds it through the same `lib/devmatch` policy the
user-space `devmgr` uses.

## Required capabilities

- `CAP_DRV_LOAD` at `register` time.
- `CAP_MMIO_MAP` to map the register window (`wiring::open_discovered`),
  reached only through the host's `MmioMapper`.
- `CAP_MEM_DMA` (fast path only) for the DMA carves through the host's
  `DmaHost`; refused, the driver runs on programmed I/O.

## Completion and timed waits

Command and transfer completions park on the controller's interrupt
(`SdhciHost::await_irq`); the spec-mandated intervals park on a timer
(`SdhciHost::delay_us`). Only the controller's own reset and clock-stable
handshakes spin, each bounded by `DEFAULT_POLL_BUDGET`. Every wait fails
closed with `DriverError::DeviceFault` rather than waiting forever.

## Test surface

`cargo test -p tairix-drv-storage-emmc2`:

- Capability decode and the clock divider for both SDHCI divider forms,
  including the BCM2711's registers; the mode and current-limit policies; SCR
  and switch-status decode; ADMA2 table encoding; the command set.
- Negotiation (`bringup_tests`): DDR50 at 1.8 V with a supply and the exact
  UHS-I command order; High Speed without one; each rung's fallback — a failed
  voltage switch, a mode the board cannot carry, High Speed failing to Default
  Speed, Default Speed failing outright — with the power cycles and rail order
  each needs; a revived mute card; a refusing supply; the platform base clock
  outranking the capabilities; `ACMD41` paced one interval apart (D237).
- The trace (`bringup_tests`): every step of a UHS-I bring-up in order, from
  the reset to the link; a failed command's error status; a silent and a
  wedged controller's wait; a register that never settles; where a failed DMA
  self-check first differed.
- Transfers (`tests`): PIO and ADMA2 reads and writes, splitting across
  windows and past 65535 blocks, Auto-`CMD23`/`CMD12`, the DMA self-check
  abandoning a misdirected or failing engine, R1 and post-write status
  errors, sensitive-staging scrubbing, recovery and the card-state check, and
  the `wiring` capability gate.

## Public surface

The only public function is `register`. `Emmc2` is public so the host can
build it through `wiring::open_discovered` and reach it through `Block`;
`Emmc2::link`, `Link`, `BusMode`, `Rung`, `BringUpStage`, `BringUpFault` and
the `trace` module are its diagnostic surface. `SdhciHost`, `DmaRegion`, `DmaArea`, `CompletionWait`,
`CardSupply`, `SignalVoltage`, `Board`, `IrqSdhci` and the `DMA_*` sizing
constants are the seam a host implements or feeds.
