//! A folder's sample, where its cards stand, and the picture composed from
//! the shipped back and front masters and the members' own pictures.

extern crate std;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::fs::FileId;
use tairix_abi::time::Time64;
use tairix_geometry::Rect;
use tairix_raster::{Color, Pixel, Surface};
use tairix_svg::font::NoFonts;

use super::{
    card_slots, print_card, DrawnCard, FolderSample, SampleCard, DESIGN, INK, MIN_COMPOSITE_SIDE,
    SLOTS,
};
use crate::artwork::{
    icon_vector_path, render_artwork, ArtworkKey, ArtworkRasteriser, ArtworkReader,
};
use crate::glyph::IconKind;
use crate::picture::Fitted;
use crate::thumbnail::{ArtworkDocument, DocumentStamp, Reading, Thumbnail};

fn kinds(kinds: &[IconKind]) -> FolderSample {
    FolderSample::new(kinds.iter().map(|kind| SampleCard::Kind(*kind)))
}

/// The member picture the fixtures list.
const PHOTO_PATH: &str = "/Users/ann/UserFiles/Pictures/cat.jpg";
const PHOTO_ID: FileId = FileId {
    volume: [3; 16],
    node: 9,
};
const PHOTO_WRITTEN: Time64 = Time64::from_secs(1_700_000_000);
const PHOTO_BYTES: u64 = 12;
const PHOTO_GEN: u64 = 5;

const PHOTO: DocumentStamp = DocumentStamp {
    size: PHOTO_BYTES,
    modified: PHOTO_WRITTEN,
    id: PHOTO_ID,
    content_gen: PHOTO_GEN,
};

fn photo() -> SampleCard {
    SampleCard::Picture(
        IconKind::ImageJpeg,
        Thumbnail {
            path: String::from(PHOTO_PATH),
            stamp: PHOTO,
            reading: Reading::Signature,
        },
    )
}

/// A sample keeps three cards at most, in the order given, and is empty only
/// when it holds none.
#[test]
fn a_sample_holds_at_most_three_cards_in_order() {
    let sample = kinds(&[
        IconKind::Image,
        IconKind::Text,
        IconKind::Audio,
        IconKind::Video,
    ]);
    assert_eq!(sample.len(), 3);
    assert_eq!(
        sample.cards().map(DrawnCard::kind).collect::<Vec<_>>(),
        [IconKind::Image, IconKind::Text, IconKind::Audio]
    );
    assert!(FolderSample::new([]).is_empty());
    assert!(!kinds(&[IconKind::Pdf]).is_empty());
}

/// A member too large to share a folder's read with the other cards prints as
/// its kind, so drawing a folder reads no more than drawing one picture does.
#[test]
fn a_member_past_its_share_of_the_read_is_drawn_as_its_kind() {
    let SampleCard::Picture(kind, mut thumbnail) = photo() else {
        unreachable!("a photo is a picture card");
    };
    thumbnail.stamp.size = FolderSample::MAX_CARD_BYTES + 1;
    let sample = FolderSample::new([SampleCard::Picture(kind, thumbnail)]);
    assert!(!sample.has_pictures());
    assert_eq!(
        sample.cards().map(DrawnCard::kind).collect::<Vec<_>>(),
        [kind]
    );
    assert!(
        FolderSample::new([photo()]).has_pictures(),
        "one within its share prints"
    );
}

/// A sample showing a member's picture reads member files, so it is
/// thumbnail-class work; it draws its cards as their kinds meanwhile, and a
/// cache keyed by it is charged for the paths it holds.
#[test]
fn a_sample_with_a_picture_is_thumbnail_class_and_draws_its_kinds_meanwhile() {
    let sample = FolderSample::new([photo(), SampleCard::Kind(IconKind::Text)]);
    assert!(sample.has_pictures());
    assert!(ArtworkKey::Folder(sample.clone()).is_thumbnail_class());
    let meanwhile = sample.kinds_only();
    assert!(!meanwhile.has_pictures());
    assert!(!ArtworkKey::Folder(meanwhile.clone()).is_thumbnail_class());
    assert_eq!(
        meanwhile.cards().map(DrawnCard::kind).collect::<Vec<_>>(),
        [IconKind::ImageJpeg, IconKind::Text]
    );
    assert_eq!(
        meanwhile,
        FolderSample::new([
            SampleCard::Kind(IconKind::ImageJpeg),
            SampleCard::Kind(IconKind::Text)
        ]),
        "every folder drawing the same kinds is one picture"
    );
    assert!(sample.heap_bytes() >= PHOTO_PATH.len());
    assert_eq!(
        meanwhile.heap_bytes(),
        sample.heap_bytes(),
        "one shared copy"
    );
}

/// The sample's first card is drawn last, at the front; each card is turned
/// about the middle of its foot, the outer two away from each other, and every
/// foot stays below the front pocket's top, so each card stands in the folder.
#[test]
fn every_card_stands_in_the_pocket_turned_about_its_foot() {
    // The front pocket's top on the design grid (`folder-front.svg`).
    const POCKET: f64 = 112.0;
    for (count, slots) in SLOTS.iter().enumerate() {
        let all = [IconKind::Image, IconKind::Text, IconKind::Audio];
        let sample = kinds(&all[..=count]);
        let placed: Vec<_> = card_slots(&sample, DESIGN).collect();
        assert_eq!(placed.len(), count + 1);
        assert_eq!(
            placed.last().map(|(_, card)| card.kind()),
            Some(IconKind::Image)
        );
        for (place, _) in &placed {
            let side = f64::from(place.side);
            let (x, y) = place.transform().apply((side / 2.0, side));
            assert!(
                (x - (f64::from(place.x) + side / 2.0)).abs() < 1e-6,
                "the foot moved"
            );
            assert!(y > POCKET, "a card of {count} floats above the pocket");
        }
        if let [(_, _, _, right), (_, _, _, left), ..] = slots {
            assert!(*right > 0.0 && *left < 0.0, "the outer cards lean apart");
        }
    }
}

/// A print is the picture's own shape laid on white inside a fixed-ink edge,
/// centred on its card, and a picture that is empty prints nothing.
#[test]
fn a_print_is_the_pictures_shape_on_white_inside_an_edge() {
    let mut square = Surface::new(64, 64).expect("a surface");
    let red = Color::rgb(200, 20, 20);
    square.fill_rect(0, 16, 64, 32, red);
    let card = print_card(&square, Rect::new(0, 16, 64, 32), 84).expect("a print");
    let at = |x: u32, y: u32| card.get(x, y).expect("on the card");
    assert_eq!(
        at(42, 42),
        red.premultiply(),
        "the picture is printed whole"
    );
    assert_eq!(at(42, 2).a, 0, "a wide picture prints a wide card");
    let top_border = (0..84)
        .map(|y| at(42, y))
        .find(|pixel| pixel.a != 0)
        .expect("the print has an edge");
    assert_ne!(
        top_border,
        red.premultiply(),
        "the picture sits inside a border"
    );
    assert!(print_card(&square, Rect::EMPTY, 84).is_none());
}

/// Reads the shipped masters from the crate's own assets, and opens the one
/// listed member picture.
struct Shipped {
    photo: bool,
}

impl ArtworkReader for Shipped {
    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        let name = path.rsplit('/').next()?;
        std::fs::read(alloc::format!(
            "{}/assets/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .ok()
    }

    fn open(&mut self, path: &str) -> Option<Box<dyn ArtworkDocument + '_>> {
        (self.photo && path == PHOTO_PATH).then(|| Box::new(Photo) as Box<dyn ArtworkDocument>)
    }
}

/// The member picture's open handle.
struct Photo;

impl ArtworkDocument for Photo {
    fn stamp(&self) -> DocumentStamp {
        PHOTO
    }

    fn read_at(&mut self, _offset: u64, into: &mut [u8]) -> Option<usize> {
        into.fill(0);
        Some(into.len())
    }
}

/// The colour the fixture's member picture decodes to.
const PHOTO_COLOUR: Color = Color::rgb(0x20, 0xA0, 0x40);

/// Rasterises an SVG master as the sandbox does, and decodes the member
/// picture to a wide band of [`PHOTO_COLOUR`].
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

    fn thumbnail(
        &mut self,
        side: u32,
        _reading: Reading,
        _document: &mut dyn ArtworkDocument,
    ) -> Option<Fitted> {
        let band = side / 2;
        let top = side / 4;
        let mut pixels = vec![0u8; (side * side * 4) as usize];
        for row in top..top + band {
            for column in 0..side {
                let at = ((row * side + column) * 4) as usize;
                pixels[at..at + 4].copy_from_slice(&[
                    PHOTO_COLOUR.r,
                    PHOTO_COLOUR.g,
                    PHOTO_COLOUR.b,
                    0xFF,
                ]);
            }
        }
        Some(Fitted {
            pixels,
            bounds: Rect::new(0, i32::try_from(top).expect("small"), side, band),
        })
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
    let sample = kinds(&[IconKind::Text]);
    let artwork = render_artwork(
        &mut Shipped { photo: false },
        &mut Svg,
        &ArtworkKey::Folder(sample),
        side,
    )
    .expect("the picture draws");
    let picture = artwork.surface();
    let body = Color::rgb(0x4C, 0x9A, 0xF0).premultiply();
    assert_eq!(
        at(picture, 64, 96),
        body,
        "the front covers the folder's foot"
    );
    // Within the card, above the pocket: paper, or the glyph's ink on it.
    let card: Vec<Pixel> = (22..50).map(|y| at(picture, 64, y)).collect();
    assert!(
        card.contains(&INK.premultiply()),
        "the glyph is drawn in its ink"
    );
    assert!(
        card.iter().all(|pixel| pixel.a == u8::MAX),
        "the card is opaque"
    );
}

/// A picture card prints the member's own picture; a member that will not
/// open draws its kind's card instead, and the folder still draws.
#[test]
fn a_picture_card_prints_the_members_picture_or_falls_to_its_kind() {
    let side = 128;
    let key = ArtworkKey::Folder(FolderSample::new([photo()]));
    let printed = render_artwork(&mut Shipped { photo: true }, &mut Svg, &key, side)
        .expect("the picture draws");
    let shows = |picture: &Surface| {
        picture
            .pixels()
            .iter()
            .any(|pixel| *pixel == PHOTO_COLOUR.premultiply())
    };
    assert!(shows(printed.surface()), "the member's picture is printed");
    let fallen = render_artwork(&mut Shipped { photo: false }, &mut Svg, &key, side)
        .expect("the picture still draws");
    assert!(
        !shows(fallen.surface()),
        "a member that will not open is its kind"
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
                .then(|| Shipped { photo: false }.read(path))
                .flatten()
        }
    }
    let key = ArtworkKey::Folder(kinds(&[IconKind::Image]));
    let mut shipped = Shipped { photo: false };
    assert!(render_artwork(&mut NoBack, &mut Svg, &key, 128).is_none());
    assert!(render_artwork(&mut shipped, &mut Svg, &key, MIN_COMPOSITE_SIDE - 1).is_none());
    assert!(render_artwork(&mut shipped, &mut Svg, &key, MIN_COMPOSITE_SIDE).is_some());
}
