//! A picture in a drawing: where its pixels land, what it may hold, how one
//! document's pictures are decoded once, and what a reader keeps of the bytes
//! each was read from.

use alloc::sync::Arc;
use alloc::vec;

use tairix_bezier::{Bounds, Point};
use tairix_collections::SharedSlice;
use tairix_image::{DecodeError, JpegOptions, Orientation, Picture as Raw};
use tairix_raster::{Color, Pyramid, Surface};

use super::{
    decode_encoded, decode_sprite, fits_within, survey, Encoding, Image, Pictures, Source, Survey,
    DECODE_LIMITS,
};
use crate::limits::{MAX_COORDINATE, MAX_IMAGE_SOURCE_BYTES};
use crate::{DrawingError, ReadError};
use tairix_image::MAX_SPRITE_EIG;

/// A `width`×`height` picture whose every pixel is `colour`.
fn filled(width: u32, height: u32, colour: Color) -> Surface {
    let mut surface = Surface::new(width, height).expect("a surface");
    surface.fill(colour);
    surface
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Bounds {
    Bounds::from_points([Point::new(x0, y0), Point::new(x1, y1)])
}

/// `pixels` as straight RGBA8 rows.
fn rgba(pixels: &Surface) -> Raw {
    let mut bytes = vec![0; pixels.pixels().len() * 4];
    tairix_raster::unpremultiply_span(pixels.pixels(), bytes.as_chunks_mut::<4>().0);
    Raw::rgba(pixels.width(), pixels.height(), bytes).expect("a picture")
}

#[test]
fn a_picture_fills_its_rectangle_pixel_by_pixel() {
    let image = Image::new(
        filled(4, 2, Color::rgb(9, 8, 7)),
        rect(10.0, 20.0, 50.0, 30.0),
        None,
    )
    .expect("a picture");
    let to_place = image.to_place();
    assert_eq!(to_place.map(Point::ZERO), Point::new(10.0, 20.0));
    assert_eq!(to_place.map(Point::new(4.0, 2.0)), Point::new(50.0, 30.0));
    assert!(image.covers(Point::new(49.0, 29.0)));
    assert!(!image.covers(Point::new(51.0, 25.0)));
    assert_eq!(image.outline().expect("its frame").bounds(), image.place());
    assert_eq!(image.pyramid().depth(), 3, "4×2, 2×1, 1×1");
}

#[test]
fn a_picture_placed_again_keeps_its_pixels_and_its_source() {
    let source = Source {
        encoding: Encoding::Png,
        bytes: SharedSlice::try_from_slice(b"\x89PNG").expect("bytes"),
    };
    let image = Image::new(
        filled(2, 2, Color::rgb(1, 2, 3)),
        rect(0.0, 0.0, 2.0, 2.0),
        Some(source.clone()),
    )
    .expect("a picture");
    let moved = image.placed(rect(5.0, 5.0, 9.0, 7.0)).expect("placed");
    assert!(Arc::ptr_eq(image.pyramid(), moved.pyramid()));
    assert_eq!(moved.source(), Some(&source));
    assert_ne!(moved, image, "a picture elsewhere is another");
    assert_eq!(moved.placed(image.place()).expect("back"), image);
}

#[test]
fn the_same_pixels_from_two_decodes_are_the_same_picture() {
    let place = rect(0.0, 0.0, 3.0, 3.0);
    let one = Image::new(filled(3, 3, Color::rgb(4, 5, 6)), place, None).expect("one");
    let two = Image::new(filled(3, 3, Color::rgb(4, 5, 6)), place, None).expect("two");
    assert!(!Arc::ptr_eq(one.pyramid(), two.pyramid()));
    assert_eq!(one, two);
    let other = Image::new(filled(3, 3, Color::rgb(4, 5, 7)), place, None).expect("other");
    assert_ne!(one, other);
}

#[test]
fn a_rectangle_a_drawing_cannot_hold_is_refused() {
    let pixels = || filled(2, 2, Color::rgb(0, 0, 0));
    for place in [
        rect(0.0, 0.0, 0.0, 5.0),
        rect(0.0, 0.0, 5.0, 0.0),
        rect(0.0, 0.0, MAX_COORDINATE * 2.0, 5.0),
        rect(f64::NAN, 0.0, 1.0, 1.0),
    ] {
        assert_eq!(
            Image::new(pixels(), place, None).err(),
            Some(DrawingError::Invalid),
            "{place:?}"
        );
    }
}

#[test]
fn a_source_whose_facts_the_pixels_contradict_is_refused() {
    let jpeg = |stored, components, tag| Source {
        encoding: Encoding::Jpeg(tairix_image::JpegFrame {
            stored,
            components,
            progressive: false,
            orientation: Orientation::from_tag(tag).expect("defined"),
            density: None,
        }),
        bytes: SharedSlice::try_from_slice(&[0xFF, 0xD8]).expect("bytes"),
    };
    let place = rect(0.0, 0.0, 4.0, 2.0);
    let make = |source| Image::new(filled(4, 2, Color::rgb(1, 1, 1)), place, Some(source));
    assert!(make(jpeg((4, 2), 3, 1)).is_ok());
    assert!(
        make(jpeg((2, 4), 1, 6)).is_ok(),
        "a stored raster its turn stands up"
    );
    assert_eq!(make(jpeg((2, 4), 3, 1)).err(), Some(DrawingError::Invalid));
    assert_eq!(make(jpeg((4, 2), 4, 1)).err(), Some(DrawingError::Invalid));
    let sprite = |eig| Source {
        encoding: Encoding::Sprite { eig },
        bytes: SharedSlice::try_from_slice(&[0; 44]).expect("bytes"),
    };
    assert!(make(sprite((MAX_SPRITE_EIG, 0))).is_ok());
    assert_eq!(
        make(sprite((MAX_SPRITE_EIG + 1, 0))).err(),
        Some(DrawingError::Invalid)
    );
    let empty = Source {
        encoding: Encoding::Png,
        bytes: SharedSlice::new(),
    };
    assert_eq!(make(empty).err(), Some(DrawingError::Invalid));
    let huge = Source {
        encoding: Encoding::Png,
        bytes: SharedSlice::try_from_vec(vec![0; MAX_IMAGE_SOURCE_BYTES + 1]).expect("bytes"),
    };
    assert_eq!(make(huge).err(), Some(DrawingError::TooComplex));
}

#[test]
fn a_shared_pyramid_is_laid_again_without_its_pixels_being_copied() {
    let pyramid = Arc::new(Pyramid::new(filled(2, 2, Color::rgb(3, 3, 3))).expect("levels"));
    let a = Image::shared(pyramid.clone(), rect(0.0, 0.0, 1.0, 1.0), None).expect("a");
    let b = Image::shared(pyramid.clone(), rect(4.0, 4.0, 9.0, 9.0), None).expect("b");
    assert!(Arc::ptr_eq(a.pyramid(), b.pyramid()));
    assert_eq!(Arc::strong_count(&pyramid), 3);
}

#[test]
fn a_survey_says_what_pixels_a_writer_must_carry() {
    let mut pixels = filled(3, 1, Color::rgb(7, 7, 7));
    assert_eq!(
        survey(&pixels),
        Survey {
            opaque: true,
            binary: true,
            grey: true
        }
    );
    pixels.set(0, 0, Color::rgba(0, 0, 0, 0).premultiply());
    assert_eq!(
        survey(&pixels),
        Survey {
            opaque: false,
            binary: true,
            grey: true
        }
    );
    pixels.set(1, 0, Color::rgba(200, 10, 10, 128).premultiply());
    assert_eq!(
        survey(&pixels),
        Survey {
            opaque: false,
            binary: false,
            grey: false
        }
    );
}

#[test]
fn a_png_and_a_jpeg_are_kept_with_their_bytes_and_anything_else_is_not_read() {
    let limits = DECODE_LIMITS;
    let pixels = filled(3, 2, Color::rgba(250, 20, 30, 255));
    let png = SharedSlice::try_from_vec(tairix_image::encode_png(&rgba(&pixels)).expect("a png"))
        .expect("bytes");
    let (decoded, source) = decode_encoded(png.clone(), &limits).expect("read");
    assert_eq!(decoded.pixels(), pixels.pixels(), "PNG is exact");
    assert_eq!(
        source,
        Some(Source {
            encoding: Encoding::Png,
            bytes: png
        })
    );

    let options = JpegOptions::new(95, [255, 255, 255]).expect("options");
    let jpeg = SharedSlice::try_from_vec(
        tairix_image::encode_jpeg(&rgba(&pixels), options).expect("a jpeg"),
    )
    .expect("bytes");
    let (decoded, source) = decode_encoded(jpeg.clone(), &limits).expect("read");
    assert_eq!((decoded.width(), decoded.height()), (3, 2));
    let Some(Source {
        encoding: Encoding::Jpeg(frame),
        bytes,
    }) = source
    else {
        panic!("a JPEG is kept as one: {source:?}");
    };
    assert_eq!(bytes, jpeg);
    assert_eq!((frame.stored, frame.components), ((3, 2), 3));

    let gif = SharedSlice::try_from_slice(b"GIF89a\x01\x00\x01\x00").expect("bytes");
    assert_eq!(
        decode_encoded(gif, &limits).err(),
        Some(DecodeError::UnknownFormat)
    );
}

#[test]
fn a_sprite_is_kept_as_its_own_bytes_with_the_size_of_its_pixels() {
    let pixels = filled(2, 2, Color::rgb(40, 50, 60));
    let source = rgba(&pixels);
    let sprite = tairix_image::encode_sprite(&tairix_image::SpriteInput::Picture {
        name: tairix_image::SpriteName::new("picture").expect("a name"),
        mode: tairix_image::SpriteMode::truecolour((2, 1), false),
        palette: &tairix_image::SpritePalette::Implied,
        masked: false,
        source: &source,
    })
    .expect("a sprite");
    let mut padded = sprite.clone();
    padded.extend_from_slice(&[0; 8]);
    let (decoded, kept) = decode_sprite(&padded, &DECODE_LIMITS).expect("read");
    assert_eq!(decoded.pixels(), pixels.pixels());
    let kept = kept.expect("kept");
    assert_eq!(kept.encoding, Encoding::Sprite { eig: (2, 1) });
    assert_eq!(
        &*kept.bytes,
        &sprite[..],
        "the sprite alone, not what follows it"
    );
}

#[test]
fn a_documents_picture_is_decoded_once_however_often_it_is_shown() {
    let mut pictures = Pictures::new();
    let mut decodes = 0;
    let key: &[u8] = b"one";
    for _ in 0..3 {
        let picture = pictures
            .picture(key, |_| {
                decodes += 1;
                Ok((filled(2, 2, Color::rgb(1, 1, 1)), None))
            })
            .expect("read")
            .expect("a picture");
        assert_eq!(picture.size(), (2, 2));
    }
    assert_eq!(decodes, 1);
    let first = pictures.picture(key, |_| unreachable!()).expect("read");
    let again = pictures.picture(key, |_| unreachable!()).expect("read");
    assert!(Arc::ptr_eq(
        &first.expect("one").pyramid,
        &again.expect("one").pyramid
    ));

    let unread = pictures
        .picture(b"broken", |_| Err(DecodeError::UnknownFormat))
        .expect("lost, not refused");
    assert!(unread.is_none());
    assert!(pictures
        .picture(b"broken", |_| unreachable!())
        .expect("remembered")
        .is_none());
    assert_eq!(
        pictures
            .picture(b"short", |_| Err(DecodeError::OutOfMemory))
            .err(),
        Some(ReadError::OutOfMemory)
    );
}

#[test]
fn a_document_whose_pictures_outgrow_a_drawing_is_too_complex() {
    let mut pictures = Pictures::new();
    pictures.pixels_left = 10;
    let pixels = filled(4, 4, Color::rgb(1, 2, 3));
    let png = SharedSlice::try_from_vec(tairix_image::encode_png(&rgba(&pixels)).expect("a png"))
        .expect("bytes");
    assert_eq!(
        pictures
            .picture(b"big", |limits| decode_encoded(png.clone(), limits))
            .err(),
        Some(ReadError::TooComplex)
    );
    pictures.pixels_left = 16;
    assert!(pictures
        .picture(b"fits", |limits| decode_encoded(png.clone(), limits))
        .expect("read")
        .is_some());
    assert_eq!(pictures.pixels_left, 0);
}

#[test]
fn which_pictures_a_document_loses_never_depends_on_their_order() {
    let mut pictures = Pictures::new();
    pictures.pixels_left = 1;
    let mut asked = None;
    let lost = pictures
        .picture(b"wide", |limits| {
            asked = Some(*limits);
            Err(DecodeError::WidthExceedsLimit)
        })
        .expect("lost, not refused");
    assert!(
        lost.is_none(),
        "too wide for any drawing, wherever it comes"
    );
    assert_eq!(
        asked,
        Some(DECODE_LIMITS),
        "however little the document has left"
    );
    let tall = pictures
        .picture(b"tall", |_| Err(DecodeError::HeightExceedsLimit))
        .expect("lost, not refused");
    assert!(tall.is_none());
    assert_eq!(
        pictures
            .picture(b"two", |_| Ok((filled(2, 1, Color::rgb(1, 2, 3)), None)))
            .err(),
        Some(ReadError::TooComplex),
        "past what the document's pictures may hold together"
    );
}

#[test]
fn a_placed_picture_spans_the_size_its_density_states() {
    use super::natural_size;
    use tairix_image::{Density, DensityUnit};
    assert_eq!(natural_size((300, 150), None), Point::new(300.0, 150.0));
    let inch = Density::whole(300, 150, DensityUnit::Inch);
    assert_eq!(natural_size((300, 150), inch), Point::new(96.0, 96.0));
    let centimetre = Density::whole(100, 100, DensityUnit::Centimetre);
    let width = natural_size((254, 254), centimetre).x;
    assert!((width - 96.0).abs() < 1e-9, "{width}");
    let shape = Density::whole(1, 2, DensityUnit::Aspect);
    assert_eq!(
        natural_size((10, 10), shape),
        Point::new(10.0, 5.0),
        "a pixel half as tall as it is wide"
    );
}

#[test]
fn a_density_that_would_lay_a_picture_past_a_drawing_leaves_it_its_own_pixels() {
    use super::{natural_size, placeable};
    use tairix_image::{Density, DensityUnit};
    let tall = Density::new((u32::MAX, 1), (1, 1), DensityUnit::Aspect);
    assert_eq!(natural_size((1, 1), tall), Point::new(1.0, 1.0));
    let sparse = Density::new((1, u32::MAX), (1, u32::MAX), DensityUnit::Inch);
    assert_eq!(natural_size((3, 2), sparse), Point::new(3.0, 2.0));
    assert!(placeable(natural_size((65_536, 65_536), None)));
    assert!(!placeable(Point::new(0.0, 4.0)) && !placeable(Point::new(f64::NAN, 4.0)));
}

#[test]
fn a_placed_picture_keeps_a_png_or_jpeg_as_it_came_and_any_other_as_its_pixels() {
    let pixels = filled(4, 2, Color::rgba(10, 200, 30, 255));
    let png = tairix_image::encode_png(&rgba(&pixels)).expect("a png");
    let placed = super::decode_placed(&png).expect("placed");
    assert_eq!(placed.pixels.pixels(), pixels.pixels());
    assert_eq!(placed.natural, Point::new(4.0, 2.0));
    assert_eq!(
        placed.source.map(|source| source.encoding),
        Some(Encoding::Png)
    );

    let bmp = tairix_image::encode_bmp(&rgba(&pixels)).expect("a bmp");
    let placed = super::decode_placed(&bmp).expect("placed");
    assert_eq!(placed.pixels.pixels(), pixels.pixels());
    assert!(placed.source.is_none(), "written again from its pixels");

    let sprite = tairix_image::encode_sprite_area(&[tairix_image::SpriteInput::Picture {
        name: tairix_image::SpriteName::new("one").expect("a name"),
        mode: tairix_image::SpriteMode::truecolour((2, 2), false),
        palette: &tairix_image::SpritePalette::Implied,
        masked: false,
        source: &rgba(&pixels),
    }])
    .expect("an area");
    let placed = super::decode_placed(&sprite).expect("placed");
    assert_eq!(
        placed.natural,
        Point::new(4.0 * 96.0 / 45.0, 2.0 * 96.0 / 45.0),
        "four OS units a pixel is forty-five to the inch"
    );

    assert_eq!(
        super::decode_placed(b"plain text").err(),
        Some(ReadError::Unrecognised)
    );
    assert_eq!(
        super::decode_placed(&png[..png.len() - 8]).err(),
        Some(ReadError::Malformed)
    );
}

/// A drawing of one layer holding `nodes`.
fn drawing_of(ids: &mut crate::Ids, nodes: alloc::vec::Vec<Arc<crate::Node>>) -> crate::Drawing {
    let layer = crate::Node::new(
        ids.issue(),
        crate::Kind::Group {
            children: crate::Children::from_nodes(nodes).expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    crate::Drawing::new(
        crate::Page {
            width: 100.0,
            height: 100.0,
            unit: crate::Unit::Px,
            background: None,
        },
        crate::Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        SharedSlice::new(),
        crate::Grid::default(),
    )
    .expect("a drawing")
}

#[test]
fn a_picture_is_counted_for_every_object_showing_it_and_once_against_the_bound() {
    use crate::testing::image as node_of;
    let mut ids = crate::Ids::new();
    let eight = Image::new(
        filled(4, 2, Color::rgb(1, 2, 3)),
        rect(0.0, 0.0, 4.0, 2.0),
        None,
    )
    .expect("eight pixels");
    let again = |ids: &mut crate::Ids| {
        node_of(
            ids,
            eight.placed(rect(9.0, 0.0, 13.0, 2.0)).expect("again"),
            tairix_bezier::Affine::IDENTITY,
        )
    };
    let shown = vec![again(&mut ids), again(&mut ids), again(&mut ids)];
    let drawing = drawing_of(&mut ids, shown);
    assert_eq!(drawing.layers().summary().picture_pixels, 24, "per object");
    assert!(
        fits_within(&drawing, &[], 24).expect("counted"),
        "within, counted per object"
    );
    assert!(
        fits_within(&drawing, &[again(&mut ids)], 8).expect("counted"),
        "shown more often, still one picture"
    );
    let other = node_of(
        &mut ids,
        Image::new(
            filled(2, 2, Color::rgb(4, 5, 6)),
            rect(0.0, 0.0, 2.0, 2.0),
            None,
        )
        .expect("four"),
        tairix_bezier::Affine::IDENTITY,
    );
    assert!(fits_within(&drawing, core::slice::from_ref(&other), 12).expect("counted"));
    assert!(
        !fits_within(&drawing, core::slice::from_ref(&other), 11).expect("counted"),
        "a picture of its own adds its pixels"
    );
}

#[test]
fn a_picture_a_clip_holds_is_counted_as_held() {
    use crate::testing::image as node_of;
    let mut ids = crate::Ids::new();
    let clip = node_of(
        &mut ids,
        Image::new(
            filled(3, 3, Color::rgb(1, 2, 3)),
            rect(0.0, 0.0, 3.0, 3.0),
            None,
        )
        .expect("nine"),
        tairix_bezier::Affine::IDENTITY,
    );
    let clipped = crate::Node::new(
        ids.issue(),
        crate::Kind::Group {
            children: crate::Children::new(),
            clip: Some(clip),
        },
    )
    .expect("a clipped group");
    assert_eq!(clipped.picture_pixels(), 9);
    let drawing = drawing_of(&mut ids, vec![Arc::new(clipped)]);
    assert!(fits_within(&drawing, &[], 9).expect("counted"));
    assert!(!fits_within(&drawing, &[], 8).expect("counted"));
}
