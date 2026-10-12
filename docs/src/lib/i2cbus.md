# `tairix-i2cbus`

`lib/i2cbus` is the controller side of `i2c-v1` (`plans/I2C.md`): the
endpoint every I²C controller's driver serves, and the bring-up and loop
around it. A driver supplies only its bus.

## The endpoint

`Controller` serves one controller node's endpoint over an `I2cBus`: the facts
the bus states — its speed and the repeated STARTs it can issue — and a
transaction run through a channel's buffer. Every request quotes an I²C link,
and counts once the kernel attests the caller holds it; a link naming another
endpoint is `OutOfRange`.

- **Open** makes the channel the link names: one per holder and link, a second
  `AlreadyExists`. Its buffer, `I2C_BUFFER_LEN` bytes, is made for the channel
  and granted to the caller, who is watched for its end; the reply carries the
  bus's facts and the grant.
- **Transfer** runs a transaction over the channel's buffer. A target link
  reaches the one address it names and a bus link every address, and a
  message aimed elsewhere refuses the whole transaction `PermissionDenied`; a
  shape the bus cannot issue as one is `NotSupported`. Both are refused before
  the bus is touched. The reply carries how the transaction ended on the bus,
  or the refusal of a controller that could not run it.
- **Close** drops the channel and its buffer.

A holder's end drops every channel it had. Channels live in a list searched
per request: one per link opened, and the links are those the tree grants the
targets on one bus.

## Bring-up and serving

`open` reads the node's grants and finds its I²C duty, answering
[`tairix-drvrt`](drvrt.md)'s `DutyNode`; `declared_speed` answers the speed
its node declares, standard mode when it declares none. `serve` builds the
endpoint over the driver's bus, each channel's buffer a shared region granted
to its client, and serves it through `tairix_drvrt`'s supplier loop. Every
failure ends the driver with a stable exit code and its reason logged.

## Events

| Id | Level | Decision |
|---|---|---|
| 24100 | Info | serving |
| 24101 | Error | ending, with the reason |
| 24102 | Info | a channel opened, and what its link reaches |
| 24103 | Info | a channel closed |
| 24104 | Info | channels dropped with their ended holder |
| 24105 | Info / Warn | a transaction that faulted: a target not answering, or a fault of the bus |
| 24106 | Warn | a request refused |

## Stability

Tier: `experimental` (see the crate `README.md`).
