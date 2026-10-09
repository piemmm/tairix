use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::font_ipc::{FamilyEntry, FamilyKind};
use tairix_theme::{DesktopText, FamilyKey, Fonts, Theme};

use super::{resolve, ResolvedText, TextFamily, TextSize, TEXT_POINTS_MAX, TEXT_POINTS_MIN};

fn key(name: &str) -> FamilyKey {
    FamilyKey::new(name).expect("a well-formed key")
}

/// The shipped store: Inter, Inconsolata EX and Noto, with their line boxes.
fn store() -> Vec<FamilyEntry> {
    vec![
        FamilyEntry::new(key("inter"), "Inter", FamilyKind::Proportional, 1210).expect("entry"),
        FamilyEntry::new(
            FamilyKey::MONO,
            "Inconsolata EX",
            FamilyKind::Monospace,
            1110,
        )
        .expect("entry"),
        FamilyEntry::new(
            key("noto-sans"),
            "Noto Sans",
            FamilyKind::Proportional,
            1362,
        )
        .expect("entry"),
    ]
}

fn shipped() -> Fonts {
    *Theme::dark().fonts()
}

fn text(family: &str, size_px: u16) -> Option<DesktopText> {
    DesktopText::new(key(family), size_px).ok()
}

#[test]
fn both_values_spell_the_theme_and_a_choice() {
    assert_eq!(TextFamily::from_value("theme"), Some(TextFamily::Theme));
    assert_eq!(
        TextFamily::from_value("noto-serif"),
        Some(TextFamily::Named(key("noto-serif")))
    );
    assert_eq!(TextFamily::from_value("Noto Serif"), None);
    assert_eq!(TextFamily::from_value(""), None);
    assert_eq!(TextFamily::from_value("../fonts"), None);
    for family in [TextFamily::Theme, TextFamily::Named(key("inter"))] {
        assert_eq!(TextFamily::from_value(&family.render_value()), Some(family));
    }

    assert_eq!(TextSize::from_value("theme"), Some(TextSize::Theme));
    assert_eq!(TextSize::from_value("12"), Some(TextSize::Points(12)));
    for size in [
        TextSize::Theme,
        TextSize::Points(TEXT_POINTS_MIN),
        TextSize::Points(TEXT_POINTS_MAX),
    ] {
        assert_eq!(TextSize::from_value(&size.render_value()), Some(size));
    }
    // Outside the bounds, signed or spaced: refused rather than clamped.
    for refused in ["5", "49", "0", "-12", "+12", " 12", "12pt", "1e1", ""] {
        assert_eq!(TextSize::from_value(refused), None, "{refused:?}");
    }
}

#[test]
fn the_theme_own_text_lays_nothing_over() {
    let fonts = shipped();
    for (family, size) in [
        (TextFamily::Theme, TextSize::Theme),
        // Ten points of Inter is the shipped 16 px: still the theme's own.
        (TextFamily::Theme, TextSize::Points(10)),
        (TextFamily::Named(key("inter")), TextSize::Theme),
    ] {
        assert_eq!(
            resolve(family, size, &fonts, &store()),
            ResolvedText::default(),
            "{family:?} at {size:?}"
        );
    }
}

#[test]
fn a_size_in_points_keeps_its_em_across_families() {
    let fonts = shipped();
    // Eleven points of Inter is the 18 px body the desktop used to ship.
    assert_eq!(
        resolve(TextFamily::Theme, TextSize::Points(11), &fonts, &store()).text,
        text("inter", 18)
    );
    // The theme's ten points in Noto, whose line is taller for the same em.
    assert_eq!(
        resolve(
            TextFamily::Named(key("noto-sans")),
            TextSize::Theme,
            &fonts,
            &store()
        )
        .text,
        text("noto-sans", 18)
    );
    assert_eq!(
        resolve(
            TextFamily::Named(FamilyKey::MONO),
            TextSize::Points(12),
            &fonts,
            &store()
        )
        .text,
        text("mono", 18)
    );
}

#[test]
fn a_family_the_store_does_not_offer_falls_back_and_is_named() {
    let fonts = shipped();
    let gone = key("gone-family");
    let resolved = resolve(
        TextFamily::Named(gone),
        TextSize::Points(12),
        &fonts,
        &store(),
    );
    assert_eq!(resolved.unknown, Some(gone));
    assert_eq!(resolved.text, text("inter", 19), "the size still holds");
    let theme_size = resolve(TextFamily::Named(gone), TextSize::Theme, &fonts, &store());
    assert_eq!(
        theme_size,
        ResolvedText {
            text: None,
            unknown: Some(gone)
        }
    );
}

#[test]
fn with_no_store_listed_the_theme_draws_its_own() {
    let fonts = shipped();
    assert_eq!(
        resolve(TextFamily::Theme, TextSize::Points(14), &fonts, &[]),
        ResolvedText::default()
    );
    let resolved = resolve(
        TextFamily::Named(key("noto-sans")),
        TextSize::Points(14),
        &fonts,
        &[],
    );
    assert_eq!(resolved.text, None);
    assert_eq!(resolved.unknown, Some(key("noto-sans")));
}
