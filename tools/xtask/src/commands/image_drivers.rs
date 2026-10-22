//! Cross-compile and sign the autoloaded `/System/Drivers/` bundles the
//! flashable Raspberry Pi image ships (`plans/PI.md` P10 D4).
//!
//! `tools/mkimage` is a pure library: it plants bundle *bytes* into the
//! read-only `/System` store but never drives `cargo`. This module is the
//! orchestration half — it builds each user-space driver crate
//! position-independent for the freestanding `aarch64-unknown-none` target,
//! converts the linked PIE ELF to an `rxe` image (relocated for the
//! production user-image bias and stamped with the kernel's compiled-in
//! syscall CFI tag), and wraps that `rxe` as the signed payload of a
//! `kind = UserSpace` [`DriverManifest`]. The bundle is signed with the build
//! key set's driver-signing seed (`tools/signkeys`), the key the kernel's
//! build derives its embedded trust anchor from, so the booted kernel admits
//! it through the signed load gate.
//!
//! The signed-bundle composer and the ELF->rxe converter are the shared
//! definitions the kernel `build.rs` and the autoload fixtures also use
//! (`tairix_itest_harness`), so the wire layout is never
//! re-rolled here. This is host-only build glue; the
//! production image stays Rust-only.

use std::sync::OnceLock;

use tairix_abi::{CapabilityId, DriverKind, DriverManifest, DRIVER_MANIFEST_MAGIC};
use tairix_itest_harness::driver_image::build_signed_driver_image;
use tairix_itest_harness::elf2rxe::elf_to_rxe;
use tairix_itest_harness::pie::PieArch;
use tairix_itest_harness::USER_IMAGE_BIAS;

use tairix_mkimage::ImageProfile;
use tairix_signkeys::Domain;

use super::image_apps::{memo_slot, AppStoreFile, MEMO_SLOTS};
use super::pie_build::cross_compile_pie_elf;
use crate::Context;

/// Store path of the `VideoCore` mailbox service-driver bundle, **relative to
/// the `/System` volume root** (whose root *is* `/System`, design B). The
/// namespace is `Drivers/<class>[_<subtype>]/<leaf>/<driver>`: class
/// `bus`, subtype `mailbox`, the `vcmailbox` leaf naming the device, the
/// `Run` entry binary.
pub const VCMAILBOX_STORE_PATH: &[&[u8]] = &[b"Drivers", b"bus_mailbox", b"vcmailbox", b"Run"];

/// Store path of the BCM2711 PCIe root-complex bus-driver bundle: class `bus`,
/// subtype `pcie`, the chip leaf `bcm2711` (the
/// vendor/chip name appears only at the leaf, the class namespace above it
/// stays vendor-neutral).
pub const PCIE_BRCM_STORE_PATH: &[&[u8]] = &[b"Drivers", b"bus_pcie", b"bcm2711", b"Run"];

/// Store path of the VL805 USB host-controller bus-driver bundle: class `bus`,
/// subtype `usb`, the chip leaf `vl805`.
pub const VL805_STORE_PATH: &[&[u8]] = &[b"Drivers", b"bus_usb", b"vl805", b"Run"];

/// Store path of the xHCI USB host-controller driver (HCD) bundle: class
/// `bus`, subtype `usb`, the `xhci` leaf naming the (vendor-neutral) generic
/// host-controller class it drives.
pub const USB_XHCI_STORE_PATH: &[&[u8]] = &[b"Drivers", b"bus_usb", b"xhci", b"Run"];

/// Store path of the USB HID class-driver bundle: class `input`, the
/// `usb_hid` leaf naming the (vendor-neutral) driver.
pub const USB_HID_STORE_PATH: &[&[u8]] = &[b"Drivers", b"input", b"usb_hid", b"Run"];

/// Store path of the virtio-input keyboard/pointer driver bundle: class
/// `input`, the `virtio_kbd` leaf naming the (vendor-neutral) driver — the
/// same path the `-M virt` autoload vertical's fixture plants.
pub const VIRTIO_KBD_STORE_PATH: &[&[u8]] = &[b"Drivers", b"input", b"virtio_kbd", b"Run"];

/// Store path of the framebuffer display-service bundle: class `display`,
/// the `framebuffer` leaf naming the (vendor-neutral) service that drives
/// any platform-published linear scan-out surface.
pub const FRAMEBUFFER_STORE_PATH: &[&[u8]] = &[b"Drivers", b"display", b"framebuffer", b"Run"];

/// Store path of the virtio-gpu display bundle: class `display`, the
/// `virtio_gpu` leaf naming the (vendor-neutral) driver.
pub const VIRTIO_GPU_STORE_PATH: &[&[u8]] = &[b"Drivers", b"display", b"virtio_gpu", b"Run"];

/// Store path of the BCM2711 display engine bundle: class `display`, the part
/// leaf `bcm2711_hvs` (the HVS and the pixel valves feeding the HDMI outputs).
pub const BCM2711_HVS_STORE_PATH: &[&[u8]] = &[b"Drivers", b"display", b"bcm2711_hvs", b"Run"];

/// Store path of the BCM2711 HDMI transmitter bundle: class `display`, the
/// part leaf `bcm2711_hdmi` (each HDMI block, an encoder of the display
/// engine).
pub const BCM2711_HDMI_STORE_PATH: &[&[u8]] = &[b"Drivers", b"display", b"bcm2711_hdmi", b"Run"];

/// Store path of the virtio-net link-layer driver bundle: class `network`,
/// the `virtio_net` leaf naming the (vendor-neutral) driver — the path the
/// `-M virt` two-process netstack autoload vertical's disk plants it at.
pub const VIRTIO_NET_STORE_PATH: &[&[u8]] = &[b"Drivers", b"network", b"virtio_net", b"Run"];

/// Store path of the virtio sound driver bundle: class `audio`, the
/// `virtio_snd` leaf naming the (vendor-neutral) driver. The class namespace
/// above the leaf names what the device *is*, never who made it.
pub const VIRTIO_SND_STORE_PATH: &[&[u8]] = &[b"Drivers", b"audio", b"virtio_snd", b"Run"];

/// Store path of the USB Audio Class driver bundle: class `audio`, the
/// `usb_uac` leaf naming the class it serves.
pub const USB_UAC_STORE_PATH: &[&[u8]] = &[b"Drivers", b"audio", b"usb_uac", b"Run"];

/// Store path of the HD Audio driver bundle: class `audio`, the `hda` leaf
/// naming the controller class it serves.
pub const HDA_STORE_PATH: &[&[u8]] = &[b"Drivers", b"audio", b"hda", b"Run"];

/// Store path of the Raspberry Pi 4 headphone-jack driver bundle: class
/// `audio`, the part leaf `bcm2711_pwm`.
pub const PWM_AUDIO_STORE_PATH: &[&[u8]] = &[b"Drivers", b"audio", b"bcm2711_pwm", b"Run"];

/// Store path of the BCM2711 PCM/I2S interface driver bundle: class
/// `audio`, the part leaf `bcm2711_i2s`.
pub const I2S_AUDIO_STORE_PATH: &[&[u8]] = &[b"Drivers", b"audio", b"bcm2711_i2s", b"Run"];

/// Store path of the PCM5102A codec driver bundle: class `audio`, the part
/// leaf `pcm5102a`.
pub const PCM5102A_STORE_PATH: &[&[u8]] = &[b"Drivers", b"audio", b"pcm5102a", b"Run"];

/// Store path of the PCM5122 codec driver bundle: class `audio`, the part
/// leaf `pcm5122`.
pub const PCM5122_STORE_PATH: &[&[u8]] = &[b"Drivers", b"audio", b"pcm5122", b"Run"];

/// `/System`-volume-relative store path of the GENET link-layer driver
/// bundle (the Raspberry Pi 4B's on-board gigabit Ethernet).
pub const GENET_STORE_PATH: &[&[u8]] = &[b"Drivers", b"network", b"genet", b"Run"];

/// Store path of the USB mass-storage class-driver bundle: class `storage`,
/// the `usb_msd` leaf naming the (vendor-neutral) driver.
pub const USB_MSD_STORE_PATH: &[&[u8]] = &[b"Drivers", b"storage", b"usb_msd", b"Run"];

/// Store path of the volume-manager policy-driver bundle: class `storage`,
/// the `volmgr` leaf naming the (vendor-neutral, bus-neutral) policy
/// driver that binds the per-LUN block-service nodes.
pub const VOLMGR_STORE_PATH: &[&[u8]] = &[b"Drivers", b"storage", b"volmgr", b"Run"];

/// Store path of the RAID member-agent driver bundle: class `storage`, the
/// `raid_member` leaf naming the (vendor-neutral, bus-neutral) driver that
/// binds the array-member nodes the volume manager emits.
pub const RAID_MEMBER_STORE_PATH: &[&[u8]] = &[b"Drivers", b"storage", b"raid_member", b"Run"];

/// Store path of the RAID array-composer driver bundle: class `storage`, the
/// `raid` leaf naming the (vendor-neutral, bus-neutral) policy driver that
/// binds the kernel's synthetic virtual bus and composes the offered members
/// into served arrays.
pub const RAID_STORE_PATH: &[&[u8]] = &[b"Drivers", b"storage", b"raid", b"Run"];

/// Store path of the PL031 real-time-clock driver bundle: class `rtc`, the
/// chip leaf `pl031` (the part name appears only at the leaf; the class
/// namespace above it stays vendor-neutral).
pub const PL031_STORE_PATH: &[&[u8]] = &[b"Drivers", b"rtc", b"pl031", b"Run"];

/// Store path of the Goldfish real-time-clock driver bundle: class `rtc`,
/// the chip leaf `goldfish` (the riscv64 `virt` board's clock).
pub const GOLDFISH_STORE_PATH: &[&[u8]] = &[b"Drivers", b"rtc", b"goldfish", b"Run"];

/// Store path of the MC146818 real-time-clock driver bundle: class `rtc`,
/// the chip leaf `mc146818` (the PC-compatible CMOS clock).
pub const MC146818_STORE_PATH: &[&[u8]] = &[b"Drivers", b"rtc", b"mc146818", b"Run"];

/// Store path of the Raspberry Pi real-time-clock driver bundle: class `rtc`,
/// the chip leaf `rpi` (the Pi 5's PMIC clock, reached over the firmware
/// mailbox).
pub const RPI_RTC_STORE_PATH: &[&[u8]] = &[b"Drivers", b"rtc", b"rpi", b"Run"];

/// Store path of the Raspberry Pi firmware-clocks driver bundle: class
/// `clock`, the leaf `rpi_firmware` (the clocks the `VideoCore` firmware owns,
/// reached over the firmware mailbox).
pub const RPI_FIRMWARE_CLOCKS_STORE_PATH: &[&[u8]] =
    &[b"Drivers", b"clock", b"rpi_firmware", b"Run"];

/// Store path of the Broadcom Serial Controller I2C bus driver bundle: class
/// `bus_i2c`, the controller leaf `bcm2835` (the part name appears only at
/// the leaf; the class namespace above it stays vendor-neutral).
pub const I2C_BCM2835_STORE_PATH: &[&[u8]] = &[b"Drivers", b"bus_i2c", b"bcm2835", b"Run"];

/// Store path of the BCM2711 HDMI blocks' DDC controller driver bundle: class
/// `bus_i2c`, the controller-family leaf `brcmstb`.
pub const I2C_BRCMSTB_STORE_PATH: &[&[u8]] = &[b"Drivers", b"bus_i2c", b"brcmstb", b"Run"];

/// Store path of the Broadcom legacy DMA engine driver bundle: class `dma`,
/// the leaf `bcm2835` its binding is named for.
pub const DMA_BCM2835_STORE_PATH: &[&[u8]] = &[b"Drivers", b"dma", b"bcm2835", b"Run"];

/// Store path of the Broadcom second-level interrupt controller driver bundle:
/// class `irq`, the leaf `brcmstb_l2` its controller family is named for.
pub const IRQ_BRCMSTB_L2_STORE_PATH: &[&[u8]] = &[b"Drivers", b"irq", b"brcmstb_l2", b"Run"];

/// Store path of the BCM2711 clock manager driver bundle: class `clock`, the
/// part leaf `bcm2711_cprman`.
pub const CPRMAN_BCM2711_STORE_PATH: &[&[u8]] = &[b"Drivers", b"clock", b"bcm2711_cprman", b"Run"];

/// Store path of the BCM2711 DVP clock-gate driver bundle: class `clock`, the
/// part leaf `bcm2711_dvp`.
pub const DVP_BCM2711_STORE_PATH: &[&[u8]] = &[b"Drivers", b"clock", b"bcm2711_dvp", b"Run"];

/// Store path of the DS3231 / DS1307 real-time-clock driver bundle: class
/// `rtc`, the chip leaf `ds3231`.
pub const DS3231_STORE_PATH: &[&[u8]] = &[b"Drivers", b"rtc", b"ds3231", b"Run"];

/// Store path of the PCF8523 real-time-clock driver bundle: class `rtc`, the
/// chip leaf `pcf8523`.
pub const PCF8523_STORE_PATH: &[&[u8]] = &[b"Drivers", b"rtc", b"pcf8523", b"Run"];

/// Store path of the PCF85063A real-time-clock driver bundle: class `rtc`,
/// the chip leaf `pcf85063a`.
pub const PCF85063A_STORE_PATH: &[&[u8]] = &[b"Drivers", b"rtc", b"pcf85063a", b"Run"];

/// Cross-compile `package` for the freestanding aarch64 target, convert the
/// linked PIE ELF to a production-biased `rxe`, and wrap it as the signed
/// payload of a `kind = UserSpace` bundle requesting exactly `caps` and
/// carrying the driver crate's own canonical `bind_keys`. The single composer every installed user-space
/// bundle shares, so the wire layout, the signing seed, and the fail-closed
/// sanity check live in one place.
///
/// # Errors
///
/// A string describing a failed cross-compile, a missing ELF artefact, an
/// ELF->rxe conversion failure, or a structurally invalid composed bundle.
fn build_bundle(
    ctx: &Context,
    arch: PieArch,
    package: &str,
    caps: &[CapabilityId],
    bind_keys: &[tairix_abi::DriverBindKey],
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    // A driver crate's `Run` binary shares the package name; cargo resolves
    // the crate from `-p <package>`, and every program links the one shared
    // PIE script, so no per-crate source directory is needed here.
    let elf = cross_compile_pie_elf(ctx, arch, "image-drivers", package, package, profile)?;
    let rxe = elf_to_rxe(
        &elf,
        &tairix_kernel_syscall::SYSCALL_TABLE_HASH,
        USER_IMAGE_BIAS,
    )
    .map_err(|e| format!("image: convert {package} driver ELF to rxe: {e}"))?;

    let keys = ctx.signing_keys().map_err(|e| format!("image: {e}"))?;
    let signed = build_signed_driver_image(
        keys.seed(Domain::DriverSigning),
        DriverKind::UserSpace,
        caps,
        bind_keys,
        tairix_kernel_syscall::SYSCALL_TABLE_HASH,
        &rxe,
    );
    verify_signed_bundle(&signed.image)?;
    Ok(signed.image)
}

/// Build and sign the user-space `VideoCore` mailbox service-driver bundle for
/// installation into the image's `/System/Drivers/` store.
///
/// Returns the signed `.rxe` bundle bytes exactly as the store scan
/// reads them back. The driver requests only the capabilities it needs — a
/// mapped doorbell window (`CAP_MMIO_MAP`), a coherent DMA property buffer
/// (`CAP_MEM_DMA`), the inbox interrupt every reply wait parks on instead of
/// polling the doorbell (`CAP_IRQ_BIND`), the privilege to create the
/// restricted-sender mailbox endpoint (`CAP_IPC_BIND_PRIVILEGED`), and the
/// log a refused firmware request is recorded on (`CAP_LOG_EMIT`). The set
/// and the bind table are `lib/vcmailbox`'s own, so neither drifts from the
/// service.
///
/// # Errors
///
/// A string describing a failed cross-compile, a missing ELF artefact, or an
/// ELF->rxe conversion failure.
pub fn build_vcmailbox_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-bus-mailbox-vcmailbox",
        tairix_vcmailbox::SERVICE_CAPS,
        tairix_vcmailbox::BIND_KEYS,
        profile,
    )
}

/// Build and sign the BCM2711 PCIe root-complex bus-driver bundle.
///
/// It maps its discovered register window (`CAP_MMIO_MAP`), trains the link,
/// assigns the VL805 BAR, allocates the controller's MSI vector so the matched
/// xHCI driver parks on its completion interrupt rather than busy-polling
/// (`CAP_IRQ_BIND`, which the `msi_alloc` trap is gated on), and publishes the
/// enumerated USB host function into the live hardware tree (`CAP_HW_EMIT`) —
/// and nothing more (no ambient authority). Carries
/// `tairix_drv_bus_pcie_brcm::BIND_KEYS`, so it autoloads against the
/// discovered `brcm,bcm2711-pcie` node.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_pcie_brcm_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-bus-pcie-brcm",
        tairix_drv_bus_pcie_brcm::REQUIRED_CAPABILITIES,
        tairix_drv_bus_pcie_brcm::BIND_KEYS,
        profile,
    )
}

/// Build and sign the VL805 USB host-controller bus-driver bundle.
///
/// It reloads the controller's firmware over the `vcmailbox` property mailbox
/// (`CAP_MAILBOX`) and then publishes the controller as an xHCI node
/// forwarding the BAR + DMA grants it received (`CAP_HW_EMIT`) — and nothing
/// more. It holds neither `CAP_MMIO_MAP` nor `CAP_MEM_DMA`: it forwards the
/// grants without mapping them (least privilege). Carries
/// `tairix_drv_bus_usb_vl805::BIND_KEYS`, so it autoloads against the VL805
/// PCI node the PCIe driver emitted.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_vl805_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-bus-usb-vl805",
        tairix_drv_bus_usb_vl805::REQUIRED_CAPABILITIES,
        tairix_drv_bus_usb_vl805::BIND_KEYS,
        profile,
    )
}

/// Build and sign the xHCI USB host-controller driver (HCD) bundle.
///
/// It maps the controller's register BAR (`CAP_MMIO_MAP`), carves its DMA
/// working set (`CAP_MEM_DMA`), binds the completion interrupt
/// (`CAP_IRQ_BIND`), creates the shared URB data buffer (`CAP_SHM`), binds the
/// restricted-sender URB transport endpoint (`CAP_IPC_BIND_PRIVILEGED`),
/// publishes the per-interface node (`CAP_HW_EMIT`), emits a one-shot
/// bring-up diagnostic (`CAP_LOG_EMIT`), and enters the strict-priority
/// real-time scheduling class (`CAP_SCHED_REALTIME`) so its
/// controller-interrupt report pump preempts CPU-bound work and cannot be
/// starved (`plans/USB.md`) — and nothing more. Carries
/// `tairix_drv_bus_usb::BIND_KEYS`, so it autoloads against the `usb,xhci`
/// node the VL805 driver emitted.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_xhci_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-bus-usb",
        tairix_drv_bus_usb::REQUIRED_CAPABILITIES,
        tairix_drv_bus_usb::BIND_KEYS,
        profile,
    )
}

/// Build and sign the USB HID **class**-driver bundle.
///
/// Its manifest is authored from the driver crate's own `REQUIRED_CAPABILITIES` and
/// `BIND_KEYS`, so the granted set is the set the program requests: inject
/// seat records, map its interface's shared buffer, call its interface's URB
/// endpoint, and log. It holds **no** MMIO, DMA, or IRQ authority, and
/// autoloads against every HID interface node a host-controller driver emits
/// (`plans/HID.md` H5).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_usb_hid_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-input-usb-hid",
        tairix_drv_input_usb_hid::REQUIRED_CAPABILITIES,
        tairix_drv_input_usb_hid::BIND_KEYS,
        profile,
    )
}

/// Build and sign the USB mass-storage **class**-driver bundle.
///
/// A pure BOT/SCSI class driver: it holds **no** MMIO, DMA, or IRQ
/// authority. Its manifest is authored from the driver crate's own
/// `REQUIRED_CAPABILITIES` and `BIND_KEYS`, so the granted set matches the set the
/// program requests and it autoloads against the mass-storage interface
/// node the HCD emitted (`plans/DEVICES.md` D2).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_usb_msd_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-storage-usb-msd",
        tairix_drv_storage_usb_msd::REQUIRED_CAPABILITIES,
        tairix_drv_storage_usb_msd::BIND_KEYS,
        profile,
    )
}

/// Build and sign the volume-manager **policy**-driver bundle.
///
/// A pure policy driver: it maps the shared data window its block driver
/// forwarded (`CAP_SHM`), issues blkio calls on its one granted
/// block-service endpoint (`CAP_IPC_ENDPOINT`), requests the audited
/// kernel attach of each recognised volume (`CAP_FS_MOUNT`), publishes the
/// array-member node for a device whose metadata says it belongs to a RAID
/// array (`CAP_HW_EMIT`), and emits diagnostics (`CAP_LOG_EMIT`) — and
/// nothing more. It holds **no** MMIO, DMA, or IRQ authority, and its node
/// emission can only republish transport the kernel already granted it.
/// Carries `tairix_drv_storage_volmgr::BIND_KEYS`, so it autoloads against the
/// per-LUN block-service storage node the mass-storage class driver
/// emitted (`plans/DEVICES.md` D3c).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_volmgr_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-storage-volmgr",
        tairix_drv_storage_volmgr::REQUIRED_CAPABILITIES,
        tairix_drv_storage_volmgr::BIND_KEYS,
        profile,
    )
}

/// Build and sign the RAID member-agent driver bundle.
///
/// Matched to an array-member node, it delegates its one granted
/// block-service endpoint (`CAP_IPC_ENDPOINT`) and its one granted data
/// window (`CAP_SHM`) to the array composer's reserved rendezvous, and emits
/// diagnostics (`CAP_LOG_EMIT`) — and nothing more. It never reads or writes
/// the device, holds no MMIO, DMA, IRQ, node-emission, or mount authority, and
/// can delegate only what it was granted. Carries
/// `tairix_drv_storage_raid_member::BIND_KEYS`, so it autoloads against the
/// `tairix,raid-member` node the volume manager emits for a device whose own
/// first block probed as array metadata (`plans/FIX-IO.md` `IO6c`), and stays
/// unbound on a machine with no array members.
///
/// This driver is deliberately its own bundle, separate from the sibling
/// array-composer driver crate (`drivers/storage/raid`): one signed bundle
/// grants its whole manifest's capability set to every instance loaded from
/// it, and one instance of this driver runs per member disk, so a shared
/// bundle would hand every per-disk agent the composer's
/// privileged-endpoint-bind and node-emit authority it has no need of.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_raid_member_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-storage-raid-member",
        tairix_drv_storage_raid_member::REQUIRED_CAPABILITIES,
        tairix_drv_storage_raid_member::BIND_KEYS,
        profile,
    )
}

/// Build and sign the RAID array-composer **policy**-driver bundle.
///
/// Matched to the kernel's synthetic virtual bus, one instance owns the
/// reserved rendezvous the per-disk member agents delegate to. It binds that
/// reserved endpoint id and each composed array's own block-service endpoint
/// and connects to each member (`CAP_IPC_ENDPOINT` plus
/// `CAP_IPC_BIND_PRIVILEGED`, which the reserved id requires so a squatter
/// cannot claim the rendezvous first and harvest the members' transports),
/// maps each offered data window and creates each array's own
/// (`CAP_SHM`), publishes the composed array as a `tairix,raid-array` storage
/// node (`CAP_HW_EMIT`) so the volume manager mounts its filesystems through
/// the unchanged path, and records its decisions (`CAP_LOG_EMIT`) — and
/// nothing more. It holds **no** MMIO, DMA, IRQ, or mount authority: it never
/// touches hardware directly and never mounts. Carries
/// `tairix_drv_storage_raid::BIND_KEYS`, so it autoloads against the one
/// synthetic virtual-bus node and needs no member present to start
/// (`plans/FIX-IO.md` `IO6d`).
///
/// It is deliberately a separate bundle from the sibling member agent: one
/// signed bundle grants its whole manifest's capability set to every instance
/// loaded from it, and the agent runs once per member disk, so sharing a
/// bundle would hand every per-disk agent this driver's privileged-bind and
/// node-emit authority it has no need of.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_raid_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-storage-raid",
        tairix_drv_storage_raid::REQUIRED_CAPABILITIES,
        tairix_drv_storage_raid::BIND_KEYS,
        profile,
    )
}

/// Build and sign the virtio-input keyboard/pointer driver bundle.
///
/// The QEMU `virt` sibling of the USB keyboard: it maps its granted register
/// window (`CAP_MMIO_MAP`), carves its virtqueue DMA slab (`CAP_MEM_DMA`),
/// parks on the device's interrupt line (`CAP_IRQ_BIND`), and injects decoded
/// key edges into the kernel input-focus arbiter (`CAP_INPUT_INJECT`) — and
/// nothing more. Carries `tairix_drv_input_virtio_input::BIND_KEYS`, so it
/// autoloads against a discovered virtio-input node (and stays unbound on the
/// Pi, whose tree carries none).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_virtio_kbd_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-input-virtio-kbd",
        tairix_drv_input_virtio_kbd::REQUIRED_CAPABILITIES,
        tairix_drv_input_virtio_input::BIND_KEYS,
        profile,
    )
}

/// Build and sign the framebuffer display-service bundle.
///
/// The zero-copy, lease-gated display half of the desktop present path
/// (`plans/DISPLAY.md` D7b/D7d): it maps its granted scan-out surface
/// (`CAP_MMIO_MAP` — the geometry rides the node's `Framebuffer` resource),
/// maps each session's granted frame region at `Configure` (`CAP_SHM`), binds
/// the reserved `DISPLAY_ENDPOINT` rendezvous (`CAP_IPC_BIND_PRIVILEGED`), and
/// emits its one-shot first-present record (`CAP_LOG_EMIT`) — and nothing more.
/// Every present is gated kernel-side on the caller's live seat lease
/// (`call_peer_seat`, no capability — the authority is serving the in-flight
/// call). Carries `tairix_drv_display_framebuffer::BIND_KEYS`, so it autoloads
/// against the boot display node the kernel publishes for its
/// platform-programmed scan-out surface (and stays unbound on a headless boot).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_framebuffer_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-display-framebuffer",
        tairix_drv_display_framebuffer::REQUIRED_CAPABILITIES,
        tairix_drv_display_framebuffer::BIND_KEYS,
        profile,
    )
}

/// Build and sign the virtio-gpu display bundle.
///
/// It reaches its device through the transport's windows (`CAP_MMIO_MAP`),
/// carves the frames and console the device reads under its node's DMA grant
/// (`CAP_MEM_DMA`), answers every command on the device interrupt
/// (`CAP_IRQ_BIND`), grants the frames to the presenter (`CAP_SHM`), binds the
/// reserved `DISPLAY_ENDPOINT` (`CAP_IPC_BIND_PRIVILEGED`) and records its
/// decisions (`CAP_LOG_EMIT`). Carries `tairix_drv_display_gpu_virtio::BIND_KEYS`,
/// so it autoloads against a discovered virtio GPU on either bus.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_virtio_gpu_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-display-gpu-virtio",
        tairix_drv_display_gpu_virtio::REQUIRED_CAPABILITIES,
        tairix_drv_display_gpu_virtio::BIND_KEYS,
        profile,
    )
}

/// Build and sign the BCM2711 display engine bundle.
///
/// It maps the HVS's and the pixel valves' windows (`CAP_MMIO_MAP`), carves
/// its scan-out frames (`CAP_MEM_DMA`), maps the client frames it presents
/// (`CAP_SHM`), calls its encoders and its core clock (`CAP_IPC_ENDPOINT`),
/// binds the seat's display endpoint (`CAP_IPC_BIND_PRIVILEGED`), tells the
/// firmware the display is its own (`CAP_MAILBOX`) and logs its decisions
/// (`CAP_LOG_EMIT`). Carries `tairix_drv_display_bcm2711_hvs::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_bcm2711_hvs_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-display-bcm2711-hvs",
        tairix_drv_display_bcm2711_hvs::REQUIRED_CAPABILITIES,
        tairix_drv_display_bcm2711_hvs::BIND_KEYS,
        profile,
    )
}

/// Build and sign the BCM2711 HDMI transmitter bundle.
///
/// It maps its block's windows (`CAP_MMIO_MAP`) and its DDC channel's and
/// audio DMA buffers (`CAP_SHM`), calls its clocks, its DDC bus, its interrupt
/// controller and its DMA channel (`CAP_IPC_ENDPOINT`), binds its encoder and
/// audio channel endpoints (`CAP_IPC_BIND_PRIVILEGED`), publishes the audio
/// channel's node (`CAP_HW_EMIT`) and logs its decisions (`CAP_LOG_EMIT`).
/// Carries `tairix_drv_display_bcm2711_hdmi::BIND_KEYS`, both transmitters'.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_bcm2711_hdmi_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-display-bcm2711-hdmi",
        tairix_drv_display_bcm2711_hdmi::REQUIRED_CAPABILITIES,
        tairix_drv_display_bcm2711_hdmi::BIND_KEYS,
        profile,
    )
}

/// Build and sign the virtio-net link-layer driver bundle.
///
/// The `-M virt` two-process netstack path's link driver: it maps its granted
/// register window (`CAP_MMIO_MAP`), carves its virtqueue DMA slab
/// (`CAP_MEM_DMA`), parks on the device interrupt its serve loop waits on
/// (`CAP_IRQ_BIND`), maps the shared frame region (`CAP_SHM`), claims and binds
/// the reserved device-channel endpoint (`CAP_IPC_ENDPOINT`,
/// `CAP_IPC_BIND_PRIVILEGED`), publishes its `netchan` node (`CAP_HW_EMIT`),
/// and emits its readiness beacon (`CAP_LOG_EMIT`) — and nothing more. Carries
/// `tairix_drv_network_virtio_net::BIND_KEYS`, so it autoloads against a
/// discovered virtio-net node (and stays unbound on a machine whose tree
/// carries none).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_virtio_net_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-network-virtio-net-driver",
        tairix_drv_network_virtio_net_driver::REQUIRED_CAPABILITIES,
        tairix_drv_network_virtio_net::BIND_KEYS,
        profile,
    )
}

/// Build and sign the virtio sound driver bundle.
///
/// It maps its granted register window (`CAP_MMIO_MAP`), carves its DMA
/// period buffers (`CAP_MEM_DMA`), parks on the device interrupt its serve
/// loop waits on (`CAP_IRQ_BIND`), maps the mixer's granted PCM regions
/// (`CAP_SHM`), claims and binds the reserved device-channel endpoint
/// (`CAP_IPC_ENDPOINT`, `CAP_IPC_BIND_PRIVILEGED`), publishes its
/// `audiochan` node (`CAP_HW_EMIT`), and emits its readiness beacon
/// (`CAP_LOG_EMIT`) — the same set the virtio-net bundle carries. It
/// deliberately does **not** hold `CAP_AUDIO_DEVICE`: that is the authority
/// to *command* an audio driver, which the mixer holds and this process is
/// the subject of. Carries `tairix_drv_audio_virtio_snd::BIND_KEYS`, so it
/// autoloads against a discovered virtio sound device on either bus (and
/// stays unbound on a machine that presents none).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_virtio_snd_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-audio-virtio-snd",
        tairix_drv_audio_virtio_snd::REQUIRED_CAPABILITIES,
        tairix_drv_audio_virtio_snd::BIND_KEYS,
        profile,
    )
}

/// Build and sign the USB Audio Class driver bundle.
///
/// It reaches its function only through the URB transport the host
/// controller serves: it maps its interface's shared buffer and its streams'
/// regions (`CAP_SHM`), calls its URB endpoint and binds its stream ports
/// (`CAP_IPC_ENDPOINT`), claims and binds the reserved device-channel endpoint
/// (`CAP_IPC_BIND_PRIVILEGED`), publishes its `audiochan` node (`CAP_HW_EMIT`),
/// and emits its diagnostics (`CAP_LOG_EMIT`) — no register window, DMA or
/// interrupt. Carries `tairix_drv_audio_usb_uac::BIND_KEYS`, so it autoloads
/// against any audio function's control interface a host controller
/// publishes.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_usb_uac_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-audio-usb-uac",
        tairix_drv_audio_usb_uac::REQUIRED_CAPABILITIES,
        tairix_drv_audio_usb_uac::BIND_KEYS,
        profile,
    )
}

/// Build and sign the HD Audio driver bundle.
///
/// It maps its controller's register window (`CAP_MMIO_MAP`), carves its
/// rings and stream buffers (`CAP_MEM_DMA`), parks on the controller's
/// interrupt (`CAP_IRQ_BIND`), maps the mixer's regions (`CAP_SHM`), claims
/// and binds the reserved device-channel endpoint (`CAP_IPC_ENDPOINT`,
/// `CAP_IPC_BIND_PRIVILEGED`), publishes its `audiochan` node
/// (`CAP_HW_EMIT`), and reports why it ends (`CAP_LOG_EMIT`). Carries
/// `tairix_drv_audio_hda::BIND_KEYS`, so it autoloads against any HD Audio
/// controller the kernel discovers.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_hda_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-audio-hda",
        tairix_drv_audio_hda::REQUIRED_CAPABILITIES,
        tairix_drv_audio_hda::BIND_KEYS,
        profile,
    )
}

/// Build and sign the Raspberry Pi 4 headphone-jack driver bundle.
///
/// It maps the jack's PWM block (`CAP_MMIO_MAP`), calls the DMA controller and
/// the clock manager its node's links name (`CAP_IPC_ENDPOINT`), maps the DMA
/// buffer and the mixer's ring (`CAP_SHM`), claims and binds the reserved
/// device-channel endpoint (`CAP_IPC_BIND_PRIVILEGED`), publishes its
/// `audiochan` node (`CAP_HW_EMIT`), and reports why it ends
/// (`CAP_LOG_EMIT`). Carries `tairix_drv_audio_bcm2711_pwm::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_pwm_audio_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-audio-bcm2711-pwm",
        tairix_drv_audio_bcm2711_pwm::REQUIRED_CAPABILITIES,
        tairix_drv_audio_bcm2711_pwm::BIND_KEYS,
        profile,
    )
}

/// Build and sign the BCM2711 PCM/I2S interface driver bundle.
///
/// It maps the block (`CAP_MMIO_MAP`), calls the DMA controller, the clock
/// manager and the codec its node's links name (`CAP_IPC_ENDPOINT`), maps the
/// DMA buffer and the mixer's ring (`CAP_SHM`), claims and binds the reserved
/// device-channel endpoint (`CAP_IPC_BIND_PRIVILEGED`), publishes its
/// `audiochan` node (`CAP_HW_EMIT`), and reports why it ends
/// (`CAP_LOG_EMIT`). Carries `tairix_drv_audio_bcm2711_i2s::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_i2s_audio_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-audio-bcm2711-i2s",
        tairix_drv_audio_bcm2711_i2s::REQUIRED_CAPABILITIES,
        tairix_drv_audio_bcm2711_i2s::BIND_KEYS,
        profile,
    )
}

/// Build and sign the PCM5102A codec driver bundle.
///
/// The part has no control port, so the driver needs only to bind its codec
/// endpoint (`CAP_IPC_BIND_PRIVILEGED`) and record its refusals
/// (`CAP_LOG_EMIT`). Carries `tairix_drv_audio_pcm5102a::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_pcm5102a_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-audio-pcm5102a",
        tairix_drv_audio_pcm5102a::REQUIRED_CAPABILITIES,
        tairix_drv_audio_pcm5102a::BIND_KEYS,
        profile,
    )
}

/// Build and sign the PCM5122 codec driver bundle.
///
/// Like the I2C clock chips it holds no register window: it calls the I2C
/// controller its node's link names (`CAP_IPC_ENDPOINT`) and maps the
/// channel buffer it is granted (`CAP_SHM`), binds its codec endpoint
/// (`CAP_IPC_BIND_PRIVILEGED`) and records its refusals (`CAP_LOG_EMIT`).
/// Carries `tairix_drv_audio_pcm5122::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_pcm5122_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-audio-pcm5122",
        tairix_drv_audio_pcm5122::REQUIRED_CAPABILITIES,
        tairix_drv_audio_pcm5122::BIND_KEYS,
        profile,
    )
}

/// Build and sign the GENET link-layer driver bundle.
///
/// The Raspberry Pi 4B's on-board gigabit NIC: it maps its granted register
/// window (`CAP_MMIO_MAP`), carves its frame buffers (`CAP_MEM_DMA`), parks on
/// the device interrupt its serve loop waits on (`CAP_IRQ_BIND`), maps the
/// shared frame region (`CAP_SHM`), claims and binds the reserved
/// device-channel endpoint (`CAP_IPC_ENDPOINT`, `CAP_IPC_BIND_PRIVILEGED`),
/// publishes its `netchan` node (`CAP_HW_EMIT`), and emits its readiness beacon
/// (`CAP_LOG_EMIT`) — the same set the virtio-net bundle carries, and nothing
/// more. Carries `tairix_drv_network_genet::BIND_KEYS`, so it autoloads against
/// a discovered `brcm,bcm2711-genet-v5` node (and stays unbound on a machine
/// whose tree carries none).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_genet_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-network-genet",
        tairix_drv_network_genet::REQUIRED_CAPABILITIES,
        tairix_drv_network_genet::BIND_KEYS,
        profile,
    )
}

/// Build and sign the PL031 real-time-clock driver bundle.
///
/// It maps its discovered counter window (`CAP_MMIO_MAP`) and binds the
/// well-known RTC service endpoint restricted-sender (`CAP_IPC_BIND_PRIVILEGED`)
/// — and nothing more. It holds **no** clock authority: the reading is served
/// to the one holder of `CAP_TIME_SET`, which tags its provenance itself, so
/// a clock chip can never assert its way past a network sync. Carries
/// `tairix_drv_rtc_pl031::BIND_KEYS`, so it autoloads against a discovered
/// `arm,pl031` node (and stays unbound on a board that has none).
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_pl031_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-rtc-pl031",
        tairix_drv_rtc_pl031::REQUIRED_CAPABILITIES,
        tairix_drv_rtc_pl031::BIND_KEYS,
        profile,
    )
}

/// Build and sign the Goldfish real-time-clock driver bundle.
///
/// The riscv64 `virt` board's clock: the same two capabilities and the same
/// no-clock-authority split as [`build_pl031_bundle`], differing only in the
/// chip it binds. Carries `tairix_drv_rtc_goldfish::BIND_KEYS`, so it
/// autoloads against a discovered `google,goldfish-rtc` node.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_goldfish_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-rtc-goldfish",
        tairix_drv_rtc_goldfish::REQUIRED_CAPABILITIES,
        tairix_drv_rtc_goldfish::BIND_KEYS,
        profile,
    )
}

/// Build and sign the MC146818 real-time-clock driver bundle.
///
/// The PC-compatible CMOS clock, reached over its granted `0x70`/`0x71` port
/// pair rather than a mapped window — so `CAP_MMIO_MAP` here gates the
/// bounded port-I/O trap instead, the same authority the node's port
/// resource already requires. Carries `tairix_drv_rtc_mc146818::BIND_KEYS`,
/// so it autoloads against the `motorola,mc146818` node the x86_64
/// legacy-fallback discovery path synthesises.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_mc146818_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-rtc-mc146818",
        tairix_drv_rtc_mc146818::REQUIRED_CAPABILITIES,
        tairix_drv_rtc_mc146818::BIND_KEYS,
        profile,
    )
}

/// Build and sign the Raspberry Pi real-time-clock driver bundle.
///
/// The Pi 5's clock lives in the board's power-management IC rather than any
/// MMIO space, so this is the one RTC bundle that requests no
/// `CAP_MMIO_MAP`: its only path to the hardware is a property exchange with
/// the `vcmailbox` service, which the kernel gates on `CAP_MAILBOX`. The same
/// no-clock-authority split as [`build_pl031_bundle`] otherwise. Carries
/// `tairix_drv_rtc_rpi::BIND_KEYS`, so it autoloads against a discovered
/// `raspberrypi,rpi-rtc` node and stays unbound on a Pi 3 or Pi 4, which have
/// none.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_rpi_rtc_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-rtc-rpi",
        tairix_drv_rtc_rpi::REQUIRED_CAPABILITIES,
        tairix_drv_rtc_rpi::BIND_KEYS,
        profile,
    )
}

/// Build and sign the Raspberry Pi firmware-clocks driver bundle.
///
/// Like [`build_rpi_rtc_bundle`] it requests no `CAP_MMIO_MAP` — the clocks
/// belong to the firmware, so its only path to them is a property exchange the
/// kernel gates on `CAP_MAILBOX` — and no `CAP_IRQ_BIND`, since it owns no
/// interrupt line. It holds `CAP_CPUFREQ` to take the machine's frequency
/// mechanism role for the ARM clock, and `CAP_IPC_BIND_PRIVILEGED` to serve the
/// node's `clock-v1` endpoint for the display's clocks. Carries
/// `tairix_drv_clock_rpi_firmware::BIND_KEYS`, so it autoloads against a
/// discovered `raspberrypi,firmware-clocks` node and stays unbound on a board
/// with none.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_rpi_firmware_clocks_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-clock-rpi-firmware",
        tairix_drv_clock_rpi_firmware::REQUIRED_CAPABILITIES,
        tairix_drv_clock_rpi_firmware::BIND_KEYS,
        profile,
    )
}

/// Build and sign the Broadcom Serial Controller I2C bus-driver bundle.
///
/// It owns the controller's register window (`CAP_MMIO_MAP`) and its
/// interrupt line (`CAP_IRQ_BIND`, which every transfer parks on rather than
/// spinning), serves `i2c-v1` under its node's duty
/// (`CAP_IPC_BIND_PRIVILEGED`), grants each channel its buffer (`CAP_SHM`) and
/// describes the VPU clock over its clock link (`CAP_IPC_ENDPOINT`). It holds
/// no clock authority: the chips above it report readings, and only `timed`
/// sets the machine clock. Carries `tairix_drv_bus_i2c_bcm2835::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_i2c_bcm2835_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-bus-i2c-bcm2835",
        tairix_drv_bus_i2c_bcm2835::REQUIRED_CAPABILITIES,
        tairix_drv_bus_i2c_bcm2835::BIND_KEYS,
        profile,
    )
}

/// Build and sign the BCM2711 HDMI DDC controller driver bundle.
///
/// It owns the controller's and the automatic I2C engine's windows
/// (`CAP_MMIO_MAP`), serves `i2c-v1` under its node's duty
/// (`CAP_IPC_BIND_PRIVILEGED`) and grants each channel its buffer
/// (`CAP_SHM`). It binds no interrupt: the tree wires none, so a command's
/// completion is read after sleeping its time on the wire. Carries
/// `tairix_drv_bus_i2c_brcmstb::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_i2c_brcmstb_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-bus-i2c-brcmstb",
        tairix_drv_bus_i2c_brcmstb::REQUIRED_CAPABILITIES,
        tairix_drv_bus_i2c_brcmstb::BIND_KEYS,
        profile,
    )
}

/// Build and sign the Broadcom legacy DMA engine driver bundle.
///
/// It is the one process that maps the engines' registers (`CAP_MMIO_MAP`) or
/// writes a control block, parks on each channel's line (`CAP_IRQ_BIND`),
/// binds the node's endpoint under its controller duty
/// (`CAP_IPC_BIND_PRIVILEGED`), and carves the chains and the buffers it hands
/// its consumers (`CAP_MEM_DMA`, `CAP_SHM`). Carries
/// `tairix_drv_dma_bcm2835::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_dma_bcm2835_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-dma-bcm2835",
        tairix_drv_dma_bcm2835::REQUIRED_CAPABILITIES,
        tairix_drv_dma_bcm2835::BIND_KEYS,
        profile,
    )
}

/// Build and sign the Broadcom second-level interrupt controller driver
/// bundle for installation into the image's `/System/Drivers/` store.
///
/// It maps the controller's registers (`CAP_MMIO_MAP`), parks on its own line
/// of the root controller (`CAP_IRQ_BIND`) and binds the node's endpoint under
/// its interrupt-line duty (`CAP_IPC_BIND_PRIVILEGED`). Carries
/// `tairix_drv_irq_brcmstb_l2::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_irq_brcmstb_l2_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-irq-brcmstb-l2",
        tairix_drv_irq_brcmstb_l2::REQUIRED_CAPABILITIES,
        tairix_drv_irq_brcmstb_l2::BIND_KEYS,
        profile,
    )
}

/// Build and sign the BCM2711 clock manager driver bundle.
///
/// It is the one process that maps the clock manager's registers
/// (`CAP_MMIO_MAP`) and binds the node's endpoint under its clock duty
/// (`CAP_IPC_BIND_PRIVILEGED`). Carries
/// `tairix_drv_clock_bcm2711_cprman::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_cprman_bcm2711_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-clock-bcm2711-cprman",
        tairix_drv_clock_bcm2711_cprman::REQUIRED_CAPABILITIES,
        tairix_drv_clock_bcm2711_cprman::BIND_KEYS,
        profile,
    )
}

/// Build and sign the BCM2711 DVP clock-gate driver bundle.
///
/// It is the one process that maps the DVP block's registers
/// (`CAP_MMIO_MAP`) and binds the node's endpoint under its clock duty
/// (`CAP_IPC_BIND_PRIVILEGED`). Carries
/// `tairix_drv_clock_bcm2711_dvp::BIND_KEYS`.
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_dvp_bcm2711_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-clock-bcm2711-dvp",
        tairix_drv_clock_bcm2711_dvp::REQUIRED_CAPABILITIES,
        tairix_drv_clock_bcm2711_dvp::BIND_KEYS,
        profile,
    )
}

/// Build and sign the DS3231 / DS1307 clock-chip driver bundle.
///
/// Like every I2C clock chip it requests no `CAP_MMIO_MAP`: it owns no
/// registers, only `CAP_IPC_ENDPOINT` to call the I2C controller its node's
/// link names, `CAP_SHM` to map the channel buffer it is granted, and
/// `CAP_IPC_BIND_PRIVILEGED` to serve the RTC endpoint `timed` reads. The same
/// no-clock-authority split as [`build_pl031_bundle`].
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_ds3231_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-rtc-ds3231",
        tairix_drv_rtc_ds3231::REQUIRED_CAPABILITIES,
        tairix_drv_rtc_ds3231::BIND_KEYS,
        profile,
    )
}

/// Build and sign the PCF8523 clock-chip driver bundle, on the same terms as
/// [`build_ds3231_bundle`].
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_pcf8523_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-rtc-pcf8523",
        tairix_drv_rtc_pcf8523::REQUIRED_CAPABILITIES,
        tairix_drv_rtc_pcf8523::BIND_KEYS,
        profile,
    )
}

/// Build and sign the PCF85063A clock-chip driver bundle, on the same terms
/// as [`build_ds3231_bundle`].
///
/// # Errors
///
/// As [`build_vcmailbox_bundle`].
pub fn build_pcf85063a_bundle(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    build_bundle(
        ctx,
        arch,
        "tairix-drv-rtc-pcf85063a",
        tairix_drv_rtc_pcf85063a::REQUIRED_CAPABILITIES,
        tairix_drv_rtc_pcf85063a::BIND_KEYS,
        profile,
    )
}

/// The composed, signed driver bundles the `-M virt` autoload verticals
/// plant into their whole-disk fixture's `/System/Drivers/` store, each
/// paired with its store path as an [`AppStoreFile`] the planter lays down:
/// the virtio-input keyboard driver, the framebuffer display service, and
/// the virtio-net link-layer driver.
///
/// Built **once per xtask process** and shared by every consumer (the
/// concurrent QEMU matrix and the long-CI flake hunt plant the identical
/// set), so the three cross-compiles are paid a single time; a build
/// failure is memoised too and returned to every caller (fail closed,
/// never a partial store). Concurrent first callers are serialised by
/// cargo's own build locking and one result wins.
///
/// # Errors
///
/// A string describing a failed cross-compile, ELF→rxe conversion, or a
/// structurally invalid composed bundle.
pub fn autoload_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            autoload_set(
                ctx,
                arch,
                profile,
                store_file(
                    FRAMEBUFFER_STORE_PATH,
                    build_framebuffer_bundle(ctx, arch, profile)?,
                ),
            )
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The [`autoload_driver_store_files`] set with the virtio-gpu driver as its
/// display, for the verticals whose board's only display is a virtio GPU.
/// Built once per xtask process and memoised.
///
/// # Errors
///
/// As [`autoload_driver_store_files`].
pub fn virtio_gpu_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            autoload_set(
                ctx,
                arch,
                profile,
                store_file(
                    VIRTIO_GPU_STORE_PATH,
                    build_virtio_gpu_bundle(ctx, arch, profile)?,
                ),
            )
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The keyboard, `display` and NIC drivers an autoload disk carries.
fn autoload_set(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
    display: AppStoreFile,
) -> Result<Vec<AppStoreFile>, String> {
    Ok(vec![
        store_file(
            VIRTIO_KBD_STORE_PATH,
            build_virtio_kbd_bundle(ctx, arch, profile)?,
        ),
        display,
        store_file(
            VIRTIO_NET_STORE_PATH,
            build_virtio_net_bundle(ctx, arch, profile)?,
        ),
    ])
}

/// The signed **virtio sound driver bundle alone**, paired with its store
/// path — the `/System/Drivers/` set the audio verticals plant. Those
/// verticals drive the guest over a text (UART) console, so they carry only
/// the sound driver: a display driver would take over console 0 and defeat
/// the serial-scripted login. Built once per xtask process and memoised.
///
/// # Errors
///
/// As [`build_virtio_snd_bundle`].
pub fn audio_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            Ok(vec![store_file(
                VIRTIO_SND_STORE_PATH,
                build_virtio_snd_bundle(ctx, arch, profile)?,
            )])
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The signed **HD Audio driver bundle alone**, paired with its store path:
/// the `/System/Drivers/` set the HD Audio verticals plant, with no display
/// or input driver, so the console stays the text console the serial script
/// drives. Built once per xtask process and memoised.
///
/// # Errors
///
/// As [`build_hda_bundle`].
pub fn hda_audio_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            Ok(vec![store_file(
                HDA_STORE_PATH,
                build_hda_bundle(ctx, arch, profile)?,
            )])
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The signed **USB audio driver pair** — the xHCI host-controller driver and
/// the USB Audio Class driver — each paired with its store path: the
/// `/System/Drivers/` set the USB audio verticals plant. As for the virtio
/// audio set, no display or input driver rides with them, so the console
/// stays the text console the serial script drives. Built once per xtask
/// process and memoised.
///
/// # Errors
///
/// As [`build_usb_uac_bundle`].
pub fn usb_audio_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            Ok(vec![
                store_file(USB_XHCI_STORE_PATH, build_xhci_bundle(ctx, arch, profile)?),
                store_file(
                    USB_UAC_STORE_PATH,
                    build_usb_uac_bundle(ctx, arch, profile)?,
                ),
            ])
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The signed **USB optical set** — the xHCI host-controller driver, the USB
/// mass-storage driver and the volume manager — each paired with its store
/// path: the `/System/Drivers/` set the USB optical vertical plants, from the
/// controller down to the volume a disc is attached as. No display or input
/// driver rides with them, so the console stays the text console the serial
/// script drives. Built once per xtask process and memoised.
///
/// # Errors
///
/// As [`build_volmgr_bundle`].
pub fn optical_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            Ok(vec![
                store_file(USB_XHCI_STORE_PATH, build_xhci_bundle(ctx, arch, profile)?),
                store_file(
                    USB_MSD_STORE_PATH,
                    build_usb_msd_bundle(ctx, arch, profile)?,
                ),
                store_file(VOLMGR_STORE_PATH, build_volmgr_bundle(ctx, arch, profile)?),
            ])
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The signed **virtio-net driver bundle alone**, paired with its store path
/// — the `/System/Drivers/` set the stream vertical (`plans/NETWORK.md` N5c)
/// plants. That vertical drives the network stack over a text (UART) console,
/// so it deliberately carries *only* the NIC driver, not the input/display
/// drivers [`autoload_driver_store_files`] plants: a display driver would take
/// over console 0 and defeat the serial-scripted login. Built once per xtask
/// process and memoised, like the full autoload set; the underlying
/// [`build_virtio_net_bundle`] is itself memoised, so this shares that work.
///
/// # Errors
///
/// As [`build_virtio_net_bundle`].
pub fn net_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            Ok(vec![store_file(
                VIRTIO_NET_STORE_PATH,
                build_virtio_net_bundle(ctx, arch, profile)?,
            )])
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The signed **clock-chip driver bundle alone**, paired with its store path
/// — the `/System/Drivers/` set the real-time-clock vertical
/// (`plans/TIMESYNC.md` TS-3) plants.
///
/// Which chip depends on the board: the PL031 on aarch64 `virt`, the
/// Goldfish RTC on riscv64 `virt`, the CMOS clock on a PC.
///
/// Deliberately only the RTC driver: a display driver would take over
/// console 0, and a NIC driver would give `timed` a network path, which
/// would make a clock the network could equally have established. The
/// vertical's whole claim is that the clock came from the chip before any
/// network existed.
///
/// Built once per xtask process and memoised, like the other store sets.
///
/// # Errors
///
/// As [`build_pl031_bundle`].
pub fn rtc_driver_store_files(
    ctx: &Context,
    arch: PieArch,
    profile: ImageProfile,
) -> Result<&'static [AppStoreFile], String> {
    static FILES: [OnceLock<Result<Vec<AppStoreFile>, String>>; MEMO_SLOTS] =
        [const { OnceLock::new() }; MEMO_SLOTS];
    FILES[memo_slot(arch, profile)]
        .get_or_init(|| {
            // Each board has its own clock chip, so the set is the one
            // driver that board could bind — never all of them.
            Ok(vec![match arch {
                PieArch::Aarch64 => {
                    store_file(PL031_STORE_PATH, build_pl031_bundle(ctx, arch, profile)?)
                }
                PieArch::Riscv64 => store_file(
                    GOLDFISH_STORE_PATH,
                    build_goldfish_bundle(ctx, arch, profile)?,
                ),
                PieArch::X86_64 => store_file(
                    MC146818_STORE_PATH,
                    build_mc146818_bundle(ctx, arch, profile)?,
                ),
            }])
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The admin alias the platform image's network configuration binds the
/// board's on-board gigabit NIC to.
const PLATFORM_WAN_ALIAS: &str = "wan";

/// The admin alias the platform image's network configuration binds the
/// virtio-net NIC of an emulated or virtualised boot to.
pub const PLATFORM_VIRT_ALIAS: &str = "vwan";

/// The MAC address the interactive QEMU session pins its virtio-net NIC to,
/// and the identity [`platform_network_conf`] binds [`PLATFORM_VIRT_ALIAS`]
/// by. Locally administered and unicast (bit 1 of the first octet set, bit 0
/// clear), in QEMU's own `52:54:00` range.
///
/// It is one definition because the two halves must agree exactly: the runner
/// creates the device with this MAC and the guest's shipped `network.conf`
/// finds the interface by it. A second copy would let the session boot with a
/// NIC no managed interface claims — the silent no-networking failure this
/// constant removes.
///
/// Deliberately *not* QEMU's own default first-NIC address
/// (`52:54:00:12:34:56`): were it that, a dropped `mac=` would still match by
/// accident and the pin would stop being load-bearing.
pub const VIRT_SESSION_NIC_MAC: &str = "52:54:00:00:00:01";

/// The per-interface network configuration the flashable Raspberry Pi image
/// ships: DHCPv4 plus IPv6 SLAAC on whichever of its two declared NICs the
/// machine actually presents.
///
/// Both interfaces are bound by **stable hardware identity**, which is
/// `plans/NETWORK.md` §6.1's rule and what `devmgr` requires — an `ethernet`
/// interface carrying neither `match.mac` nor `match.node` is refused rather
/// than guessed at:
///
/// * `wan` is the board's on-board gigabit NIC, bound by the GENET register
///   aperture the discovered node names, taken from the driver's own
///   [`tairix_drv_network_genet::GENET_REGS_CPU_BASE`] so the planted default
///   and the location the device manager resolves cannot drift.
/// * `vwan` is the virtio-net NIC an emulated or virtualised boot of this
///   same image presents (`cargo xtask run`), bound by the
///   [`VIRT_SESSION_NIC_MAC`] the runner pins on the device. A MAC rather
///   than a location because the virtio-mmio slot a NIC lands in depends on
///   how many other virtio devices the session attaches, while the MAC is
///   the runner's to fix.
///
/// Exactly one of the two ever binds: the machine that has a GENET has no
/// virtio-net NIC and vice versa. The device manager offers a configuration
/// only to a NIC it names, so the absent one costs nothing. Shipping both is
/// what lets this one image boot on the board and under emulation with
/// networking either way, rather than only on the board.
///
/// DHCPv4 + SLAAC is the addressing every desktop system defaults to, and it
/// is what makes the DHCP client (`plans/DHCP.md`) reachable without an
/// operator editing anything. `mkimage` re-parses the document through the
/// same `tairix_netconfig` engine `netstack` reads it with, so a malformed
/// default fails the image build rather than the boot, and plants it on the
/// read-only `/System` volume — the one place the device manager's pre-unlock
/// read resolves it.
#[must_use]
pub fn platform_network_conf() -> String {
    format!(
        "# TAIRiX Raspberry Pi network configuration.\n\
         # Two NICs, one per way this image boots; whichever the machine\n\
         # presents binds, the other is never configured. Both take DHCPv4\n\
         # with IPv6 stateless autoconfiguration -- the default every desktop\n\
         # system ships.\n\
         #\n\
         # The board's on-board gigabit NIC, by the stable bus location of\n\
         # its GENET register aperture.\n\
         {wan}.kind ethernet\n\
         {wan}.match.node {genet:#x}\n\
         {wan}.ipv4.method dhcp\n\
         {wan}.ipv6.method slaac\n\
         #\n\
         # The virtio-net NIC an emulated or virtualised boot presents, by\n\
         # the MAC the runner pins on it.\n\
         {virt}.kind ethernet\n\
         {virt}.match.mac {mac}\n\
         {virt}.ipv4.method dhcp\n\
         {virt}.ipv6.method slaac\n",
        wan = PLATFORM_WAN_ALIAS,
        virt = PLATFORM_VIRT_ALIAS,
        genet = tairix_drv_network_genet::GENET_REGS_CPU_BASE,
        mac = VIRT_SESSION_NIC_MAC,
    )
}

/// Pair a `/System`-volume-relative store path with a built bundle's bytes
/// as the [`AppStoreFile`] the planter accepts.
fn store_file(path: &[&[u8]], bytes: Vec<u8>) -> AppStoreFile {
    AppStoreFile {
        components: path.iter().map(|c| c.to_vec()).collect(),
        bytes,
    }
}

/// Fail-closed sanity check on a freshly composed bundle before it is planted
/// into the image (never ship a malformed store entry).
///
/// It re-decodes the bundle through the same `tairix_abi` definition the
/// kernel's store scan and load gate use, asserting it is a well-formed,
/// signed `kind = UserSpace` manifest carrying a non-empty payload — so a
/// broken cross-compile/sign step fails the image build loudly instead of
/// emitting an image whose driver the kernel would reject at boot. The
/// signature *verifies* against the kernel's embedded anchor by construction
/// (it is signed with the key set the kernel's build derives that anchor
/// from); the end-to-end signed-gate→spawn path is proven by the `-M virt`
/// autoload vertical.
///
/// # Errors
///
/// A string describing the structural defect found.
fn verify_signed_bundle(image: &[u8]) -> Result<(), String> {
    if image.len() <= DriverManifest::WIRE_LEN {
        return Err("image: composed driver bundle carries no payload".to_string());
    }
    let manifest = DriverManifest::from_bytes(image)
        .map_err(|e| format!("image: composed driver bundle's manifest does not decode: {e:?}"))?;
    if manifest.magic != DRIVER_MANIFEST_MAGIC {
        return Err("image: composed driver bundle has the wrong manifest magic".to_string());
    }
    if manifest.kind != DriverKind::UserSpace {
        return Err("image: composed driver bundle is not kind = UserSpace".to_string());
    }
    if manifest.signature == [0u8; 64] {
        return Err("image: composed driver bundle is unsigned".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The autoload-decision coverage for the shipped driver store: that
    //! each driver crate's own `BIND_KEYS` (the same table the autoload
    //! builders sign into its bundle) is discovered by the production store
    //! scan and resolved by the shared match policy to the intended node —
    //! and to no other. It also pins the shipped Raspberry Pi addressing
    //! default against the one `network.conf` engine that parses it. The bundles are signed here
    //! from a stub payload rather than a cross-compiled `rxe`: the scan and
    //! the match decode only the manifest and its bind table, never the
    //! program image, so a real payload adds nothing to what is exercised.
    use super::*;

    use tairix_abi::{DriverBindKey, Errno, HwMatchKey, SIMPLE_FRAMEBUFFER_COMPATIBLE};
    use tairix_devmatch::{resolve, MatchResolution};
    use tairix_drv_network_virtio_net::VIRTIO_NET_DEVICE_ID;
    use tairix_drvhost::store::scan_store;
    use tairix_drvhost::{DriverStore, ImageSource};
    // These tests assert through the resolved match, not the audit stream,
    // so the scan's records have nowhere to go.
    use tairix_log::DiscardSink;
    use tairix_virtio_input::VIRTIO_INPUT_DEVICE_ID;

    /// An arbitrary non-empty program image: the store scan and the match
    /// policy never look at the payload, only the signed manifest and bind
    /// table, so its exact bytes are irrelevant to these tests.
    const STUB_PAYLOAD: &[u8] = b"payload-stub";

    /// Any key: neither the store scan nor the match checks a signature.
    const STUB_SIGNING_SEED: [u8; 32] = [0x5a; 32];

    /// The `/`-joined string the store scanner addresses a bundle by,
    /// derived from its single store-path definition so the scan address and
    /// the plant path cannot drift.
    fn path_str(components: &[&[u8]]) -> String {
        let mut s = String::new();
        for c in components {
            s.push('/');
            s.push_str(core::str::from_utf8(c).expect("store path components are ASCII"));
        }
        s
    }

    /// Sign a `kind = UserSpace` bundle carrying `bind_keys` — the exact table
    /// the matching autoload builder signs into the shipped bundle.
    fn sign(bind_keys: &[DriverBindKey]) -> Vec<u8> {
        build_signed_driver_image(
            &STUB_SIGNING_SEED,
            DriverKind::UserSpace,
            &[],
            bind_keys,
            tairix_kernel_syscall::SYSCALL_TABLE_HASH,
            STUB_PAYLOAD,
        )
        .image
    }

    /// The signed bundles keyed by their store path, serving the production
    /// store scanner's [`ImageSource`] reads.
    struct BundleSource {
        kbd: (String, Vec<u8>),
        framebuffer: (String, Vec<u8>),
        network: (String, Vec<u8>),
        genet: (String, Vec<u8>),
        rtc: (String, Vec<u8>),
        engine: (String, Vec<u8>),
    }

    impl BundleSource {
        fn new() -> Self {
            Self {
                kbd: (
                    path_str(VIRTIO_KBD_STORE_PATH),
                    sign(tairix_drv_input_virtio_input::BIND_KEYS),
                ),
                framebuffer: (
                    path_str(FRAMEBUFFER_STORE_PATH),
                    sign(tairix_drv_display_framebuffer::BIND_KEYS),
                ),
                network: (
                    path_str(VIRTIO_NET_STORE_PATH),
                    sign(tairix_drv_network_virtio_net::BIND_KEYS),
                ),
                genet: (
                    path_str(GENET_STORE_PATH),
                    sign(tairix_drv_network_genet::BIND_KEYS),
                ),
                rtc: (
                    path_str(PL031_STORE_PATH),
                    sign(tairix_drv_rtc_pl031::BIND_KEYS),
                ),
                engine: (
                    path_str(BCM2711_HVS_STORE_PATH),
                    sign(tairix_drv_display_bcm2711_hvs::BIND_KEYS),
                ),
            }
        }

        /// The bundles in scan order, so a test's candidate indices and the
        /// scanner's agree by construction.
        fn all(&self) -> [&(String, Vec<u8>); 6] {
            [
                &self.kbd,
                &self.framebuffer,
                &self.network,
                &self.genet,
                &self.rtc,
                &self.engine,
            ]
        }
    }

    impl ImageSource for BundleSource {
        fn read(&self, path: &str, buf: &mut Vec<u8>) -> Result<(), Errno> {
            for (store_path, bytes) in self.all() {
                if store_path == path {
                    buf.extend_from_slice(bytes);
                    return Ok(());
                }
            }
            Err(Errno::NotFound)
        }
    }

    /// Scan the whole store, candidate indices pinned by scan order (input 0,
    /// display 1, virtio-net 2, GENET 3, PL031 4, display engine 5).
    fn scanned_store(source: &BundleSource) -> DriverStore {
        let paths: Vec<&str> = source.all().iter().map(|(p, _)| p.as_str()).collect();
        scan_store(source, &paths, &DiscardSink)
    }

    #[test]
    fn the_shipped_pi_network_default_parses_and_binds_both_nics_by_identity() {
        // The document the image plants is validated through the very engine
        // `netstack` reads it with, so a shipped default can never fail the
        // parser at boot.
        let config = tairix_netconfig::NetworkConfig::parse(&platform_network_conf())
            .expect("the shipped default parses");
        assert_eq!(
            config.interfaces().len(),
            2,
            "the board's NIC and the virtio-net NIC an emulated boot presents"
        );

        let wan = config
            .interface(PLATFORM_WAN_ALIAS)
            .expect("the board's NIC is declared");
        assert_eq!(wan.kind(), tairix_netconfig::IfaceKind::Ethernet);
        // Bound by the GENET aperture the driver itself declares, so the
        // planted default and the discovered location cannot drift.
        assert_eq!(
            wan.match_node,
            Some(tairix_drv_network_genet::GENET_REGS_CPU_BASE)
        );
        assert_eq!(wan.match_mac, None);

        let virt = config
            .interface(PLATFORM_VIRT_ALIAS)
            .expect("the emulated boot's NIC is declared");
        assert_eq!(virt.kind(), tairix_netconfig::IfaceKind::Ethernet);
        // Bound by the MAC the runner pins on the device it creates; the
        // interactive session and this document read the one constant, so a
        // session can never boot a NIC no managed interface claims.
        assert_eq!(
            virt.match_mac.map(|m| m.render()),
            Some(VIRT_SESSION_NIC_MAC.to_string())
        );
        assert_eq!(virt.match_node, None);

        for iface in config.interfaces() {
            assert_eq!(iface.ipv4_method(), tairix_netconfig::Ipv4Method::Dhcp);
            assert_eq!(iface.ipv6_method(), tairix_netconfig::Ipv6Method::Slaac);
            // DHCP/SLAAC form the addresses, so no static one is pinned.
            assert_eq!(iface.ipv4_address, None);
            assert_eq!(iface.ipv6_address, None);
            // `devmgr` maps each to a deliverable interface configuration
            // rather than refusing it for want of a hardware identity.
            assert!(iface.match_node.is_some() || iface.match_mac.is_some());
        }
    }

    #[test]
    fn each_autoload_bundle_is_a_signed_userspace_driver_manifest() {
        for bundle in [
            sign(tairix_drv_input_virtio_input::BIND_KEYS),
            sign(tairix_drv_display_framebuffer::BIND_KEYS),
            sign(tairix_drv_network_virtio_net::BIND_KEYS),
            sign(tairix_drv_network_genet::BIND_KEYS),
            sign(tairix_drv_rtc_pl031::BIND_KEYS),
            sign(tairix_drv_display_bcm2711_hvs::BIND_KEYS),
        ] {
            // The same fail-closed structural check the image build applies
            // to every planted bundle accepts each one.
            verify_signed_bundle(&bundle).expect("the signed bundle is well-formed");
        }
    }

    #[test]
    fn the_store_scan_discovers_the_bundles_and_each_binds_its_node() {
        // The production store scan decodes each bundle's bind table
        // fail-closed, and the shared match policy resolves a discovered
        // virtio-input node to the keyboard driver, a boot display node (the
        // kernel's `simple-framebuffer` publication) to the display service,
        // a virtio-net node to the virtio NIC driver, and a BCM2711 GENET
        // node to the Pi's on-board NIC driver — the exact autoload decisions
        // the booted kernel makes off the mounted root, with no
        // cross-binding.
        let source = BundleSource::new();
        let store = scanned_store(&source);
        let candidates = store.candidates();
        assert_eq!(candidates.len(), 6, "every signed bundle is a candidate");

        let input_keys = [HwMatchKey::virtio(VIRTIO_INPUT_DEVICE_ID)];
        match resolve(&input_keys, &candidates) {
            MatchResolution::Winner { candidate, .. } => assert_eq!(candidate, 0),
            other => panic!("the virtio-input node must bind the keyboard bundle, got {other:?}"),
        }

        let display_keys = [HwMatchKey::compatible(SIMPLE_FRAMEBUFFER_COMPATIBLE).expect("fits")];
        match resolve(&display_keys, &candidates) {
            MatchResolution::Winner { candidate, .. } => assert_eq!(candidate, 1),
            other => panic!("the boot display node must bind the display bundle, got {other:?}"),
        }

        let network_keys = [HwMatchKey::virtio(VIRTIO_NET_DEVICE_ID)];
        match resolve(&network_keys, &candidates) {
            MatchResolution::Winner { candidate, .. } => assert_eq!(candidate, 2),
            other => panic!("the virtio-net node must bind the network bundle, got {other:?}"),
        }

        let genet_keys =
            [HwMatchKey::compatible(tairix_drv_network_genet::GENET_COMPATIBLE).expect("fits")];
        match resolve(&genet_keys, &candidates) {
            MatchResolution::Winner { candidate, .. } => assert_eq!(candidate, 3),
            other => panic!("a GENET node must bind the GENET bundle, got {other:?}"),
        }

        let rtc_keys =
            [HwMatchKey::compatible(tairix_drv_rtc_pl031::PL031_COMPATIBLE).expect("fits")];
        match resolve(&rtc_keys, &candidates) {
            MatchResolution::Winner { candidate, .. } => assert_eq!(candidate, 4),
            other => panic!("a PL031 node must bind the clock-chip bundle, got {other:?}"),
        }
    }

    /// The engine's node binds the display engine, and a generic surface the
    /// generic service: the engine never takes a surface it does not scan.
    #[test]
    fn the_engine_s_node_binds_the_engine_and_a_generic_surface_the_generic_service() {
        let source = BundleSource::new();
        let store = scanned_store(&source);
        let candidates = store.candidates();
        match resolve(&[tairix_vcmailbox::DISPLAY_ENGINE_KEY], &candidates) {
            MatchResolution::Winner { candidate, .. } => assert_eq!(candidate, 5),
            other => panic!("the engine's node must bind the engine, got {other:?}"),
        }
        let generic = [HwMatchKey::compatible(SIMPLE_FRAMEBUFFER_COMPATIBLE).expect("fits")];
        match resolve(&generic, &candidates) {
            MatchResolution::Winner { candidate, .. } => assert_eq!(candidate, 1),
            other => panic!("a generic surface must bind the generic service, got {other:?}"),
        }
    }

    #[test]
    fn an_unrelated_node_binds_no_bundle() {
        // A node advertising a different virtio device id matches nothing —
        // each bundle binds only its declared device.
        let source = BundleSource::new();
        let store = scanned_store(&source);
        let candidates = store.candidates();
        let node_keys = [HwMatchKey::virtio(VIRTIO_INPUT_DEVICE_ID + 1)];
        assert_eq!(resolve(&node_keys, &candidates), MatchResolution::Unmatched);
    }

    /// Whether `text` names a driver's one declared capability list.
    fn declared(text: &str) -> bool {
        ["REQUIRED_CAPABILITIES", "SERVICE_CAPS"]
            .iter()
            .any(|name| text.contains(name))
    }

    /// The first argument of each call to `callee` in `source`.
    fn first_arguments<'s>(source: &'s str, callee: &str) -> Vec<&'s str> {
        source
            .match_indices(callee)
            .filter_map(|(at, _)| {
                let rest = &source[at + callee.len()..];
                rest.find([',', ')']).map(|end| rest[..end].trim())
            })
            .collect()
    }

    /// A driver's manifest and its binary's own re-check of its grants name
    /// one capability list, so the two cannot drift apart.
    #[test]
    fn every_bundle_and_its_binary_request_the_drivers_one_declared_list() {
        // Each builder passes the context, the architecture, the package and
        // then the capabilities.
        let calls: Vec<&str> = include_str!("image_drivers.rs")
            .split("build_bundle(\n        ctx,\n")
            .skip(1)
            .collect();
        assert!(!calls.is_empty(), "the builders are found");
        for call in calls {
            let caps = call.lines().nth(2).map(str::trim);
            assert!(
                caps.is_some_and(declared),
                "a bundle states its capabilities other than by its driver's list: {caps:?}"
            );
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("xtask sits two levels below the workspace root");
        let mut sources = Vec::new();
        rust_sources_below(&root.join("drivers"), &mut sources);
        assert!(!sources.is_empty(), "the driver tree is found");
        for path in sources {
            let source = std::fs::read_to_string(&path).expect("a readable source");
            for argument in [
                "RtDriverHost::from_grants_query(",
                "RtDriverHost::new(",
                "Node::open(",
                "tairix_clock::open(",
                "tairix_i2cbus::open(",
            ]
            .into_iter()
            .flat_map(|callee| first_arguments(&source, callee))
            {
                let bound = source
                    .split(&format!("let {argument} = "))
                    .nth(1)
                    .and_then(|rest| rest.split(';').next());
                assert!(
                    declared(argument) || bound.is_some_and(declared),
                    "{} builds its grants' capability set other than from its declared list",
                    path.display()
                );
            }
        }
    }

    fn rust_sources_below(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_sources_below(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
}
