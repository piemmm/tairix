# `tairix-drv-sensor-bcm2711-thermal`

Autoloaded user-space driver for the **BCM2711 AVS temperature sensor**, the
Raspberry Pi 4's SoC temperature. Stability tier: `experimental`.

## Supported hardware

- `brcm,bcm2711-thermal`: the sensor inside the AVS monitor
  (`brcm,bcm2711-avs-monitor`). The node has no window of its own; the
  aarch64 port grants it the monitor's, and the monitor's node then holds
  none.

It binds only through the discovery match (`BIND_KEYS`).

## Required capabilities

| Capability | Why |
|---|---|
| `CAP_DRV_LOAD` | the load-time gate every driver clears |
| `CAP_MMIO_MAP` | the monitor's register window |
| `CAP_IPC_BIND_PRIVILEGED` | the node's `THERMAL_SENSOR_ENDPOINTS` channel |
| `CAP_HW_EMIT` | the `tairix,thermal-sensor` channel node |
| `CAP_LOG_EMIT` | the record of how it ended |

The channel is bound restricted-sender requiring `CAP_SYSINFO_HW`, which the
driver never holds.

## The service

`thermal-sensor-v1` (`plans/COOLING.md` CL4): `Read` answers the processor's
temperature in millidegrees Celsius. The status register's ten-bit code,
valid once either completion bit is set, is converted by the linear
calibration the thermal zone states (`coefficients`, published as the node's
`ThermalCalibration` fact): `slope × code + offset`. A status with no
completed conversion answers `NoData`; a node whose zone states no
calibration serves nothing, since a reading without one would be invented.

## Testing

Host tests convert the BCM2711's documented codes through its zone's
calibration, read the code beneath the status's other bits, and refuse a
missing conversion, a faulting read, an overflowing calibration and a short
window. QEMU does not model the BCM2711.

## References

- The Linux `bcm2711_thermal` driver, for the status register's layout.
