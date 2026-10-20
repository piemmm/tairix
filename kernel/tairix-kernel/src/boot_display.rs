//! Where the framebuffer console is shown, decided once discovery is done
//! ([`place_console`], `plans/DISPLAY.md` D7d).
//!
//! A display engine discovery found takes the screen over and hosts the
//! console: its node states [`HwProperty::ConsoleHost`], its driver hosts the
//! console on memory the kernel owns (`console_adopt`), and every firmware
//! surface is withheld, so nothing writes or scans out memory the firmware may
//! take back. Otherwise a surface the firmware lit is published as a
//! [`HwDeviceClass::Display`] node carrying its [`HwResource::framebuffer`]
//! grant and the [`SIMPLE_FRAMEBUFFER_COMPATIBLE`] key the framebuffer driver
//! binds, in place of the firmware's own `simple-framebuffer` nodes, which
//! carry no geometry a grant needs.
//!
//! The node joins the boot-discovered tree, so autoload matches it as any
//! other device. The boot display is the boot seat's display, so no seat is
//! minted here (`plans/DISPLAY.md` D6).

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
    /// The mode the surface runs.
    const fn mode(&self) -> DisplayMode {
        DisplayMode {
            width_px: self.width_px,
            height_px: self.height_px,
            stride_bytes: self.stride_bytes,
            format: self.format,
        }
    }

    /// The surface as the one [`HwResource::framebuffer`] grant request a
    /// node carries, sized and validated from the discovered mode, or `None`
    /// for a degenerate mode (zero extent, an under-sized stride, an
    /// overflowing length) no correct, bounded grant can be minted for.
    fn grant(&self) -> Option<HwResource> {
        HwResource::framebuffer(self.base, &self.mode(), self.memory).ok()
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
fn observe_boot_display(
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
enum FirmwareSurfaces {
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
                // A surface stands for the window at its base, however
                // degenerate the mode its length came from.
                Self::Published { base, len } => node.resources().iter().any(|window| {
                    window.kind() == Some(HwResourceKind::Mmio)
                        && window.base() < base.saturating_add(len.max(1))
                        && base < window.base().saturating_add(window.length())
                }),
            }
    }
}

/// Withhold from `sink` the firmware's `simple-framebuffer` nodes `surfaces`
/// stands in for, and anything discovered beneath one.
fn withhold_firmware_surfaces(sink: &mut CollectingHwNodeSink, surfaces: FirmwareSurfaces) {
    let mut withheld: Vec<u32> = Vec::new();
    sink.retain(|node| {
        let excluded = withheld.contains(&node.parent()) || surfaces.stands_in_for(node);
        if excluded && withheld.try_reserve(1).is_ok() {
            withheld.push(node.id());
        }
        !excluded
    });
}

/// Where [`place_console`] decided the framebuffer console is shown.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConsolePlacement {
    /// A display engine hosts it, taking the screen over from any firmware
    /// surface.
    Hosted,
    /// The firmware's surface shows it, published as the boot display.
    Firmware,
    /// An engine takes the screen over but could not be told it hosts the
    /// console, so nothing may paint the firmware's surface.
    Displaced,
    /// No display exists.
    Nowhere,
}

/// The first virtio GPU discovery found: a display engine that takes the
/// screen over and hosts the console.
#[must_use]
pub fn virtio_gpu(sink: &CollectingHwNodeSink) -> Option<u32> {
    let key = HwMatchKey::virtio(tairix_abi::driver::virtio::VIRTIO_GPU_DEVICE_ID);
    sink.nodes()
        .iter()
        .find(|node| {
            node.class() == Some(HwDeviceClass::Display) && node.match_keys().contains(&key)
        })
        .map(HwNode::id)
}

/// Decide, once discovery is done, where the framebuffer console is shown:
/// on `host`, the node of the display engine that takes the screen over,
/// which is told it hosts the console and takes every firmware surface with
/// it; else on the firmware's `scanout`, published as the boot display node in
/// place of the firmware's own description of it.
pub fn place_console(
    sink: &mut CollectingHwNodeSink,
    host: Option<u32>,
    scanout: Option<&BootScanout>,
) -> ConsolePlacement {
    if let Some(host) = host {
        let marked = sink
            .nodes_mut()
            .iter_mut()
            .find(|node| node.id() == host)
            .is_some_and(|node| {
                node.push_resource(HwResource::property(HwProperty::ConsoleHost, 1))
                    .is_ok()
            });
        withhold_firmware_surfaces(sink, FirmwareSurfaces::TakenOver);
        return if marked {
            ConsolePlacement::Hosted
        } else {
            ConsolePlacement::Displaced
        };
    }
    let Some(scanout) = scanout else {
        return ConsolePlacement::Nowhere;
    };
    withhold_firmware_surfaces(
        sink,
        FirmwareSurfaces::Published {
            base: scanout.base,
            len: scanout.mode().len_bytes(),
        },
    );
    // The collecting sink never fills; a degenerate mode publishes nothing.
    let _ = observe_boot_display(scanout, sink);
    ConsolePlacement::Firmware
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
    fn an_engine_taking_the_screen_over_hosts_the_console_and_leaves_no_firmware_surface() {
        let engine = HwNode::new(1, HW_NODE_ROOT_ID, HwDeviceClass::Other);
        let child = HwNode::new(4, 2, HwDeviceClass::Other);
        let mut sink = collected(&[
            engine,
            firmware_surface(2, 0x3e40_2000),
            firmware_surface(3, 0x1000_0000),
            child,
        ]);
        let scanout = BootScanout {
            base: 0x3e40_2000,
            ..ramfb_scanout()
        };
        assert_eq!(
            place_console(&mut sink, Some(1), Some(&scanout)),
            ConsolePlacement::Hosted
        );
        assert_eq!(ids(&sink), [1], "every surface, and what hangs beneath one");
        let engine = sink.nodes()[0];
        assert_eq!(engine.property(HwProperty::ConsoleHost), Some(1));
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

    #[test]
    fn an_engine_no_firmware_lit_still_hosts_the_console() {
        let mut sink = collected(&[HwNode::new(1, HW_NODE_ROOT_ID, HwDeviceClass::Display)]);
        assert_eq!(
            place_console(&mut sink, Some(1), None),
            ConsolePlacement::Hosted
        );
        assert_eq!(sink.nodes()[0].property(HwProperty::ConsoleHost), Some(1));
    }

    #[test]
    fn the_first_virtio_gpu_is_the_engine_that_hosts_the_console() {
        let gpu = |id| {
            let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Display);
            node.push_match_key(HwMatchKey::virtio(
                tairix_abi::driver::virtio::VIRTIO_GPU_DEVICE_ID,
            ))
            .expect("room");
            node
        };
        let mut sound = HwNode::new(5, HW_NODE_ROOT_ID, HwDeviceClass::Audio);
        sound.push_match_key(HwMatchKey::virtio(25)).expect("room");
        let mut sink = collected(&[sound, firmware_surface(6, 0x3e40_2000), gpu(7), gpu(8)]);
        let host = virtio_gpu(&sink);
        assert_eq!(host, Some(7));
        assert_eq!(
            place_console(&mut sink, host, None),
            ConsolePlacement::Hosted
        );
        assert_eq!(ids(&sink), [5, 7, 8], "no firmware surface beside it");
        assert_eq!(sink.nodes()[1].property(HwProperty::ConsoleHost), Some(1));
        assert_eq!(sink.nodes()[2].property(HwProperty::ConsoleHost), None);
        assert_eq!(virtio_gpu(&collected(&[sound])), None);
    }

    #[test]
    fn an_engine_that_cannot_be_told_still_takes_the_screen_and_shows_nothing() {
        let mut full = HwNode::new(1, HW_NODE_ROOT_ID, HwDeviceClass::Display);
        while full.push_resource(HwResource::mmio(0x1000, 0x1000)).is_ok() {}
        let mut sink = collected(&[full, firmware_surface(2, 0x3e40_2000)]);
        assert_eq!(
            place_console(&mut sink, Some(1), Some(&ramfb_scanout())),
            ConsolePlacement::Displaced
        );
        assert_eq!(ids(&sink), [1], "no boot display for a screen taken over");
        assert_eq!(
            place_console(&mut collected(&[]), Some(9), None),
            ConsolePlacement::Displaced,
            "an engine whose node is not in the tree"
        );
    }

    #[test]
    fn the_boot_display_node_replaces_only_the_firmware_surface_it_describes() {
        let mut sink = collected(&[
            firmware_surface(2, 0x3e40_2000),
            firmware_surface(3, 0x1000_0000),
        ]);
        let scanout = BootScanout {
            base: 0x3e40_2000,
            ..ramfb_scanout()
        };
        assert_eq!(
            place_console(&mut sink, None, Some(&scanout)),
            ConsolePlacement::Firmware
        );
        assert_eq!(ids(&sink), [3, BOOT_DISPLAY_NODE_ID]);
        assert_eq!(
            place_console(&mut collected(&[]), None, None),
            ConsolePlacement::Nowhere
        );
        // A mode with no length still stands for the surface at its base.
        let mut sink = collected(&[firmware_surface(2, 0x3e40_2000)]);
        let degenerate = BootScanout {
            base: 0x3e40_2000,
            height_px: 0,
            ..ramfb_scanout()
        };
        let _ = place_console(&mut sink, None, Some(&degenerate));
        assert!(!ids(&sink).contains(&2), "{:?}", ids(&sink));
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
