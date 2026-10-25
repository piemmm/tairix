# `tairix-thermald` — the cooling service

Stability tier: **experimental**.

`thermald` reads the processor's temperature sensors and switches the CPU fan
wired to one of the board's header lines by the machine's `cooling.fan.*`
policy (`plans/COOLING.md`). It is the only holder of `CAP_GPIO`. Installed
at `/System/Services/thermald.app/Run` and started by PID 1 from the enrolled
tier, it runs on every machine: one with no header or no sensor states the
fault its policy meets and otherwise sleeps.

## What it holds

| Capability | Why |
|---|---|
| `CAP_GPIO` | the fan's header line: claim, switch, release |
| `CAP_SYSINFO_HW` | the hardware tree, its generation, and the sensors' channels |
| `CAP_FS_ACCESS` | `system.conf` and the watch on its directory |
| `CAP_IPC_BIND_PRIVILEGED` | the reserved `THERMAL_ENDPOINT` |
| `CAP_LOG_EMIT` | its records, events 32 000–32 999 |

## The policy

- **Off** gives the line back undriven. **On** holds it asserted, whatever
  an automatic sample last decided. **Automatic** samples the hottest
  processor sensor once a second: the fan starts at `cooling.fan.start` and
  stops once the processor has cooled by `cooling.fan.hysteresis`. An
  automatic fan runs from its claim until its first sample decides, and runs
  whenever a reading cannot be taken or no processor could be at it, the
  fault stated, until readings return.
- A policy naming a new line or polarity gives the old claim back before
  taking the new one. A header that leaves the tree takes the claim with it;
  the line is taken again when the header returns.
- A call the header leaves unanswered keeps the claim and is asked again a
  second later until it is answered; a line is claimed only once the header
  has answered that it holds none of the fan's.
- A header is believed only under a controller discovery states lends header
  lines, its endpoint that controller's own entry, so no other driver can
  stand in for it.

## Waking

Its wait set: the status endpoint, the configuration's directory watch (the
mount table's notice until that directory exists), and the hardware tree's
generation. Its deadline is an automatic fan's next sample, or a call the
header left unanswered asked again; off, always on, or with no line to
switch, and owing the header no answer, the service arms none. A call to a
driver parks on a second set, observing only its reply, for at most a
quarter of a second.

## The status

`thermal-v1` `Status` is ungated. It reports readings at most a second old, so
no number of callers reads a sensor or the header more often than that.

## Testing

Host tests drive the service over a fake machine: each mode, the hysteresis
band, a stopped automatic fan set always on, a sensor failure and its
recovery, a reading no processor could be at, a missing sensor, a refused
claim, a refused switch retried, a switch, claim and release the header
leaves unanswered, a fault met again and again, a line or polarity change,
the header leaving and returning, the channels the tree vouches for, a tree
whose sensors cannot be held, and the status's reading cache.
