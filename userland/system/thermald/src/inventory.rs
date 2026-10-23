//! What the hardware tree holds for the cooling service: the header whose
//! lines its drivers lend, and the processor's sensors, each found by the
//! channel node its driver publishes once it serves.

use alloc::vec::Vec;

use tairix_abi::driver::gpio::GPIO_HEADER_NODE_COMPATIBLE;
use tairix_abi::driver::thermal::{ThermalZone, THERMAL_SENSOR_NODE_COMPATIBLE};
use tairix_abi::hwtree::{HwMatchKind, HwResourceKind};
use tairix_abi::{HwNode, HwProperty};

/// The header and the processor's sensors, by the endpoints they serve.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inventory {
    /// The header's endpoint: the first header in tree order, since a board
    /// has one.
    pub header: Option<u64>,
    /// Every processor sensor's endpoint, once each, in tree order.
    pub sensors: Vec<u64>,
}

impl Inventory {
    /// What `nodes`, a whole snapshot, hold.
    #[must_use]
    pub fn of<'n>(nodes: impl IntoIterator<Item = &'n HwNode>) -> Self {
        let mut inventory = Self::default();
        for node in nodes {
            let Some(endpoint) = channel_endpoint(node) else {
                continue;
            };
            if names(node, GPIO_HEADER_NODE_COMPATIBLE) {
                inventory.header = inventory.header.or(Some(endpoint));
            } else if names(node, THERMAL_SENSOR_NODE_COMPATIBLE)
                && node
                    .property(HwProperty::ThermalZone)
                    .and_then(ThermalZone::from_u64)
                    == Some(ThermalZone::Processor)
                && !inventory.sensors.contains(&endpoint)
            {
                inventory.sensors.push(endpoint);
            }
        }
        inventory
    }
}

fn names(node: &HwNode, compatible: &[u8]) -> bool {
    node.match_keys().iter().any(|key| {
        key.kind() == Some(HwMatchKind::Compatible) && key.compatible_bytes() == compatible
    })
}

/// The endpoint a channel node names; [`None`] for a node naming none,
/// never guessed at.
fn channel_endpoint(node: &HwNode) -> Option<u64> {
    node.resources()
        .iter()
        .find(|resource| resource.kind() == Some(HwResourceKind::Endpoint))
        .map(tairix_abi::HwResource::base)
}

#[cfg(test)]
mod tests {
    use super::Inventory;
    use tairix_abi::driver::gpio::{GPIO_HEADER_ENDPOINTS, GPIO_HEADER_NODE_COMPATIBLE};
    use tairix_abi::driver::thermal::{
        ThermalZone, THERMAL_SENSOR_ENDPOINTS, THERMAL_SENSOR_NODE_COMPATIBLE,
    };
    use tairix_abi::{HwDeviceClass, HwMatchKey, HwNode, HwProperty, HwResource, HW_NODE_ROOT_ID};

    fn channel(id: u32, compatible: &[u8], endpoint: u64, zone: Option<u64>) -> HwNode {
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Other);
        node.push_match_key(HwMatchKey::compatible(compatible).unwrap())
            .unwrap();
        node.push_resource(HwResource::endpoint(endpoint)).unwrap();
        if let Some(zone) = zone {
            node.push_resource(HwResource::property(HwProperty::ThermalZone, zone))
                .unwrap();
        }
        node
    }

    #[test]
    fn the_header_and_the_processor_sensors_are_found_by_their_channels() {
        let processor = ThermalZone::Processor.as_u64();
        let nodes = [
            channel(
                4,
                THERMAL_SENSOR_NODE_COMPATIBLE,
                THERMAL_SENSOR_ENDPOINTS.endpoint(3),
                Some(processor),
            ),
            channel(
                6,
                GPIO_HEADER_NODE_COMPATIBLE,
                GPIO_HEADER_ENDPOINTS.endpoint(5),
                None,
            ),
            channel(
                8,
                GPIO_HEADER_NODE_COMPATIBLE,
                GPIO_HEADER_ENDPOINTS.endpoint(7),
                None,
            ),
            channel(
                9,
                THERMAL_SENSOR_NODE_COMPATIBLE,
                THERMAL_SENSOR_ENDPOINTS.endpoint(2),
                Some(99),
            ),
            channel(
                10,
                THERMAL_SENSOR_NODE_COMPATIBLE,
                THERMAL_SENSOR_ENDPOINTS.endpoint(1),
                None,
            ),
            channel(11, b"tairix,audiochan", 1, None),
        ];
        let found = Inventory::of(&nodes);
        assert_eq!(
            found.header,
            Some(GPIO_HEADER_ENDPOINTS.endpoint(5)),
            "the first header"
        );
        assert_eq!(
            found.sensors,
            [THERMAL_SENSOR_ENDPOINTS.endpoint(3)],
            "processor sensors only"
        );
    }

    /// A restarted driver publishes its channel again beside the node it
    /// published before, naming the same endpoint.
    #[test]
    fn a_channel_published_twice_is_one_sensor() {
        let processor = Some(ThermalZone::Processor.as_u64());
        let endpoint = THERMAL_SENSOR_ENDPOINTS.endpoint(3);
        let nodes = [
            channel(4, THERMAL_SENSOR_NODE_COMPATIBLE, endpoint, processor),
            channel(12, THERMAL_SENSOR_NODE_COMPATIBLE, endpoint, processor),
        ];
        assert_eq!(Inventory::of(&nodes).sensors, [endpoint]);
    }

    #[test]
    fn a_channel_naming_no_endpoint_is_passed_over() {
        let mut bare = HwNode::new(2, HW_NODE_ROOT_ID, HwDeviceClass::Other);
        bare.push_match_key(HwMatchKey::compatible(GPIO_HEADER_NODE_COMPATIBLE).unwrap())
            .unwrap();
        assert_eq!(Inventory::of(&[bare]), Inventory::default());
    }
}
