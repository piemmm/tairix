//! A device channel's publication to the hardware tree, behind a
//! host-testable seam over the two calls it makes.

use tairix_abi::hwtree::{HwResource, HW_NODE_ROOT};
use tairix_abi::{HwDeviceClass, HwMatchKey, HwNode};

/// The hardware-tree calls a channel's publication makes.
pub(crate) trait TreeCalls {
    /// Mirrors [`tairix_rt::hw_emit_node`].
    fn emit(&mut self, node: &HwNode) -> i64;
    /// Mirrors [`tairix_rt::hw_node_settle`].
    fn settle(&mut self) -> i64;
}

/// Publish through `tree` the node `compatible` of `class` naming a bound
/// channel's `endpoint` and stating `facts`, then settle the driver's own
/// node: the channel is the one child a channel-serving driver publishes.
/// Answers the node's id; [`None`] on any refusal, settling nothing.
pub(crate) fn publish_channel(
    tree: &mut impl TreeCalls,
    class: HwDeviceClass,
    compatible: &[u8],
    endpoint: u64,
    facts: &[HwResource],
) -> Option<u32> {
    let mut node = HwNode::new(0, HW_NODE_ROOT, class);
    node.push_match_key(HwMatchKey::compatible(compatible).ok()?)
        .ok()?;
    node.push_resource(HwResource::endpoint(endpoint)).ok()?;
    for fact in facts {
        node.push_resource(*fact).ok()?;
    }
    let id = u32::try_from(tree.emit(&node)).ok()?;
    // A refused settle only leaves the device manager to wait out its bound.
    let _ = tree.settle();
    Some(id)
}
