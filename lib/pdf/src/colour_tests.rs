//! Colour spaces: the device spaces, the calibrated ones, and the special
//! spaces built on them.

use alloc::sync::Arc;

use super::{Space, Srgb};
use crate::document::Document;
use crate::fixture::{reference, with_objects, OPTIONS};
use crate::function::Work;
use crate::object::{Dict, Name, Object};
use crate::PdfError;

/// A colour converted with work to spare, as every test here wants.
trait Converted {
    fn colour_of(&self, components: &[f64]) -> Option<Srgb>;
}

impl Converted for Space {
    fn colour_of(&self, components: &[f64]) -> Option<Srgb> {
        self.srgb(components, &Work::new()).expect("work to spare")
    }
}

#[track_caller]
fn near(actual: Option<Srgb>, expected: Srgb, within: f64) {
    let actual = actual.expect("a colour");
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() <= within, "{actual:?} against {expected:?}");
    }
}

fn space(file: &[u8], num: u32) -> Result<Arc<Space>, PdfError> {
    let document = Document::open(file, &OPTIONS)?;
    Space::load(&document, &reference(num), None)
}

#[test]
fn device_colours_come_to_srgb() {
    near(Space::Gray.colour_of(&[0.5]), [0.5; 3], 0.0);
    near(
        Space::Rgb.colour_of(&[0.25, 0.5, 1.0]),
        [0.25, 0.5, 1.0],
        0.0,
    );
    near(Space::Rgb.colour_of(&[2.0, -1.0]), [1.0, 0.0, 0.0], 0.0);
    near(Space::Cmyk.colour_of(&[0.0, 0.0, 0.0, 1.0]), [0.0; 3], 0.0);
    near(
        Space::Cmyk.colour_of(&[1.0, 0.0, 0.0, 0.0]),
        [0.0, 1.0, 1.0],
        0.0,
    );
    near(Space::Cmyk.colour_of(&[0.0, 0.0, 0.0, 0.0]), [1.0; 3], 0.0);
}

#[test]
fn calibrated_white_is_white_whatever_its_white_point() {
    let file = with_objects(
        &[
            (10, "[/Lab <</WhitePoint [0.9505 1 1.089]>>]"),
            (
                11,
                "[/Lab <</WhitePoint [0.9642 1 0.8249] /Range [-128 127 -128 127]>>]",
            ),
            (
                12,
                "[/CalGray <</WhitePoint [0.9642 1 0.8249] /Gamma 2.2>>]",
            ),
            (13, "[/CalRGB <</WhitePoint [0.9505 1 1.089]>>]"),
        ],
        &[],
    );
    for num in [10, 11] {
        near(
            space(&file, num)
                .expect("a Lab space")
                .colour_of(&[100.0, 0.0, 0.0]),
            [1.0; 3],
            0.01,
        );
        near(
            space(&file, num)
                .expect("a Lab space")
                .colour_of(&[0.0, 0.0, 0.0]),
            [0.0; 3],
            0.01,
        );
    }
    near(
        space(&file, 12).expect("a CalGray space").colour_of(&[1.0]),
        [1.0; 3],
        0.01,
    );
    let red = space(&file, 13)
        .expect("a CalRGB space")
        .colour_of(&[1.0, 0.0, 0.0])
        .expect("a colour");
    assert!(red[0] > red[1] && red[0] > red[2], "{red:?}");
}

#[test]
fn a_calibrated_space_needs_a_white_point() {
    let file = with_objects(
        &[
            (10, "[/Lab <<>>]"),
            (11, "[/CalGray <</WhitePoint [1 2 1]>>]"),
        ],
        &[],
    );
    assert_eq!(space(&file, 10).err(), Some(PdfError::Malformed));
    assert_eq!(space(&file, 11).err(), Some(PdfError::Malformed));
}

#[test]
fn an_indexed_space_looks_its_colours_up() {
    let file = with_objects(
        &[
            (10, "[/Indexed /DeviceRGB 1 <ff0000 0000ff>]"),
            (11, "[/Indexed /DeviceRGB 1 12 0 R]"),
        ],
        &[(12, "", b"\x00\xff\x00\xff\xff\xff")],
    );
    let indexed = space(&file, 10).expect("an Indexed space");
    near(indexed.colour_of(&[0.0]), [1.0, 0.0, 0.0], 0.0);
    near(indexed.colour_of(&[1.0]), [0.0, 0.0, 1.0], 0.0);
    near(indexed.colour_of(&[9.0]), [0.0, 0.0, 1.0], 0.0);
    let streamed = space(&file, 11).expect("an Indexed space");
    near(streamed.colour_of(&[0.0]), [0.0, 1.0, 0.0], 0.0);
    assert_eq!(streamed.components(), 1);
}

#[test]
fn a_separation_tints_through_its_function_and_none_paints_nothing() {
    let file = with_objects(
        &[
            (10, "[/Separation /Spot /DeviceCMYK <</FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 1 0 0] /N 1>>]"),
            (11, "[/Separation /None /DeviceGray <</FunctionType 2 /Domain [0 1] /N 1>>]"),
            (12, "[/DeviceN [/None /None] /DeviceGray 13 0 R]"),
        ],
        &[(13, "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1]", b"{ pop }")],
    );
    let spot = space(&file, 10).expect("a Separation space");
    near(spot.colour_of(&[1.0]), [1.0, 0.0, 1.0], 0.0);
    near(spot.colour_of(&[0.0]), [1.0; 3], 0.0);
    assert_eq!(spot.initial().as_slice(), &[1.0]);
    assert_eq!(
        space(&file, 11)
            .expect("a Separation space")
            .colour_of(&[1.0]),
        None
    );
    assert_eq!(
        space(&file, 12)
            .expect("a DeviceN space")
            .colour_of(&[1.0, 1.0]),
        None
    );
}

#[test]
fn an_icc_space_reads_through_its_alternate_or_its_component_count() {
    let file = with_objects(
        &[(10, "[/ICCBased 12 0 R]"), (11, "[/ICCBased 13 0 R]")],
        &[
            (12, "/N 3", b"icc"),
            (13, "/N 4 /Alternate /DeviceGray", b"icc"),
        ],
    );
    assert!(matches!(
        *space(&file, 10).expect("an ICC space"),
        Space::Rgb
    ));
    assert!(matches!(
        *space(&file, 11).expect("an ICC space"),
        Space::Gray
    ));
}

#[test]
fn a_name_is_looked_up_in_the_resources() {
    let file = with_objects(&[], &[]);
    let document = Document::open(&file, &OPTIONS).expect("a document");
    let spaces = Dict::from_written(alloc::vec![(
        Name(b"CS0".to_vec()),
        Object::Name(Name(b"DeviceCMYK".to_vec()))
    )])
    .expect("room");
    let named = Space::load(
        &document,
        &Object::Name(Name(b"CS0".to_vec())),
        Some(&spaces),
    )
    .expect("a space");
    assert!(matches!(*named, Space::Cmyk));
    assert_eq!(named.initial().as_slice(), &[0.0, 0.0, 0.0, 1.0]);
    let missing = Space::load(
        &document,
        &Object::Name(Name(b"CS9".to_vec())),
        Some(&spaces),
    );
    assert_eq!(missing.err(), Some(PdfError::Malformed));
}

#[test]
fn a_space_that_names_itself_is_damage() {
    let file = with_objects(&[(10, "[/Indexed 10 0 R 1 <00>]")], &[]);
    assert_eq!(space(&file, 10).err(), Some(PdfError::Malformed));
}

#[test]
fn with_no_resources_only_a_device_space_may_be_named() {
    let file = with_objects(&[], &[]);
    let document = Document::open(&file, &OPTIONS).expect("a document");
    let device = Space::load(&document, &Object::Name(Name(b"DeviceRGB".to_vec())), None);
    assert!(matches!(device.as_deref(), Ok(Space::Rgb)));
    let named = Space::load(&document, &Object::Name(Name(b"CS0".to_vec())), None);
    assert_eq!(named.err(), Some(PdfError::Malformed));
}

#[test]
fn a_lab_range_that_runs_backwards_is_damage() {
    let file = with_objects(
        &[(
            10,
            "[/Lab <</WhitePoint [0.9505 1 1.089] /Range [100 -100 0 0]>>]",
        )],
        &[],
    );
    assert_eq!(space(&file, 10).err(), Some(PdfError::Malformed));
}
