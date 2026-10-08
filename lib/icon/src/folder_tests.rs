//! A folder's sample, where its cards stand, and the picture composed from
//! the shipped back and front masters.

extern crate std;

use alloc::vec::Vec;

use tairix_raster::{Color, Pixel, Surface};
use tairix_svg::font::NoFonts;

use super::{card_slots, FolderSample, DESIGN, INK, MIN_COMPOSITE_SIDE, SLOTS};
use crate::artwork::{
    icon_vector_path, render_artwork, ArtworkKey, ArtworkRasteriser, ArtworkReader,
};
use crate::glyph::IconKind;

/// A sample keeps three kinds at most, in the order given, and is empty only
/// when it holds none.
#[test]
fn a_sample_holds_at_most_three_kinds_in_order() {
    let sample = FolderSample::new([
        IconKind::Image,
        IconKind::Text,
        IconKind::Audio,
        IconKind::Video,
    ]);
    assert_eq!(sample.len(), 3);
    assert_eq!(
        sample.kinds().collect::<Vec<_>>(),
        [IconKind::Image, IconKind::Text, IconKind::Audio]
    );
    assert!(FolderSample::new([]).is_empty());
    assert!(!FolderSample::new([IconKind::Pdf]).is_empty());
}

/// The most frequent family's card is drawn last, at the front, and every
/// card's foot is below the front pocket's top, so each stands in the folder.
#[test]
fn the_first_kind_stands_in_front_and_every_card_stands_in_the_pocket() {
    // The front pocket's top on the design grid (`folder-front.svg`).
    const POCKET: u32 = 112;
    for (count, slots) in SLOTS.iter().enumerate() {
        let kinds = [IconKind::Image, IconKind::Text, IconKind::Audio];
        let sample = FolderSample::new(kinds.iter().copied().take(count + 1));
        let placed: Vec<_> = card_slots(sample, DESIGN).collect();
        assert_eq!(placed.len(), count + 1);
        assert_eq!(placed.last().map(|(_, kind)| *kind), Some(IconKind::Image));
        for &(_, top, side) in *slots {
            assert!(
                top + side > POCKET,
                "a card of {count} floats above the pocket"
            );
        }
    }
}

/// Reads the shipped masters from the crate's own assets.
struct Shipped;

impl ArtworkReader for Shipped {
    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        let name = path.rsplit('/').next()?;
        std::fs::read(alloc::format!(
            "{}/assets/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .ok()
    }
}

/// Rasterises an SVG master as the sandbox does, refusing anything else.
struct Svg;

impl ArtworkRasteriser for Svg {
    fn rasterise(&mut self, side: u32, bytes: &[u8]) -> Option<Vec<u8>> {
        let surface = crate::svg::decode(bytes, &mut NoFonts)
            .ok()?
            .rasterise(side)?;
        Some(
            surface
                .pixels()
                .iter()
                .flat_map(|pixel| {
                    let colour = pixel.unpremultiply();
                    [colour.r, colour.g, colour.b, colour.a]
                })
                .collect(),
        )
    }
}

fn at(picture: &Surface, x: u32, y: u32) -> Pixel {
    picture.pixels()[(y * picture.width() + x) as usize]
}

/// A folder's picture is its back, its cards and its front: the front covers
/// the foot of the folder, and a card standing above it shows its glyph on
/// paper where its kind ships no artwork.
#[test]
fn a_folder_picture_stands_its_cards_between_its_back_and_front() {
    let side = 128;
    let sample = FolderSample::new([IconKind::Text]);
    let picture = render_artwork(&mut Shipped, &mut Svg, &ArtworkKey::Folder(sample), side)
        .expect("the picture draws");
    let body = Color::rgb(0x4C, 0x9A, 0xF0).premultiply();
    assert_eq!(
        at(&picture, 64, 96),
        body,
        "the front covers the folder's foot"
    );
    // Within the card, above the pocket: paper, or the glyph's ink on it.
    let card: Vec<Pixel> = (20..50).map(|y| at(&picture, 64, y)).collect();
    assert!(
        card.contains(&INK.premultiply()),
        "the glyph is drawn in its ink"
    );
    assert!(
        card.iter().all(|pixel| pixel.a == u8::MAX),
        "the card is opaque"
    );
}

/// No picture of the folder's contents where its back will not draw, or where
/// its cards would be too small to read: the request falls to the filled
/// folder instead.
#[test]
fn a_folder_picture_declines_without_its_back_or_below_a_readable_side() {
    struct NoBack;
    impl ArtworkReader for NoBack {
        fn read(&mut self, path: &str) -> Option<Vec<u8>> {
            let back = icon_vector_path(IconKind::FolderBack);
            (path != back.as_str())
                .then(|| Shipped.read(path))
                .flatten()
        }
    }
    let key = ArtworkKey::Folder(FolderSample::new([IconKind::Image]));
    assert!(render_artwork(&mut NoBack, &mut Svg, &key, 128).is_none());
    assert!(render_artwork(&mut Shipped, &mut Svg, &key, MIN_COMPOSITE_SIDE - 1).is_none());
    assert!(render_artwork(&mut Shipped, &mut Svg, &key, MIN_COMPOSITE_SIDE).is_some());
}
