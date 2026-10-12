# `tairix-clock`

`lib/clock` is the controller side of `clock-v1`
([clock-controller drivers](../drivers/clock.md)): the endpoint every clock
controller's driver serves, and the bring-up and loop around it. A driver
supplies only its hardware.

## The endpoint

`Controller` serves one controller node's endpoint over a `ClockHardware`:
which clocks it serves and by which selector, each clock's binding id and
policy, its rate, the setting nearest a rate, and running or stopping a clock.
A request counts once the kernel attests the caller holds the clock link it
quotes; a request naming another endpoint is `OutOfRange`, a clock the
hardware does not serve `NotSupported`.

A clock is held by every process instance that runs or floors it, and its
policy decides how holders share it:

- **Exact.** `Run { hz }` runs it at the setting nearest `hz`. Another
  instance joins at the rate it runs at and is refused `Busy` at any other;
  only a sole holder may retune it. One consumer's stream is never retimed
  under it.
- **Floored.** `Floor { hz }` states the least rate this holder needs. The
  clock runs at the lowest rate the hardware makes at or above the highest
  floor stated, and comes down as holders lower their floors or leave. A floor
  above what the hardware can make is `OutOfRange`, never met with less.

Each policy refuses the other's request `NotSupported`. The last holder's
release, or its end, returns the clock unheld: stopped, or for a clock
something else needs running, at the rate it ran at before. A holder whose
end the kernel has not yet reported is found out by the kernel's refusal to
watch it, so a restarted consumer is not refused for its predecessor's hold.
A change that fails leaves the clock as it was: every hold already standing
stands, and the requester gains none.

## Bring-up and serving

`open` reads the node's grants and finds its clock duty, answering
[`tairix-drvrt`](drvrt.md)'s `DutyNode`, which maps the node's one register
window; `fixed_rate` answers the rate the tree states for a `fixed-clock`
entry of the node's `clocks`. `serve` builds the endpoint over the driver's
hardware and serves it through `tairix_drvrt`'s supplier loop. Every failure
ends the driver with a stable exit code and its reason logged.

## Events

| Id | Level | Decision |
|---|---|---|
| 24300 | Info | serving |
| 24301 | Error | ending, with the reason |
| 24302 | Info | a clock set running, retuned, or raised or lowered to a floor |
| 24303 | Info | a consumer joined an exact clock |
| 24304 | Info | a clock returned with its last holder |
| 24305 | Info | a hold dropped with its ended holder |
| 24306 | Warn | a clock's old setting cut off mid-period |
| 24307 | Error | a clock that would not change, left as it is |
| 24308 | Warn | a request refused |

## Testing

`testkit::Rig`, behind the `test-support` feature, drives an endpoint over a
driver's hardware through the shared fake kernel, so each clock driver's
tests exercise its own hardware behind the one endpoint. The crate's own
tests run scripted hardware of both policies, including a seeded walk that a
clock runs exactly while held at its policy's rate.

## Stability

Tier: `experimental` (see the crate `README.md`).
