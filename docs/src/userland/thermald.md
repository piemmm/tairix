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
  **On** holds it asserted. **Automatic** samples the hottest processor
  sensor once a second; the fan starts at `start` and stops once the
  processor has cooled by `hysteresis`, so a fan near its threshold does not
  chatter.
- An automatic fan runs from its claim until its first sample decides, and
  runs whenever a reading cannot be taken, with the fault stated, until
  readings return: a fan that runs needlessly costs noise, one that stops
  needlessly costs the processor.
- A policy naming another line or polarity gives the old claim back before
  taking the new one.
- A drive the header refuses means it no longer honours the claim: its
  controller restarted holding none, or ended. The line is given back and
  taken again at once. Every hardware-tree change drives the held line again,
  so a restarted controller is found out even while the fan has nothing to
  switch.

## Faults

| Fault | Meaning |
|---|---|
| `NoLine` | the policy runs the fan but names no line |
| `NoHeader` | the machine has no header served |
| `LineUnavailable` | the header would not lend the line: not one it lends, in use, or its controller not answering |
| `NoSensor` | automatic with no processor sensor; the fan runs |
| `SensorFailed` | a reading could not be taken; the fan runs |

Every fault is recorded once as it begins, with the refusal behind it, and
once as it clears. A missing header or sensor, or a line that could not be
taken, is retried when the hardware tree changes, never on a timer.

## Waking

One wait set: its status endpoint, the machine configuration's directory
watch (the mount table's notice until that directory exists), and the
hardware tree's generation. Its only deadline is an automatic fan's next
sample, so a fan that is off, always on, or has no line to switch costs no
wake-ups at all. A call to a sensor or the header is answered within a
quarter of a second or counted refused, so a wedged driver costs the fan a
reading, which runs it, and never stalls the service. The configuration is followed through `tairix_rt::follow`,
the one file-following engine it shares with the time service.

## The status

`thermal-v1` `Status` on `THERMAL_ENDPOINT` is ungated: the processor's
temperature and whether the fan runs are facts every system shows to every
user. It answers the hottest processor reading, the policy in effect, whether
the fan runs, the fault, and the header's lent lines as two masks: every line
lent, and those the fan may take, free or its own already. How the other
lines are routed or held is hardware inventory, read under `CAP_SYSINFO_HW`,
so the status does not repeat it. Readings are at most a second old, so no
number of callers reads a sensor or the header more often than once a
second.
