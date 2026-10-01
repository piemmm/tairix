use alloc::vec;
use alloc::vec::Vec;

use tairix_image::{IndexDepth, SpriteMode, SpriteName, SpritePalette};
use tairix_sandbox::imageedit::{
    EditDocument, EditKept, EditPicture, EditPixels, EditSprite, KeptReason,
};
use tairix_sandbox::imagerender::ViewFormat;

use super::{Assembly, Refusal};
use crate::canvas::Sample;
use crate::document::{Entry, Origin};

fn opened(count: u32, sprites: bool) -> EditDocument {
    EditDocument {
        format: if sprites {
            ViewFormat::Sprite
        } else {
            ViewFormat::Png
        },
        sprites,
        count,
        unkept: tairix_image::Unkept::default(),
    }
}

/// A file longer than any test's entries need.
const ROOMY: usize = 1 << 20;

fn paletted(width: u32, height: u32, plane: bool) -> EditPicture {
    let pixels = EditPixels::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0, 0, 0, 255], [255, 255, 255, 255], [9, 9, 9, 255]],
        plane,
    };
    let sprite = EditSprite {
        name: SpriteName::new("dot").expect("a name"),
        mode: SpriteMode::indexed(IndexDepth::Two, (1, 1), false),
        masked: plane,
        palette: SpritePalette::Full,
    };
    EditPicture::new(width, height, pixels, Some(sprite)).expect("a picture")
}

#[test]
fn rows_land_where_they_were_read_from() {
    let picture = paletted(70, 2, true);
    let mut assembly = Assembly::new(opened(1, true), ROOMY).expect("room");
    let mut built = assembly.canvas_for(&picture).expect("fits");
    let row: Vec<u8> = (0..70u8).map(|x| x % 3).collect();
    let mask: Vec<u8> = (0..70u8).map(|x| if x < 35 { 255 } else { 0 }).collect();
    built.row(1, &row, &mask);
    assembly.picture(built, &picture);
    let document = assembly.finish().expect("whole");
    let Some(Entry::Picture(held)) = document.entries().first() else {
        panic!("a picture");
    };
    assert_eq!(held.canvas.sample(68, 1), Some(Sample::Index(2, 0)));
    assert_eq!(held.canvas.sample(34, 1), Some(Sample::Index(1, 255)));
    assert_eq!(
        held.canvas.sample(3, 0),
        Some(Sample::Index(0, 255)),
        "unread rows stay blank"
    );
    assert_eq!(
        held.sprite.as_ref().map(|s| s.name.as_bytes()),
        Some(&b"dot"[..])
    );
    assert_eq!(document.origin(), Origin::Read(ViewFormat::Sprite));
}

#[test]
fn a_document_short_or_over_its_count_is_refused() {
    let short = Assembly::new(opened(2, true), ROOMY).expect("room");
    assert!(short.finish().is_none());
    let picture = paletted(1, 1, false);
    let mut over = Assembly::new(opened(1, true), ROOMY).expect("room");
    let first = over.canvas_for(&picture).expect("fits");
    over.picture(first, &picture);
    let second = over.canvas_for(&picture).expect("fits");
    over.picture(second, &picture);
    assert!(over.finish().is_none());
}

#[test]
fn a_kept_sprite_is_held_as_its_bytes() {
    let mut assembly = Assembly::new(opened(1, true), ROOMY).expect("room");
    let kept = EditKept {
        name: SpriteName::new("cmyk").expect("a name"),
        reason: KeptReason::UnsupportedType,
        length: 48,
    };
    assembly.claim_kept(&kept).expect("the file holds it");
    assembly.kept(&kept, vec![7; 48]);
    let document = assembly.finish().expect("whole");
    let Some(Entry::Kept(held)) = document.entries().first() else {
        panic!("kept");
    };
    assert_eq!(held.bytes.len(), 48);
    assert_eq!(held.reason, KeptReason::UnsupportedType);
    assert!(document.picture().is_none());
}

#[test]
fn a_colour_picture_builds_a_colour_canvas() {
    let picture = EditPicture::new(2, 1, EditPixels::Rgba, None).expect("a picture");
    let mut assembly = Assembly::new(opened(1, false), ROOMY).expect("room");
    let mut built = assembly.canvas_for(&picture).expect("fits");
    built.row(0, &[1, 2, 3, 4, 5, 6, 7, 8], &[]);
    assembly.picture(built, &picture);
    let document = assembly.finish().expect("whole");
    assert_eq!(
        document.picture().and_then(|p| p.canvas.sample(1, 0)),
        Some(Sample::Rgba([5, 6, 7, 8]))
    );
}

#[test]
fn sprites_needing_more_than_their_file_holds_are_not_believed() {
    // A 64×64 two-bit sprite is at least its control block and 64 rows of
    // four words: 1068 bytes.
    let picture = paletted(64, 64, false);
    let mut fits = Assembly::new(opened(2, true), 1068 + 44).expect("room");
    assert!(fits.canvas_for(&picture).is_ok());
    let kept = EditKept {
        name: SpriteName::new("cmyk").expect("a name"),
        reason: KeptReason::UnsupportedType,
        length: 48,
    };
    assert_eq!(
        fits.claim_kept(&kept),
        Err(Refusal::Unbelieved),
        "48 bytes when 44 are left"
    );
    let mut short = Assembly::new(opened(1, true), 1067).expect("room");
    assert_eq!(short.canvas_for(&picture).err(), Some(Refusal::Unbelieved));
}

#[test]
fn a_single_picture_is_not_charged_against_its_file() {
    // A compressed picture decodes to far more than its file is long.
    let picture = EditPicture::new(256, 256, EditPixels::Rgba, None).expect("a picture");
    let mut assembly = Assembly::new(opened(1, false), 100).expect("room");
    assert!(assembly.canvas_for(&picture).is_ok());
}
