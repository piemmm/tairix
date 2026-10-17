use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::window_ipc::AppMenuSymbol;

use super::menu_symbol_icon;
use crate::artwork::glyph_mask;
use crate::glyph::IconKind;

/// The sides a menu row draws its picture at, from the reference density up
/// past double.
const MENU_SIDES: [u32; 3] = [16, 19, 38];

/// `kind`'s coverage at `side`, one byte a pixel.
fn coverage(kind: IconKind, side: u32) -> Vec<u8> {
    glyph_mask(kind, side)
        .unwrap_or_else(|| panic!("{kind:?} rasterises at {side}"))
        .pixels()
        .iter()
        .map(|pixel| pixel.a)
        .collect()
}

#[test]
fn every_function_has_a_picture_that_draws_at_a_menu_rows_side() {
    for symbol in AppMenuSymbol::ALL {
        let kind = menu_symbol_icon(symbol);
        assert_ne!(
            kind,
            IconKind::Generic,
            "{symbol:?} has a picture of its own"
        );
        for side in MENU_SIDES {
            let inked = coverage(kind, side).iter().filter(|&&a| a > 127).count();
            assert!(
                inked > 0,
                "{symbol:?} ({kind:?}) draws nothing at {side} px"
            );
        }
    }
}

#[test]
fn no_two_pictures_a_menu_draws_are_the_same_mask() {
    let mut seen: BTreeMap<Vec<u8>, IconKind> = BTreeMap::new();
    for kind in AppMenuSymbol::ALL.map(menu_symbol_icon) {
        let mask = coverage(kind, MENU_SIDES[1]);
        if let Some(&other) = seen.get(&mask) {
            assert_eq!(other, kind, "{kind:?} draws exactly as {other:?}");
        }
        seen.insert(mask, kind);
    }
}

#[test]
fn a_command_picture_is_chrome_and_carries_no_badge() {
    for kind in AppMenuSymbol::ALL.map(menu_symbol_icon) {
        if let Some(marks) = super::marks(kind) {
            assert!(!marks.is_empty(), "{kind:?}");
            assert!(kind.badge().is_none(), "{kind:?} is tinted, never a badge");
        }
    }
}

#[test]
fn the_redrawn_command_glyphs_are_symbols() {
    for kind in [IconKind::Refresh, IconKind::ZoomActual, IconKind::Info] {
        assert!(super::marks(kind).is_some(), "{kind:?}");
    }
}
