use alloc::vec;
use alloc::vec::Vec;

use tairix_image::{IndexDepth, SpriteMode, SpriteName, SpritePalette};
use tairix_sandbox::imageedit::{
    EditDocument, EditKept, EditKind, EditPicture, EditPixels, EditSprite, KeptReason,
};
use tairix_sandbox::imagerender::ViewFormat;

use super::{Assembly, Refusal};
use crate::canvas::Sample;
use crate::document::{Entry, Origin};

fn opened(count: u32, sprites: bool) -> EditDocument {
    let format = if sprites {
        ViewFormat::Sprite
    } else {
        ViewFormat::Png
    };
    EditDocument {
        format,
        kind: EditKind::of(format),
        count,
        unkept: tairix_image::Unkept::default(),
        written: tairix_image::Written::Plain,
        canvas: None,
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
    assembly.picture(built, &picture).expect("room");
    let document = assembly.finish().expect("whole");
    let Some(Entry::Picture(held)) = document.entries().first() else {
        panic!("a picture");
    };
    assert_eq!(held.canvas().sample(68, 1), Some(Sample::Index(2, 0)));
    assert_eq!(held.canvas().sample(34, 1), Some(Sample::Index(1, 255)));
    assert_eq!(
        held.canvas().sample(3, 0),
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
    over.picture(first, &picture).expect("room");
    let second = over.canvas_for(&picture).expect("fits");
    over.picture(second, &picture).expect("room");
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
    assembly.picture(built, &picture).expect("room");
    let document = assembly.finish().expect("whole");
    assert_eq!(
        document.picture().and_then(|p| p.canvas().sample(1, 0)),
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

fn layered(count: u32, canvas: Option<(u32, u32)>) -> EditDocument {
    EditDocument {
        format: ViewFormat::OpenRaster,
        kind: EditKind::Layers,
        count,
        unkept: tairix_image::Unkept::default(),
        written: tairix_image::Written::Plain,
        canvas,
    }
}

fn layer(width: u32, height: u32, name: &str, at: (i32, i32)) -> EditPicture {
    EditPicture::new(width, height, EditPixels::Rgba, None)
        .expect("a picture")
        .with_layer(tairix_sandbox::imageedit::EditLayer {
            name: alloc::string::String::from(name),
            at,
            opacity: 77,
            visible: false,
        })
        .expect("a layer")
}

/// Each layer lands where its document places it on its canvas, what it
/// does not cover left clear and costing nothing of its own; the top is the
/// one painted on.
#[test]
fn a_layered_document_is_one_picture_of_its_layers() {
    let mut assembly = Assembly::new(layered(2, Some((200, 3))), ROOMY).expect("room");
    let ground = layer(200, 3, "Ground", (0, 0));
    let mut rows = assembly.canvas_for(&ground).expect("fits");
    for y in 0..3 {
        rows.row(y, &[9; 800], &[]);
    }
    assembly.picture(rows, &ground).expect("room");
    let spot = layer(2, 2, "Spot", (-1, 2));
    let mut rows = assembly.canvas_for(&spot).expect("fits");
    rows.row(0, &[1, 2, 3, 4, 5, 6, 7, 8], &[]);
    rows.row(1, &[7; 8], &[]);
    rows.row(0, &[0; 4], &[]);
    assembly.picture(rows, &spot).expect("room");
    let document = assembly.finish().expect("whole");
    assert_eq!(document.entries().len(), 1);
    let picture = document.picture().expect("a picture");
    assert_eq!((picture.layers().len(), picture.active()), (2, 1));
    let top = &picture.layers()[1];
    assert_eq!(
        (top.name.as_str(), top.opacity, top.visible),
        ("Spot", 77, false)
    );
    assert_eq!(
        top.canvas.sample(0, 2),
        Some(Sample::Rgba([5, 6, 7, 8])),
        "placed off the left"
    );
    assert_eq!(
        top.canvas.sample(1, 2),
        Some(Sample::Rgba([0; 4])),
        "past its right edge"
    );
    assert_eq!(
        top.canvas.sample(0, 0),
        Some(Sample::Rgba([0; 4])),
        "above it, clear"
    );
    assert!(
        alloc::sync::Arc::ptr_eq(top.canvas.tile(1), top.canvas.tile(2)),
        "what a layer does not cover is one shared clear tile"
    );
    assert_eq!(document.origin(), Origin::Read(ViewFormat::OpenRaster));
}

#[test]
fn a_layered_document_that_does_not_add_up_is_refused() {
    let picture = layer(1, 1, "a", (0, 0));
    assert!(Assembly::new(layered(1, None), ROOMY).is_err(), "no canvas");
    let crowd = u32::try_from(crate::document::MOST_LAYERS + 1).expect("small");
    assert_eq!(
        Assembly::new(layered(crowd, Some((1, 1))), ROOMY).err(),
        Some(Refusal::Unbelieved)
    );
    let mut plain = Assembly::new(opened(1, false), ROOMY).expect("room");
    assert_eq!(
        plain.canvas_for(&picture).err(),
        Some(Refusal::Unbelieved),
        "a stray layer"
    );
    let flat = EditPicture::new(1, 1, EditPixels::Rgba, None).expect("a picture");
    let mut stack = Assembly::new(layered(1, Some((1, 1))), ROOMY).expect("room");
    assert_eq!(
        stack.canvas_for(&flat).err(),
        Some(Refusal::Unbelieved),
        "not a layer"
    );
    let mut over = Assembly::new(layered(1, Some((1, 1))), ROOMY).expect("room");
    for _ in 0..2 {
        let rows = over.canvas_for(&picture).expect("fits");
        over.picture(rows, &picture).expect("room");
    }
    assert!(over.finish().is_none(), "more layers than it said");
    let short = Assembly::new(layered(2, Some((1, 1))), ROOMY).expect("room");
    assert!(short.finish().is_none(), "fewer");
}
