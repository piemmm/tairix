# Temperature-sensor drivers

A temperature sensor's driver serves `thermal-sensor-v1`
(`tairix_abi::driver::thermal`, `plans/COOLING.md` CL4) on its node's
`THERMAL_SENSOR_ENDPOINTS` id, bound restricted-sender requiring
`CAP_SYSINFO_HW`. Once it serves it publishes a `tairix,thermal-sensor` child
node carrying the endpoint, so the cooling service finds it in the tree. What
the sensor measures is never the driver's word: the shared device-tree walk
states it on the sensor's own node (`ThermalZone`, today `Processor`) when a
zone named for the processor (`cpu-thermal`, or `soc-thermal`) reads it
alone, a fact no driver may publish. Its one request,
`Read`, answers millidegrees Celsius, or `NoData` while the hardware holds no
valid conversion. Every sensor driver serves through the one loop
`tairix_drvrt::serve::thermal_sensor`.

## Calibration

A raw code becomes a temperature by the linear conversion the device tree's
thermal zone states: `coefficients` of a zone that reads exactly one sensor,
its slope and offset. The shared device-tree walk publishes it as the sensor
node's `ThermalCalibration` fact (`tairix_fdt::thermal`). A zone over several
sensors, or with coefficients of any other length, states nothing, and a
sensor with no stated calibration serves nothing rather than invent one.

## `drivers/sensor/bcm2711_thermal`

The BCM2711's sensor (`brcm,bcm2711-thermal`) is a register of the AVS
monitor above it (`brcm,bcm2711-avs-monitor`) and has no window of its own.
The aarch64 port grants the monitor's window to the one enabled sensor
directly inside it, and that monitor's node then holds none, so one node
holds the registers; any other node naming the sensor is granted nothing. The status register at `0x200`
carries a ten-bit code, valid once either completion bit is set; the
Raspberry Pi 4's zone converts it as `-487 × code + 410040`. QEMU does not
model the BCM2711.
