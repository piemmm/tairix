use super::*;
use crate::fixture::scan;

fn files(font: &crate::build::Font, codes: Codes) -> FontData {
    let metrics = Metrics::read(&font.metrics()).expect("metrics");
    let outlines = match outlines::File::read(font.outlines()).expect("outlines") {
        outlines::File::Outlines(outlines) => outlines,
        outlines::File::Bitmaps(_) | outlines::File::Redirect(_) => panic!("a whole outlines file"),
    };
    assemble(
        codes,
        false,
        Glyphs::Files(Box::new(Files {
            metrics,
            outline_boxes: None,
            drawn: Drawn::Outlines(outlines),
        })),
        font.name.clone(),
        b"HostFS:$.Lib.Scan.".to_vec(),
    )
}

#[test]
fn a_guest_font_measures_by_its_metrics_and_is_as_big_as_its_chunks() {
    let data = files(&scan(), Codes::Glyph);
    assert_eq!(data.count, 128, "four chunks of 32 reach i");
    assert_eq!(
        data.bbox,
        metrics::Box {
            x0: -100,
            y0: -200,
            x1: 1101,
            y1: 901
        }
    );
    assert_eq!(data.characteristics, 0);
    assert_eq!(data.flags, 0x0C);
    let a = data.glyph(65).expect("A");
    assert_eq!(a.advance, (700, 0));
    assert_eq!(
        a.bbox,
        metrics::Box {
            x0: 50,
            y0: 0,
            x1: 651,
            y1: 701
        }
    );
    assert_eq!(data.kern(65, 66), (-100, 0));
    assert_eq!(data.kern(66, 65), (37, 0));
    assert_eq!(data.kern(66, 66), (0, 0));
    assert_eq!(data.kern(65, 1 << 20), (0, 0), "no pair past sixteen bits");
    assert!(data.glyph(90).is_none());
    assert!(data.bytes() > scan().outlines().len());
}

#[test]
fn codes_map_through_none_an_encoding_or_a_missing_base() {
    let glyph = files(&scan(), Codes::Glyph);
    assert_eq!(
        (glyph.code(65), glyph.code(127), glyph.code(128)),
        (Some(65), Some(127), None)
    );
    let base = EncodingFile::read(b"/.notdef /A /B".to_vec());
    let target = EncodingFile::read(b"/.notdef /B /A".to_vec());
    let mapped = files(
        &scan(),
        Codes::Mapped(Box::new(CodeMap::based(&target, &base))),
    );
    assert_eq!(
        (
            mapped.code(0),
            mapped.code(1),
            mapped.code(2),
            mapped.code(3)
        ),
        (None, Some(2), Some(1), None)
    );
    let missing = files(&scan(), Codes::NoBase(b"/Base9".to_vec()));
    assert_eq!(missing.code(65), None);
}

#[test]
fn a_face_is_a_rom_font_filled_non_zero_measured_by_its_outlines() {
    let mut relay = crate::build::square_faces;
    let desktop = tairix_riscos_kernel::desktop::DesktopFont::SHIPPED.family;
    let face = Face::load(
        b"Homerton.Medium",
        desktop,
        &mut relay,
        [32, 65, 66, 83].into_iter(),
    )
    .expect("a face");
    let data = assemble(
        Codes::Glyph,
        false,
        Glyphs::Face(face),
        b"Homerton.Medium".to_vec(),
        b"Resources:$.Fonts.Homerton.Medium.".to_vec(),
    );
    assert_eq!(
        data.characteristics,
        characteristic::ROM | characteristic::NON_ZERO
    );
    assert_eq!(
        data.count, 480,
        "base encoding 0's 455 glyphs in chunks of 32"
    );
    assert_eq!(data.flags, FACE_FLAGS);
    let misc = data.misc.expect("a face's measures");
    assert_eq!((misc.ascender, misc.descender), (800, -200));
    assert_eq!(data.glyph(65).map(|g| g.advance), Some((600, 0)));
    assert_eq!(data.kern(65, 66), (0, 0), "a face has no kern pairs");
    assert!(data.kerns().is_empty());
}

/// `font`'s metrics with no boxes: its flag set and its table taken out.
fn boxless(font: &crate::build::Font) -> Metrics {
    const FLAGS_AT: usize = 50;
    const BOXES_AT: usize = 52 + 256;
    let mut bytes = font.metrics();
    bytes[FLAGS_AT] |= crate::metrics::flag::NO_BOXES;
    let table = 4 * size_of::<i16>() * (font.characters.len() + 1);
    bytes.drain(BOXES_AT..BOXES_AT + table);
    Metrics::read(&bytes).expect("metrics")
}

#[test]
fn a_box_the_metrics_lack_is_its_outline_s_made_when_first_asked_for() {
    let font = crate::build::Font {
        kerns: Vec::new(),
        ..scan()
    };
    let metrics = boxless(&font);
    assert!(!metrics.has_boxes());
    let outlines = match outlines::File::read(font.outlines()).expect("outlines") {
        outlines::File::Outlines(outlines) => outlines,
        outlines::File::Bitmaps(_) | outlines::File::Redirect(_) => panic!("a whole outlines file"),
    };
    let cells = (0..outlines.slots()).map(|_| OnceCell::new()).collect();
    let data = assemble(
        Codes::Glyph,
        false,
        Glyphs::Files(Box::new(Files {
            metrics,
            drawn: Drawn::Outlines(outlines),
            outline_boxes: Some(cells),
        })),
        font.name.clone(),
        Vec::new(),
    );
    let made = |data: &FontData| match &data.glyphs {
        Glyphs::Files(files) => files
            .outline_boxes
            .iter()
            .flatten()
            .filter(|cell| cell.get().is_some())
            .count(),
        Glyphs::Face(_) => 0,
    };
    assert_eq!(made(&data), 0, "none made in loading it");
    let a = metrics::Box {
        x0: 50,
        y0: 0,
        x1: 651,
        y1: 701,
    };
    assert_eq!(data.glyph(65).map(|g| g.bbox), Some(a));
    assert_eq!(data.glyph(65).map(|g| g.bbox), Some(a));
    assert_eq!(made(&data), 1, "A's alone, once");
    assert_eq!(
        data.glyph(90).map(|g| g.bbox),
        None,
        "nothing the metrics do not measure"
    );
}
