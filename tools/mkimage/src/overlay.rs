//! The first-party device-tree overlays the image's `config.txt` applies,
//! written as the firmware's overlay loader reads them: each fragment targets
//! a label of the base tree, every phandle it names left unresolved and listed
//! under `__fixups__` for the loader to resolve against the base tree's
//! `__symbols__`.

use tairix_drv_audio_bcm2711_pwm::PWM_AUDIO_COMPATIBLE;
use tairix_drv_display_bcm2711_hvs::HVS_WINDOW;
use tairix_fdt::write::FdtWriter;

use crate::MkimageError;

/// The PWM-audio overlay's name, as `config.txt`'s `dtoverlay=` names it.
pub const PWM_AUDIO_OVERLAY: &str = "tairix-pwm-audio";

/// The PWM-audio overlay's path on the boot partition.
pub const PWM_AUDIO_OVERLAY_PATH: &str = "overlays/tairix-pwm-audio.dtbo";

/// GPIO 40 and 41, which carry nothing but the jack on a Pi 4, given their
/// first alternate function: PWM1's two channels.
pub const JACK_PINS: &str = "gpio=40,41=a0";

/// The display overlay's name, as `config.txt`'s `dtoverlay=` names it.
pub const DISPLAY_OVERLAY: &str = "tairix-display";

/// The display overlay's path on the boot partition.
pub const DISPLAY_OVERLAY_PATH: &str = "overlays/tairix-display.dtbo";

/// The base tree's labels for the nodes the native display engine binds,
/// which it leaves disabled: the engine, both HDMI blocks, their DDC buses,
/// the gates their audio is clocked through, and the interrupt controller
/// their hotplug lines reach.
const DISPLAY_NODES: [&str; 7] = ["hvs", "hdmi0", "hdmi1", "ddc0", "ddc1", "dvp", "aon_intr"];

/// The base tree's labels for the firmware's own display devices, its
/// framebuffer and its firmware-KMS planes. The firmware hands the display to
/// the ARM only while both are disabled, as Linux's `vc4-kms-v3d` leaves them;
/// otherwise it keeps driving the display and ignores the engine's notice.
const FIRMWARE_DISPLAYS: [&str; 2] = ["fb", "firmwarekms"];

/// Each display overlay fragment: the label it targets and the status it
/// gives that node.
fn display_fragments() -> impl Iterator<Item = (&'static str, &'static str)> {
    DISPLAY_NODES
        .into_iter()
        .map(|label| (label, "okay"))
        .chain(
            FIRMWARE_DISPLAYS
                .into_iter()
                .map(|label| (label, "disabled")),
        )
}

/// The engine's label: the base tree leaves its window unnamed.
const ENGINE: &str = "hvs";

/// The DDC buses' labels: the base tree omits the cell counts the I²C
/// controller binding states, by which a node is read as a bus.
const DDC_BUSES: [&str; 2] = ["ddc0", "ddc1"];

/// A phandle as an overlay states it until the loader resolves it.
const UNRESOLVED: u32 = 0xFFFF_FFFF;

/// The base tree's label for the PWM block the board wires to the jack.
const JACK_PWM: &str = "pwm1";

/// The base tree's label for the DMA controller the PWM blocks' request
/// lines reach.
const DMA: &str = "dma";

/// The DMA request line PWM1 raises.
const PWM1_DREQ: u32 = 1;

/// The PWM block's own binding, which the jack's keeps behind its own.
const PWM_COMPATIBLE: &str = "brcm,bcm2835-pwm";

/// The overlay that enables the jack's PWM block, names it for the
/// headphone-jack driver ahead of its own binding, and gives it the DMA
/// request line its node lacks.
///
/// # Errors
///
/// [`MkimageError::Overlay`] if the tree could not be written.
pub fn pwm_audio_overlay() -> Result<Vec<u8>, MkimageError> {
    let jack = core::str::from_utf8(PWM_AUDIO_COMPATIBLE)
        .map_err(|_| MkimageError::Overlay("the jack's binding is not text"))?;
    let mut tree = FdtWriter::new();
    tree.begin_node("");
    tree.prop_str("compatible", "brcm,bcm2711");
    tree.begin_node("fragment@0");
    tree.prop_u32("target", UNRESOLVED);
    tree.begin_node("__overlay__");
    tree.prop_strs("compatible", &[jack, PWM_COMPATIBLE]);
    tree.prop_cells("dmas", &[UNRESOLVED, PWM1_DREQ]);
    tree.prop_str("dma-names", "tx");
    tree.prop_str("status", "okay");
    tree.end_node();
    tree.end_node();
    tree.begin_node("__fixups__");
    tree.prop_str(JACK_PWM, "/fragment@0:target:0");
    tree.prop_str(DMA, "/fragment@0/__overlay__:dmas:0");
    tree.end_node();
    tree.end_node();
    tree.finish()
        .map_err(|_| MkimageError::Overlay("the PWM-audio overlay is not a well-formed tree"))
}

/// The overlay that enables the nodes the native display engine binds, names
/// the engine's window, gives the DDC buses their cell counts, and disables
/// the firmware's own display devices.
///
/// # Errors
///
/// [`MkimageError::Overlay`] if the tree could not be written.
pub fn display_overlay() -> Result<Vec<u8>, MkimageError> {
    let engine_window = core::str::from_utf8(HVS_WINDOW)
        .map_err(|_| MkimageError::Overlay("the engine's window name is not text"))?;
    let mut tree = FdtWriter::new();
    tree.begin_node("");
    tree.prop_str("compatible", "brcm,bcm2711");
    for (index, (label, status)) in display_fragments().enumerate() {
        tree.begin_node(&format!("fragment@{index}"));
        tree.prop_u32("target", UNRESOLVED);
        tree.begin_node("__overlay__");
        if label == ENGINE {
            tree.prop_str("reg-names", engine_window);
        }
        if DDC_BUSES.contains(&label) {
            tree.prop_u32("#address-cells", 1);
            tree.prop_u32("#size-cells", 0);
        }
        tree.prop_str("status", status);
        tree.end_node();
        tree.end_node();
    }
    tree.begin_node("__fixups__");
    for (index, (label, _)) in display_fragments().enumerate() {
        tree.prop_str(label, &format!("/fragment@{index}:target:0"));
    }
    tree.end_node();
    tree.end_node();
    tree.finish()
        .map_err(|_| MkimageError::Overlay("the display overlay is not a well-formed tree"))
}

#[cfg(test)]
mod tests {
    use tairix_fdt::Fdt;

    use super::{
        display_overlay, pwm_audio_overlay, DDC_BUSES, DISPLAY_NODES, ENGINE, FIRMWARE_DISPLAYS,
        PWM1_DREQ, UNRESOLVED,
    };

    fn cells(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }

    #[test]
    fn the_overlay_names_the_jacks_block_for_its_driver_with_its_request_line() {
        let blob = pwm_audio_overlay().expect("the overlay");
        let fdt = Fdt::new(&blob).expect("a valid blob");
        let node = |name: &[u8]| {
            fdt.nodes()
                .filter_map(Result::ok)
                .find(|node| node.name() == name)
                .expect("the node")
        };
        let value = |node: &tairix_fdt::Node<'_>, name: &str| {
            node.property(name)
                .map(|property| property.value().to_vec())
                .expect("the property")
        };
        let fragment = node(b"fragment@0");
        assert_eq!(value(&fragment, "target"), cells(&[UNRESOLVED]));
        let overlay = node(b"__overlay__");
        assert_eq!(
            value(&overlay, "compatible"),
            b"tairix,bcm2711-pwm-audio\0brcm,bcm2835-pwm\0".to_vec(),
            "the jack's binding first"
        );
        assert_eq!(value(&overlay, "dmas"), cells(&[UNRESOLVED, PWM1_DREQ]));
        assert_eq!(value(&overlay, "dma-names"), b"tx\0".to_vec());
        assert_eq!(value(&overlay, "status"), b"okay\0".to_vec());
        let fixups = node(b"__fixups__");
        assert_eq!(value(&fixups, "pwm1"), b"/fragment@0:target:0\0".to_vec());
        assert_eq!(
            value(&fixups, "dma"),
            b"/fragment@0/__overlay__:dmas:0\0".to_vec()
        );
    }

    #[test]
    fn the_display_overlay_enables_the_engine_and_disables_the_firmware_s_displays() {
        let blob = display_overlay().expect("the overlay");
        let fdt = Fdt::new(&blob).expect("a valid blob");
        let nodes: Vec<_> = fdt.nodes().filter_map(Result::ok).collect();
        let value = |node: &tairix_fdt::Node<'_>, name: &str| {
            node.property(name)
                .map(|property| property.value().to_vec())
        };
        let fixups = nodes
            .iter()
            .find(|node| node.name() == b"__fixups__")
            .expect("the fixups");
        let expected = DISPLAY_NODES
            .iter()
            .map(|&label| (label, &b"okay\0"[..]))
            .chain(
                FIRMWARE_DISPLAYS
                    .iter()
                    .map(|&label| (label, &b"disabled\0"[..])),
            );
        let mut fragments = 0;
        for (index, (label, status)) in expected.enumerate() {
            let fragment = nodes
                .iter()
                .position(|node| node.name() == format!("fragment@{index}").as_bytes())
                .expect("the fragment");
            assert_eq!(
                value(&nodes[fragment], "target"),
                Some(cells(&[UNRESOLVED]))
            );
            let overlay = &nodes[fragment + 1];
            assert_eq!(overlay.name(), b"__overlay__");
            assert_eq!(value(overlay, "status").as_deref(), Some(status), "{label}");
            let bus = DDC_BUSES.contains(&label);
            assert_eq!(value(overlay, "#address-cells").is_some(), bus, "{label}");
            assert_eq!(
                value(overlay, "#size-cells"),
                bus.then(|| cells(&[0])),
                "{label}"
            );
            let named = (label == ENGINE).then(|| b"hvs\0".to_vec());
            assert_eq!(value(overlay, "reg-names"), named, "{label}");
            assert_eq!(
                value(fixups, label),
                Some(format!("/fragment@{index}:target:0\0").into_bytes())
            );
            fragments += 1;
        }
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node.name().starts_with(b"fragment@"))
                .count(),
            fragments,
            "no fragment beyond those"
        );
    }

    /// Every label the overlay names is one the pinned base tree exports, on
    /// the node it means: a label the tree lacks would leave the loader
    /// refusing the whole overlay on the card. Skipped where the firmware
    /// cache is not populated.
    #[test]
    fn the_display_overlay_names_labels_the_pinned_tree_exports() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/pi-firmware/bcm2711-rpi-4-b.dtb"
        );
        let Ok(base) = std::fs::read(path) else {
            eprintln!("SKIP: no firmware DTB at {path}");
            return;
        };
        let fdt = Fdt::new(&base).expect("the pinned tree");
        let symbols = fdt
            .nodes()
            .filter_map(Result::ok)
            .find(|node| node.name() == b"__symbols__")
            .expect("the tree exports its labels");
        let enabled = [
            ("hvs", "brcm,bcm2711-hvs"),
            ("hdmi0", "brcm,bcm2711-hdmi0"),
            ("hdmi1", "brcm,bcm2711-hdmi1"),
            ("ddc0", "brcm,bcm2711-hdmi-i2c"),
            ("ddc1", "brcm,bcm2711-hdmi-i2c"),
            ("dvp", "brcm,brcm2711-dvp"),
            ("aon_intr", "brcm,bcm2711-l2-intc"),
        ];
        let disabled = [
            ("fb", "brcm,bcm2708-fb"),
            ("firmwarekms", "raspberrypi,rpi-firmware-kms-2711"),
        ];
        assert_eq!(enabled.map(|(label, _)| label), DISPLAY_NODES);
        assert_eq!(disabled.map(|(label, _)| label), FIRMWARE_DISPLAYS);
        for (label, compatible) in enabled.into_iter().chain(disabled) {
            let path = symbols
                .property(label)
                .map(|property| property.value().to_vec())
                .expect("the label is exported");
            let path = &path[..path.len() - 1];
            let mut ancestors: Vec<&[u8]> = Vec::new();
            let node = fdt.nodes().filter_map(Result::ok).find(|node| {
                let depth = node.depth() as usize;
                if depth == 0 {
                    return false;
                }
                ancestors.truncate(depth - 1);
                ancestors.push(node.name());
                let at: Vec<u8> = ancestors
                    .iter()
                    .flat_map(|name| core::iter::once(&b'/').chain(name.iter()))
                    .copied()
                    .collect();
                at == path
            });
            assert!(
                node.is_some_and(|node| node.is_compatible(compatible.as_bytes())),
                "{label} names a {compatible}"
            );
        }
    }
}
