use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{composed, encode_ora, OraDocument, OraLayer, OraLayerSource};
use crate::zip::Writer;
use crate::{
    decode_as, open_native, probe_as, sniff, DecodeError, DecodeLimits, ImageFormat,
    NativeDocument, Picture,
};

fn limits() -> DecodeLimits {
    DecodeLimits::new(4096, 4096, 4096 * 4096, 0)
}

fn flat(width: u32, height: u32, colour: [u8; 4]) -> Picture {
    let pixels = colour
        .iter()
        .copied()
        .cycle()
        .take((width * height * 4) as usize)
        .collect();
    Picture::rgba(width, height, pixels).expect("valid")
}

fn document() -> OraDocument {
    OraDocument {
        width: 4,
        height: 3,
        layers: vec![
            OraLayer {
                name: String::from("Back & <ground>"),
                picture: flat(4, 3, [255, 0, 0, 255]),
                at: (0, 0),
                opacity: 255,
                visible: true,
            },
            OraLayer {
                name: String::from("Top"),
                picture: flat(2, 2, [0, 0, 255, 255]),
                at: (1, 1),
                opacity: 128,
                visible: false,
            },
        ],
    }
}

/// `document` written as OpenRaster with `merged` and `thumbnail`.
fn written(document: &OraDocument, merged: &Picture, thumbnail: &Picture) -> Vec<u8> {
    let layers: Vec<OraLayerSource<'_>> = document
        .layers
        .iter()
        .map(|layer| OraLayerSource {
            name: &layer.name,
            picture: &layer.picture,
            at: layer.at,
            opacity: layer.opacity,
            visible: layer.visible,
        })
        .collect();
    encode_ora(
        (document.width, document.height),
        &layers,
        merged,
        thumbnail,
    )
    .expect("encodes")
}

fn native(bytes: &[u8]) -> (OraDocument, crate::Unkept) {
    match open_native(ImageFormat::OpenRaster, bytes, &limits()).expect("opens") {
        NativeDocument::Layers { document, unkept } => (document, unkept),
        _ => panic!("an OpenRaster file opens as its layers"),
    }
}

#[test]
fn a_document_reads_back_layer_for_layer() {
    let document = document();
    let merged = flat(4, 3, [255, 0, 0, 255]);
    let ora = written(&document, &merged, &flat(4, 3, [1, 2, 3, 255]));
    assert_eq!(sniff(&ora), Some(ImageFormat::OpenRaster));
    let (read, unkept) = native(&ora);
    assert_eq!(
        unkept,
        crate::Unkept::default(),
        "nothing beside what is written"
    );
    assert_eq!(read.layers.len(), 2);
    assert_eq!(
        read.layers[0].name, "Back & <ground>",
        "escaped and read back"
    );
    assert_eq!((read.width, read.height), (4, 3));
    assert_eq!(read.layers[1].at, (1, 1));
    assert!(!read.layers[1].visible);
    assert!(
        (127..=129).contains(&read.layers[1].opacity),
        "{}",
        read.layers[1].opacity
    );
    assert_eq!(read.layers[0].picture, document.layers[0].picture);
    let shown = decode_as(ImageFormat::OpenRaster, &ora, &limits()).expect("decodes");
    assert_eq!(
        &shown.pixels()[..4],
        &[255, 0, 0, 255],
        "the merged picture"
    );
    let info = probe_as(ImageFormat::OpenRaster, &ora).expect("probes");
    assert_eq!((info.width(), info.height()), (4, 3));
}

/// An archive of `stack` and `entries`, `mimetype` first.
fn archive(stack: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = Writer::new();
    zip.store("mimetype", b"image/openraster").expect("room");
    zip.store("stack.xml", stack.as_bytes()).expect("room");
    for (name, data) in entries {
        zip.store(name, data).expect("room");
    }
    zip.finish().expect("room")
}

#[test]
fn nested_stacks_fold_into_their_layers_and_say_what_folding_loses() {
    let png = crate::encode_png(&flat(1, 1, [9, 9, 9, 255])).expect("encodes");
    let stack = r#"<image w="1" h="1"><stack>
        <layer name="above" src="a.png"/>
        <stack opacity="0.5" visibility="hidden"><layer name="inside" src="a.png" opacity="0.5"/></stack>
    </stack></image>"#;
    let (read, unkept) = native(&archive(stack, &[("a.png", &png)]));
    assert_eq!(read.layers.len(), 2);
    let inside = &read.layers[0];
    assert_eq!(inside.name, "inside", "the bottom first");
    assert!(!inside.visible, "hidden with its group");
    assert!(
        (63..=65).contains(&inside.opacity),
        "as faint as both: {}",
        inside.opacity
    );
    assert!(
        unkept.extras,
        "a faded group is not kept exactly by folding"
    );
    let blended = r#"<image w="1" h="1"><stack><layer src="a.png" composite-op="svg:multiply"/></stack></image>"#;
    assert!(
        native(&archive(blended, &[("a.png", &png)])).1.extras,
        "a blend other than plain compositing"
    );
}

#[test]
fn a_damaged_or_foreign_document_is_refused() {
    let png = crate::encode_png(&flat(1, 1, [9, 9, 9, 255])).expect("encodes");
    let missing = archive(
        r#"<image w="1" h="1"><stack><layer src="nowhere.png"/></stack></image>"#,
        &[],
    );
    assert_eq!(
        open_native(ImageFormat::OpenRaster, &missing[..], &limits()).err(),
        Some(DecodeError::OraMissingLayer)
    );
    let mut zip = Writer::new();
    zip.store("mimetype", b"application/zip").expect("room");
    let foreign = zip.finish().expect("room");
    assert_eq!(
        decode_as(ImageFormat::OpenRaster, &foreign, &limits()).err(),
        Some(DecodeError::OraBadMimetype)
    );
    let unparsed = archive("<image w=\"1\"", &[("a.png", &png)]);
    assert_eq!(
        probe_as(ImageFormat::OpenRaster, &unparsed).err(),
        Some(DecodeError::OraBadStack)
    );
    let layers: String = (0..=super::MOST_LAYERS)
        .map(|_| "<layer src=\"a.png\"/>")
        .collect();
    let crowded = archive(
        &alloc::format!("<image w=\"1\" h=\"1\"><stack>{layers}</stack></image>"),
        &[("a.png", &png)],
    );
    assert_eq!(
        open_native(ImageFormat::OpenRaster, &crowded[..], &limits()).err(),
        Some(DecodeError::OraTooManyLayers)
    );
}

#[test]
fn without_a_merged_picture_the_visible_layers_are_composed() {
    let document = document();
    let shown = composed(&document).expect("composes");
    assert_eq!(&shown.pixels()[..4], &[255, 0, 0, 255]);
    // Pixel (1, 1), four across.
    let at = (4 + 1) * 4;
    assert_eq!(
        &shown.pixels()[at..at + 4],
        &[255, 0, 0, 255],
        "the hidden layer leaves it"
    );
}

#[test]
fn an_empty_stack_a_crowded_one_or_an_empty_canvas_is_not_written() {
    let picture = flat(2, 2, [9, 9, 9, 255]);
    let layer = OraLayerSource {
        name: "only",
        picture: &picture,
        at: (0, 0),
        opacity: 255,
        visible: true,
    };
    let refused =
        |canvas, layers: &[OraLayerSource<'_>]| encode_ora(canvas, layers, &picture, &picture);
    assert_eq!(refused((2, 2), &[]), Err(crate::EncodeError::LayerCount));
    assert_eq!(
        refused((2, 2), &vec![layer; super::MOST_LAYERS + 1]),
        Err(crate::EncodeError::LayerCount)
    );
    assert_eq!(refused((0, 2), &[layer]), Err(crate::EncodeError::TooLarge));
    assert!(
        refused((2, 2), &vec![layer; super::MOST_LAYERS]).is_ok(),
        "the most is written"
    );
}

#[test]
fn a_palette_layer_is_written_as_its_own_png_and_read_back_as_colour() {
    let ink = Picture::indexed(
        2,
        1,
        crate::IndexDepth::One,
        vec![[0, 0, 0, 255], [255, 255, 255, 255]],
        vec![0, 1],
        None,
    )
    .expect("valid");
    let layer = OraLayerSource {
        name: "ink",
        picture: &ink,
        at: (0, 0),
        opacity: 255,
        visible: true,
    };
    let ora = encode_ora((2, 1), &[layer], &ink, &ink).expect("encodes");
    let (read, _) = native(&ora);
    assert_eq!(
        read.layers[0].picture.pixels(),
        &crate::Pixels::Rgba(vec![0, 0, 0, 255, 255, 255, 255, 255])
    );
}

/// A merged picture of another size than the canvas is not what the stack
/// shows, so the layers are composed instead.
#[test]
fn a_merged_picture_that_does_not_fit_the_canvas_is_passed_over() {
    let document = document();
    let ora = written(&document, &flat(2, 2, [9, 9, 9, 255]), &flat(1, 1, [0; 4]));
    let shown = decode_as(ImageFormat::OpenRaster, &ora, &limits()).expect("decodes");
    assert_eq!((shown.width(), shown.height()), (4, 3), "the canvas");
    assert_eq!(
        &shown.pixels()[..4],
        &[255, 0, 0, 255],
        "the layers, composed"
    );
}
