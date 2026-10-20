//! Framebuffer boot console: the surface this port lights at boot, and its
//! half of the console the kernel hosts (`tairix_fbcon::port`).
//!
//! On the Raspberry Pi the display pipeline belongs to the `VideoCore`
//! firmware, so the surface comes from its mailbox property channel
//! (`tairix_vcmailbox`); on QEMU's `virt` board, from `ramfb` over `fw_cfg`
//! when QEMU was started with `-device ramfb`. Discovery runs **before the
//! MMU is enabled**, with the data caches off, so the firmware exchange is
//! coherent without maintenance; afterwards every console write is cleaned
//! to the point of coherency so a scan-out reading SDRAM sees it.
//!
//! No mailbox and no ramfb, a detached display, or any malformed firmware
//! answer leaves no surface to publish, and the UART keeps the console.

use core::ptr::NonNull;

use tairix_abi::driver::display::{DisplayFormat, DisplayMode};
use tairix_abi::hwtree::FramebufferMemory;
/// Named only by the surface-extent page arithmetic, which the host build
/// carries for its tests alone.
#[cfg(any(all(target_arch = "aarch64", target_os = "none"), test))]
use tairix_abi::PAGE_SIZE;
use tairix_fbcon::port::{ConsolePort, ConsoleSurface};
use tairix_fbcon::Geometry;
/// Re-exported so the boot consumer names the surface disposition it hands
/// this port without naming `tairix_fbcon` directly.
pub use tairix_fbcon::Surface;
use tairix_vcmailbox::{
    discover_framebuffer, query_display_size, FramebufferRequest, MailboxTransport,
};

use crate::irqmask::PortIrqControl;

/// Fixed scan-out width of the QEMU `virt` ramfb boot console — the
/// shared `tairix_fwcfg` console geometry, so the port programming the
/// device and every other consumer read one definition.
pub const RAMFB_WIDTH_PX: u32 = tairix_fwcfg::RAMFB_CONSOLE_WIDTH_PX;

/// Fixed scan-out height of the QEMU `virt` ramfb boot console.
pub const RAMFB_HEIGHT_PX: u32 = tairix_fwcfg::RAMFB_CONSOLE_HEIGHT_PX;

/// Text geometry of the fixed-size ramfb surface, tightly packed.
#[must_use]
pub fn ramfb_geometry() -> Option<Geometry> {
    Geometry::for_display(RAMFB_WIDTH_PX, RAMFB_HEIGHT_PX, RAMFB_WIDTH_PX * 4)
}

/// This port's half of the hosted console: `AT` probes, the direct map, and
/// the clean to the point of coherency a non-snooping scan-out needs.
pub struct FramebufferPort;

impl ConsolePort for FramebufferPort {
    type Irq = PortIrqControl;

    const CLEANS: bool = true;

    fn reachable(base: NonNull<u32>, bytes: usize) -> bool {
        surface_translates(base.as_ptr().addr(), bytes)
    }

    fn direct_map(phys: u64, len: u64) -> Option<NonNull<u32>> {
        if !crate::paging::physmap_covers(phys, len) {
            return None;
        }
        let va = usize::try_from(crate::paging::physmap_virt(phys)).ok()?;
        NonNull::new(core::ptr::with_exposed_provenance_mut(va))
    }

    fn publish(first: NonNull<u32>, bytes: usize) {
        crate::paging::clean_range_to_poc(first.as_ptr().addr() as u64, bytes as u64);
    }
}

/// `true` when every 4 KiB page of the `bytes` at `base` translates for an
/// EL1 write under the active root.
///
/// Probing every page rather than the extremities is what makes the answer
/// total: a hole anywhere inside would otherwise stay invisible until a write
/// landed in it.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn surface_translates(base: usize, bytes: usize) -> bool {
    let Some((first, last)) = surface_page_range(base, bytes) else {
        return false;
    };
    for page in (first..=last).step_by(crate::paging::PAGE_SIZE) {
        if crate::paging::par_faulted(crate::paging::translate_el1(page, true)) {
            return false;
        }
    }
    true
}

/// The host has no translation regime to probe, so nothing it holds is a
/// scan-out.
#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
fn surface_translates(_base: usize, _bytes: usize) -> bool {
    false
}

/// A firmware-allocated scan-out surface ready to host the console.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ConfiguredFramebuffer {
    /// ARM-physical base of the surface (page-aligned, in SDRAM).
    pub phys_base: u64,
    /// Allocated surface length in bytes.
    pub len_bytes: u32,
    /// Validated text geometry for the surface.
    pub geometry: Geometry,
    /// Pixel encoding the firmware programmed the surface with — the
    /// format the framebuffer request asked for, confirmed by the
    /// firmware's acceptance of the allocation.
    pub format: DisplayFormat,
}

/// Probe the attached display and allocate a matching scan-out surface
/// over `transport`.
///
/// Asks the firmware for the display's native (EDID-derived) size and
/// requests a 32-bit surface at exactly that size. `None` — leaving the UART
/// as the console — when no display is attached (`0×0`), or when any firmware
/// answer fails validation: the firmware is an external input.
pub fn bring_up(transport: &mut dyn MailboxTransport) -> Option<ConfiguredFramebuffer> {
    let size = query_display_size(transport).ok()?;
    if !size.is_attached() {
        return None;
    }
    let request = FramebufferRequest {
        width_px: size.width_px,
        height_px: size.height_px,
        format: DisplayFormat::Bgra8888,
    };
    let firmware = discover_framebuffer(transport, &request).ok()?;
    let phys_base = firmware.arm_physical_base().ok()?;
    let geometry =
        Geometry::for_display(firmware.width_px, firmware.height_px, firmware.pitch_bytes)?;
    Some(ConfiguredFramebuffer {
        phys_base,
        len_bytes: firmware.size_bytes,
        geometry,
        format: request.format,
    })
}

/// First and last 4 KiB page base spanned by the `bytes` at `fb_base`, or
/// `None` for an empty or unrepresentable extent.
///
/// Pure, so the boundary the reachability check walks is host-tested: a last
/// page left out of the range would leave a hole at the end of the surface
/// invisible until a write landed in it.
#[cfg(any(all(target_arch = "aarch64", target_os = "none"), test))]
#[must_use]
pub(crate) fn surface_page_range(fb_base: usize, bytes: usize) -> Option<(u64, u64)> {
    let base = fb_base as u64;
    let last = base.checked_add((bytes as u64).checked_sub(1)?)?;
    let page = PAGE_SIZE as u64;
    Some((base & !(page - 1), last & !(page - 1)))
}

/// What the boot audit line records about the video console bring-up.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DiscoveredVideo {
    /// CPU-physical base of the mailbox doorbell the exchange used —
    /// an MMIO fact the identity map's Device gigapage mask must cover.
    pub doorbell_base: u64,
    /// CPU-physical base of the firmware-allocated scan-out surface —
    /// RAM the renderer writes, so the identity map's RAM gigapage mask
    /// must cover it.
    pub fb_base: u64,
    /// Byte length of the firmware-allocated scan-out surface.
    pub fb_len_bytes: u64,
    /// Confirmed surface width in pixels.
    pub width_px: u32,
    /// Confirmed surface height in pixels.
    pub height_px: u32,
    /// Distance in bytes between the start of consecutive scanlines.
    pub stride_bytes: u32,
    /// Pixel encoding the surface was programmed with.
    pub format: DisplayFormat,
    /// CPU mapping policy the surface backing requires when a user-space
    /// display driver maps it. A boot-console scan-out is CPU-written and
    /// read by a display engine that does not snoop the CPU caches (the Pi's
    /// HVS, QEMU's `ramfb`), so it needs write-combining (Normal
    /// non-cacheable) memory — never write-back, which strands the CPU's
    /// writes in the data cache.
    pub memory: FramebufferMemory,
    /// The kernel owns the surface (`ramfb`'s), rather than the firmware.
    pub kernel_owned: bool,
}

impl DiscoveredVideo {
    /// The surface the console is published on.
    #[must_use]
    pub const fn surface(&self) -> ConsoleSurface {
        ConsoleSurface {
            phys: self.fb_base,
            mode: DisplayMode {
                width_px: self.width_px,
                height_px: self.height_px,
                stride_bytes: self.stride_bytes,
                format: self.format,
            },
            kernel_owned: self.kernel_owned,
        }
    }

    /// Where the boot CPU reaches the surface with its MMU off: identity.
    #[must_use]
    pub fn identity_base(&self) -> Option<NonNull<u32>> {
        let base = usize::try_from(self.fb_base).ok()?;
        NonNull::new(core::ptr::with_exposed_provenance_mut(base))
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub use metal::configure;

/// The freestanding half: the firmware exchange and the boot surface.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
mod metal {
    use core::cell::UnsafeCell;

    use tairix_fdt::Fdt;
    use tairix_fwcfg::{FwCfg, MmioDma, RamfbConfig, DRM_FORMAT_XRGB8888};

    use crate::firmware::DiscoveredMailbox;

    use super::{bring_up, ramfb_geometry, DiscoveredVideo, Geometry, MailboxTransport};

    /// Bring the framebuffer console's surface up: over `firmware`, the
    /// transport to the `VideoCore` firmware, where the tree carries a mailbox
    /// (the Pi), else the QEMU `virt` `fw_cfg` / `ramfb` fallback in `fdt`.
    ///
    /// **Boot-CPU, pre-MMU only**: with the data caches off the firmware
    /// exchange is coherent. `None` on any failure: the UART keeps the
    /// console.
    #[must_use]
    pub fn configure(
        fdt: &Fdt<'_>,
        firmware: Option<(DiscoveredMailbox, &mut dyn MailboxTransport)>,
    ) -> Option<DiscoveredVideo> {
        match firmware {
            Some((mailbox, transport)) => configure_mailbox(mailbox, transport),
            None => configure_ramfb(fdt),
        }
    }

    /// The Pi's mailbox-allocated surface.
    fn configure_mailbox(
        mailbox: DiscoveredMailbox,
        transport: &mut dyn MailboxTransport,
    ) -> Option<DiscoveredVideo> {
        let configured = bring_up(transport)?;
        let fb_base = usize::try_from(configured.phys_base).ok()?;
        // The firmware allocated `[fb_base, fb_base + len_bytes)` page-aligned
        // inside the validated `VideoCore` SDRAM aperture.
        discovered(
            fb_base,
            u64::from(configured.len_bytes),
            configured.geometry,
            mailbox.base,
            configured.format,
            false,
        )
    }

    /// Pixels in the statically-reserved ramfb scan-out surface.
    const RAMFB_PIXEL_COUNT: usize =
        super::RAMFB_WIDTH_PX as usize * super::RAMFB_HEIGHT_PX as usize;

    /// The QEMU `virt` ramfb scan-out surface: `ramfb` scans guest RAM
    /// directly, so the kernel supplies the surface itself, as zero-filled
    /// BSS that keeps the pre-heap bring-up allocation-free. Written only
    /// through the console, under its discipline.
    struct RamfbSurface(UnsafeCell<[u32; RAMFB_PIXEL_COUNT]>);

    // SAFETY: reached only through the hosted console, whose slot discipline
    // serialises every access (written once pre-publication on the boot CPU,
    // under the render lock afterwards).
    unsafe impl Sync for RamfbSurface {}

    static RAMFB_SURFACE: RamfbSurface = RamfbSurface(UnsafeCell::new([0; RAMFB_PIXEL_COUNT]));

    /// The QEMU `virt` ramfb surface over `fw_cfg`, only if the `etc/ramfb`
    /// item exists.
    fn configure_ramfb(fdt: &Fdt<'_>) -> Option<DiscoveredVideo> {
        let dma = MmioDma::from_dtb(fdt).ok()?;
        let doorbell_base = dma.base();
        let geometry = ramfb_geometry()?;
        let fb_base = RAMFB_SURFACE.0.get().addr();
        let fb_len_bytes = (RAMFB_PIXEL_COUNT * 4) as u64;
        FwCfg::new(dma)
            .program_ramfb(&RamfbConfig {
                phys_base: fb_base as u64,
                drm_format: DRM_FORMAT_XRGB8888,
                flags: 0,
                width: geometry.width_px,
                height: geometry.height_px,
                stride: geometry.stride_px * 4,
            })
            .ok()?;
        // `DRM_FORMAT_XRGB8888` is little-endian `0xXXRRGGBB`: in memory B, G,
        // R, X — `Bgra8888` with the alpha byte ignored by the scan-out.
        discovered(
            fb_base,
            fb_len_bytes,
            geometry,
            doorbell_base,
            super::DisplayFormat::Bgra8888,
            true,
        )
    }

    /// The surface both bring-up paths end with, once its extent is proved to
    /// hold the mode; the kernel publishes it to its console.
    ///
    /// The caller guarantees `[fb_base, fb_base + fb_len_bytes)` is
    /// identity-addressed RAM it exclusively owns for scan-out — the kernel's
    /// own when `kernel_owned`, else the firmware's.
    fn discovered(
        fb_base: usize,
        fb_len_bytes: u64,
        geometry: Geometry,
        doorbell_base: u64,
        format: super::DisplayFormat,
        kernel_owned: bool,
    ) -> Option<DiscoveredVideo> {
        if u64::try_from(geometry.pixel_count().checked_mul(4)?).ok()? > fb_len_bytes
            || fb_base == 0
        {
            return None;
        }
        Some(DiscoveredVideo {
            doorbell_base,
            fb_base: fb_base as u64,
            fb_len_bytes,
            width_px: geometry.width_px,
            height_px: geometry.height_px,
            stride_bytes: geometry.stride_px * 4,
            format,
            memory: super::FramebufferMemory::WriteCombine,
            kernel_owned,
        })
    }
}

#[cfg(test)]
mod tests {
    use tairix_vcmailbox::mock::MockFirmware;

    use super::*;

    #[test]
    fn a_page_aligned_surface_of_one_page_spans_exactly_that_page() {
        assert_eq!(surface_page_range(0x1000, 4096), Some((0x1000, 0x1000)));
    }

    #[test]
    fn an_unaligned_surface_spans_every_page_it_touches() {
        // 16 bytes from 0x1ffc end at 0x200b, so the extent straddles the
        // boundary and both pages must be probed.
        assert_eq!(surface_page_range(0x1ffc, 16), Some((0x1000, 0x2000)));
    }

    #[test]
    fn the_last_page_of_a_surface_is_inside_the_range() {
        // The Pi 4 scan-out a metal capture reported: 0x7f8000 bytes from
        // 0x3e402000, ending at 0x3ebf9fff.
        let (first, last) = surface_page_range(0x3e40_2000, 0x7f_8000).expect("real extent");
        assert_eq!(first, 0x3e40_2000);
        assert_eq!(last, 0x3ebf_9000);
        assert!(last + (PAGE_SIZE as u64) > 0x3e40_2000 + 0x7f_8000 - 1);
    }

    #[test]
    fn an_empty_or_unrepresentable_surface_has_no_range() {
        assert_eq!(surface_page_range(0x1000, 0), None);
        assert_eq!(surface_page_range(usize::MAX, 2), None);
    }

    #[test]
    fn the_host_port_reaches_no_surface() {
        assert!(!FramebufferPort::reachable(NonNull::dangling(), 4));
    }

    /// A mock firmware whose answers are mutually consistent for a
    /// 1920×1080 display: pitch one scanline, surface one full frame.
    fn full_hd_firmware() -> MockFirmware {
        let mut firmware = MockFirmware::healthy();
        firmware.fb_pitch = 1920 * 4;
        firmware.fb_size = 1920 * 4 * 1080;
        firmware
    }

    #[test]
    fn bring_up_allocates_the_displays_native_mode() {
        let mut firmware = full_hd_firmware();
        let configured = bring_up(&mut firmware).expect("bring-up");
        assert_eq!(configured.phys_base, 0x1000_0000);
        assert_eq!(configured.len_bytes, 1920 * 4 * 1080);
        let geometry = configured.geometry;
        assert_eq!((geometry.width_px, geometry.height_px), (1920, 1080));
        assert_eq!(geometry.stride_px, 1920);
        assert_eq!(
            (geometry.columns(), geometry.rows()),
            (240, 67),
            "1080p holds more cells of the authored 8×16 size, never magnified ones"
        );
    }

    #[test]
    fn bring_up_fails_closed_with_no_display_attached() {
        let mut firmware = full_hd_firmware();
        (firmware.display_w, firmware.display_h) = (0, 0);
        assert!(bring_up(&mut firmware).is_none());
    }

    #[test]
    fn bring_up_fails_closed_on_an_inconsistent_answer() {
        // The display reports 1920×1080 but the allocate answer carries
        // the healthy mock's 640×480 pitch — narrower than a scanline.
        let mut firmware = MockFirmware::healthy();
        assert!(bring_up(&mut firmware).is_none());
    }

    #[test]
    fn ramfb_geometry_is_always_renderable() {
        let geometry = ramfb_geometry().expect("the fixed ramfb mode is renderable");
        assert_eq!(geometry.width_px, RAMFB_WIDTH_PX);
        assert_eq!(geometry.height_px, RAMFB_HEIGHT_PX);
        assert_eq!(geometry.stride_px, RAMFB_WIDTH_PX, "tightly packed");
        assert_eq!(
            (geometry.columns(), geometry.rows()),
            (128, 48),
            "1024×768 is the conventional 128×48 text grid"
        );
    }
}
