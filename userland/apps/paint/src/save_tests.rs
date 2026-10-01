use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_image::{
    desktop_palette, open_native, DecodeLimits, ImageFormat, IndexDepth, NativeDocument, Pixels,
    SpriteEntry, SpriteMode, SpriteName, SpritePalette, Unkept,
};
use tairix_sandbox::imageedit::KeptReason;
use tairix_sandbox::imagerender::ViewFormat;

use super::{
    encode, format_for, format_named, lost_in, natural, restated, save_endings, write_back,
    NotWrittenBack, SaveFormat, SaveRefusal,
};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::document::{Document, Entry, Kept, Origin, Picture, SpriteInfo};

const LIMITS: DecodeLimits = DecodeLimits::new(4096, 4096, 1 << 24, 0);

fn indexed(palette: Vec<[u8; 4]>, masked: bool) -> Kind {
    Kind::Indexed {
        depth: IndexDepth::Four,
        palette,
        masked,
    }
}

fn wimp16() -> Vec<[u8; 4]> {
    desktop_palette(IndexDepth::Four)
        .iter()
        .map(|&[r, g, b]| [r, g, b, 255])
        .collect()
}

/// A 5×3 palette picture whose pixel `(x, y)` is entry `(x + y) % 16`.
fn stripes(masked: bool) -> Canvas {
    let mut built =
        CanvasBuilder::new(5, 3, indexed(wimp16(), masked), Sample::Index(0, 255)).expect("fits");
    for y in 0..3u32 {
        for x in 0..5u32 {
            let index = u8::try_from((x + y) % 16).expect("small");
            let alpha = if masked && x == 4 { 0 } else { 255 };
            built.set(x, y, Sample::Index(index, alpha));
        }
    }
    built.finish()
}

fn snapshot_of(document: &Document) -> crate::document::Snapshot {
    document.snapshot().expect("room")
}

#[test]
fn a_name_asks_for_the_format_of_its_extension_or_file_type() {
    let named = |name| format_named(name);
    assert_eq!(named("a.png"), Ok(Some(SaveFormat::Png)));
    assert_eq!(named("a.JPEG"), Ok(Some(SaveFormat::Jpeg)));
    assert_eq!(named("a.jpg"), Ok(Some(SaveFormat::Jpeg)));
    assert_eq!(named("icons.spr"), Ok(Some(SaveFormat::Sprites)));
    assert_eq!(named("Icons,ff9"), Ok(Some(SaveFormat::Sprites)));
    assert_eq!(named("Picture,b60"), Ok(Some(SaveFormat::Png)));
    assert_eq!(named("Photo,c85"), Ok(Some(SaveFormat::Jpeg)));
    assert_eq!(named("Untitled"), Ok(None));
    assert_eq!(named(".png"), Ok(None), "a dot file is named, not typed");
    assert_eq!(named("x.unknownext"), Ok(None));
    assert_eq!(
        named("still.gif"),
        Err(SaveRefusal::Unwritable(String::from(".gif")))
    );
    assert_eq!(
        named("ReadMe,fff"),
        Err(SaveRefusal::Unwritable(String::from(",fff")))
    );
}

#[test]
fn a_document_of_several_entries_is_written_only_as_a_sprite_area() {
    let two = Document::of(
        vec![
            Entry::Picture(Picture::plain(stripes(false))),
            Entry::Picture(Picture::plain(stripes(true))),
        ],
        Origin::New,
        Unkept::default(),
    )
    .expect("entries");
    assert_eq!(
        format_for("both.png", two.entries(), two.origin()),
        Err(SaveRefusal::SeveralPictures(2))
    );
    assert_eq!(
        format_for("both.spr", two.entries(), two.origin()),
        Ok(SaveFormat::Sprites)
    );
    assert_eq!(
        format_for("both", two.entries(), two.origin()),
        Err(SaveRefusal::SpritesUnnamed),
        "a sprite file nothing could open again"
    );
    let kept = Document::of(
        vec![Entry::Kept(Kept {
            name: SpriteName::new("odd").expect("a name"),
            reason: KeptReason::UnknownMode,
            bytes: Arc::new(vec![0; 44]),
        })],
        Origin::Read(ViewFormat::Sprite),
        Unkept::default(),
    )
    .expect("entries");
    assert_eq!(
        format_for("odd.png", kept.entries(), kept.origin()),
        Err(SaveRefusal::KeptSprite)
    );
    let jpeg = Document::of(
        vec![Entry::Picture(Picture::plain(stripes(false)))],
        Origin::Read(ViewFormat::Jpeg),
        Unkept::default(),
    )
    .expect("entries");
    assert_eq!(
        format_for("photo", jpeg.entries(), jpeg.origin()),
        Ok(SaveFormat::Jpeg)
    );
}

#[test]
fn a_palette_picture_is_written_as_a_png_that_reads_back_the_same() {
    let doc = Document::new(Picture::plain(stripes(false)));
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Png, "s.png").expect("encodes");
    let Ok(NativeDocument::Picture { picture, .. }) =
        open_native(ImageFormat::Png, &bytes, &LIMITS)
    else {
        panic!("a PNG picture");
    };
    let Pixels::Indexed {
        palette, indices, ..
    } = picture.pixels()
    else {
        panic!("still paletted");
    };
    assert_eq!(palette, &wimp16());
    assert_eq!(indices[..5], [0, 1, 2, 3, 4]);
}

#[test]
fn a_jpeg_is_written_at_the_documents_quality() {
    let mut doc = Document::new(Picture::plain(stripes(false)));
    doc.set_jpeg_quality(40);
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Jpeg, "s.jpg").expect("encodes");
    assert_eq!(bytes[..2], [0xFF, 0xD8]);
}

fn sprites_of(bytes: &[u8]) -> Vec<SpriteEntry> {
    let Ok(NativeDocument::Sprites(mut reader)) = open_native(ImageFormat::Sprite, bytes, &LIMITS)
    else {
        panic!("a sprite area");
    };
    (0..reader.count())
        .map(|index| reader.sprite(index).expect("reads").expect("present"))
        .collect()
}

#[test]
fn a_picture_with_no_sprite_details_is_named_after_its_file_and_keeps_its_mask() {
    let doc = Document::new(Picture::plain(stripes(true)));
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Sprites, "My Icons.spr").expect("encodes");
    let sprites = sprites_of(&bytes);
    let [SpriteEntry::Picture(sprite)] = &sprites[..] else {
        panic!("one sprite");
    };
    assert_eq!(sprite.name.as_bytes(), b"my_icons");
    assert!(sprite.masked);
    assert_eq!(
        sprite.palette,
        SpritePalette::Implied,
        "the desktop's colours state none"
    );
    assert_eq!(sprite.picture.to_rgba(), Some(flattened(&stripes(true))));
}

#[test]
fn derived_names_do_not_collide_with_names_already_held() {
    let mut first = Picture::plain(stripes(false));
    first.sprite = Some(SpriteInfo {
        name: SpriteName::new("pic").expect("a name"),
        mode: SpriteMode::indexed(IndexDepth::Four, (1, 1), false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    let doc = Document::of(
        vec![
            Entry::Picture(first),
            Entry::Picture(Picture::plain(stripes(false))),
        ],
        Origin::New,
        Unkept::default(),
    )
    .expect("entries");
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Sprites, "pic.spr").expect("encodes");
    let names: Vec<Vec<u8>> = sprites_of(&bytes)
        .iter()
        .map(|entry| match entry {
            SpriteEntry::Picture(sprite) => sprite.name.as_bytes().to_vec(),
            SpriteEntry::Opaque(kept) => kept.name.as_bytes().to_vec(),
        })
        .collect();
    assert_eq!(names, vec![b"pic".to_vec(), b"pic1".to_vec()]);
}

#[test]
fn a_changed_palette_is_stated_in_full_and_a_desktop_one_not_at_all() {
    let mut colours = wimp16();
    assert_eq!(
        restated(&indexed(colours.clone(), false)),
        SpritePalette::Implied
    );
    colours[3] = [1, 2, 3, 255];
    assert_eq!(
        restated(&indexed(colours.clone(), false)),
        SpritePalette::Full
    );
    assert_eq!(restated(&Kind::Rgba), SpritePalette::Implied);
    let mut built =
        CanvasBuilder::new(2, 1, indexed(colours, false), Sample::Index(3, 255)).expect("fits");
    built.set(1, 0, Sample::Index(0, 255));
    let doc = Document::new(Picture::plain(built.finish()));
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Sprites, "c.spr").expect("encodes");
    let [SpriteEntry::Picture(sprite)] = &sprites_of(&bytes)[..] else {
        panic!("one sprite");
    };
    assert_eq!(
        sprite.picture.to_rgba().expect("pixels")[..4],
        [1, 2, 3, 255]
    );
}

#[test]
fn a_translucent_palette_entry_is_written_into_the_sprite_mask() {
    let palette = vec![[255, 0, 0, 255], [0, 0, 255, 128]];
    let kind = Kind::Indexed {
        depth: IndexDepth::One,
        palette,
        masked: false,
    };
    let mut built = CanvasBuilder::new(2, 1, kind, Sample::Index(0, 255)).expect("fits");
    built.set(1, 0, Sample::Index(1, 255));
    let doc = Document::new(Picture::plain(built.finish()));
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Sprites, "t.spr").expect("encodes");
    let [SpriteEntry::Picture(sprite)] = &sprites_of(&bytes)[..] else {
        panic!("one sprite");
    };
    assert!(sprite.masked);
    assert!(sprite.mode.alpha_mask(), "half opacity needs an alpha mask");
    let rgba = sprite.picture.to_rgba().expect("pixels");
    assert_eq!(rgba[..4], [255, 0, 0, 255]);
    assert_eq!(rgba[4..], [0, 0, 255, 128]);
}

#[test]
fn a_kept_sprite_is_written_back_exactly() {
    let donor = Document::new(Picture::plain(stripes(false)));
    let bytes = encode(&snapshot_of(&donor), SaveFormat::Sprites, "d.spr").expect("encodes");
    // The one sprite's own bytes: a file holds its area without the area's
    // length word, so the first sprite starts after three header words.
    let sprite_bytes = bytes[12..].to_vec();
    let doc = Document::of(
        vec![
            Entry::Kept(Kept {
                name: SpriteName::new("d").expect("a name"),
                reason: KeptReason::UnsupportedType,
                bytes: Arc::new(sprite_bytes.clone()),
            }),
            Entry::Picture(Picture::plain(stripes(true))),
        ],
        Origin::Read(ViewFormat::Sprite),
        Unkept::default(),
    )
    .expect("entries");
    let written = encode(&snapshot_of(&doc), SaveFormat::Sprites, "d.spr").expect("encodes");
    assert_eq!(written[12..12 + sprite_bytes.len()], sprite_bytes[..]);
}

#[test]
fn a_colour_picture_with_partial_alpha_is_written_with_an_alpha_mask() {
    let canvas = Canvas::new(3, 2, Kind::Rgba, Sample::Rgba([10, 20, 30, 100])).expect("fits");
    let doc = Document::new(Picture::plain(canvas));
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Sprites, "a.spr").expect("encodes");
    let [SpriteEntry::Picture(sprite)] = &sprites_of(&bytes)[..] else {
        panic!("one sprite");
    };
    assert!(sprite.masked && sprite.mode.alpha_mask());
    assert_eq!(
        sprite.picture.to_rgba().expect("pixels")[..4],
        [10, 20, 30, 100]
    );
}

/// `canvas`'s pixels as straight RGBA, as a decoded picture flattens them.
fn flattened(canvas: &Canvas) -> Vec<u8> {
    let mut out = Vec::new();
    let mut row = vec![[0u8; 4]; canvas.width() as usize];
    for y in 0..canvas.height() {
        canvas.row_colours(y, 0, &mut row);
        out.extend(row.iter().flatten());
    }
    out
}

/// A file narrowed on the way in.
const NARROWED: Unkept = Unkept {
    precision: true,
    extras: false,
};

/// A file holding more than its picture.
const BESIDE: Unkept = Unkept {
    precision: false,
    extras: true,
};

/// A document of `entries` read as `format`, its file holding what `unkept`
/// says they do not.
fn read(entries: Vec<Entry>, format: ViewFormat, unkept: Unkept) -> Document {
    Document::of(entries, Origin::Read(format), unkept).expect("entries")
}

fn kept(length: usize) -> Entry {
    Entry::Kept(Kept {
        name: SpriteName::new("odd").expect("a name"),
        reason: KeptReason::Damaged,
        bytes: Arc::new(vec![0; length]),
    })
}

#[test]
fn a_damaged_kept_sprite_is_refused_before_the_file_is_touched() {
    let off_word = read(vec![kept(46)], ViewFormat::Sprite, Unkept::default());
    assert_eq!(
        format_for("odd.spr", off_word.entries(), off_word.origin()),
        Err(SaveRefusal::KeptSpriteOffWord(
            SpriteName::new("odd").expect("a name")
        ))
    );
    let whole = read(vec![kept(48)], ViewFormat::Sprite, Unkept::default());
    assert_eq!(
        format_for("odd.spr", whole.entries(), whole.origin()),
        Ok(SaveFormat::Sprites)
    );
}

#[test]
fn a_file_is_written_back_only_as_what_it_was_read_as() {
    let picture = || vec![Entry::Picture(Picture::plain(stripes(false)))];
    let png = read(picture(), ViewFormat::Png, Unkept::default());
    assert_eq!(write_back("a.png", &png), Ok(()));
    assert_eq!(write_back("a", &png), Ok(()), "an unnamed PNG is still one");
    assert_eq!(write_back("a.jpg", &png), Err(NotWrittenBack::Misnamed));
    assert_eq!(
        write_back("deep.png", &read(picture(), ViewFormat::Png, NARROWED)),
        Err(NotWrittenBack::Reduced)
    );
    for format in [ViewFormat::Png, ViewFormat::Jpeg] {
        assert_eq!(
            write_back("held.png", &read(picture(), format, BESIDE)),
            Err(NotWrittenBack::Extras),
            "{format:?}"
        );
    }
    assert_eq!(
        write_back(
            "still.gif",
            &read(picture(), ViewFormat::Gif, Unkept::default())
        ),
        Err(NotWrittenBack::Format)
    );
    let new = Document::new(Picture::plain(stripes(false)));
    assert_eq!(write_back("a.png", &new), Err(NotWrittenBack::Format));
    let area = read(vec![kept(46)], ViewFormat::Sprite, Unkept::default());
    assert!(matches!(
        write_back("odd.spr", &area),
        Err(NotWrittenBack::Refused(SaveRefusal::KeptSpriteOffWord(_)))
    ));
}

#[test]
fn a_new_document_is_offered_the_name_of_what_it_is() {
    let one = Document::new(Picture::plain(stripes(false)));
    assert_eq!(natural(one.entries(), one.origin()), SaveFormat::Png);
    let two = Document::of(
        vec![
            Entry::Picture(Picture::plain(stripes(false))),
            Entry::Picture(Picture::plain(stripes(true))),
        ],
        Origin::New,
        Unkept::default(),
    )
    .expect("entries");
    assert_eq!(natural(two.entries(), two.origin()), SaveFormat::Sprites);
}

/// A save is held to the endings of every format that can hold the
/// document, its own first, so a name given none takes that.
#[test]
fn a_documents_save_endings_are_those_of_the_formats_that_hold_it() {
    let listed = |document: &Document| -> Vec<String> {
        save_endings(document.entries(), document.origin())
            .expect("some format holds it")
            .iter()
            .map(String::from)
            .collect()
    };
    let one = Document::new(Picture::plain(stripes(false)));
    assert_eq!(
        listed(&one),
        [".png", ",b60", ".jpg", ".jpeg", ",c85", ".spr", ",ff9"]
    );
    let jpeg = read(
        vec![Entry::Picture(Picture::plain(stripes(false)))],
        ViewFormat::Jpeg,
        Unkept::default(),
    );
    assert_eq!(listed(&jpeg)[0], ".jpg", "what it is comes first");
    let two = Document::of(
        vec![
            Entry::Picture(Picture::plain(stripes(false))),
            Entry::Picture(Picture::plain(stripes(true))),
        ],
        Origin::New,
        Unkept::default(),
    )
    .expect("entries");
    assert_eq!(listed(&two), [".spr", ",ff9"], "several only as sprites");
    for format in SaveFormat::ALL {
        assert_eq!(
            format_named(&alloc::format!("a.{}", format.extension())),
            Ok(Some(format))
        );
    }
}

/// A document no format can hold opens no picker: the refusal is said.
#[test]
fn a_document_no_format_holds_has_no_save_endings() {
    let area = read(vec![kept(46)], ViewFormat::Sprite, Unkept::default());
    assert_eq!(
        save_endings(area.entries(), area.origin()),
        Err(SaveRefusal::KeptSpriteOffWord(
            SpriteName::new("odd").expect("a name")
        ))
    );
}

/// A JPEG of a picture with any transparency says it was laid over white;
/// nothing else is said to be lost.
#[test]
fn a_jpeg_of_a_transparent_picture_says_what_it_lost() {
    let clear = Canvas::new(2, 2, Kind::Rgba, Sample::Rgba([0, 0, 0, 0])).expect("fits");
    let transparent = snapshot_of(&Document::new(Picture::plain(clear)));
    assert!(lost_in(&transparent, SaveFormat::Jpeg).is_some());
    assert_eq!(lost_in(&transparent, SaveFormat::Png), None);
    let opaque = snapshot_of(&Document::new(Picture::plain(stripes(false))));
    assert_eq!(lost_in(&opaque, SaveFormat::Jpeg), None);
}

/// A sprite of pixels other than square, saved as a PNG or a JPEG, says its
/// pixels' shape was not kept; saved as a sprite it says nothing.
#[test]
fn a_tall_pixeled_sprite_saved_as_a_png_says_its_shape_was_lost() {
    let mut picture = Picture::plain(stripes(false));
    picture.sprite = Some(crate::document::SpriteInfo {
        name: SpriteName::new("tall").expect("a name"),
        mode: SpriteMode::truecolour((1, 2), false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    let tall = snapshot_of(&Document::new(picture));
    assert!(lost_in(&tall, SaveFormat::Png).is_some());
    assert!(lost_in(&tall, SaveFormat::Jpeg).is_some());
    assert_eq!(lost_in(&tall, SaveFormat::Sprites), None);
}
