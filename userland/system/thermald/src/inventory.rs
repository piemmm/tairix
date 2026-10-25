//! What the hardware tree holds for the cooling service: the header whose
//! lines its drivers lend, and the processor's sensors, each found by the
//! channel node its driver publishes once it serves.
//!
//! A channel is believed only where the kernel's binding vouches for it: its
//! endpoint is its block's entry for the node it hangs from, which only the
//! driver loaded for that node may serve. A header must also hang from a
//! controller discovery states lends header lines, and a sensor from a node
//! discovery states measures the processor; no driver may publish either
//! fact, so no other driver can stand in for one.

use alloc::vec::Vec;

use tairix_abi::driver::gpio::{GPIO_HEADER_ENDPOINTS, GPIO_HEADER_NODE_COMPATIBLE};
use tairix_abi::driver::thermal::{
    ThermalZone, THERMAL_SENSOR_ENDPOINTS, THERMAL_SENSOR_NODE_COMPATIBLE,
};
use tairix_abi::hwtree::NodeEndpointBlock;
use tairix_abi::{Errno, HwNode, HwProperty};

/// The header and the processor's sensors, by the endpoints they serve.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inventory {
    /// The header's endpoint: the first in tree order, since a board has one.
    pub header: Option<u64>,
    /// Every processor sensor's endpoint, once each, in tree order.
    pub sensors: Vec<u64>,
}

impl Inventory {
    /// What `nodes`, a whole snapshot, hold.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the sensors cannot be held.
    pub fn of<I>(nodes: I) -> Result<Self, Errno>
    where
        I: IntoIterator<Item = HwNode>,
        I::IntoIter: Clone,
    {
        let nodes = nodes.into_iter();
        let mut inventory = Self::default();
        for node in nodes.clone() {
            if inventory.header.is_none() {
                inventory.header = header(&node, nodes.clone());
            }
            let Some(sensor) = processor_sensor(&node, nodes.clone()) else {
                continue;
            };
            if !inventory.sensors.contains(&sensor) {
                inventory
                    .sensors
                    .try_reserve(1)
                    .map_err(|_| Errno::OutOfMemory)?;
                inventory.sensors.push(sensor);
            }
        }
        Ok(inventory)
    }
}

/// The header `node` publishes, when it hangs from a controller discovery
/// states lends header lines.
fn header(node: &HwNode, mut nodes: impl Iterator<Item = HwNode>) -> Option<u64> {
    let endpoint = bound(node, GPIO_HEADER_NODE_COMPATIBLE, GPIO_HEADER_ENDPOINTS)?;
    nodes
        .any(|controller| {
            controller.id() == node.parent()
                && controller
                    .property(HwProperty::GpioHeaderLines)
                    .is_some_and(|lines| lines != 0)
        })
        .then_some(endpoint)
}

/// The processor sensor `node` publishes, when it hangs from a sensor
/// discovery states measures the processor.
fn processor_sensor(node: &HwNode, mut nodes: impl Iterator<Item = HwNode>) -> Option<u64> {
    let endpoint = bound(
        node,
        THERMAL_SENSOR_NODE_COMPATIBLE,
        THERMAL_SENSOR_ENDPOINTS,
    )?;
    nodes
        .any(|sensor| {
            sensor.id() == node.parent()
                && sensor
                    .property(HwProperty::ThermalZone)
                    .and_then(ThermalZone::from_u64)
                    == Some(ThermalZone::Processor)
        })
        .then_some(endpoint)
}

/// The endpoint `node` names as a `compatible` channel, when it is `block`'s
/// entry for the node it hangs from.
fn bound(node: &HwNode, compatible: &[u8], block: NodeEndpointBlock) -> Option<u64> {
    node.channel_endpoint(compatible)
        .filter(|&endpoint| block.node_of(endpoint) == Some(node.parent()))
}

#[cfg(test)]
mod tests {
    use super::Inventory;
    use tairix_abi::driver::gpio::{
        HeaderLines, GPIO_HEADER_ENDPOINTS, GPIO_HEADER_NODE_COMPATIBLE,
    };
    use tairix_abi::driver::thermal::{
        ThermalZone, THERMAL_SENSOR_ENDPOINTS, THERMAL_SENSOR_NODE_COMPATIBLE,
    };
    use tairix_abi::{
        Errno, HwDeviceClass, HwMatchKey, HwNode, HwProperty, HwResource, HW_NODE_ROOT_ID,
    };

    const PROCESSOR: Option<u64> = Some(ThermalZone::Processor.as_u64());

    /// The channel `compatible` naming `endpoint`, published as `id` by the
    /// driver of `parent`.
    fn channel(
        id: u32,
        parent: u32,
        compatible: &[u8],
        endpoint: u64,
        zone: Option<u64>,
    ) -> HwNode {
        let mut node = HwNode::new(id, parent, HwDeviceClass::Other);
        node.push_match_key(HwMatchKey::compatible(compatible).unwrap())
            .unwrap();
        node.push_resource(HwResource::endpoint(endpoint)).unwrap();
        if let Some(zone) = zone {
            node.push_resource(HwResource::property(HwProperty::ThermalZone, zone))
                .unwrap();
        }
        node
    }

    /// The device `id`; where `lends`, a controller discovery states lends
    /// header lines.
    fn device(id: u32, lends: bool) -> HwNode {
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Other);
        if lends {
            let lines = HeaderLines::span(2, 27).mask();
            node.push_resource(HwResource::property(HwProperty::GpioHeaderLines, lines))
                .unwrap();
        }
        node
    }

    /// The sensor device `id`, which discovery states measures `zone`.
    fn measuring(id: u32, zone: Option<u64>) -> HwNode {
        let mut node = device(id, false);
        if let Some(zone) = zone {
            node.push_resource(HwResource::property(HwProperty::ThermalZone, zone))
                .unwrap();
        }
        node
    }

    /// The header the driver of `controller` publishes as `id`.
    fn header(id: u32, controller: u32) -> HwNode {
        let endpoint = GPIO_HEADER_ENDPOINTS.endpoint(controller);
        channel(id, controller, GPIO_HEADER_NODE_COMPATIBLE, endpoint, None)
    }

    /// The sensor channel the driver of `device` publishes as `id`.
    fn sensor(id: u32, device: u32) -> HwNode {
        let endpoint = THERMAL_SENSOR_ENDPOINTS.endpoint(device);
        channel(id, device, THERMAL_SENSOR_NODE_COMPATIBLE, endpoint, None)
    }

    #[test]
    fn the_header_and_the_processor_sensors_are_found_by_their_channels() {
        let nodes = [
            measuring(1, None),
            measuring(2, Some(99)),
            measuring(3, PROCESSOR),
            device(5, true),
            device(7, true),
            sensor(4, 3),
            header(6, 5),
            header(8, 7),
            sensor(9, 2),
            sensor(10, 1),
            channel(11, 1, b"tairix,audiochan", 1, None),
        ];
        let found = Inventory::of(nodes).unwrap();
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
        let nodes = [measuring(3, PROCESSOR), sensor(4, 3), sensor(12, 3)];
        assert_eq!(
            Inventory::of(nodes).unwrap().sensors,
            [THERMAL_SENSOR_ENDPOINTS.endpoint(3)]
        );
    }

    #[test]
    fn a_channel_naming_no_endpoint_is_passed_over() {
        let mut bare = HwNode::new(2, 1, HwDeviceClass::Other);
        bare.push_match_key(HwMatchKey::compatible(GPIO_HEADER_NODE_COMPATIBLE).unwrap())
            .unwrap();
        assert_eq!(
            Inventory::of([device(1, true), bare]).unwrap(),
            Inventory::default()
        );
    }

    /// D1337: a header any driver could publish — under a controller discovery
    /// does not say lends header lines — is passed over however early it is.
    #[test]
    fn only_a_header_under_a_lending_controller_is_believed() {
        let nodes = [
            device(3, false),
            header(4, 3),
            device(5, true),
            header(6, 5),
        ];
        assert_eq!(
            Inventory::of(nodes).unwrap().header,
            Some(GPIO_HEADER_ENDPOINTS.endpoint(5))
        );
    }

    /// D1337: a channel naming an endpoint the driver it hangs from does not
    /// serve — another node's, another block's, or no device channel — is
    /// passed over.
    #[test]
    fn a_channel_its_parent_does_not_serve_is_passed_over() {
        let foreign_header = GPIO_HEADER_ENDPOINTS.endpoint(9);
        let nodes = [
            measuring(3, PROCESSOR),
            device(5, true),
            channel(6, 5, GPIO_HEADER_NODE_COMPATIBLE, foreign_header, None),
            channel(
                7,
                5,
                GPIO_HEADER_NODE_COMPATIBLE,
                THERMAL_SENSOR_ENDPOINTS.endpoint(5),
                None,
            ),
            channel(8, 5, GPIO_HEADER_NODE_COMPATIBLE, 0x5448_4D00, None),
            channel(
                10,
                3,
                THERMAL_SENSOR_NODE_COMPATIBLE,
                THERMAL_SENSOR_ENDPOINTS.endpoint(4),
                None,
            ),
            channel(
                11,
                3,
                THERMAL_SENSOR_NODE_COMPATIBLE,
                GPIO_HEADER_ENDPOINTS.endpoint(3),
                None,
            ),
        ];
        assert_eq!(Inventory::of(nodes).unwrap(), Inventory::default());
    }

    /// D1334: sensors that cannot be held refuse the tree, so what the
    /// service held stands, rather than ending it.
    #[test]
    fn sensors_that_cannot_be_held_refuse_the_tree() {
        let nodes = [measuring(3, PROCESSOR), sensor(4, 3)];
        let held = tairix_fuzzseed::meter::refusing_above(0, || Inventory::of(nodes));
        assert_eq!(held, Err(Errno::OutOfMemory));
    }

    /// D1337: what a sensor measures is discovery's to state on its own node;
    /// a channel claiming the processor itself, under a sensor discovery says
    /// nothing of, is passed over, and one under a processor sensor is
    /// believed without a word of its own.
    #[test]
    fn only_a_sensor_discovery_says_measures_the_processor_is_believed() {
        let claimed = channel(
            4,
            3,
            THERMAL_SENSOR_NODE_COMPATIBLE,
            THERMAL_SENSOR_ENDPOINTS.endpoint(3),
            PROCESSOR,
        );
        let nodes = [
            measuring(3, None),
            claimed,
            measuring(5, PROCESSOR),
            sensor(6, 5),
        ];
        assert_eq!(
            Inventory::of(nodes).unwrap().sensors,
            [THERMAL_SENSOR_ENDPOINTS.endpoint(5)]
        );
    }
}
