# COOLING — temperature sensors, header GPIO lines, and the CPU fan

Binding under `AGENTS.md`. How TAIRiX reads the processor's temperature,
lends a board header's GPIO lines to a system service, and switches a CPU fan
wired to one of them: off, on, or by temperature with hysteresis. The machine
policy is set on the Settings application's Raspberry Pi pane.

## Ledger

| Id | Item | Status |
|---|---|---|
| CL1 | The board's identity: the device-tree walk gives the hardware tree's root node the tree's root `compatible` strings as match keys, served by the ungated `PLATFORM_COMPATIBLE` System Information query and read through `tairix_procinfo::platform_compatible` | done |
| CL2 | `CAP_GPIO` and `gpio-header-v1` (`lib/abi/src/driver/gpio.rs`): the lines a board header lends to system services, listed, claimed as outputs with a polarity, driven and released; the arch port's `GpioHeaderLines` fact names them on the controller's node | done |
| CL3 | The BCM2835-family GPIO controller driver (`drivers/gpio/bcm2835`): `irqline-v1` for linked consumers (`plans/GPIO.md` G2) and `gpio-header-v1` for the header's lines | done |
| CL4 | `thermal-sensor-v1` (`lib/abi/src/driver/thermal.rs`) and the BCM2711 AVS sensor driver (`drivers/sensor/bcm2711_thermal`); the arch port grants the sensor its monitor's window and the thermal zone's calibration | done |
| CL5 | The `cooling.fan.*` machine settings in `lib/sysconfig`, written through `settingsd` by `configure` | done |
| CL6 | `thermald`: finds sensors and the header in the hardware tree, follows `system.conf`, runs the fan policy, serves the ungated `thermal-v1` status | done |
| CL7 | The Settings application's Raspberry Pi category: shown only on a Raspberry Pi, the fan's staged policy beside the live temperature and fan state | done |

## The board's identity (CL1)

- The root node of every device-tree port carries the tree's root
  `compatible` list, at most `HW_NODE_MAX_MATCH_KEYS` of it in document
  order. No driver matches the root, so the keys are inventory only.
- `PLATFORM_COMPATIBLE` answers those strings to any caller: the same fact
  Linux gives every user in `/proc/device-tree/compatible`. A port that does
  not describe the board (ACPI) answers none.

## Header lines (CL2)

- **Which lines are lent is a board fact**, so the architecture port states
  it: `HwProperty::GpioHeaderLines`, a bit per line, on the controller node
  whose lines reach the board's expansion header. On a Raspberry Pi with the
  SoC's GPIO block (Pi 3, 4, 400, the compute modules' carrier headers) that
  is GPIO 2–27: GPIO 0 and 1 are the HAT identification EEPROM's bus, which
  the firmware owns. A controller with no such fact lends nothing.
- **`CAP_GPIO`** guards every line of every header: the authority to switch
  whatever is wired to the board. Its one holder is `thermald`; its
  enforcement point is the header endpoint's send gate, which the kernel
  checks on every call.
- The controller's driver binds its node's `GPIO_HEADER_ENDPOINTS` id, then
  publishes a `tairix,gpio-header` child node carrying it, so a service finds
  the header in the tree once it serves.
- `Lines` lists each lent line and how it stands: free, routed to a
  peripheral (its function select is an alternate function — the serial
  console's pins, an I²C or I²S bus the firmware set up), held as an
  interrupt by a linked device, or driven by a holder. `Claim` takes a free
  line as an output with a polarity and an initial state, setting the level
  before the direction so the line never glitches; `Drive` switches it;
  `Release` returns it to an input. A holder that ends has every line it held
  returned to an input.
- A line routed to a peripheral is never lent: the driver reads the function
  select and refuses it. A line is held by one consumer at a time, whether a
  linked interrupt consumer or a header holder.

## The GPIO controller (CL3)

- One driver owns the block: function selects, output set/clear, the
  event-detect enables and status, and its node's bank interrupts (three: GPIO
  0–27, 28–45, 46 up). Pulls are the board's wiring, stated by the firmware's
  `gpio=` lines, and are never touched.
- An interrupt consumer's selector is the binding's two cells: the line and
  its trigger. The line is set to input. A level line is masked by clearing
  its detect enable as it fires and re-armed by the next `Wait`; an edge
  line's detect stays enabled while held and a firing with no `Wait` posted is
  latched in the driver, so no edge is lost and a level line cannot storm.

## Sensors (CL4)

- A sensor driver binds its node's `THERMAL_SENSOR_ENDPOINTS` id, gated on
  `CAP_SYSINFO_HW`, and publishes a `tairix,thermal-sensor` child node
  carrying it and the zone it measures (`HwProperty::ThermalZone`). `Read`
  answers millidegrees Celsius, or `NotReady` when the hardware has no valid
  conversion.
- The BCM2711 AVS sensor has no window of its own: it is a register of the
  AVS monitor above it. The arch port grants it the monitor's window and the
  linear calibration (`HwProperty::ThermalCalibration`) from the thermal zone
  whose `thermal-sensors` names it, so the conversion is the tree's, not a
  constant. A sensor whose zone states no `coefficients` serves nothing.

## The fan settings (CL5)

| Key | Values | Default |
|---|---|---|
| `cooling.fan.mode` | `off`, `on`, `auto` | `off` |
| `cooling.fan.line` | `none`, a GPIO line 0–63 | `none` |
| `cooling.fan.signal` | `active-high`, `active-low` | `active-high` |
| `cooling.fan.start` | °C, 40–85 | 65 |
| `cooling.fan.hysteresis` | °C, 2–20 | 10 |

- The fan stops at `start − hysteresis`, so two independent ranges make an
  inverted band unrepresentable.
- `off` leaves the line undriven, so another use may take it.

## thermald (CL6)

- Holds `CAP_SYSINFO_HW` (the tree and the sensors), `CAP_GPIO` (the fan's
  line), `CAP_FS_ACCESS` (`system.conf` and its directory watch),
  `CAP_IPC_BIND_PRIVILEGED` (its reserved endpoint) and `CAP_LOG_EMIT`.
- Parks on one wait set: its endpoint, the settings directory's watch (the
  mount table's notice until that directory exists), the hardware tree's
  generation, and a one-shot deadline armed only while the fan is automatic.
  `on` and `off` sample nothing.
- **Automatic**: every second, the hottest processor sensor; the fan starts
  at `start` and stops at `start − hysteresis`. A reading that cannot be
  taken runs the fan and states the fault until readings return.
- Every call to a sensor or the header carries a quarter-second deadline, so
  a wedged driver is a refused call — a failed reading runs the fan — and
  never stalls the service.
- A line the controller refuses, a missing header or a missing sensor is a
  fault stated on the status and in the log, retried when the tree changes,
  never on a timer.
- A drive the header refuses means the claim is gone — its controller
  restarted holding none, or ended — so the line is given back and taken
  again at once. Every tree change drives the held line again, so a restart
  is found out while the fan has nothing to switch. A restarted driver's
  channel node stands beside the one it published before (`plans/OPEN-DEFECTS.md`
  D1271), so sensors are counted once per endpoint.
- `thermal-v1` `Status` is ungated and answers the hottest processor
  temperature, the policy in effect, whether the fan is running, the fault if
  any, and which lent header lines the fan may take (`FanLines`: the lent
  and free masks). How the other lines are routed is hardware inventory,
  read under `CAP_SYSINFO_HW`, and the ungated status does not repeat it.

## The Raspberry Pi pane (CL7)

- At the user's direction the category is board-specific: it is shown only
  when the board's identity names a Raspberry Pi (`raspberrypi,`), read
  before the window opens, and is absent from the strip, the search index,
  the category menu and every route by name otherwise. This is the charter's
  one board-named userland surface.
- The CPU FAN group stages the five settings and applies them through the one
  elevated `configure` run, as the other machine panes do. Lines are offered
  as `GPIO n (pin p)` with the 40-pin header's pin; a line something else
  holds is named in the row's description, never offered.
- The STATUS group reads `thermal-v1` on a worker while the pane is on show,
  re-asked on a two-second one-shot deadline; a reading that could not be
  taken renders unmeasured.

## Invariants

- A GPIO line has at most one consumer, and only the controller's driver
  touches its registers.
- No line routed to a peripheral is driven, and no line outside the board's
  stated header is lent.
- The fan's policy samples only while automatic, and never without a
  deadline it armed itself.
- Settings holds no authority over the fan: it reads an ungated status and
  writes through the elevated settings writer.
