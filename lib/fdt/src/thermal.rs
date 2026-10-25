//! Thermal zones (the device tree's `thermal-zones` node): what a zone states
//! for the one sensor it reads —
//! the linear conversion from its raw code, and whether it measures the
//! processor.
//!
//! A zone's `coefficients` weight each sensor it lists and end in a constant
//! offset, the binding's linear equation. For a zone over exactly one
//! single-channel sensor they are that sensor's slope and offset. A zone over
//! several sensors states nothing for any one of them, and a sensor of
//! several channels has nothing for its node as a whole, so no reading is
//! ever derived from a calibration half understood.

use crate::{be_u32, Fdt};

/// Cells of a single-sensor zone's `coefficients`: the slope, then the offset.
const COEFFICIENT_CELLS: usize = 2;

/// The slope and offset, in millidegrees Celsius per count and at count zero,
/// the zones reading only the sensor whose phandle is `sensor` state for it.
///
/// A zone listing other sensors beside it, or stating no `coefficients`,
/// states nothing for it. [`None`] for a sensor of several channels
/// (`#thermal-sensor-cells` other than zero), when no zone states a
/// conversion, when one states other than two cells, when two disagree, and
/// for a tree that cannot be walked whole.
#[must_use]
pub fn sensor_coefficients(fdt: &Fdt<'_>, sensor: u32) -> Option<(i32, i32)> {
    let channels = fdt
        .node_by_phandle(sensor)?
        .property("#thermal-sensor-cells")?;
    if be_u32(channels.value(), 0)? != 0 {
        return None;
    }
    let mut stated = None;
    for node in fdt.nodes() {
        let zone = node.ok()?;
        let Some(coefficients) = zone
            .property("coefficients")
            .filter(|_| reads_alone(&zone, sensor))
        else {
            continue;
        };
        let conversion = slope_and_offset(coefficients.value())?;
        if stated.is_some_and(|held| held != conversion) {
            return None;
        }
        stated = Some(conversion);
    }
    stated
}

/// The names a zone for the processor is given: its own, and the
/// system-on-chip's that carries it.
const PROCESSOR_ZONES: [&[u8]; 2] = [b"cpu-thermal", b"soc-thermal"];

/// Whether a zone named for the processor (`cpu-thermal`, `soc-thermal`) reads
/// the sensor whose phandle is `sensor`, and only it: the reading a cooling
/// policy keeps the processor within. A tree that cannot be walked whole
/// says no.
#[must_use]
pub fn reads_processor_zone(fdt: &Fdt<'_>, sensor: u32) -> bool {
    let mut found = false;
    for node in fdt.nodes() {
        let Ok(zone) = node else {
            return false;
        };
        found |=
            PROCESSOR_ZONES.contains(&crate::name_stem(zone.name())) && reads_alone(&zone, sensor);
    }
    found
}

/// Whether `zone` reads the sensor whose phandle is `sensor`, and no other.
fn reads_alone(zone: &crate::Node<'_>, sensor: u32) -> bool {
    zone.property("thermal-sensors")
        .is_some_and(|sensors| sensors.value() == sensor.to_be_bytes().as_slice())
}

/// A single-sensor zone's `coefficients`: its slope and its offset, exactly.
fn slope_and_offset(cells: &[u8]) -> Option<(i32, i32)> {
    if cells.len() != COEFFICIENT_CELLS * 4 {
        return None;
    }
    let cell = |at| be_u32(cells, at).map(|raw| i32::from_be_bytes(raw.to_be_bytes()));
    Some((cell(0)?, cell(4)?))
}

#[cfg(test)]
mod tests {
    use super::{reads_processor_zone, sensor_coefficients};
    use crate::write::FdtWriter;
    use crate::Fdt;
    use alloc::vec::Vec;

    const SENSOR: u32 = 7;
    const OTHER: u32 = 8;

    /// Zone names, so a tree may hold several.
    const ZONES: [&str; 2] = ["cpu-thermal", "soc-thermal"];

    fn cells(values: &[i32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// Two sensors, `thermal` (`SENSOR`, of `channels` cells) and `other`
    /// (`OTHER`, of none), and a zone for each `(thermal-sensors,
    /// coefficients)` of `zones`, stating no `coefficients` where `None`.
    fn zoned(channels: u32, zones: &[(&[i32], Option<&[i32]>)]) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        for (name, phandle, sensor_cells) in [("thermal", SENSOR, channels), ("other", OTHER, 0)] {
            b.begin_node(name);
            b.prop_str("compatible", "test,sensor");
            b.prop_u32("#thermal-sensor-cells", sensor_cells);
            b.prop_u32("phandle", phandle);
            b.end_node();
        }
        b.begin_node("thermal-zones");
        for (name, (sensors, coefficients)) in ZONES.iter().zip(zones) {
            b.begin_node(name);
            b.prop_u32("polling-delay", 1000);
            b.prop("thermal-sensors", &cells(sensors));
            if let Some(coefficients) = coefficients {
                b.prop("coefficients", &cells(coefficients));
            }
            b.end_node();
        }
        b.end_node();
        b.end_node();
        b.build()
    }

    fn tree(zone_sensors: &[i32], coefficients: &[i32]) -> Vec<u8> {
        zoned(0, &[(zone_sensors, Some(coefficients))])
    }

    fn coefficients(blob: &[u8], sensor: u32) -> Option<(i32, i32)> {
        sensor_coefficients(&Fdt::new(blob).expect("a valid tree"), sensor)
    }

    #[test]
    fn a_single_sensor_zone_states_its_slope_and_offset() {
        let blob = tree(&[7], &[-500, 400_000]);
        assert_eq!(coefficients(&blob, SENSOR), Some((-500, 400_000)));
        assert_eq!(coefficients(&blob, OTHER), None, "no zone reads it");
    }

    #[test]
    fn a_zone_over_two_sensors_states_neither_conversion() {
        let blob = tree(&[7, 8], &[1, 1, 0]);
        assert_eq!(coefficients(&blob, SENSOR), None);
    }

    #[test]
    fn coefficients_of_any_other_length_state_nothing() {
        assert_eq!(coefficients(&tree(&[7], &[-500]), SENSOR), None);
        assert_eq!(coefficients(&tree(&[7], &[-500, 1, 2]), SENSOR), None);
    }

    /// Each zone reads one channel, so none states the node's conversion.
    #[test]
    fn a_sensor_of_several_channels_states_no_conversion_for_its_node() {
        let blob = zoned(1, &[(&[7, 0][..], Some(&[-500, 400_000][..]))]);
        assert_eq!(coefficients(&blob, SENSOR), None);
    }

    #[test]
    fn a_zone_stating_no_coefficients_is_passed_over_wherever_it_stands() {
        let unstated: (&[i32], Option<&[i32]>) = (&[7], None);
        let stated: (&[i32], Option<&[i32]>) = (&[7], Some(&[-500, 400_000]));
        for zones in [[unstated, stated], [stated, unstated]] {
            assert_eq!(
                coefficients(&zoned(0, &zones), SENSOR),
                Some((-500, 400_000))
            );
        }
    }

    #[test]
    fn zones_reading_one_sensor_must_agree() {
        let first: (&[i32], Option<&[i32]>) = (&[7], Some(&[-500, 400_000]));
        let other: (&[i32], Option<&[i32]>) = (&[7], Some(&[-500, 401_000]));
        assert_eq!(coefficients(&zoned(0, &[first, other]), SENSOR), None);
        assert_eq!(
            coefficients(&zoned(0, &[first, first]), SENSOR),
            Some((-500, 400_000))
        );
    }

    /// A zone over several sensors states nothing for any one, and leaves the
    /// conversion to the zone reading it alone.
    #[test]
    fn a_zone_over_several_sensors_leaves_the_conversion_to_one_reading_it_alone() {
        let shared: (&[i32], Option<&[i32]>) = (&[8, 7], Some(&[1, 1, 0]));
        let alone: (&[i32], Option<&[i32]>) = (&[7], Some(&[-500, 400_000]));
        assert_eq!(
            coefficients(&zoned(0, &[shared, alone]), SENSOR),
            Some((-500, 400_000))
        );
    }

    /// A channel count no list could hold is refused, never multiplied out.
    #[test]
    fn a_channel_count_past_any_list_states_nothing() {
        let blob = zoned(u32::MAX, &[(&[7][..], Some(&[-500, 400_000][..]))]);
        assert_eq!(coefficients(&blob, SENSOR), None);
    }

    /// A sensor measures the processor where a zone named for it reads the
    /// sensor alone; another zone, or one sharing its sensor, says no.
    #[test]
    fn only_a_processor_zone_reading_a_sensor_alone_names_the_processor() {
        let zones = |blob: &[u8], sensor| {
            reads_processor_zone(&Fdt::new(blob).expect("a valid tree"), sensor)
        };
        // `ZONES` names the first zone `cpu-thermal`, the second `soc-thermal`.
        let alone: (&[i32], Option<&[i32]>) = (&[7], None);
        let shared: (&[i32], Option<&[i32]>) = (&[7, 8], None);
        assert!(zones(&zoned(0, &[alone]), SENSOR));
        assert!(!zones(&zoned(0, &[alone]), OTHER), "no zone reads it");
        assert!(!zones(&zoned(0, &[shared]), SENSOR));
        assert!(zones(&zoned(0, &[shared, alone]), SENSOR), "the SoC's zone");
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("thermal-zones");
        b.begin_node("gpu-thermal");
        b.prop("thermal-sensors", &cells(&[7]));
        b.end_node();
        b.end_node();
        b.end_node();
        assert!(!zones(&b.build(), SENSOR), "a zone for another part");
    }
}
