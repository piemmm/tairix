use super::*;
use crate::build::SquareFaces;

/// The family a desktop font is given in these tests.
const DESKTOP: FamilyKey = match FamilyKey::new("noto-sans") {
    Ok(key) => key,
    Err(_) => panic!("a family key"),
};

#[test]
fn a_rom_font_is_its_family_s_face_at_its_weight_and_slant() {
    let asking = Asking::of(b"Trinity.Bold.Italic", DESKTOP).expect("a ROM font");
    assert_eq!(asking.family.as_str(), "serif");
    assert_eq!(asking.weight.axis_value(), 700);
    assert_eq!(asking.style, FontStyle::Italic);
    assert_eq!(
        Asking::of(b"Homerton.Medium.Oblique", DESKTOP)
            .expect("ROM")
            .style,
        FontStyle::Oblique
    );
    assert!(!Face::exists(b"Selwyn"));
    assert!(Face::exists(b"Corpus.Bold"));
}

#[test]
fn the_desktop_font_is_the_family_the_session_gives_at_its_weight_and_slant() {
    let asking = Asking::of(b"desktop.Bold.Oblique", DESKTOP).expect("the desktop font");
    assert_eq!(asking.family, DESKTOP);
    assert_eq!(asking.weight.axis_value(), 700);
    assert_eq!(
        asking.style,
        FontStyle::Oblique,
        "a sans-serif interface face leans"
    );
    assert_eq!(
        Asking::of(b"Desktop.Medium", DESKTOP)
            .expect("medium")
            .style,
        FontStyle::Normal
    );
    assert!(Face::exists(b"Desktop.Medium"));
    assert!(
        !Face::exists(b"Desktops.Medium"),
        "the family, not a prefix of it"
    );
    let mut service = SquareFaces::capped(FONT_MAX_GLYPH_RUN);
    Face::load(
        b"Desktop.Medium",
        DESKTOP,
        &mut |q, r| service.answer(q, r),
        65..67,
    )
    .expect("answered");
    assert_eq!(
        service.families,
        [DESKTOP],
        "asked of the font service by the session's family"
    );
}

#[test]
fn every_glyph_wanted_is_fetched_once_a_run_at_a_time_in_thousandths_of_an_em() {
    let mut service = SquareFaces::capped(FONT_MAX_GLYPH_RUN);
    let wanted = (32..120).chain(32..40).chain([0, 999]);
    let face = Face::load(
        b"Homerton.Medium",
        DESKTOP,
        &mut |q, r| service.answer(q, r),
        wanted,
    )
    .expect("answered");
    assert_eq!(service.requests, 88usize.div_ceil(FONT_MAX_GLYPH_RUN));
    assert_eq!(
        service.asked.len(),
        88,
        "each glyph asked once; glyph 0 and one past the base, never"
    );
    let a = face.shape(65).expect("A");
    assert_eq!(a.advance, 600);
    assert_eq!(
        a.bbox,
        metrics::Box {
            x0: 0,
            y0: 0,
            x1: 500,
            y1: 700
        }
    );
    assert!(face.shape(0).is_none() && face.shape(200).is_none());
    assert_eq!(face.extent(), (800, -200));
    assert_eq!(
        face.bbox(),
        metrics::Box {
            x0: 0,
            y0: 0,
            x1: 500,
            y1: 700
        }
    );
    assert!(face.bytes() > 88 * size_of::<Shape>());
}

#[test]
fn a_service_answering_a_prefix_is_asked_for_the_rest() {
    let mut service = SquareFaces::capped(5);
    let face = Face::load(
        b"Corpus.Medium",
        DESKTOP,
        &mut |q, r| service.answer(q, r),
        32..64,
    )
    .expect("answered");
    assert_eq!(service.requests, 7, "five a time for 32 glyphs");
    assert!((32..64).all(|g| face.shape(g).is_some()));
}

#[test]
fn a_service_that_does_not_answer_fails_the_load() {
    assert_eq!(
        Face::load(
            b"Corpus.Medium",
            DESKTOP,
            &mut |_: &[u8], _: &mut Vec<u8>| false,
            32..40
        )
        .map(|_| ()),
        Err(Unanswered)
    );
    let mut garbage = |_: &[u8], reply: &mut Vec<u8>| {
        reply.extend_from_slice(b"not a reply");
        true
    };
    assert_eq!(
        Face::load(b"Corpus.Medium", DESKTOP, &mut garbage, 32..40).map(|_| ()),
        Err(Unanswered)
    );
    assert_eq!(
        Face::load(b"Selwyn", DESKTOP, &mut garbage, 32..40).map(|_| ()),
        Err(Unanswered)
    );
}
