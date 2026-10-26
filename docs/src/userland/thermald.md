# Cooling service (`thermald`)

`thermald` reads the processor's temperature sensors and switches the CPU fan
on a board's header line by the machine's `cooling.fan.*` policy
(`plans/COOLING.md` CL6). It is the only holder of `CAP_GPIO`, runs as the
`thermald` service account (uid 23), and is started by PID 1 from the
enrolled tier with `restart=on-failure`, since the fan's line is undriven
while it is down. Its log events are 32 000–32 999.

## The policy

| Key | Values | Default |
|---|---|---|
| `cooling.fan.mode` | `off`, `on`, `auto` | `off` |
| `cooling.fan.line` | `none`, a GPIO line 0–63 | `none` |
| `cooling.fan.signal` | `active-high`, `active-low` | `active-high` |
| `cooling.fan.start` | °C, 40–85 | 65 |
| `cooling.fan.hysteresis` | °C, 2–20 | 10 |

- **Off** gives the line back undriven, so another holder may take it.
  **On** holds it asserted, whatever an automatic sample last decided.
  **Automatic** samples the hottest processor sensor once a second; the fan
  starts at `start` and stops once the processor has cooled by
  `hysteresis`, so a fan near its threshold does not chatter.
- An automatic fan runs from its claim until its first sample decides, and
  runs whenever a reading cannot be taken or reads what no processor could
  be at — below −55 °C, the coldest ambient silicon is rated to run in, or
  above 150 °C, its absolute-maximum junction temperature — with the fault
  stated, until readings return: a fan that runs needlessly costs noise, one
  that stops needlessly costs the processor.
- A policy naming another line or polarity gives the old claim back before
  taking the new one.
- A drive the header refuses means it no longer honours the claim: its
  controller restarted holding none, or ended. The line is given back and
  taken again at once. Every hardware-tree change drives the held line again,
  so a restarted controller is found out even while the fan has nothing to
  switch.
- A call the header leaves unanswered past its deadline is not a refusal:
  whether the header acted on it is unknown. The claim is kept, the fault
  stated, the fan decided as if it may be running, and the call asked again a
  second later until the header answers. A line is claimed only once the
  header has answered that it holds none of the fan's, so a claim that landed
  unanswered is switched rather than claimed again, never refused as busy.
- A channel is believed only where the kernel's binding vouches for it: its
  endpoint is its block's entry for the node it hangs from, which only the
  driver loaded for that node may serve. A header must also hang from a
  controller discovery states lends header lines, and a sensor from a node
  discovery states measures the processor; no driver may publish either
  fact, so no other driver can stand in for one.

## Faults

| Fault | Meaning |
|---|---|
| `NoLine` | the policy runs the fan but names no line |
| `NoHeader` | the machine has no header served |
| `LineUnavailable` | the header would not lend the line: not one it lends, in use, or its controller not answering |
| `NoSensor` | automatic with no processor sensor; the fan runs |
| `SensorFailed` | a reading could not be taken, or no processor could be at it; the fan runs |

Every fault is recorded once as it begins, with what is behind it — the
refusal of the call that met it, `TimedOut` for one the header left
unanswered, `OutOfRange` for an implausible reading — and once as it clears.
A missing header or sensor, or a line the header refused, is retried when the
hardware tree changes, never on a timer; a call the header left unanswered is
the one asked again on a timer, a second later.

## Waking

Its wait set: its status endpoint, the machine configuration's directory
watch (the mount table's notice until that directory can be reached — absent,
or on a root not yet served), and the
hardware tree's generation. Its deadline is an automatic fan's next sample,
or a call the header left unanswered asked again, so a fan that is off,
always on, or has no line to switch, and owes the header no answer, costs no
wake-ups at all. A call to a sensor or the header parks on a second wait set,
observing only that call's reply, for at most a quarter of a second, so a
wedged driver never stalls the service, though its endpoint, the watch and
the tree wait meanwhile: a wedged sensor costs the fan a reading, which runs
it. Every such call goes through `tairix_rt::call_within`, the deadline call
the block client shares. The configuration is followed through
`tairix_rt::follow`, the one file-following engine it shares with the time
service.

## The status

`thermal-v1` `Status` on `THERMAL_ENDPOINT` is ungated: the processor's
temperature and whether the fan runs are facts every system shows to every
user. It answers the hottest processor reading, the policy in effect, whether
the fan runs, the fault, and the header's lent lines as two masks — every
line lent, and those the fan may take, free or its own already — or none when
no header serves or its lines could not be read. How the other
lines are routed or held is hardware inventory, read under `CAP_SYSINFO_HW`,
so the status does not repeat it. Readings are at most a second old, so no
number of callers reads a sensor or the header more often than once a
second.
