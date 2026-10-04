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

/// One layer, `data/l.png`, on a canvas a pixel square, with `image` and
/// `layer` carrying further attributes.
fn lone_stack(image: &str, layer: &str) -> String {
    alloc::format!(
        r#"<image w="1" h="1"{image}><stack><layer src="data/l.png"{layer}/></stack></image>"#
    )
}

/// A canvas past the limits is refused before anything is decoded, by the
/// decode and by the estimate a caller admits the decode on alike.
#[test]
fn a_canvas_past_the_limits_is_refused_by_the_decode_and_its_estimate() {
    let ora = archive(
        r#"<image w="4294967295" h="4294967295"><stack/></image>"#,
        &[],
    );
    assert_eq!(
        super::peak_bytes(&ora, &limits()),
        Err(DecodeError::WidthExceedsLimit)
    );
    assert_eq!(
        decode_as(ImageFormat::OpenRaster, &ora, &limits()).err(),
        Some(DecodeError::WidthExceedsLimit)
    );
}

/// Layers naming one picture each take it whole, wherever and however
/// faintly each lays it.
#[test]
fn layers_naming_one_picture_each_take_it() {
    let shared = flat(2, 1, [10, 20, 30, 255]);
    let other = flat(1, 1, [1, 1, 1, 255]);
    let stack = r#"<image w="4" h="2"><stack>
<layer src="data/a.png" x="2" y="1" opacity="0.5"/>
<layer src="data/b.png"/>
<layer src="data/a.png" visibility="hidden"/>
</stack></image>"#;
    let a = crate::encode_png(&shared).expect("encodes");
    let b = crate::encode_png(&other).expect("encodes");
    let (read, unkept) = native(&archive(stack, &[("data/a.png", &a), ("data/b.png", &b)]));
    assert_eq!(unkept, crate::Unkept::default());
    let pictures: Vec<_> = read.layers.iter().map(|layer| &layer.picture).collect();
    assert_eq!(pictures, [&shared, &other, &shared], "the bottom first");
    assert!(!read.layers[0].visible);
    assert_eq!((read.layers[2].at, read.layers[2].opacity), ((2, 1), 128));
}

/// What a layer's PNG holds that a colour layer cannot keep is stated: its
/// sixteen-bit samples, a chunk beside its picture and its palette, and a
/// resolution the stack states.
#[test]
fn what_a_layer_cannot_keep_is_stated() {
    use crate::png_fixture::{build_png, chunk};
    let plain = crate::encode_png(&flat(1, 1, [1, 2, 3, 255])).expect("encodes");
    let deep = build_png(1, 1, 16, 6, 0, None, None, &[0, 1, 2, 3, 4, 5, 6, 255, 255]);
    let mut noted = deep.clone();
    let after_header = crate::PNG_SIGNATURE.len() + 25;
    let text = chunk(*b"tEXt", b"Comment\0kept elsewhere");
    noted.splice(after_header..after_header, text);
    let ink = Picture::indexed(
        1,
        1,
        crate::IndexDepth::One,
        vec![[9, 8, 7, 255]; 2],
        vec![1],
        None,
    )
    .expect("valid");
    let palette = crate::encode_png(&ink).expect("encodes");
    let held = |image: &str, png: &[u8]| {
        native(&archive(&lone_stack(image, ""), &[("data/l.png", png)])).1
    };
    assert_eq!(held("", &plain), crate::Unkept::default());
    let narrowed = held("", &deep);
    assert!(narrowed.precision && !narrowed.extras, "{narrowed:?}");
    assert!(held("", &noted).extras);
    let restated = held("", &palette);
    assert!(restated.converted && !restated.precision, "{restated:?}");
    assert!(held(r#" xres="300" yres="300""#, &plain).extras);
}

/// A stored merged picture of the canvas's size is shown, or refuses the
/// decode, before any layer is read: the estimate counts no layer for it.
#[test]
fn the_estimate_counts_no_layer_a_fitting_merged_picture_answers_for() {
    let mut document = document();
    let merged = flat(4, 3, [255, 0, 0, 255]);
    let thumbnail = flat(1, 1, [0; 4]);
    let few =
        super::peak_bytes(&written(&document, &merged, &thumbnail), &limits()).expect("estimated");
    document.layers.push(OraLayer {
        name: String::from("Wide"),
        picture: flat(512, 512, [1, 2, 3, 255]),
        at: (0, 0),
        opacity: 255,
        visible: true,
    });
    let ora = written(&document, &merged, &thumbnail);
    let more = super::peak_bytes(&ora, &limits()).expect("estimated");
    // The archive it holds grows by the layer's PNG; its pixels are not held.
    let decoded = 512 * 512 * 4;
    assert!(more < few + decoded, "{few} then {more}");
    let passed_over = written(&document, &flat(2, 2, [0; 4]), &thumbnail);
    let layered = super::peak_bytes(&passed_over, &limits()).expect("estimated");
    assert!(layered > few + decoded, "{layered}");
}

/// A layer is composed clipped to the canvas wherever it lies: over its left
/// and top edges, past its right and bottom, or wholly off it.
#[test]
fn a_layer_is_composed_clipped_to_the_canvas_wherever_it_lies() {
    let layer = |at: (i32, i32), colour: [u8; 4], opacity: u8| OraLayer {
        name: String::new(),
        picture: flat(2, 2, colour),
        at,
        opacity,
        visible: true,
    };
    let document = OraDocument {
        width: 3,
        height: 3,
        layers: vec![
            layer((-1, -1), [255, 0, 0, 255], 255),
            layer((2, 2), [0, 255, 0, 255], 255),
            layer((1, 0), [0, 0, 255, 255], 128),
            layer((3, 0), [9, 9, 9, 255], 255),
            layer((0, -2), [9, 9, 9, 255], 255),
            layer((-2, 1), [9, 9, 9, 255], 255),
        ],
    };
    let shown = composed(&document).expect("composes");
    let pixel = |x: usize, y: usize| {
        let at = (y * 3 + x) * 4;
        <[u8; 4]>::try_from(&shown.pixels()[at..at + 4]).expect("a pixel")
    };
    assert_eq!(pixel(0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(2, 2), [0, 255, 0, 255]);
    assert_eq!(pixel(1, 1), [0, 0, 255, 128], "half seen over clear");
    assert_eq!(pixel(2, 0), [0, 0, 255, 128]);
    for (x, y) in [(1, 2), (0, 2), (0, 1)] {
        assert_eq!(pixel(x, y), [0; 4], "({x}, {y}) is clear");
    }
}

/// A layer whose place or opacity will not read refuses the stack; an
/// opacity outside the whole is held to it.
#[test]
fn a_layer_whose_place_or_opacity_will_not_read_is_refused() {
    let png = crate::encode_png(&flat(1, 1, [1, 2, 3, 255])).expect("encodes");
    let opened = |layer: &str| {
        open_native(
            ImageFormat::OpenRaster,
            &archive(&lone_stack("", layer), &[("data/l.png", &png)])[..],
            &limits(),
        )
        .err()
    };
    for bad in [
        r#" opacity="half""#,
        r#" opacity="NaN""#,
        r#" opacity="""#,
        r#" x="1.5""#,
        r#" y="9999999999""#,
    ] {
        assert_eq!(opened(bad), Some(DecodeError::OraBadStack), "{bad}");
    }
    let opacity = |value: &str| {
        let ora = archive(
            &lone_stack("", &alloc::format!(r#" opacity="{value}""#)),
            &[("data/l.png", &png)],
        );
        native(&ora).0.layers[0].opacity
    };
    assert_eq!((opacity("2"), opacity("-1")), (255, 0));
}
