//! The interpreter: paths and their painting, the graphics state, clips,
//! colour, shadings and patterns, forms, optional content and damage.

#![allow(
    clippy::float_cmp,
    reason = "positions, widths and opacities are compared exactly: each is the exact product of small whole numbers"
)]

use alloc::format;
use alloc::vec::Vec;

use tairix_bezier::offset::{Cap, Join};
use tairix_bezier::{FillRule, Point};
use tairix_colour::Rgba;

use crate::display::{DisplayList, GradientKind, Item, Lost, Paint};
use crate::document::Document;
use crate::fixture::{one_page, Builder, OPTIONS};
use crate::PdfError;

fn list(content: &str, resources: &str) -> DisplayList {
    try_list(content.as_bytes(), resources).expect("a display list")
}

fn try_list(content: &[u8], resources: &str) -> Result<DisplayList, PdfError> {
    let file = one_page(content, resources);
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS)?;
    let page = document.page(0)?;
    page.display_list()
}

fn colour(paint: &Paint) -> Rgba {
    match paint {
        Paint::Colour(colour) => *colour,
        Paint::Gradient(_) => panic!("a colour"),
    }
}

/// Where `point` of an item's own space lands on the page.
fn placed(item: &Item, point: Point) -> Point {
    match item {
        Item::Fill { transform, .. }
        | Item::Stroke { transform, .. }
        | Item::Clip { transform, .. } => transform.map(point),
        _ => panic!("an item with geometry"),
    }
}

#[test]
fn a_filled_rectangle_lands_y_down_from_the_top_left() {
    let painted = list("1 0 0 rg 10 10 20 30 re f", "<<>>");
    assert_eq!(painted.size, (200.0, 100.0));
    let [item] = painted.items.as_slice() else {
        panic!("one item: {:?}", painted.items);
    };
    let Item::Fill {
        path, rule, paint, ..
    } = item
    else {
        panic!("a fill");
    };
    assert_eq!(*rule, FillRule::NonZero);
    assert_eq!(colour(paint), Rgba::new(255, 0, 0, 255));
    assert_eq!(path.subpaths.len(), 1);
    assert!(path.subpaths[0].closed);
    assert_eq!(placed(item, Point::new(10.0, 10.0)), Point::new(10.0, 90.0));
    assert_eq!(placed(item, Point::new(30.0, 40.0)), Point::new(30.0, 60.0));
    assert!(painted.lost.is_empty());
}

#[test]
fn a_stroke_carries_its_pen() {
    let painted = list(
        "4 w 1 J 2 j 3 M [6 2] 1 d 0 0 1 RG 0 0 m 10 0 l 10 10 l S",
        "<<>>",
    );
    let [Item::Stroke {
        path, pen, paint, ..
    }] = painted.items.as_slice()
    else {
        panic!("one stroke: {:?}", painted.items);
    };
    assert_eq!(pen.style.width, 4.0);
    assert_eq!(
        (pen.style.start_cap, pen.style.end_cap),
        (Cap::Round, Cap::Round)
    );
    assert_eq!(pen.style.join, Join::Bevel);
    assert_eq!(pen.style.miter_limit, 3.0);
    assert_eq!(pen.dashes.as_slice(), &[6.0, 2.0]);
    assert_eq!(pen.dash_offset, 1.0);
    assert_eq!(colour(paint), Rgba::new(0, 0, 255, 255));
    assert!(!path.subpaths[0].closed);
    assert_eq!(path.subpaths[0].elements.len(), 2);
}

#[test]
fn closing_operators_close_and_fill_and_stroke_paint_both() {
    let painted = list(
        "0 0 m 10 0 l 10 10 l b 0 0 m 5 5 l B* 0 0 m 1 1 l s",
        "<<>>",
    );
    let kinds: Vec<&str> = painted
        .items
        .iter()
        .map(|item| match item {
            Item::Fill {
                rule: FillRule::NonZero,
                ..
            } => "fill",
            Item::Fill {
                rule: FillRule::EvenOdd,
                ..
            } => "even-odd fill",
            Item::Stroke { .. } => "stroke",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        ["fill", "stroke", "even-odd fill", "stroke", "stroke"]
    );
    let Item::Stroke { path, .. } = &painted.items[4] else {
        panic!("a stroke");
    };
    assert!(path.subpaths[0].closed);
}

#[test]
fn a_save_and_restore_bracket_the_state_and_its_clips() {
    let painted = list(
        "q 0.5 g 2 0 0 2 0 0 cm 0 0 10 10 re W n 0 0 5 5 re f Q 0 0 1 1 re f",
        "<<>>",
    );
    let [Item::Clip { rule, .. }, inner @ Item::Fill { paint: grey, .. }, Item::Unclip, outer @ Item::Fill { paint: black, .. }] =
        painted.items.as_slice()
    else {
        panic!("a clip, a fill, its end and a fill: {:?}", painted.items);
    };
    assert_eq!(*rule, FillRule::NonZero);
    assert_eq!(colour(grey), Rgba::new(128, 128, 128, 255));
    assert_eq!(colour(black), Rgba::new(0, 0, 0, 255));
    assert_eq!(placed(inner, Point::new(5.0, 5.0)), Point::new(10.0, 90.0));
    assert_eq!(placed(outer, Point::new(1.0, 1.0)), Point::new(1.0, 99.0));
}

#[test]
fn clips_left_open_are_closed_at_the_end_of_the_page() {
    let painted = list("q 0 0 10 10 re W* n 0 0 1 1 re W n 0 0 5 5 re f", "<<>>");
    let ends = painted
        .items
        .iter()
        .filter(|item| matches!(item, Item::Unclip))
        .count();
    let begins = painted
        .items
        .iter()
        .filter(|item| matches!(item, Item::Clip { .. }))
        .count();
    assert_eq!((begins, ends), (2, 2));
    assert!(matches!(painted.items.last(), Some(Item::Unclip)));
}

#[test]
fn colour_operators_set_their_spaces() {
    let painted = list(
        "0 0 0 1 k 0 0 1 1 re f 0.2 0.4 0.6 rg 0 0 1 1 re f /DeviceGray cs 0.25 sc 0 0 1 1 re f /CS0 cs 1 scn 0 0 1 1 re f",
        "<</ColorSpace <</CS0 [/Indexed /DeviceRGB 1 <000000 00ff00>]>>>>",
    );
    let colours: Vec<Rgba> = painted
        .items
        .iter()
        .map(|item| match item {
            Item::Fill { paint, .. } => colour(paint),
            _ => panic!("a fill"),
        })
        .collect();
    assert_eq!(
        colours,
        [
            Rgba::new(0, 0, 0, 255),
            Rgba::new(51, 102, 153, 255),
            Rgba::new(64, 64, 64, 255),
            Rgba::new(0, 255, 0, 255)
        ]
    );
}

#[test]
fn a_graphics_state_sets_opacity_and_the_pen() {
    let painted = list(
        "/G0 gs 1 0 0 rg 0 0 1 1 re f 0 0 m 1 1 l S",
        "<</ExtGState <</G0 <</ca 0.5 /CA 0.25 /LW 3 /LC 2 /LJ 1 /D [[2] 0]>>>>>>",
    );
    let [Item::Fill { paint: fill, .. }, Item::Stroke {
        paint: stroke, pen, ..
    }] = painted.items.as_slice()
    else {
        panic!("a fill and a stroke: {:?}", painted.items);
    };
    assert_eq!(colour(fill).a, 128);
    assert_eq!(colour(stroke).a, 64);
    assert_eq!(pen.style.width, 3.0);
    assert_eq!(pen.style.end_cap, Cap::Square);
    assert_eq!(pen.style.join, Join::Round);
    assert_eq!(pen.dashes.as_slice(), &[2.0]);
}

#[test]
fn a_blend_mode_is_recorded_as_lost() {
    let painted = list(
        "/G0 gs 0 0 1 1 re f",
        "<</ExtGState <</G0 <</BM /Multiply>>>>>>",
    );
    assert!(painted.lost.contains(Lost::BlendModes));
    assert_eq!(painted.items.len(), 1);
}

#[test]
fn a_shading_paints_the_whole_clip_as_a_gradient() {
    let painted = list(
        "q 0 0 50 50 re W n /S0 sh Q",
        "<</Shading <</S0 <</ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 50 0] /Function <</FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1>>>>>>>>",
    );
    let [Item::Clip { .. }, fill @ Item::Fill {
        paint: Paint::Gradient(gradient),
        path,
        ..
    }, Item::Unclip] = painted.items.as_slice()
    else {
        panic!("a clipped gradient fill: {:?}", painted.items);
    };
    assert!(matches!(gradient.kind, GradientKind::Axial { .. }));
    assert_eq!(gradient.stops.len(), 2);
    // The fill covers the whole page.
    let corners: Vec<Point> = core::iter::once(path.subpaths[0].start)
        .chain(
            path.subpaths[0]
                .elements
                .iter()
                .map(|element| element.end()),
        )
        .map(|corner| placed(fill, corner))
        .collect();
    for (corner, expected) in
        corners
            .iter()
            .zip([(0.0, 0.0), (200.0, 0.0), (200.0, 100.0), (0.0, 100.0)])
    {
        assert!(
            (corner.x - expected.0).abs() < 1e-9 && (corner.y - expected.1).abs() < 1e-9,
            "{corners:?}"
        );
    }
}

#[test]
fn a_shading_pattern_maps_from_the_pages_default_space() {
    let painted = list(
        "2 0 0 2 0 0 cm /Pattern cs /P0 scn 0 0 10 10 re f",
        "<</Pattern <</P0 <</PatternType 2 /Matrix [1 0 0 1 5 0] /Shading <</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function <</FunctionType 2 /Domain [0 1] /N 1>>>>>>>>>>",
    );
    let [Item::Fill {
        paint: Paint::Gradient(gradient),
        ..
    }] = painted.items.as_slice()
    else {
        panic!("a gradient fill: {:?}", painted.items);
    };
    // Pattern space's origin is (5, 0) in default space, (2.5, 0) in user space.
    let origin = gradient.transform.map(Point::new(0.0, 0.0));
    assert!(
        (origin.x - 2.5).abs() < 1e-9 && origin.y.abs() < 1e-9,
        "{origin:?}"
    );
}

#[test]
fn a_tiling_pattern_is_lost_and_an_uncoloured_one_keeps_its_colour() {
    let resources =
        "<</Pattern <</P0 <</PatternType 1 /PaintType 2>> /P1 <</PatternType 1 /PaintType 1>>>>>>";
    let painted = list(
        "[/Pattern /DeviceRGB] cs 0 1 0 /P0 scn 0 0 1 1 re f /Pattern cs /P1 scn 0 0 1 1 re f",
        resources,
    );
    assert!(painted.lost.contains(Lost::TilingPatterns));
    let [Item::Fill { paint, .. }] = painted.items.as_slice() else {
        panic!("one fill: {:?}", painted.items);
    };
    assert_eq!(colour(paint), Rgba::new(0, 255, 0, 255));
}

/// A one-page document whose resources name form `F0`, written as `form`.
fn with_form(content: &str, form_entries: &str, form_content: &str) -> DisplayList {
    formed("", content, form_entries, form_content)
}

/// As `with_form`, the page stating `page_entries` besides.
fn formed(
    page_entries: &str,
    content: &str,
    form_entries: &str,
    form_content: &str,
) -> DisplayList {
    let page = format!(
        "<</Type /Page /MediaBox [0 0 200 100] {page_entries} /Contents 4 0 R /Resources <</XObject <</F0 5 0 R>>>>>>"
    );
    let form = format!("/Type /XObject /Subtype /Form {form_entries}");
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, &page)
        .stream(4, "", content.as_bytes())
        .stream(5, &form, form_content.as_bytes());
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let page = document.page(0).expect("a page");
    page.display_list().expect("a display list")
}

#[test]
fn a_form_paints_in_its_own_space_clipped_to_its_box() {
    let painted = with_form(
        "/F0 Do",
        "/BBox [0 0 10 10] /Matrix [1 0 0 1 50 0]",
        "0 0 5 5 re f",
    );
    let [clip @ Item::Clip { .. }, fill @ Item::Fill { .. }, Item::Unclip] =
        painted.items.as_slice()
    else {
        panic!("a clipped fill: {:?}", painted.items);
    };
    assert_eq!(placed(clip, Point::new(0.0, 0.0)), Point::new(50.0, 100.0));
    assert_eq!(placed(fill, Point::new(5.0, 5.0)), Point::new(55.0, 95.0));
}

#[test]
fn a_transparency_group_is_drawn_as_one_at_the_fill_opacity() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</XObject <</F0 5 0 R>> /ExtGState <</G0 <</ca 0.5 /CA 0.5>>>>>>>>")
        .stream(4, "", b"/G0 gs /F0 Do")
        .stream(5, "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Group <</S /Transparency>>", b"0 0 5 5 re f");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    let [Item::Group { opacity }, Item::Clip { .. }, Item::Fill { paint, .. }, Item::Unclip, Item::Ungroup] =
        painted.items.as_slice()
    else {
        panic!("a group: {:?}", painted.items);
    };
    assert_eq!(*opacity, 0.5);
    assert_eq!(
        colour(paint).a,
        255,
        "the group's content is opaque within it"
    );
}

#[test]
fn a_form_that_paints_itself_is_damage_not_a_loop() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(
            3,
            "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources 6 0 R>>",
        )
        .stream(4, "", b"/F0 Do")
        .stream(
            5,
            "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 6 0 R",
            b"0 0 1 1 re f /F0 Do",
        )
        .object(6, "<</XObject <</F0 5 0 R>>>>");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    assert!(painted.lost.contains(Lost::Damaged));
    assert_eq!(
        painted
            .items
            .iter()
            .filter(|item| matches!(item, Item::Fill { .. }))
            .count(),
        1
    );
}

#[test]
fn hidden_optional_content_paints_nothing() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R /OCProperties <</OCGs [10 0 R 11 0 R] /D <</OFF [11 0 R]>>>>>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</Properties <</On 10 0 R /Off 11 0 R>>>>>>")
        .stream(4, "", b"/OC /On BDC 0 0 1 1 re f EMC /OC /Off BDC 0 0 2 2 re f /Tag BMC 0 0 3 3 re f EMC EMC 0 0 4 4 re f")
        .object(10, "<</Type /OCG>>")
        .object(11, "<</Type /OCG>>");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    let sizes: Vec<f64> = painted
        .items
        .iter()
        .map(|item| match item {
            Item::Fill { path, .. } => path.subpaths[0].elements[0].end().x,
            _ => panic!("a fill"),
        })
        .collect();
    assert_eq!(sizes, [1.0, 4.0]);
}

/// The steps a page with group 10 hidden takes to test what governs its
/// content: `properties` its resources' optional content names, membership
/// `9 0 R` naming group 10 two thousand times, painting `content`; and how
/// many fills it painted.
fn tested(properties: &str, content: &str) -> (u64, usize) {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R /OCProperties <</OCGs [10 0 R] /D <</OFF [10 0 R]>>>>>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, &format!("<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</Properties {properties}>>>>"))
        .stream(4, "", content.as_bytes())
        .object(9, &format!("[{}]", "10 0 R ".repeat(2000)))
        .object(10, "<</Type /OCG>>");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    (
        crate::limits::MAX_EVALUATION_STEPS - document.work().left(),
        painted.items.len(),
    )
}

#[test]
fn a_membership_named_in_resources_is_tested_once_however_often_it_governs() {
    let content = format!(
        "{}0 0 4 4 re f",
        "/OC /P0 BDC 0 0 1 1 re f EMC ".repeat(500)
    );
    assert_eq!(
        tested("<</P0 <</Type /OCMD /OCGs 9 0 R>>>>", &content),
        (2000, 1)
    );
}

#[test]
fn an_inline_membership_is_charged_each_time_it_is_tested() {
    // Content holds no references, so an inline list names no group and
    // hides nothing; testing it is still work.
    let content = format!(
        "{}0 0 4 4 re f",
        "/OC <</Type /OCMD /OCGs [1 2 3]>> BDC 0 0 1 1 re f EMC ".repeat(10)
    );
    assert_eq!(tested("<<>>", &content), (10 * 3, 11));
}

#[test]
fn damaged_content_is_passed_over_and_recorded() {
    let painted = list(") 1 0 0 rg 0 0 1 re 0 0 1 1 re f (unclosed", "<<>>");
    assert!(painted.lost.contains(Lost::Damaged));
    let [Item::Fill { paint, .. }] = painted.items.as_slice() else {
        panic!("one fill: {:?}", painted.items);
    };
    assert_eq!(colour(paint), Rgba::new(255, 0, 0, 255));
}

#[test]
fn unbalanced_saves_and_restores_are_absorbed() {
    let mut content = "q ".repeat(200);
    content.push_str("0 0 10 10 re W n Q Q Q 0 0 1 1 re f");
    let painted = list(&content, "<<>>");
    let begins = painted
        .items
        .iter()
        .filter(|item| matches!(item, Item::Clip { .. }))
        .count();
    let ends = painted
        .items
        .iter()
        .filter(|item| matches!(item, Item::Unclip))
        .count();
    assert_eq!(begins, ends);
    let painted = list("Q Q 0 0 1 1 re f", "<<>>");
    assert_eq!(painted.items.len(), 1);
}

#[test]
fn a_page_past_the_operator_bound_is_too_complex() {
    let room = crate::Options {
        password: None,
        budget: 1 << 26,
    };
    let run = |operators: usize| {
        let file = one_page("n\n".repeat(operators).as_bytes(), "<<>>");
        let document = Document::open(crate::fixture::kept(&file), &room).expect("a document");
        document
            .page(0)
            .expect("a page")
            .display_list()
            .map(|painted| painted.items.len())
    };
    assert_eq!(run(super::MAX_OPERATORS), Ok(0));
    assert_eq!(run(super::MAX_OPERATORS + 1), Err(PdfError::TooComplex));
}

#[test]
fn an_inline_image_counts_against_the_operator_bound() {
    let room = crate::Options {
        password: None,
        budget: 1 << 26,
    };
    let mut content = "n\n".repeat(super::MAX_OPERATORS);
    content.push_str("BI /W 1 /H 1 /IM true ID \x00 EI");
    let file = one_page(content.as_bytes(), "<<>>");
    let document = Document::open(crate::fixture::kept(&file), &room).expect("a document");
    let painted = document.page(0).expect("a page").display_list();
    assert_eq!(painted.err(), Some(PdfError::TooComplex));
}

#[test]
fn annotations_are_recorded_as_lost() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /Annots [<</Subtype /Link>>]>>");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    assert!(painted.lost.contains(Lost::Annotations));
}

#[test]
fn a_gradients_own_alpha_mask_becomes_its_stops_alpha() {
    let mut pdf = Builder::new();
    let function = "<</FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1>>";
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources 6 0 R>>")
        .stream(4, "", b"q /M0 gs /S0 sh Q")
        .stream(5, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group <</S /Transparency /CS /DeviceGray>> /Resources 6 0 R", b"/S1 sh")
        .object(
            6,
            &format!(
                "<</ExtGState <</M0 <</SMask <</Type /Mask /S /Luminosity /G 5 0 R>>>>>> /Shading <</S0 <</ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Extend [true true] /Function <</FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1>>>> /S1 <</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] /Extend [true true] /Function {function}>>>>>>"
            ),
        );
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    assert!(
        !painted.lost.contains(Lost::SoftMasks),
        "{:?}",
        painted.lost
    );
    let [Item::Fill {
        paint: Paint::Gradient(gradient),
        ..
    }] = painted.items.as_slice()
    else {
        panic!("a gradient fill: {:?}", painted.items);
    };
    let stops: Vec<[u8; 4]> = gradient
        .stops
        .iter()
        .map(|stop| stop.colour.to_array())
        .collect();
    assert_eq!(stops, [[255, 0, 0, 255], [0, 0, 255, 0]]);
}

#[test]
fn any_other_soft_mask_is_recorded_as_lost() {
    let painted = list(
        "/M0 gs 0 0 1 1 re f",
        "<</ExtGState <</M0 <</SMask <</S /Alpha /G 9 0 R>>>>>>>>",
    );
    assert!(painted.lost.contains(Lost::SoftMasks));
    assert_eq!(painted.items.len(), 1);
}

#[test]
fn a_name_means_what_its_own_resources_say() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</XObject <</F0 5 0 R>> /ColorSpace <</CS0 /DeviceRGB>> /ExtGState <</G0 <</ca 0.5>>>>>>>>")
        .stream(4, "", b"/CS0 cs 1 0 0 sc /G0 gs 0 0 1 1 re f /F0 Do /CS0 cs 1 0 0 sc /G0 gs 0 0 1 1 re f")
        .stream(5, "/Type /XObject /Subtype /Form /Resources <</ColorSpace <</CS0 /DeviceGray>> /ExtGState <</G0 <</ca 0.25>>>>>>", b"/CS0 cs 1 sc /G0 gs 0 0 1 1 re f");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    let colours: Vec<Rgba> = painted
        .items
        .iter()
        .map(|item| match item {
            Item::Fill { paint, .. } => colour(paint),
            _ => panic!("a fill: {:?}", painted.items),
        })
        .collect();
    assert_eq!(
        colours,
        [
            Rgba::new(255, 0, 0, 128),
            Rgba::new(255, 255, 255, 64),
            Rgba::new(255, 0, 0, 128)
        ]
    );
}

#[test]
fn a_soft_mask_set_again_masks_in_the_space_it_is_set_in() {
    let mut pdf = Builder::new();
    let function = "<</FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1>>";
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources 6 0 R>>")
        .stream(4, "", b"q /M0 gs /S0 sh Q q 2 0 0 2 0 0 cm /M0 gs /S0 sh Q")
        .stream(5, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group <</S /Transparency /CS /DeviceGray>> /Resources 6 0 R", b"/S1 sh")
        .object(
            6,
            &format!(
                "<</ExtGState <</M0 <</SMask <</Type /Mask /S /Luminosity /G 5 0 R>>>>>> /Shading <</S0 <</ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Extend [true true] /Function <</FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1>>>> /S1 <</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] /Extend [true true] /Function {function}>>>>>>"
            ),
        );
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    assert!(
        !painted.lost.contains(Lost::SoftMasks),
        "{:?}",
        painted.lost
    );
    let alphas: Vec<Vec<u8>> = painted
        .items
        .iter()
        .map(|item| match item {
            Item::Fill {
                paint: Paint::Gradient(gradient),
                ..
            } => gradient.stops.iter().map(|stop| stop.colour.a).collect(),
            _ => panic!("a gradient fill: {:?}", painted.items),
        })
        .collect();
    assert_eq!(alphas, [[255, 0], [255, 0]]);
}

#[test]
fn a_lab_space_whose_range_runs_backwards_is_passed_over() {
    let painted = list(
        "/L cs 1 sc 0 0 1 1 re f",
        "<</ColorSpace <</L [/Lab <</WhitePoint [0.9505 1 1.089] /Range [100 -100 0 0]>>]>>>>",
    );
    assert_eq!(painted.items.len(), 1);
    assert!(painted.lost.contains(Lost::Damaged));
}

#[test]
fn a_form_cannot_restore_what_its_caller_saved() {
    let painted = with_form(
        "q .5 0 0 .5 0 0 cm /F0 Do 0 0 10 10 re f Q",
        "/BBox [0 0 1 1]",
        "Q 0 0 500 500 re f",
    );
    let [Item::Clip { .. }, Item::Fill { .. }, Item::Unclip, after @ Item::Fill { .. }] =
        painted.items.as_slice()
    else {
        panic!(
            "the form's fill within its clip, then the page's: {:?}",
            painted.items
        );
    };
    assert_eq!(placed(after, Point::new(10.0, 10.0)), Point::new(5.0, 95.0));
}

#[test]
fn a_form_painted_past_the_deepest_save_still_keeps_its_state() {
    let content = alloc::format!("{}/F0 Do 0 0 1 1 re f", "q ".repeat(super::MAX_SAVES + 6));
    let painted = with_form(&content, "", "1 0 0 1 50 0 cm 0 0 1 1 re f");
    let [form @ Item::Fill { .. }, after @ Item::Fill { .. }] = painted.items.as_slice() else {
        panic!("the form's fill and the page's: {:?}", painted.items);
    };
    assert_eq!(placed(form, Point::new(0.0, 0.0)), Point::new(50.0, 100.0));
    assert_eq!(placed(after, Point::new(0.0, 0.0)), Point::new(0.0, 100.0));
}

/// A one-page document whose optional content group 10 is hidden, its page
/// naming it `/Off`, painting `content`, with form `F0` written as
/// `form_content`.
fn with_hidden_group(content: &str, form_content: &str) -> DisplayList {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R /OCProperties <</OCGs [10 0 R] /D <</OFF [10 0 R]>>>>>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</Properties <</Off 10 0 R>> /XObject <</F0 5 0 R>>>>>>")
        .stream(4, "", content.as_bytes())
        .stream(5, "/Type /XObject /Subtype /Form /Resources <</Properties <</Off 10 0 R>>>>", form_content.as_bytes())
        .object(10, "<</Type /OCG /Name (off)>>");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list")
}

#[test]
fn a_damaged_section_marker_still_ends_at_its_own_emc() {
    let painted = with_hidden_group("/OC /Off BDC 5 BDC EMC 0 0 100 100 re f EMC", "");
    assert!(painted.items.is_empty(), "{:?}", painted.items);
    assert!(painted.lost.contains(Lost::Damaged));
}

#[test]
fn a_section_a_form_leaves_open_ends_with_the_form() {
    let painted = with_hidden_group("/F0 Do 0 0 1 1 re f", "/OC /Off BDC 0 0 5 5 re f");
    let [Item::Fill { .. }] = painted.items.as_slice() else {
        panic!("only the page's fill: {:?}", painted.items);
    };
}

/// A page painting `content` with `/M0` a luminosity soft mask whose group
/// paints `/S1`, written as `mask_shading`, and `/S0` a red to blue shading.
fn masked_page(content: &[u8], mask_shading: &str, extra: &[(u32, &str, &[u8])]) -> DisplayList {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources 6 0 R>>")
        .stream(4, "", content)
        .stream(5, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group <</S /Transparency /CS /DeviceGray>> /Resources 6 0 R", b"/S1 sh")
        .object(
            6,
            &alloc::format!(
                "<</ExtGState <</M0 <</SMask <</Type /Mask /S /Luminosity /G 5 0 R>>>>>> /XObject <</F0 7 0 R>> /Shading <</S0 <</ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Extend [true true] /Function <</FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1>>>> /S1 {mask_shading}>>>>"
            ),
        );
    for &(num, entries, data) in extra {
        pdf.stream(num, entries, data);
    }
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list")
}

#[test]
fn a_soft_masks_alpha_is_its_luminosity() {
    let painted = masked_page(
        b"q /M0 gs /S0 sh Q",
        "<</ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Extend [true true] /Function <</FunctionType 2 /Domain [0 1] /C0 [0 1 0] /C1 [0 0 0] /N 1>>>>",
        &[],
    );
    let [Item::Fill {
        paint: Paint::Gradient(gradient),
        ..
    }] = painted.items.as_slice()
    else {
        panic!("a gradient fill: {:?}", painted.items);
    };
    let alphas: Vec<u8> = gradient.stops.iter().map(|stop| stop.colour.a).collect();
    assert_eq!(alphas, [150, 0]);
}

#[test]
fn a_transparency_group_painted_under_a_soft_mask_records_the_mask_lost() {
    let painted = masked_page(
        b"/M0 gs /F0 Do",
        "<</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] /Function <</FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1>>>>",
        &[(7, "/Type /XObject /Subtype /Form /Group <</S /Transparency>>", b"0 0 1 1 re f")],
    );
    assert!(painted.lost.contains(Lost::SoftMasks), "{:?}", painted.lost);
}

#[test]
fn a_gradients_stops_count_against_the_display_lists_bound() {
    let file = one_page(b"", "<<>>");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let page = document.page(0).expect("a page");
    let mut interpreter = super::Interpreter::new(&document, &page).expect("room");
    let stop = |offset: f64| crate::display::Stop {
        offset,
        colour: Rgba::new(0, 0, 0, 255),
    };
    let gradient = crate::display::Gradient {
        kind: GradientKind::Axial {
            start: Point::new(0.0, 0.0),
            end: Point::new(1.0, 0.0),
        },
        stops: tairix_collections::SharedSlice::try_from_vec(alloc::vec![
            stop(0.0),
            stop(0.5),
            stop(1.0)
        ])
        .expect("room"),
        extend: (false, false),
        transform: tairix_bezier::Affine::IDENTITY,
    };
    interpreter
        .emit(Item::Fill {
            path: super::rect_path(crate::page::Rect::spanning(0.0, 0.0, 1.0, 1.0)).expect("room"),
            rule: FillRule::NonZero,
            paint: Paint::Gradient(alloc::sync::Arc::new(gradient)),
            transform: tairix_bezier::Affine::IDENTITY,
        })
        .expect("room");
    assert_eq!(interpreter.points, 4 + 3);
}

/// Run `test` on an interpreter of a blank page and the empty resources its
/// content is read with, for what no page's content reaches directly.
fn interpreting(test: impl FnOnce(&mut super::Interpreter<'_>, super::Resources<'_>)) {
    let file = one_page(b"", "<<>>");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let page = document.page(0).expect("a page");
    let mut interpreter = super::Interpreter::new(&document, &page).expect("room");
    let categories = super::Categories::default();
    let resources = super::Resources {
        categories: &categories,
        scope: interpreter.scope().expect("room"),
    };
    test(&mut interpreter, resources);
}

#[test]
fn content_read_again_through_forms_is_held_to_the_content_bound() {
    // The page paints A 65 times and A paints F 64 times: F, white space and
    // no operator, is read 4160 times, past the bound in bytes and far within
    // it in operators.
    let page = "/A Do\n".repeat(65);
    let form = "/F Do\n".repeat(64);
    let blank = " ".repeat(1 << 16);
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(
            3,
            "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources 7 0 R>>",
        )
        .stream(4, "", page.as_bytes())
        .stream(
            5,
            "/Type /XObject /Subtype /Form /Resources 7 0 R",
            form.as_bytes(),
        )
        .stream(6, "/Type /XObject /Subtype /Form", blank.as_bytes())
        .object(7, "<</XObject <</A 5 0 R /F 6 0 R>>>>");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let painted = document.page(0).expect("a page").display_list();
    assert_eq!(painted.err(), Some(PdfError::TooComplex));
}

#[test]
fn a_clip_placed_past_what_a_number_holds_lets_nothing_through() {
    // At twice the page's size the clip's transform overflows, and the next
    // `cm` brings the fill beneath it back within range.
    let painted = formed(
        "/UserUnit 2",
        "1 0 0 1 1.7e308 0 cm -1.7e308 0 100 100 re W n 1 0 0 1 -1.7e308 0 cm 0 0 200 100 re f",
        "",
        "",
    );
    assert!(painted.items.is_empty(), "{:?}", painted.items);
    assert!(painted.lost.contains(Lost::Damaged));
}

#[test]
fn a_form_box_placed_past_what_a_number_holds_lets_nothing_through() {
    let painted = formed(
        "/UserUnit 2",
        "1 0 0 1 1.7e308 0 cm /F0 Do",
        "/BBox [0 0 10 10]",
        "1 0 0 1 -1.7e308 0 cm 0 0 200 100 re f",
    );
    assert!(painted.items.is_empty(), "{:?}", painted.items);
    assert!(painted.lost.contains(Lost::Damaged));
}

#[test]
fn a_clip_to_no_path_lets_nothing_through_until_it_is_restored() {
    let painted = list("q 5 5 m W n 0 0 10 10 re f Q 0 0 1 1 re f", "<<>>");
    let [Item::Fill { path, .. }] = painted.items.as_slice() else {
        panic!("only the fill after the restore: {:?}", painted.items);
    };
    assert_eq!(path.subpaths[0].elements[0].end(), Point::new(1.0, 0.0));
    assert!(painted.lost.is_empty(), "{:?}", painted.lost);
}

#[test]
fn what_is_painted_past_what_a_number_holds_is_recorded_as_damage() {
    let painted = list("1e308 0 0 1 0 0 cm 10 0 0 1 0 0 cm 0 0 1 1 re f", "<<>>");
    assert!(painted.items.is_empty(), "{:?}", painted.items);
    assert!(painted.lost.contains(Lost::Damaged));
}

#[test]
fn a_gradient_placed_past_what_a_number_holds_is_recorded_as_damage() {
    let painted = list(
        "0.1 0 0 1 0 0 cm /Pattern cs /P0 scn 0 0 1 1 re f",
        "<</Pattern <</P0 <</PatternType 2 /Matrix [1e308 0 0 1 0 0] /Shading <</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function <</FunctionType 2 /Domain [0 1] /N 1>>>>>>>>>>",
    );
    assert!(painted.items.is_empty(), "{:?}", painted.items);
    assert!(painted.lost.contains(Lost::Damaged));
}

#[test]
fn every_kind_of_item_counts_against_the_display_lists_bound() {
    interpreting(|interpreter, _| {
        let mut items = Vec::with_capacity(crate::limits::MAX_ITEMS);
        items.extend(core::iter::repeat_n(
            Item::Ungroup,
            crate::limits::MAX_ITEMS - 1,
        ));
        interpreter.items = items;
        assert_eq!(interpreter.emit(Item::Group { opacity: 1.0 }), Ok(()));
        assert_eq!(interpreter.emit(Item::Ungroup), Err(PdfError::TooComplex));
    });
}

#[test]
fn a_path_being_built_counts_against_the_display_lists_bound() {
    interpreting(|interpreter, resources| {
        let identity = tairix_bezier::Affine::IDENTITY;
        interpreter.points = super::MAX_POINTS - 3;
        assert_eq!(
            interpreter.run(b"0 0 m 1 1 l 2 2 l", resources, identity),
            Ok(())
        );
        assert_eq!(
            interpreter.run(b"3 3 l", resources, identity),
            Err(PdfError::TooComplex)
        );
    });
}

#[test]
fn an_inline_image_met_again_where_it_lies_is_not_read_there_again() {
    interpreting(|interpreter, resources| {
        let identity = tairix_bezier::Affine::IDENTITY;
        let image: &[u8] = b"BI /W 1 /H 1 /CS /G /BPC 8 ID \x80 EI";
        // What lies there the second time would not parse, so its picture can
        // only be the one read there first.
        let mut garbled = b"BI".to_vec();
        garbled.resize(image.len(), b')');
        interpreter.run(image, resources, identity).expect("a run");
        interpreter
            .run(&garbled, resources, identity)
            .expect("a run");
        let [Item::Image { picture: first, .. }, Item::Image { picture: again, .. }] =
            interpreter.items.as_slice()
        else {
            panic!(
                "two pictures: {:?} {:?}",
                interpreter.items, interpreter.lost
            );
        };
        assert!(alloc::sync::Arc::ptr_eq(first, again));
        assert!(interpreter.lost.is_empty(), "{:?}", interpreter.lost);
    });
}
