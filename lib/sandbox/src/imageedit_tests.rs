//! The edit decode: every document kind read back through a loopback worker
//! exactly as its file stores it, every refusal, and every reply a parent
//! must not believe.

use alloc::vec;
use alloc::vec::Vec;

use tairix_image::{
    encode_gif, encode_jpeg, encode_png, encode_sprite_area, encode_tiff, DecodeError, Density,
    DensityUnit, GifOptions, IndexDepth, JpegOptions, Picture, PictureSource, Rgba8, SpriteInput,
    SpriteMode, SpriteName, SpritePalette, TiffCompression, TiffOptions, Unkept, Written,
};
use tairix_log::Sink;

use super::{
    close_edit, open_edit, read_kept, read_rows, select_entry, EditDocument, EditEntry,
    EditFailure, EditKind, EditPicture, EditPixels, EditRefusal, EditSprite, KeptReason,
    MAX_EDIT_SIDE, OP_EDIT_OPEN, OP_EDIT_ROWS, OP_EDIT_SELECT,
};
use crate::host::ParserSandbox;
use crate::imagerender::{
    send_document, upload_document, ImageRenderService, UploadFailure, ViewFormat,
};
use crate::loopback::LoopbackLauncher;
use crate::testing::{loopback, tampering, NullSink};
use crate::wire::Writer;
use crate::worker::Service;

type TestSandbox = ParserSandbox<LoopbackLauncher<fn() -> ImageRenderService>, NullSink>;

fn sandbox() -> TestSandbox {
    loopback()
}

fn palette(count: u8) -> Vec<Rgba8> {
    (0..count)
        .map(|i| [i, i.wrapping_mul(2), 255 - i, 255])
        .collect()
}

fn indexed_png() -> (Vec<u8>, Vec<u8>) {
    let indices: Vec<u8> = (0..24).map(|i| i % 5).collect();
    let picture =
        Picture::indexed(6, 4, IndexDepth::Four, palette(5), indices.clone(), None).expect("valid");
    (encode_png(&picture).expect("encodes"), indices)
}

/// Every row of `picture` as one buffer of samples and one of alpha.
fn collect<L: crate::host::Launcher, S: Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    picture: &EditPicture,
) -> Result<(Vec<u8>, Vec<u8>), EditFailure> {
    let (mut samples, mut mask) = (Vec::new(), Vec::new());
    let mut next = 0u32;
    read_rows(sandbox, picture, |y, line, alpha| {
        assert_eq!(y, next, "rows arrive in order");
        next += 1;
        samples.extend_from_slice(line);
        mask.extend_from_slice(alpha);
    })?;
    Ok((samples, mask))
}

fn picture(entry: EditEntry) -> EditPicture {
    match entry {
        EditEntry::Picture(picture) => picture,
        EditEntry::Kept(kept) => panic!("kept: {kept:?}"),
    }
}

#[test]
fn a_paletted_png_arrives_as_its_indices_and_palette() {
    let (png, indices) = indexed_png();
    let mut sandbox = sandbox();
    send_document(&mut sandbox, &png).expect("uploads");
    let document = open_edit(&mut sandbox, None).expect("opens");
    assert_eq!(document.format, ViewFormat::Png);
    assert_eq!(
        (
            document.kind,
            document.count,
            document.unkept,
            document.written
        ),
        (EditKind::Single, 1, Unkept::default(), Written::Plain)
    );
    let entry = picture(select_entry(&mut sandbox, document, 0).expect("selects"));
    assert_eq!((entry.width, entry.height), (6, 4));
    assert_eq!(
        entry.pixels,
        EditPixels::Indexed {
            depth: IndexDepth::Four,
            palette: palette(5),
            plane: false
        }
    );
    assert!(entry.sprite.is_none());
    let (samples, mask) = collect(&mut sandbox, &entry).expect("rows");
    assert_eq!(samples, indices);
    assert!(mask.is_empty());
    close_edit(&mut sandbox).expect("closes");
}

#[test]
fn a_truecolour_picture_arrives_as_rgba() {
    let rgba: Vec<u8> = (0..32u8).flat_map(|i| [i, 200, 7, 100 + i]).collect();
    let png = encode_png(&Picture::rgba(8, 4, rgba.clone()).expect("valid")).expect("encodes");
    let mut sandbox = sandbox();
    send_document(&mut sandbox, &png).expect("uploads");
    let document = open_edit(&mut sandbox, Some(ViewFormat::Png)).expect("opens");
    let entry = picture(select_entry(&mut sandbox, document, 0).expect("selects"));
    assert_eq!(entry.pixels, EditPixels::Rgba);
    assert_eq!(collect(&mut sandbox, &entry).expect("rows").0, rgba);
}

/// An area of a masked paletted sprite, a truecolour one, and one the
/// decoder cannot read.
fn area() -> (Vec<u8>, Vec<u8>) {
    let desktop: Vec<Rgba8> = tairix_image::desktop_palette(IndexDepth::Four)
        .iter()
        .map(|&[r, g, b]| [r, g, b, 255])
        .collect();
    let paletted = Picture::indexed(
        4,
        2,
        IndexDepth::Four,
        desktop,
        vec![0, 7, 11, 15, 1, 2, 3, 4],
        Some(vec![255, 0, 255, 255, 0, 0, 255, 255]),
    )
    .expect("valid");
    let truecolour = Picture::rgba(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).expect("valid");
    // A JPEG-type sprite: a control block the decoder refuses by type.
    let mut kept = vec![0u8; 48];
    kept[0..4].copy_from_slice(&48u32.to_le_bytes());
    kept[4..8].copy_from_slice(b"jpeg");
    kept[32..36].copy_from_slice(&44u32.to_le_bytes());
    kept[36..40].copy_from_slice(&44u32.to_le_bytes());
    let jpeg_mode: u32 = (9 << 27) | (90 << 14) | (90 << 1) | 1;
    kept[40..44].copy_from_slice(&jpeg_mode.to_le_bytes());
    let bytes = encode_sprite_area(&[
        SpriteInput::Picture {
            name: SpriteName::new("icon").expect("valid"),
            mode: SpriteMode::from_value(12).expect("mode 12"),
            palette: &SpritePalette::Implied,
            masked: true,
            source: &paletted,
        },
        SpriteInput::Picture {
            name: SpriteName::new("photo").expect("valid"),
            mode: SpriteMode::truecolour((1, 1), false),
            palette: &SpritePalette::Implied,
            masked: false,
            source: &truecolour,
        },
        SpriteInput::Opaque(&kept),
    ])
    .expect("encodes");
    (bytes, kept)
}

#[test]
fn a_sprite_area_arrives_sprite_by_sprite_with_its_details() {
    let (bytes, kept_bytes) = area();
    let mut sandbox = sandbox();
    send_document(&mut sandbox, &bytes).expect("uploads");
    let document = open_edit(&mut sandbox, Some(ViewFormat::Sprite)).expect("opens");
    assert_eq!(document.kind, EditKind::Sprites);
    assert_eq!(document.count, 3);

    let icon = picture(select_entry(&mut sandbox, document, 0).expect("selects"));
    let details = icon.sprite.clone().expect("a sprite's details");
    assert_eq!(details.name.as_bytes(), b"icon");
    assert_eq!(details.mode.value(), 12);
    assert!(details.masked);
    assert_eq!(details.palette, SpritePalette::Implied);
    let (indices, mask) = collect(&mut sandbox, &icon).expect("rows");
    assert_eq!(indices, [0, 7, 11, 15, 1, 2, 3, 4]);
    assert_eq!(mask, [255, 0, 255, 255, 0, 0, 255, 255]);

    let photo = picture(select_entry(&mut sandbox, document, 1).expect("selects"));
    assert_eq!(photo.pixels, EditPixels::Rgba);
    assert_eq!(
        collect(&mut sandbox, &photo).expect("rows").0,
        [1, 2, 3, 255, 4, 5, 6, 255]
    );

    let EditEntry::Kept(kept) = select_entry(&mut sandbox, document, 2).expect("selects") else {
        panic!("the JPEG sprite is kept");
    };
    assert_eq!(kept.reason, KeptReason::UnsupportedType);
    assert_eq!(kept.name.as_bytes(), b"jpeg");
    let mut out = Vec::new();
    read_kept(&mut sandbox, &kept, &mut out).expect("fetches");
    assert_eq!(out, kept_bytes);
}

#[test]
fn a_file_is_streamed_in_pieces_and_counted() {
    let (png, indices) = indexed_png();
    let mut sandbox = sandbox();
    let mut reads = 0;
    upload_document(&mut sandbox, png.len(), |offset, into| {
        reads += 1;
        let at = usize::try_from(offset).expect("small");
        let take = into.len().min(png.len() - at).min(7);
        into[..take].copy_from_slice(&png[at..at + take]);
        Ok::<usize, ()>(take)
    })
    .expect("uploads");
    assert!(reads > 1, "short reads are carried on from");
    let document = open_edit(&mut sandbox, None).expect("opens");
    let entry = picture(select_entry(&mut sandbox, document, 0).expect("selects"));
    assert_eq!(collect(&mut sandbox, &entry).expect("rows").0, indices);
}

#[test]
fn a_file_that_shrinks_beneath_its_upload_is_refused() {
    let mut sandbox = sandbox();
    let shrank = upload_document(&mut sandbox, 64, |offset, into| {
        Ok::<usize, ()>(if offset == 0 { into.len().min(16) } else { 0 })
    });
    assert_eq!(shrank, Err(UploadFailure::Shrank));
    let failed = upload_document(&mut sandbox, 64, |_, _| Err::<usize, u8>(7));
    assert_eq!(failed, Err(UploadFailure::Read(7)));
}

#[test]
fn every_step_out_of_order_is_refused() {
    let mut sandbox = sandbox();
    assert_eq!(
        open_edit(&mut sandbox, None),
        Err(EditFailure::Refused(EditRefusal::NoDocument))
    );
    let unopened = EditDocument {
        format: ViewFormat::Sprite,
        kind: EditKind::Sprites,
        count: 3,
        unkept: Unkept::default(),
        written: Written::Plain,
        canvas: None,
    };
    assert_eq!(
        select_entry(&mut sandbox, unopened, 0),
        Err(EditFailure::Refused(EditRefusal::NotOpen))
    );
    let (bytes, _) = area();
    send_document(&mut sandbox, &bytes).expect("uploads");
    let document = open_edit(&mut sandbox, Some(ViewFormat::Sprite)).expect("opens");
    let stranger = EditPicture {
        width: 4,
        height: 2,
        pixels: EditPixels::Rgba,
        sprite: None,
        density: None,
        layer: None,
    };
    assert_eq!(
        collect(&mut sandbox, &stranger),
        Err(EditFailure::Refused(EditRefusal::NoEntry))
    );
    assert_eq!(
        select_entry(&mut sandbox, document, 3),
        Err(EditFailure::Refused(EditRefusal::NoSuchEntry))
    );
    let EditEntry::Kept(kept) = select_entry(&mut sandbox, document, 2).expect("selects") else {
        panic!("kept");
    };
    assert_eq!(
        collect(&mut sandbox, &stranger),
        Err(EditFailure::Refused(EditRefusal::WrongKind))
    );
    select_entry(&mut sandbox, document, 1).expect("selects");
    assert_eq!(
        read_kept(&mut sandbox, &kept, &mut Vec::new()),
        Err(EditFailure::Refused(EditRefusal::WrongKind))
    );
    close_edit(&mut sandbox).expect("closes");
    assert_eq!(
        select_entry(&mut sandbox, document, 0),
        Err(EditFailure::Refused(EditRefusal::NotOpen))
    );
}

#[test]
fn a_document_that_is_no_picture_or_is_vector_art_is_refused() {
    let mut sandbox = sandbox();
    send_document(&mut sandbox, b"not a picture at all").expect("uploads");
    assert_eq!(
        open_edit(&mut sandbox, None),
        Err(EditFailure::Refused(EditRefusal::UnsupportedFormat))
    );
    send_document(&mut sandbox, b"<svg/>").expect("uploads");
    assert_eq!(
        open_edit(&mut sandbox, Some(ViewFormat::Svg)),
        Err(EditFailure::Refused(EditRefusal::UnsupportedFormat))
    );
}

#[test]
fn rows_past_one_reply_or_the_picture_are_refused_by_the_worker() {
    let (png, _) = indexed_png();
    let mut service = ImageRenderService::default();
    let mut begin = Writer::new();
    begin.u8(5);
    begin.u64(png.len() as u64);
    service.handle(&begin.finish());
    let mut push = Writer::new();
    push.u8(6);
    push.bytes(&png);
    service.handle(&push.finish());
    service.handle(&[13, 0]);
    let mut select = Writer::new();
    select.u8(OP_EDIT_SELECT);
    select.u32(0);
    service.handle(&select.finish());
    for (first, count) in [(0, 0), (3, 2), (u32::MAX, 2)] {
        let mut w = Writer::new();
        w.u8(OP_EDIT_ROWS);
        w.u32(first);
        w.u32(count);
        assert_eq!(
            service.handle(&w.finish()),
            vec![
                crate::imagerender::REPLY_ERROR,
                EditRefusal::RowsOutOfRange.to_wire()
            ]
        );
    }
}

/// Upload the paletted PNG to a worker that corrupts `op`'s reply, and read
/// the whole of it, answering the first failure.
fn drive_tampered(op: u8, tamper: fn(Vec<u8>) -> Vec<u8>) -> Result<(), EditFailure> {
    let mut sandbox = tampering::<ImageRenderService>(op, tamper);
    let (png, _) = indexed_png();
    send_document(&mut sandbox, &png)?;
    let document = open_edit(&mut sandbox, None)?;
    let entry = picture(select_entry(&mut sandbox, document, 0)?);
    collect(&mut sandbox, &entry).map(|_| ())
}

#[test]
fn an_honest_worker_is_believed() {
    assert_eq!(drive_tampered(OP_EDIT_SELECT, |reply| reply), Ok(()));
}

#[test]
fn a_description_with_more_colours_than_its_depth_is_not_believed() {
    // Bytes 0..5 are the tag and index, 5 the kind, 6..14 the geometry, and
    // 14 the depth: claiming one bit leaves five colours with nowhere to be.
    let result = drive_tampered(OP_EDIT_SELECT, |mut reply| {
        reply[14] = 1;
        reply
    });
    assert_eq!(result, Err(EditFailure::ReplyMalformed));
}

#[test]
fn a_zero_sided_picture_is_not_believed() {
    let result = drive_tampered(OP_EDIT_SELECT, |mut reply| {
        reply[6..10].copy_from_slice(&0u32.to_le_bytes());
        reply
    });
    assert_eq!(result, Err(EditFailure::ReplyMalformed));
}

#[test]
fn a_row_naming_a_colour_past_the_palette_is_not_believed() {
    let result = drive_tampered(OP_EDIT_ROWS, |mut reply| {
        // The samples follow the tag, the echoed range and their length.
        reply[13] = 9;
        reply
    });
    assert_eq!(result, Err(EditFailure::ReplyMalformed));
}

#[test]
fn a_band_echoing_another_range_is_not_believed() {
    let result = drive_tampered(OP_EDIT_ROWS, |mut reply| {
        reply[1] = reply[1].wrapping_add(1);
        reply
    });
    assert_eq!(result, Err(EditFailure::ReplyMalformed));
}

#[test]
fn a_short_band_is_not_believed() {
    let result = drive_tampered(OP_EDIT_ROWS, |mut reply| {
        reply.pop();
        reply
    });
    assert_eq!(result, Err(EditFailure::ReplyMalformed));
}

#[test]
fn a_sprite_whose_mode_contradicts_its_pixels_is_not_believed() {
    let (bytes, _) = area();
    let mut sandbox = tampering::<ImageRenderService>(OP_EDIT_SELECT, |mut reply| {
        // Mode 12 is sixteen colours; mode 0 is two, which the sixteen-colour
        // picture described cannot be.
        let at = reply
            .windows(4)
            .position(|window| window == 12u32.to_le_bytes())
            .expect("the mode word");
        reply[at] = 0;
        reply
    });
    send_document(&mut sandbox, &bytes).expect("uploads");
    let document = open_edit(&mut sandbox, Some(ViewFormat::Sprite)).expect("opens");
    assert_eq!(
        select_entry(&mut sandbox, document, 0),
        Err(EditFailure::ReplyMalformed)
    );
}

#[test]
fn an_entry_is_held_to_the_document_it_belongs_to() {
    let (png, _) = indexed_png();
    let mut sandbox = sandbox();
    send_document(&mut sandbox, &png).expect("uploads");
    let single = open_edit(&mut sandbox, None).expect("opens");
    let claimed_area = EditDocument {
        kind: EditKind::Sprites,
        format: ViewFormat::Sprite,
        ..single
    };
    assert_eq!(
        select_entry(&mut sandbox, claimed_area, 0),
        Err(EditFailure::ReplyMalformed),
        "a sprite area's picture without its sprite details"
    );

    let (bytes, _) = area();
    let mut sandbox = self::sandbox();
    send_document(&mut sandbox, &bytes).expect("uploads");
    let sprites = open_edit(&mut sandbox, Some(ViewFormat::Sprite)).expect("opens");
    let claimed_single = EditDocument {
        kind: EditKind::Single,
        format: ViewFormat::Png,
        ..sprites
    };
    assert_eq!(
        select_entry(&mut sandbox, claimed_single, 0),
        Err(EditFailure::ReplyMalformed),
        "sprite details on a single picture"
    );
    let mut sandbox = self::sandbox();
    send_document(&mut sandbox, &bytes).expect("uploads");
    open_edit(&mut sandbox, Some(ViewFormat::Sprite)).expect("opens");
    assert_eq!(
        select_entry(&mut sandbox, claimed_single, 2),
        Err(EditFailure::ReplyMalformed),
        "a kept sprite outside a sprite area"
    );
}

/// A plain JPEG, and the same with a comment segment after its start.
fn jpegs() -> (Vec<u8>, Vec<u8>) {
    let picture = Picture::rgba(4, 4, vec![200; 64]).expect("valid");
    let options = JpegOptions::new(90, [255; 3]).expect("a quality");
    let plain = encode_jpeg(&picture, options).expect("encodes");
    let mut commented = plain[..2].to_vec();
    commented.extend_from_slice(&[0xFF, 0xFE, 0, 5, b'h', b'i', b'!']);
    commented.extend_from_slice(&plain[2..]);
    (plain, commented)
}

/// What a file held beside its picture is said with the document, so an
/// editor knows that writing the picture back would lose it.
#[test]
fn a_file_holding_more_than_its_picture_says_so() {
    let (plain, commented) = jpegs();
    for (bytes, extras) in [(plain, false), (commented, true)] {
        let mut sandbox = sandbox();
        send_document(&mut sandbox, &bytes).expect("uploads");
        let document = open_edit(&mut sandbox, None).expect("opens");
        assert_eq!(document.format, ViewFormat::Jpeg);
        assert_eq!(
            document.unkept,
            Unkept {
                extras,
                ..Unkept::default()
            }
        );
    }
}

/// A loss the document's format cannot have is not believed: a JPEG is
/// never narrowed, a sprite area keeps all it holds, and a PNG never
/// restates its colours; nor is a record of how it was written that its
/// format does not make.
#[test]
fn a_loss_its_format_cannot_have_is_not_believed() {
    // The open reply: its tag, format, kind, count, three losses, and how it
    // was written.
    let narrowed: fn(Vec<u8>) -> Vec<u8> = |mut reply| {
        reply[7] = 1;
        reply
    };
    let beside: fn(Vec<u8>) -> Vec<u8> = |mut reply| {
        reply[8] = 1;
        reply
    };
    let converted: fn(Vec<u8>) -> Vec<u8> = |mut reply| {
        reply[9] = 1;
        reply
    };
    let as_gif: fn(Vec<u8>) -> Vec<u8> = |mut reply| {
        reply[10] = 1;
        reply.push(0);
        reply
    };
    let (plain, _) = jpegs();
    let (area, _) = area();
    let (png, _) = indexed_png();
    for (bytes, format, tamper) in [
        (plain, ViewFormat::Jpeg, narrowed),
        (area, ViewFormat::Sprite, beside),
        (png.clone(), ViewFormat::Png, converted),
        (png, ViewFormat::Png, as_gif),
    ] {
        let mut sandbox = tampering::<ImageRenderService>(OP_EDIT_OPEN, tamper);
        send_document(&mut sandbox, &bytes).expect("uploads");
        assert_eq!(
            open_edit(&mut sandbox, Some(format)),
            Err(EditFailure::ReplyMalformed),
            "{format:?}"
        );
    }
}

#[test]
fn a_single_picture_said_to_be_of_the_sprite_format_is_not_believed() {
    let mut sandbox = tampering::<ImageRenderService>(OP_EDIT_OPEN, |mut reply| {
        reply[1] = ViewFormat::Sprite.to_wire();
        reply
    });
    let (png, _) = indexed_png();
    send_document(&mut sandbox, &png).expect("uploads");
    assert_eq!(
        open_edit(&mut sandbox, None),
        Err(EditFailure::ReplyMalformed)
    );
}

/// A document opened as a named format is that format: a reply naming
/// another is not believed, however well formed.
#[test]
fn a_document_answered_as_another_format_than_asked_is_not_believed() {
    let mut sandbox = tampering::<ImageRenderService>(OP_EDIT_OPEN, |mut reply| {
        reply[1] = ViewFormat::Jpeg.to_wire();
        reply
    });
    let (png, _) = indexed_png();
    send_document(&mut sandbox, &png).expect("uploads");
    assert_eq!(
        open_edit(&mut sandbox, Some(ViewFormat::Png)),
        Err(EditFailure::ReplyMalformed)
    );
}

#[test]
fn a_picture_breaking_the_edit_bounds_cannot_be_made() {
    let two = |palette: usize, plane: bool| EditPixels::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0; 4]; palette],
        plane,
    };
    assert!(EditPicture::new(1, 1, EditPixels::Rgba, None).is_some());
    assert!(EditPicture::new(0, 1, EditPixels::Rgba, None).is_none());
    assert!(EditPicture::new(1, 0, EditPixels::Rgba, None).is_none());
    assert!(EditPicture::new(MAX_EDIT_SIDE + 1, 1, EditPixels::Rgba, None).is_none());
    assert!(EditPicture::new(MAX_EDIT_SIDE, MAX_EDIT_SIDE, EditPixels::Rgba, None).is_none());
    assert!(
        EditPicture::new(1, 1, two(0, false), None).is_none(),
        "no colours"
    );
    assert!(
        EditPicture::new(1, 1, two(3, false), None).is_none(),
        "past its depth"
    );
    let sprite = |mode: SpriteMode, masked: bool| {
        Some(EditSprite {
            name: SpriteName::new("one").expect("a name"),
            mode,
            masked,
            palette: SpritePalette::Full,
        })
    };
    let one_bit = SpriteMode::indexed(IndexDepth::One, (1, 1), false);
    assert!(EditPicture::new(1, 1, two(2, false), sprite(one_bit, false)).is_some());
    assert!(
        EditPicture::new(1, 1, two(2, true), sprite(one_bit, false)).is_none(),
        "a plane its sprite has no mask for"
    );
    let four_bit = SpriteMode::indexed(IndexDepth::Four, (1, 1), false);
    assert!(
        EditPicture::new(1, 1, two(2, false), sprite(four_bit, false)).is_none(),
        "a mode of another depth"
    );
    let truecolour = SpriteMode::truecolour((1, 1), false);
    assert!(
        EditPicture::new(1, 1, EditPixels::Rgba, sprite(truecolour, false)).is_none(),
        "a direct-colour sprite stating a palette"
    );
}

#[test]
fn a_decoders_error_is_told_as_what_it_amounts_to() {
    for (err, refusal) in [
        (DecodeError::UnknownFormat, EditRefusal::UnsupportedFormat),
        (DecodeError::OutOfMemory, EditRefusal::OutOfMemory),
        (DecodeError::PixelCountExceedsLimit, EditRefusal::TooLarge),
        (DecodeError::DimensionsOverflow, EditRefusal::TooLarge),
        (
            DecodeError::JpegProgressiveCoefficientStoreExceedsLimit,
            EditRefusal::TooLarge,
        ),
        (
            DecodeError::ChunkCrcMismatch,
            EditRefusal::MalformedDocument,
        ),
    ] {
        assert_eq!(EditRefusal::of_decode(&err), refusal, "{err:?}");
    }
}

#[test]
fn a_gif_arrives_as_its_indices_and_how_it_was_written() {
    let indices: Vec<u8> = (0..12).map(|i| i % 4).collect();
    let source =
        Picture::indexed(4, 3, IndexDepth::Two, palette(4), indices.clone(), None).expect("valid");
    let gif = encode_gif(&source, GifOptions { interlaced: true }).expect("encodes");
    let mut sandbox = sandbox();
    send_document(&mut sandbox, &gif).expect("uploads");
    let document = open_edit(&mut sandbox, None).expect("opens");
    assert_eq!(document.format, ViewFormat::Gif);
    assert_eq!(
        document.written,
        Written::Gif(GifOptions { interlaced: true })
    );
    let entry = picture(select_entry(&mut sandbox, document, 0).expect("selects"));
    assert_eq!(collect(&mut sandbox, &entry).expect("rows").0, indices);
}

#[test]
fn a_tiff_arrives_page_by_page() {
    let first = Picture::indexed(
        3,
        2,
        IndexDepth::Four,
        palette(16),
        vec![0, 1, 2, 3, 4, 5],
        None,
    )
    .expect("valid");
    let rgba: Vec<u8> = (0..8u8).flat_map(|i| [i, 9, 9, 255]).collect();
    let second = Picture::rgba(4, 2, rgba.clone())
        .expect("valid")
        .with_density(Density::whole(300, 300, DensityUnit::Inch));
    let pages: [&dyn PictureSource; 2] = [&first, &second];
    let tiff = encode_tiff(
        &pages,
        TiffOptions {
            compression: TiffCompression::PackBits,
        },
    )
    .expect("encodes");
    let mut sandbox = sandbox();
    send_document(&mut sandbox, &tiff).expect("uploads");
    let document = open_edit(&mut sandbox, None).expect("opens");
    assert_eq!(
        (document.format, document.kind, document.count),
        (ViewFormat::Tiff, EditKind::Pages, 2)
    );
    assert_eq!(
        document.written,
        Written::Tiff(TiffOptions {
            compression: TiffCompression::PackBits
        })
    );
    let page = picture(select_entry(&mut sandbox, document, 0).expect("selects"));
    assert!(page.sprite().is_none());
    assert_eq!(
        collect(&mut sandbox, &page).expect("rows").0,
        [0, 1, 2, 3, 4, 5]
    );
    let page = picture(select_entry(&mut sandbox, document, 1).expect("selects"));
    assert_eq!(page.density(), Density::whole(300, 300, DensityUnit::Inch));
    assert_eq!(collect(&mut sandbox, &page).expect("rows").0, rgba);
    assert_eq!(
        select_entry(&mut sandbox, document, 2),
        Err(EditFailure::Refused(EditRefusal::NoSuchEntry))
    );
}

#[test]
fn a_density_with_a_zero_figure_is_not_believed() {
    let picture = Picture::rgba(1, 1, vec![1, 2, 3, 255])
        .expect("valid")
        .with_density(Density::whole(72, 72, DensityUnit::Inch));
    let png = encode_png(&picture).expect("encodes");
    // The select reply's density follows the tag, index, kind, geometry,
    // depth, empty palette, plane and sprite flags, its own flag and unit.
    let mut sandbox = tampering::<ImageRenderService>(OP_EDIT_SELECT, |mut reply| {
        reply[23..27].fill(0);
        reply
    });
    send_document(&mut sandbox, &png).expect("uploads");
    let document = open_edit(&mut sandbox, None).expect("opens");
    assert_eq!(
        select_entry(&mut sandbox, document, 0),
        Err(EditFailure::ReplyMalformed)
    );
}

/// A layered document: two layers, the bottom whole and the top a smaller,
/// offset, faded one, with a name past what the wire carries.
fn layered() -> Vec<u8> {
    use tairix_image::{encode_ora, OraLayerSource};
    let long = "n".repeat(super::MAX_LAYER_NAME + 9);
    let ground = Picture::rgba(5, 4, vec![10; 80]).expect("valid");
    let top = Picture::rgba(2, 3, vec![200; 24]).expect("valid");
    let layers = [
        OraLayerSource {
            name: "ground",
            picture: &ground,
            at: (0, 0),
            opacity: 255,
            visible: true,
        },
        OraLayerSource {
            name: &long,
            picture: &top,
            at: (-1, 2),
            opacity: 100,
            visible: false,
        },
    ];
    encode_ora((5, 4), &layers, &ground, &ground).expect("encodes")
}

#[test]
fn a_layered_document_claiming_more_layers_than_are_read_is_not_believed() {
    let mut sandbox = tampering::<ImageRenderService>(OP_EDIT_OPEN, |mut reply| {
        let count = u32::try_from(tairix_image::MOST_ORA_LAYERS + 1).expect("small");
        reply[3..7].copy_from_slice(&count.to_le_bytes());
        reply
    });
    send_document(&mut sandbox, &layered()).expect("uploads");
    assert_eq!(
        open_edit(&mut sandbox, None),
        Err(EditFailure::ReplyMalformed)
    );
}

#[test]
fn a_layered_document_arrives_layer_by_layer_with_its_canvas() {
    let mut sandbox = sandbox();
    send_document(&mut sandbox, &layered()).expect("uploads");
    let document = open_edit(&mut sandbox, None).expect("opens");
    assert_eq!(document.format, ViewFormat::OpenRaster);
    assert_eq!((document.kind, document.count), (EditKind::Layers, 2));
    assert_eq!(document.canvas, Some((5, 4)));
    assert!(document.unkept.extras, "the long name is cut on the wire");
    let ground = picture(select_entry(&mut sandbox, document, 0).expect("selects"));
    assert_eq!(
        ground.layer().map(|layer| layer.name.as_str()),
        Some("ground")
    );
    let (samples, _) = collect(&mut sandbox, &ground).expect("rows");
    assert_eq!(samples, vec![10; 80]);
    let top = picture(select_entry(&mut sandbox, document, 1).expect("selects"));
    let layer = top.layer().expect("a layer");
    assert_eq!(layer.name.len(), super::MAX_LAYER_NAME);
    assert_eq!(
        (layer.at, layer.opacity, layer.visible),
        ((-1, 2), 100, false)
    );
    assert_eq!((top.width(), top.height()), (2, 3));
    let claimed_single = EditDocument {
        kind: EditKind::Single,
        format: ViewFormat::Png,
        canvas: None,
        ..document
    };
    assert_eq!(
        select_entry(&mut sandbox, claimed_single, 0),
        Err(EditFailure::ReplyMalformed),
        "layer details on a single picture"
    );
}
