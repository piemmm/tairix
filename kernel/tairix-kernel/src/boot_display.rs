//! Boot-display publication (`plans/DISPLAY.md` D7d).
//!
//! When an architecture port's boot path brings a framebuffer boot
//! console up, the machine has exactly one platform-programmed linear
//! scan-out surface — and until now it existed only as console state, so
//! the user-space display service (`drivers/display/framebuffer`) had no
//! display node to autoload against. This module is the arch-neutral
//! publication step: it turns the port's *discovered* scan-out facts
//! (base, geometry, pixel format — never a board constant) into a
//! [`HwDeviceClass::Display`] hardware-tree node carrying the
//! geometry-carrying [`HwResource::framebuffer`] grant request and the
//! canonical [`SIMPLE_FRAMEBUFFER_COMPATIBLE`] match key the display
//! service's `BIND_KEYS` binds.
//!
//! Where the port knows a display engine that takes the screen over from the
//! firmware, no boot display node is published: the engine's node states
//! [`HwProperty::BootConsole`] instead ([`mark_boot_console`]), and its driver
//! moves the console onto memory the kernel owns (`console_relocate`) before
//! it tells the firmware the display is the ARM's, so nothing scans out or
//! writes the firmware's surface once the firmware may take it back.
//!
//! The firmware's own `simple-framebuffer` nodes describing that surface are
//! withheld ([`withhold_firmware_surfaces`]): the walk publishes one with only
//! its register window, never the geometry a grant needs, so a driver bound to
//! it could serve nothing, and the console is already described.
//!
//! The node rides the same boot-discovered tree the block and input
//! probes buffer, so the pre-unlock autoload matches it exactly like any
//! other discovered device: the signed store scan finds the display
//! bundle, `lib/devmatch` resolves the node to it, and the spawn path
//! mints exactly one `CAP_MMIO_MAP`-gated surface grant. Seat identity is
//! untouched: the boot display *is* the boot seat's display
//! (`SEAT_PRIMARY` always exists), so no seat is minted here — only a
//! runtime `hw_emit_node` display publication creates a further seat
//! (`plans/DISPLAY.md` D6).

use alloc::vec::Vec;

use tairix_abi::driver::display::{DisplayFormat, DisplayMode};
use tairix_abi::hwtree::{FramebufferMemory, HwResource, HwResourceKind};
use tairix_abi::{
    HwDeviceClass, HwMatchKey, HwNode, HwProperty, HW_NODE_ROOT_ID, SIMPLE_FRAMEBUFFER_COMPATIBLE,
};
use tairix_arch_api::{DiscoveryError, HwNodeSink};

use crate::boot_hwtree::CollectingHwNodeSink;

/// Node id of the boot display in the discovered tree.
///
/// One entry in the shared, disjoint-by-construction node-id map
/// ([`crate::hwtree_node_ids`]), so it can never collide with a probe base
/// or another boot step's node. There is at most one boot display, so this
/// is a single id, not a base.
pub use crate::hwtree_node_ids::BOOT_DISPLAY_NODE_ID;

/// The boot console's scan-out surface, as the architecture port
/// discovered it — plain values, so this module never names an
/// arch-specific type (platform specifics stay in the port).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct BootScanout {
    /// CPU-physical base of the surface.
    pub base: u64,
    /// Surface width in pixels.
    pub width_px: u32,
    /// Surface height in pixels.
    pub height_px: u32,
    /// Distance in bytes between the start of consecutive scanlines.
    pub stride_bytes: u32,
    /// Pixel encoding the platform programmed the surface with.
    pub format: DisplayFormat,
    /// CPU mapping policy required by the discovered surface backing.
    pub memory: FramebufferMemory,
}

impl BootScanout {
    /// The surface as the one [`HwResource::framebuffer`] grant request a
    /// node carries, sized and validated from the discovered mode, or `None`
    /// for a degenerate mode (zero extent, an under-sized stride, an
    /// overflowing length) no correct, bounded grant can be minted for.
    fn grant(&self) -> Option<HwResource> {
        let mode = DisplayMode {
            width_px: self.width_px,
            height_px: self.height_px,
            stride_bytes: self.stride_bytes,
            format: self.format,
        };
        HwResource::framebuffer(self.base, &mode, self.memory).ok()
    }
}

/// Emit the boot display node for `scanout` into `sink`.
///
/// The node carries the [`SIMPLE_FRAMEBUFFER_COMPATIBLE`] match key and the
/// surface's single grant request — the matched display service is granted
/// exactly the surface window, nothing more (no ambient authority). A
/// degenerate mode is skipped fail-closed: the node is left unpublished
/// rather than half-described, and the boot proceeds with the display
/// driverless.
///
/// # Errors
///
/// Propagates the sink's [`DiscoveryError`] verbatim (a full bounded sink);
/// the caller leaves the display unpublished on any error (fail closed).
pub fn observe_boot_display(
    scanout: &BootScanout,
    sink: &mut dyn HwNodeSink,
) -> Result<(), DiscoveryError> {
    let (Some(resource), Ok(key)) = (
        scanout.grant(),
        HwMatchKey::compatible(SIMPLE_FRAMEBUFFER_COMPATIBLE),
    ) else {
        return Ok(());
    };
    let mut node = HwNode::new(
        BOOT_DISPLAY_NODE_ID,
        HW_NODE_ROOT_ID,
        HwDeviceClass::Display,
    );
    if node.push_match_key(key).is_err() || node.push_resource(resource).is_err() {
        return Ok(());
    }
    sink.emit(node)
}

/// The firmware surfaces the platform's own description of the display stands
/// in for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FirmwareSurfaces {
    /// A display engine takes the screen over, and every firmware surface
    /// with it, as Linux's `vc4` removes every firmware framebuffer.
    TakenOver,
    /// The boot display node describes the surface of `len` bytes at `base`.
    Published {
        /// The surface's first byte.
        base: u64,
        /// Its length in bytes.
        len: u64,
    },
}

impl FirmwareSurfaces {
    fn stands_in_for(self, node: &HwNode) -> bool {
        let is_surface = node
            .match_keys()
            .iter()
            .any(|key| key.compatible_bytes() == SIMPLE_FRAMEBUFFER_COMPATIBLE);
        is_surface
            && match self {
                Self::TakenOver => true,
                Self::Published { base, len } => node.resources().iter().any(|window| {
                    window.kind() == Some(HwResourceKind::Mmio)
                        && window.base() < base.saturating_add(len)
                        && base < window.base().saturating_add(window.length())
                }),
            }
    }
}

/// Withhold from `sink` the firmware's `simple-framebuffer` nodes `surfaces`
/// stands in for, and anything discovered beneath one.
pub fn withhold_firmware_surfaces(sink: &mut CollectingHwNodeSink, surfaces: FirmwareSurfaces) {
    let mut withheld: Vec<u32> = Vec::new();
    sink.retain(|node| {
        let excluded = withheld.contains(&node.parent()) || surfaces.stands_in_for(node);
        if excluded && withheld.try_reserve(1).is_ok() {
            withheld.push(node.id());
        }
        !excluded
    });
}

/// State on `engine`, the node of the display engine that takes the screen
/// over, that the boot console is its to move; answers whether it was stated.
///
/// An engine node with no room left states nothing, and its driver then
/// sets its outputs up without the console.
pub fn mark_boot_console(engine: &mut HwNode) -> bool {
    engine
        .push_resource(HwResource::property(HwProperty::BootConsole, 1))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use tairix_abi::HwResourceKind;

    /// A sink buffering every emitted node, mirroring the boot path's
    /// `boot_hwtree::CollectingHwNodeSink`.
    #[derive(Default)]
    struct CollectingSink {
        nodes: Vec<HwNode>,
    }

    impl HwNodeSink for CollectingSink {
        fn emit(&mut self, node: HwNode) -> Result<(), DiscoveryError> {
            self.nodes.push(node);
            Ok(())
        }
    }

    /// The QEMU `virt` ramfb boot console's discovered shape.
    fn ramfb_scanout() -> BootScanout {
        BootScanout {
            base: 0x4200_0000,
            width_px: 1024,
            height_px: 768,
            stride_bytes: 1024 * 4,
            format: DisplayFormat::Bgra8888,
            memory: FramebufferMemory::WriteCombine,
        }
    }

    #[test]
    fn publishes_a_display_node_with_the_framebuffer_grant_and_match_key() {
        let mut sink = CollectingSink::default();
        observe_boot_display(&ramfb_scanout(), &mut sink).expect("sink never fills");
        assert_eq!(sink.nodes.len(), 1);
        let node = &sink.nodes[0];
        assert_eq!(node.id(), BOOT_DISPLAY_NODE_ID);
        assert_eq!(node.parent(), HW_NODE_ROOT_ID);
        assert!(!node.is_root(), "a device node, never skipped as the root");
        assert_eq!(node.class(), Some(HwDeviceClass::Display));
        // The match key is the shared canonical model name, so the display
        // service's BIND_KEYS resolves it.
        assert!(node
            .match_keys()
            .iter()
            .any(|k| k.compatible_bytes() == SIMPLE_FRAMEBUFFER_COMPATIBLE));
        // Exactly one resource: the geometry-carrying, MMIO_MAP-gated
        // surface window, decoding back to the discovered mode.
        assert_eq!(node.resources().len(), 1);
        let resource = node.resources()[0];
        assert_eq!(resource.kind(), Some(HwResourceKind::Framebuffer));
        assert_eq!(resource.base(), 0x4200_0000);
        let mode = resource.framebuffer_mode().expect("mode decodes");
        assert_eq!(mode.width_px, 1024);
        assert_eq!(mode.height_px, 768);
        assert_eq!(mode.stride_bytes, 4096);
        assert_eq!(mode.format, DisplayFormat::Bgra8888);
        assert_eq!(
            resource.framebuffer_memory(),
            Ok(FramebufferMemory::WriteCombine)
        );
    }

    #[test]
    fn an_engine_taking_the_screen_over_is_told_the_console_and_granted_no_surface() {
        let mut engine = HwNode::new(9, HW_NODE_ROOT_ID, HwDeviceClass::Other);
        assert!(mark_boot_console(&mut engine));
        assert_eq!(engine.property(HwProperty::BootConsole), Some(1));
        assert!(
            !engine
                .resources()
                .iter()
                .any(|resource| resource.kind() == Some(HwResourceKind::Framebuffer)),
            "the firmware's surface is never the engine's to reach"
        );
        assert!(
            engine.match_keys().is_empty(),
            "the engine keeps its own binding"
        );
    }

    /// A firmware `simple-framebuffer` node at `base`, as the walk publishes
    /// it: its register window and nothing else.
    fn firmware_surface(id: u32, base: u64) -> HwNode {
        let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Display);
        node.push_match_key(HwMatchKey::compatible(SIMPLE_FRAMEBUFFER_COMPATIBLE).expect("key"))
            .expect("room");
        node.push_resource(HwResource::mmio(base, 0x7f_8000))
            .expect("room");
        node
    }

    fn collected(nodes: &[HwNode]) -> CollectingHwNodeSink {
        let mut sink = CollectingHwNodeSink::new();
        for node in nodes {
            sink.emit(*node).expect("the heap grows");
        }
        sink
    }

    fn ids(sink: &CollectingHwNodeSink) -> Vec<u32> {
        sink.nodes().iter().map(HwNode::id).collect()
    }

    #[test]
    fn an_engine_taking_the_screen_over_leaves_no_firmware_surface_to_bind() {
        let engine = HwNode::new(1, HW_NODE_ROOT_ID, HwDeviceClass::Other);
        let child = HwNode::new(4, 2, HwDeviceClass::Other);
        let mut sink = collected(&[
            engine,
            firmware_surface(2, 0x3e40_2000),
            firmware_surface(3, 0x1000_0000),
            child,
        ]);
        withhold_firmware_surfaces(&mut sink, FirmwareSurfaces::TakenOver);
        assert_eq!(ids(&sink), [1], "every surface, and what hangs beneath one");
    }

    #[test]
    fn the_boot_display_node_replaces_only_the_firmware_surface_it_describes() {
        let mut sink = collected(&[
            firmware_surface(2, 0x3e40_2000),
            firmware_surface(3, 0x1000_0000),
        ]);
        withhold_firmware_surfaces(
            &mut sink,
            FirmwareSurfaces::Published {
                base: 0x3e40_2000,
                len: 0x7f_8000,
            },
        );
        observe_boot_display(
            &BootScanout {
                base: 0x3e40_2000,
                ..ramfb_scanout()
            },
            &mut sink,
        )
        .expect("the heap grows");
        assert_eq!(ids(&sink), [3, BOOT_DISPLAY_NODE_ID]);
    }

    #[test]
    fn a_degenerate_mode_publishes_nothing() {
        // Zero extents and an under-sized stride each fail the
        // `HwResource::framebuffer` validation; the node is skipped rather
        // than half-described (fail closed).
        for scanout in [
            BootScanout {
                width_px: 0,
                ..ramfb_scanout()
            },
            BootScanout {
                height_px: 0,
                ..ramfb_scanout()
            },
            BootScanout {
                stride_bytes: 1024 * 4 - 1,
                ..ramfb_scanout()
            },
        ] {
            let mut sink = CollectingSink::default();
            observe_boot_display(&scanout, &mut sink).expect("skip is not an error");
            assert!(sink.nodes.is_empty(), "degenerate mode must not publish");
        }
    }
}
