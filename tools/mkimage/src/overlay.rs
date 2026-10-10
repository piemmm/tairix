//! The first-party device-tree overlays the image's `config.txt` applies,
//! written as the firmware's overlay loader reads them: each fragment targets
//! a label of the base tree, every phandle it names left unresolved and listed
//! under `__fixups__` for the loader to resolve against the base tree's
//! `__symbols__`.

use tairix_drv_audio_bcm2711_pwm::PWM_AUDIO_COMPATIBLE;
use tairix_fdt::write::FdtWriter;

use crate::MkimageError;

/// The PWM-audio overlay's name, as `config.txt`'s `dtoverlay=` names it.
pub const PWM_AUDIO_OVERLAY: &str = "tairix-pwm-audio";

/// The PWM-audio overlay's path on the boot partition.
pub const PWM_AUDIO_OVERLAY_PATH: &str = "overlays/tairix-pwm-audio.dtbo";

/// GPIO 40 and 41, which carry nothing but the jack on a Pi 4, given their
/// first alternate function: PWM1's two channels.
pub const JACK_PINS: &str = "gpio=40,41=a0";

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

#[cfg(test)]
mod tests {
    use tairix_fdt::Fdt;

    use super::{pwm_audio_overlay, PWM1_DREQ, UNRESOLVED};

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
}
