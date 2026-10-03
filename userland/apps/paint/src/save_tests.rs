use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_image::{
    desktop_palette, open_native, DecodeLimits, Density, DensityUnit, ImageFormat, IndexDepth,
    NativeDocument, Pixels, SpriteEntry, SpriteMode, SpriteName, SpritePalette, Unkept,
};
use tairix_sandbox::imageedit::KeptReason;
use tairix_sandbox::imagerender::ViewFormat;

use super::{
    encode, format_for, format_named, losses, lost_in, natural, restated, save_endings, write_back,
    Loss, NotWrittenBack, SaveFormat, SaveRefusal, SaveSettings,
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
    assert_eq!(named("still.gif"), Ok(Some(SaveFormat::Gif)));
    assert_eq!(named("scan.bmp"), Ok(Some(SaveFormat::Bmp)));
    assert_eq!(named("pages.tif"), Ok(Some(SaveFormat::Tiff)));
    assert_eq!(named("pages.tiff"), Ok(Some(SaveFormat::Tiff)));
    assert_eq!(
        named("photo.webp"),
        Err(SaveRefusal::Unwritable(String::from(".webp")))
    );
    assert_eq!(
        named("ReadMe,fff"),
        Err(SaveRefusal::Unwritable(String::from(",fff")))
    );
}

#[test]
fn a_document_of_several_entries_is_written_only_as_a_tiff_or_a_sprite_area() {
    let two = Document::of(
        vec![
            Entry::Picture(Picture::plain(stripes(false))),
            Entry::Picture(Picture::plain(stripes(true))),
        ],
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default(),
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
        format_for("both.tif", two.entries(), two.origin()),
        Ok(SaveFormat::Tiff)
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
        SaveSettings::default(),
    )
    .expect("entries");
    assert_eq!(
        format_for("odd.png", kept.entries(), kept.origin()),
        Err(SaveRefusal::KeptSprite)
    );
    assert_eq!(
        format_for("odd.tif", kept.entries(), kept.origin()),
        Err(SaveRefusal::KeptSprite),
        "a page holds pixels"
    );
    let jpeg = Document::of(
        vec![Entry::Picture(Picture::plain(stripes(false)))],
        Origin::Read(ViewFormat::Jpeg),
        Unkept::default(),
        SaveSettings::default(),
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
    doc.set_settings(SaveSettings {
        jpeg_quality: 40,
        ..SaveSettings::default()
    });
    let coarse = encode(&snapshot_of(&doc), SaveFormat::Jpeg, "s.jpg").expect("encodes");
    assert_eq!(coarse[..2], [0xFF, 0xD8]);
    doc.set_settings(SaveSettings::default());
    let fine = encode(&snapshot_of(&doc), SaveFormat::Jpeg, "s.jpg").expect("encodes");
    assert_ne!(coarse, fine, "the quality is the document's");
}

fn native(format: ImageFormat, bytes: &[u8]) -> tairix_image::Picture {
    match open_native(format, bytes, &LIMITS).expect("opens") {
        NativeDocument::Picture { picture, .. } => picture,
        NativeDocument::Pages { mut pages, .. } => pages.page(0).expect("decodes").expect("a page"),
        NativeDocument::Sprites(_) | NativeDocument::Layers { .. } => panic!("a picture"),
    }
}

#[test]
fn a_palette_picture_is_written_as_a_gif_and_a_bmp_keeping_its_indices() {
    let doc = Document::new(Picture::plain(stripes(false)));
    for (format, read_as) in [
        (SaveFormat::Gif, ImageFormat::Gif),
        (SaveFormat::Bmp, ImageFormat::Bmp),
    ] {
        let bytes = encode(&snapshot_of(&doc), format, "s").expect("encodes");
        let picture = native(read_as, &bytes);
        let Pixels::Indexed {
            palette, indices, ..
        } = picture.pixels()
        else {
            panic!("still paletted");
        };
        assert_eq!(&palette[..16], &wimp16()[..], "{format:?}");
        assert_eq!(indices[..5], [0, 1, 2, 3, 4], "{format:?}");
    }
}

#[test]
fn a_colour_picture_is_reduced_to_a_palette_for_a_gif() {
    let mut built = CanvasBuilder::new(40, 10, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for y in 0..10u32 {
        for x in 0..40u32 {
            let shade = u8::try_from(x * 6).expect("small");
            let alpha = if y == 0 { 0 } else { 255 };
            built.set(
                x,
                y,
                Sample::Rgba([
                    shade,
                    255 - shade,
                    u8::try_from(y * 20).expect("small"),
                    alpha,
                ]),
            );
        }
    }
    let doc = Document::new(Picture::plain(built.finish()));
    let snapshot = snapshot_of(&doc);
    assert_eq!(
        losses(&snapshot, SaveFormat::Gif).expect("room"),
        [Loss::Colours]
    );
    let picture = native(
        ImageFormat::Gif,
        &encode(&snapshot, SaveFormat::Gif, "s.gif").expect("encodes"),
    );
    let rgba = picture.to_rgba().expect("pixels");
    assert!(
        rgba[..40 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 0),
        "the clear row stays clear"
    );
    assert!(rgba[40 * 4..]
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == 255));
}

#[test]
fn several_pictures_are_written_as_a_tiffs_pages_with_their_densities() {
    let mut second = Picture::plain(stripes(true));
    second.density = Density::whole(300, 300, DensityUnit::Inch);
    let doc = Document::of(
        vec![
            Entry::Picture(Picture::plain(stripes(false))),
            Entry::Picture(second),
        ],
        Origin::Read(ViewFormat::Tiff),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    assert!(doc.is_pages() && !doc.is_sprite_area());
    assert_eq!(natural(doc.entries(), doc.origin()), SaveFormat::Tiff);
    let bytes = encode(&snapshot_of(&doc), SaveFormat::Tiff, "pages.tif").expect("encodes");
    let NativeDocument::Pages { mut pages, .. } =
        open_native(ImageFormat::Tiff, &bytes[..], &LIMITS).expect("opens")
    else {
        panic!("pages");
    };
    assert_eq!(pages.count(), 2);
    let last = pages.page(1).expect("decodes").expect("a page");
    assert_eq!(last.density(), Density::whole(300, 300, DensityUnit::Inch));
    assert_eq!(last.to_rgba(), Some(flattened(&stripes(true))));
}

#[test]
fn what_a_format_cannot_keep_is_said_before_and_with_the_save() {
    let mut dense = Picture::plain(stripes(false));
    dense.density = Density::whole(1, 2, DensityUnit::Aspect);
    let doc = Document::new(dense);
    let snapshot = snapshot_of(&doc);
    assert_eq!(
        losses(&snapshot, SaveFormat::Bmp).expect("room"),
        [Loss::Density],
        "a BMP states a length alone"
    );
    assert!(
        losses(&snapshot, SaveFormat::Gif).expect("room").is_empty(),
        "a GIF states a shape"
    );
    assert_eq!(
        lost_in(&snapshot, SaveFormat::Sprites).expect("room"),
        Some(Loss::Density.message())
    );
    let mut tall = Picture::plain(stripes(false));
    tall.sprite = Some(SpriteInfo {
        name: SpriteName::new("tall").expect("a name"),
        mode: SpriteMode::truecolour((1, 2), false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    let snapshot = snapshot_of(&Document::new(tall));
    assert_eq!(
        losses(&snapshot, SaveFormat::Tiff).expect("room"),
        [Loss::PixelShape, Loss::SpriteDetails]
    );
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
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default(),
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
        SaveSettings::default(),
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
    converted: false,
};

/// A file holding more than its picture.
const BESIDE: Unkept = Unkept {
    precision: false,
    extras: true,
    converted: false,
};

/// A file whose colours were restated.
const RESTATED: Unkept = Unkept {
    precision: false,
    extras: false,
    converted: true,
};

/// A document of `entries` read as `format`, its file holding what `unkept`
/// says they do not.
fn read(entries: Vec<Entry>, format: ViewFormat, unkept: Unkept) -> Document {
    Document::of(
        entries,
        Origin::Read(format),
        unkept,
        SaveSettings::default(),
    )
    .expect("entries")
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
        write_back("inks.tif", &read(picture(), ViewFormat::Tiff, RESTATED)),
        Err(NotWrittenBack::Converted)
    );
    for (name, format) in [
        ("still.gif", ViewFormat::Gif),
        ("scan.bmp", ViewFormat::Bmp),
        ("doc.tif", ViewFormat::Tiff),
    ] {
        assert_eq!(
            write_back(name, &read(picture(), format, Unkept::default())),
            Ok(()),
            "{name}"
        );
    }
    assert_eq!(
        write_back(
            "art.webp",
            &read(picture(), ViewFormat::Webp, Unkept::default())
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
    for format in SaveFormat::ALL {
        let made = Document::new_as(Picture::plain(stripes(false)), format);
        assert_eq!(natural(made.entries(), made.origin()), format, "made as it");
    }
    let two = Document::of(
        vec![
            Entry::Picture(Picture::plain(stripes(false))),
            Entry::Picture(Picture::plain(stripes(true))),
        ],
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    assert_eq!(natural(two.entries(), two.origin()), SaveFormat::Sprites);
}

/// A save is held to the endings of the format chosen, its first the one a
/// name given none takes; a format that cannot hold the document has none.
#[test]
fn a_documents_save_endings_are_those_of_the_format_chosen() {
    let listed = |document: &Document, format| -> Vec<String> {
        save_endings(document.entries(), document.origin(), format)
            .expect("the format holds it")
            .iter()
            .map(String::from)
            .collect()
    };
    let one = Document::new(Picture::plain(stripes(false)));
    assert_eq!(listed(&one, SaveFormat::Png), [".png", ",b60"]);
    assert_eq!(
        listed(&one, SaveFormat::Tiff)[0],
        ".tiff",
        "the registry's first"
    );
    for format in SaveFormat::ALL {
        for ending in listed(&one, format) {
            assert_eq!(
                format_named(&alloc::format!("a{ending}")),
                Ok(Some(format)),
                "{ending}"
            );
        }
        assert_eq!(
            format_named(&alloc::format!("a.{}", format.extension())),
            Ok(Some(format))
        );
    }
    let two = Document::of(
        vec![
            Entry::Picture(Picture::plain(stripes(false))),
            Entry::Picture(Picture::plain(stripes(true))),
        ],
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    assert_eq!(
        save_endings(two.entries(), two.origin(), SaveFormat::Png),
        Err(SaveRefusal::SeveralPictures(2))
    );
    assert_eq!(listed(&two, SaveFormat::Sprites), [".spr", ",ff9"]);
}

/// A document no format can hold opens no picker: the refusal is said.
#[test]
fn a_document_no_format_holds_has_no_save_endings() {
    let area = read(vec![kept(46)], ViewFormat::Sprite, Unkept::default());
    assert_eq!(
        save_endings(area.entries(), area.origin(), SaveFormat::Sprites),
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
    assert!(lost_in(&transparent, SaveFormat::Jpeg)
        .expect("room")
        .is_some());
    assert_eq!(lost_in(&transparent, SaveFormat::Png).expect("room"), None);
    let opaque = snapshot_of(&Document::new(Picture::plain(stripes(false))));
    assert_eq!(lost_in(&opaque, SaveFormat::Jpeg).expect("room"), None);
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
    assert!(lost_in(&tall, SaveFormat::Png).expect("room").is_some());
    assert!(lost_in(&tall, SaveFormat::Jpeg).expect("room").is_some());
    assert_eq!(lost_in(&tall, SaveFormat::Sprites).expect("room"), None);
}

fn colour_layer(colour: [u8; 4], name: &str, opacity: u8, visible: bool) -> crate::document::Layer {
    let canvas = Canvas::new(3, 2, Kind::Rgba, Sample::Rgba(colour)).expect("fits");
    let mut layer = crate::document::Layer::new(canvas, String::from(name));
    layer.opacity = opacity;
    layer.visible = visible;
    layer
}

fn three_layers() -> Picture {
    let layers = vec![
        colour_layer([0, 0, 255, 255], "Ground", 255, true),
        colour_layer([255, 0, 0, 255], "Glow & <haze>", 128, true),
        colour_layer([0, 255, 0, 255], "Off", 255, false),
    ];
    Picture::layered(layers, 1).expect("alike")
}

fn near(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&p, &q)| p.abs_diff(q) <= 1)
}

#[test]
fn a_picture_of_layers_is_kept_layer_for_layer_in_openraster() {
    let doc = Document::new_as(three_layers(), SaveFormat::OpenRaster);
    let snapshot = snapshot_of(&doc);
    assert_eq!(losses(&snapshot, SaveFormat::OpenRaster).expect("room"), []);
    let bytes = encode(&snapshot, SaveFormat::OpenRaster, "a.ora").expect("encodes");
    let Ok(NativeDocument::Layers { document, unkept }) =
        open_native(ImageFormat::OpenRaster, &bytes, &LIMITS)
    else {
        panic!("an OpenRaster file of layers");
    };
    assert_eq!(unkept, Unkept::default(), "nothing beside them");
    assert_eq!((document.width, document.height), (3, 2));
    let names: Vec<&str> = document
        .layers
        .iter()
        .map(|layer| layer.name.as_str())
        .collect();
    assert_eq!(names, ["Ground", "Glow & <haze>", "Off"]);
    assert!(document.layers[1].opacity.abs_diff(128) <= 1);
    assert!(!document.layers[2].visible);
    let merged =
        tairix_image::decode_as(ImageFormat::OpenRaster, &bytes, &LIMITS).expect("decodes");
    assert!(
        near(&merged.pixels()[..4], &[128, 0, 127, 255]),
        "{:?}",
        &merged.pixels()[..4]
    );
}

#[test]
fn a_picture_of_layers_is_written_as_they_show_in_a_format_of_one() {
    let doc = Document::new(three_layers());
    let snapshot = snapshot_of(&doc);
    assert_eq!(
        losses(&snapshot, SaveFormat::Png).expect("room"),
        [Loss::Layers]
    );
    assert_eq!(
        lost_in(&snapshot, SaveFormat::Png).expect("room"),
        Some(Loss::Layers.message())
    );
    let written = native(
        ImageFormat::Png,
        &encode(&snapshot, SaveFormat::Png, "a.png").expect("encodes"),
    );
    let Pixels::Rgba(pixels) = written.pixels() else {
        panic!("colour");
    };
    assert!(
        near(&pixels[..4], &[128, 0, 127, 255]),
        "{:?}",
        &pixels[..4]
    );
}

#[test]
fn openraster_holds_one_picture_in_colour() {
    assert_eq!(format_named("layers.ora"), Ok(Some(SaveFormat::OpenRaster)));
    assert_eq!(SaveFormat::OpenRaster.extension(), "ora");
    assert!(
        SaveFormat::OpenRaster.admits(None)
            && !SaveFormat::OpenRaster.admits(Some(IndexDepth::Four))
    );
    let palette = snapshot_of(&Document::new(Picture::plain(stripes(false))));
    assert_eq!(
        losses(&palette, SaveFormat::OpenRaster).expect("room"),
        [Loss::Palette]
    );
    let several = vec![
        Entry::Picture(Picture::plain(stripes(false))),
        Entry::Picture(Picture::plain(stripes(false))),
    ];
    assert_eq!(
        format_for("pair.ora", &several, Origin::New(SaveFormat::Png)),
        Err(SaveRefusal::SeveralPictures(2))
    );
    let read = read(
        vec![Entry::Picture(three_layers())],
        ViewFormat::OpenRaster,
        Unkept::default(),
    );
    assert_eq!(
        write_back("back.ora", &read),
        Ok(()),
        "written back as it was read"
    );
}

/// The Save As sheet offers each format with what it would lose, worked out
/// once for every format the document can be written as.
#[test]
fn the_survey_offers_every_writable_format_with_its_losses() {
    let doc = Document::new(three_layers());
    let offered = super::survey(doc.entries(), doc.current(), doc.origin()).expect("room");
    let formats: Vec<SaveFormat> = offered.iter().map(|(format, _)| *format).collect();
    assert_eq!(
        formats,
        SaveFormat::ALL,
        "a colour picture of layers may be written as any"
    );
    for (format, lost) in &offered {
        let flattened = lost.contains(&Loss::Layers);
        assert_eq!(flattened, *format != SaveFormat::OpenRaster, "{format:?}");
        let alone = super::losses_of(doc.entries(), doc.current(), *format).expect("room");
        assert_eq!(
            lost, &alone,
            "{format:?}: the survey says what a save would"
        );
    }
}
