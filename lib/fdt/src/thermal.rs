//! Thermal zones (`Documentation/devicetree/bindings/thermal/thermal-zones.yaml`):
//! the linear conversion a zone states for the one sensor it reads.
//!
//! A zone's `coefficients` weight each sensor it lists and end in a constant
//! offset. For a zone over exactly one sensor they are that sensor's slope
//! and offset, which a driver applies to its raw code, as Linux's
//! `thermal_zone_get_slope`/`get_offset` do. A zone over several sensors, or
//! with coefficients of any other length, states no conversion for any one of
//! them, so no reading is ever derived from a calibration half understood.

use crate::{be_u32, Fdt};

/// Cells of a single-sensor zone's `coefficients`: the slope, then the offset.
const COEFFICIENT_CELLS: usize = 2;

/// The slope and offset, in millidegrees Celsius per count and at count zero,
/// the zone reading only the sensor whose phandle is `sensor` states.
///
/// [`None`] when no zone names it, its zone names another sensor beside it,
/// or the zone's `coefficients` are not exactly two cells.
#[must_use]
pub fn sensor_coefficients(fdt: &Fdt<'_>, sensor: u32) -> Option<(i32, i32)> {
    for node in fdt.nodes() {
        let Ok(zone) = node else { break };
        let Some(sensors) = zone.property("thermal-sensors") else {
            continue;
        };
        if be_u32(sensors.value(), 0) != Some(sensor) {
            continue;
        }
        let cells = zone.property("coefficients")?.value();
        if cells.len() != COEFFICIENT_CELLS * 4 || !names_one_sensor(fdt, sensors.value()) {
            return None;
        }
        let cell = |at| be_u32(cells, at).map(|raw| i32::from_be_bytes(raw.to_be_bytes()));
        return Some((cell(0)?, cell(4)?));
    }
    None
}

/// Whether a `thermal-sensors` list holds a single specifier: its sensor's
/// phandle and that sensor's `#thermal-sensor-cells` argument cells.
fn names_one_sensor(fdt: &Fdt<'_>, list: &[u8]) -> bool {
    let Some(sensor) = be_u32(list, 0).and_then(|phandle| fdt.node_by_phandle(phandle)) else {
        return false;
    };
    let arguments = sensor
        .property("#thermal-sensor-cells")
        .and_then(|cells| be_u32(cells.value(), 0))
        .and_then(|cells| usize::try_from(cells).ok());
    arguments.is_some_and(|arguments| list.len() == 4 * (1 + arguments))
}

#[cfg(test)]
mod tests {
    use super::sensor_coefficients;
    use crate::write::FdtWriter;
    use crate::Fdt;
    use alloc::vec::Vec;

    const SENSOR: u32 = 7;
    const OTHER: u32 = 8;

    fn cells(values: &[i32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn tree(zone_sensors: &[i32], coefficients: &[i32]) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        for (name, phandle) in [("thermal", SENSOR), ("other", OTHER)] {
            b.begin_node(name);
            b.prop_str("compatible", "test,sensor");
            b.prop_u32("#thermal-sensor-cells", 0);
            b.prop_u32("phandle", phandle);
            b.end_node();
        }
        b.begin_node("thermal-zones");
        b.begin_node("cpu-thermal");
        b.prop_u32("polling-delay", 1000);
        b.prop("thermal-sensors", &cells(zone_sensors));
        b.prop("coefficients", &cells(coefficients));
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    fn coefficients(blob: &[u8], sensor: u32) -> Option<(i32, i32)> {
        sensor_coefficients(&Fdt::new(blob).expect("a valid tree"), sensor)
    }

    #[test]
    fn a_single_sensor_zone_states_its_slope_and_offset() {
        let blob = tree(&[7], &[-487, 410_040]);
        assert_eq!(coefficients(&blob, SENSOR), Some((-487, 410_040)));
        assert_eq!(coefficients(&blob, OTHER), None, "no zone reads it");
    }

    #[test]
    fn a_zone_over_two_sensors_states_neither_conversion() {
        let blob = tree(&[7, 8], &[1, 1, 0]);
        assert_eq!(coefficients(&blob, SENSOR), None);
    }

    #[test]
    fn coefficients_of_any_other_length_state_nothing() {
        assert_eq!(coefficients(&tree(&[7], &[-487]), SENSOR), None);
        assert_eq!(coefficients(&tree(&[7], &[-487, 1, 2]), SENSOR), None);
    }
}
