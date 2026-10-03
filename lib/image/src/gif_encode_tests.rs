//! The GIF encoder: every file read back through the crate's own decoder,
//! the palette and indices it keeps, the transparent entry it chooses, and
//! the coder's table filling and clearing across a long stream.

use alloc::vec;
use alloc::vec::Vec;

use crate::encode_fixture::{self, rgba, shown, Noise};
use crate::{
    decode_as, encode_gif, DecodeLimits, Density, DensityUnit, EncodeError, GifOptions,
    ImageFormat, IndexDepth, Picture, Pixels, Rgba8, Unkept, Written,
};

fn limits() -> DecodeLimits {
    DecodeLimits::new(4096, 4096, 4096 * 4096, 0)
}

const PLAIN: GifOptions = GifOptions { interlaced: false };
const INTERLACED: GifOptions = GifOptions { interlaced: true };

/// A palette of `count` distinct opaque colours.
fn colours(count: usize) -> Vec<Rgba8> {
    (0..count)
        .map(|at| {
            let at = u8::try_from(at).expect("at most 256");
            [at, at.wrapping_mul(7), at.wrapping_mul(13), 255]
        })
        .collect()
}

fn indexed(
    width: u32,
    height: u32,
    palette: Vec<Rgba8>,
    mask: Option<Vec<u8>>,
    mut index: impl FnMut(u32, u32) -> u8,
) -> Picture {
    let depth = IndexDepth::holding(palette.len()).expect("a palette");
    let indices = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| index(x, y))
        .collect();
    Picture::indexed(width, height, depth, palette, indices, mask).expect("valid")
}

/// Encode `picture`, check the file shows it, and answer the file and its
/// native reading.
fn round_trip(picture: &Picture, options: GifOptions) -> (Vec<u8>, Picture, Unkept, Written) {
    let gif = encode_gif(picture, options).expect("the picture encodes");
    let decoded = decode_as(ImageFormat::Gif, &gif, &limits()).expect("the file decodes");
    assert_eq!(
        shown(decoded.pixels()),
        shown(&picture.to_rgba().expect("memory")),
        "the file shows a different picture"
    );
    let (native, unkept, written) = encode_fixture::native(ImageFormat::Gif, &gif, &limits());
    (gif, native, unkept, written)
}

fn indices(picture: &Picture) -> &[u8] {
    match picture.pixels() {
        Pixels::Indexed { indices, .. } => indices,
        Pixels::Rgba(_) => panic!("a palette picture"),
    }
}

fn palette(picture: &Picture) -> &[Rgba8] {
    match picture.pixels() {
        Pixels::Indexed { palette, .. } => palette,
        Pixels::Rgba(_) => panic!("a palette picture"),
    }
}

#[test]
fn a_palette_picture_reads_back_as_its_indices_and_palette() {
    for count in [1, 2, 3, 4, 5, 16, 17, 255, 256] {
        let picture = indexed(13, 7, colours(count), None, |x, y| {
            u8::try_from((x * 5 + y * 3) as usize % count).expect("an index")
        });
        let (_, native, unkept, written) = round_trip(&picture, PLAIN);
        assert_eq!(unkept, Unkept::default(), "{count} colours");
        assert_eq!(written, Written::Gif(PLAIN));
        assert_eq!(indices(&native), indices(&picture), "{count} colours");
        // The table is padded to a power of two, never shorter than two.
        let table = count.next_power_of_two().max(2);
        assert_eq!(palette(&native).len(), table);
        assert_eq!(&palette(&native)[..count], palette(&picture));
    }
}

#[test]
fn an_interlaced_picture_reads_back_in_display_order_at_every_height() {
    for height in 1..=17 {
        let picture = indexed(3, height, colours(16), None, |x, y| {
            u8::try_from((x + y * 3) % 16).expect("an index")
        });
        let (_, native, _, written) = round_trip(&picture, INTERLACED);
        assert_eq!(written, Written::Gif(INTERLACED));
        assert_eq!(indices(&native), indices(&picture), "{height} rows");
    }
}

#[test]
fn a_long_stream_fills_and_clears_the_table_and_still_reads_back() {
    let mut noise = Noise(0x2545_F491_4F6C_DD1D);
    let picture = indexed(400, 300, colours(256), None, |_, _| noise.next());
    let (_, native, _, _) = round_trip(&picture, PLAIN);
    assert_eq!(indices(&native), indices(&picture));
    // Long runs grow the longest strings the table can hold.
    let runs = indexed(512, 512, colours(4), None, |x, y| {
        u8::try_from((x / 97 + y / 61) % 4).expect("an index")
    });
    let (_, native, _, _) = round_trip(&runs, INTERLACED);
    assert_eq!(indices(&native), indices(&runs));
}

#[test]
fn clear_pixels_take_a_clear_entry_no_shown_pixel_uses() {
    // The picture's own clear entry, which only clear pixels select.
    let mut own = colours(4);
    own[2] = [9, 9, 9, 0];
    let picture = indexed(4, 4, own, None, |x, _| u8::try_from(x).expect("an index"));
    let (_, native, _, _) = round_trip(&picture, PLAIN);
    assert_eq!(
        indices(&native),
        indices(&picture),
        "the clear entry is kept"
    );
    assert_eq!(palette(&native)[2], [9, 9, 9, 0]);

    // A mask with room in the palette: a new entry is appended.
    let mask = (0..16)
        .map(|at| if at % 3 == 0 { 0 } else { 255 })
        .collect();
    let picture = indexed(4, 4, colours(3), Some(mask), |x, _| {
        u8::try_from(x % 3).expect("an index")
    });
    let (_, native, _, _) = round_trip(&picture, PLAIN);
    assert_eq!(palette(&native)[3][3], 0, "the appended clear entry");
    for (at, &index) in indices(&native).iter().enumerate() {
        assert_eq!(index == 3, at % 3 == 0, "pixel {at}");
    }

    // A full palette: an entry no shown pixel uses becomes the clear one.
    let mask = (0..256).map(|at| if at == 0 { 0 } else { 255 }).collect();
    let picture = indexed(16, 16, colours(256), Some(mask), |x, y| {
        u8::try_from((x + y * 16).max(1) % 255 + 1).expect("an index")
    });
    let (_, native, _, _) = round_trip(&picture, PLAIN);
    assert_eq!(palette(&native)[0][3], 0, "entry 0 is shown by no pixel");
    assert_eq!(indices(&native)[0], 0);
}

#[test]
fn a_full_palette_every_entry_of_which_shows_leaves_no_room_for_a_clear_pixel() {
    let mask = (0..256 * 2)
        .map(|at| if at == 256 { 0 } else { 255 })
        .collect();
    let picture = indexed(256, 2, colours(256), Some(mask), |x, _| {
        u8::try_from(x).expect("an index")
    });
    assert_eq!(
        encode_gif(&picture, PLAIN),
        Err(EncodeError::GifPaletteFull)
    );
}

#[test]
fn a_half_transparent_pixel_shows_or_does_not_at_half_opacity() {
    let mask = vec![127, 128, 0, 255];
    let picture = indexed(4, 1, colours(2), Some(mask), |x, _| {
        u8::try_from(x % 2).expect("an index")
    });
    let gif = encode_gif(&picture, PLAIN).expect("encodes");
    let decoded = decode_as(ImageFormat::Gif, &gif, &limits()).expect("decodes");
    let alphas: Vec<u8> = decoded
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .collect();
    assert_eq!(alphas, [0, 255, 0, 255]);
}

#[test]
fn a_pixel_shows_by_its_entrys_opacity_seen_through_its_mask() {
    let mut palette = colours(2);
    palette[1][3] = 130;
    let mask = vec![130, 255, 200];
    let picture =
        Picture::indexed(3, 1, IndexDepth::One, palette, vec![1, 1, 0], Some(mask)).expect("valid");
    let gif = encode_gif(&picture, PLAIN).expect("encodes");
    let decoded = decode_as(ImageFormat::Gif, &gif, &limits()).expect("decodes");
    let alphas: Vec<u8> = decoded
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .collect();
    assert_eq!(alphas, [0, 255, 255], "130 of 130 is a quarter opaque");
}

#[test]
fn colour_is_not_a_gif() {
    let picture = rgba(2, 2, |_, _| [1, 2, 3, 255]);
    assert_eq!(encode_gif(&picture, PLAIN), Err(EncodeError::NotIndexed));
}

#[test]
fn a_gif_states_its_pixels_shape() {
    // The byte states a ratio in sixty-fourths, so a shape reads back in
    // those terms.
    let shape = |density| {
        let picture = indexed(2, 2, colours(2), None, |x, _| {
            u8::try_from(x).expect("an index")
        })
        .with_density(density);
        round_trip(&picture, PLAIN)
            .1
            .density()
            .and_then(|density| density.shape())
    };
    let tall = Density::whole(1, 2, DensityUnit::Aspect);
    assert_eq!(shape(tall), Some((1, 2)));
    assert_eq!(
        shape(Density::whole(4, 4, DensityUnit::Aspect)),
        None,
        "square"
    );
    assert_eq!(
        shape(Density::whole(300, 600, DensityUnit::Inch)),
        Some((1, 2)),
        "a physical density keeps only its shape"
    );
    assert_eq!(
        shape(Density::whole(100, 1, DensityUnit::Aspect)),
        None,
        "past the byte's range"
    );
    assert_eq!(shape(None), None);
}

/// A GIF of `blocks` after a two-colour screen two pixels square, its one
/// frame's indices 0, 1, 1, 0.
fn gif_with(blocks: &[&[u8]]) -> Vec<u8> {
    let mut gif = b"GIF89a\x02\x00\x02\x00\x80\x00\x00\x00\x00\x00\xFF\xFF\xFF".to_vec();
    for block in blocks {
        gif.extend_from_slice(block);
    }
    gif.push(0x3B);
    gif
}

/// One frame of the screen above, at `left`, `top`, `width` by `height`, its
/// pixels coded from `picture`'s first frame.
fn frame_of(left: u8, top: u8, width: u8, height: u8) -> Vec<u8> {
    let picture = indexed(
        u32::from(width),
        u32::from(height),
        colours(2),
        None,
        |x, y| u8::try_from((x + y) % 2).expect("an index"),
    );
    let encoded = encode_gif(&picture, PLAIN).expect("encodes");
    // Past the header, the screen descriptor, and the two-entry table, to
    // the image separator.
    let at = 6 + 7 + 6;
    let mut block = encoded[at..encoded.len() - 1].to_vec();
    block[1] = left;
    block[3] = top;
    block
}

#[test]
fn what_a_gif_holds_beside_its_first_frame_is_said_when_it_opens() {
    let frame = frame_of(0, 0, 2, 2);
    let held = |gif: &[u8]| encode_fixture::native(ImageFormat::Gif, gif, &limits()).1;
    assert_eq!(held(&gif_with(&[&frame])), Unkept::default());
    let beside = Unkept {
        extras: true,
        ..Unkept::default()
    };
    let comment: &[u8] = b"\x21\xFE\x04note\x00";
    assert_eq!(held(&gif_with(&[comment, &frame])), beside);
    assert_eq!(held(&gif_with(&[&frame, &frame])), beside, "a second frame");
    let looping: &[u8] = b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00";
    assert_eq!(
        held(&gif_with(&[looping, &frame])),
        Unkept::default(),
        "timing is moot"
    );
    let timed: &[u8] = b"\x21\xF9\x04\x08\x0A\x00\x00\x00";
    assert_eq!(held(&gif_with(&[timed, &frame])), Unkept::default());
    let other: &[u8] = b"\x21\xFF\x0BXMP DataXMP\x01x\x00";
    assert_eq!(held(&gif_with(&[other, &frame])), beside);
}

#[test]
fn plain_text_consumes_the_control_before_it_as_the_decoder_has_it() {
    let clear_one: &[u8] = b"\x21\xF9\x04\x01\x00\x00\x01\x00";
    let text: &[u8] = b"\x21\x01\x0Bhello world\x00";
    let gif = gif_with(&[clear_one, text, &frame_of(0, 0, 2, 2)]);
    let (picture, unkept, _) = encode_fixture::native(ImageFormat::Gif, &gif, &limits());
    assert!(unkept.extras);
    assert!(
        palette(&picture).iter().all(|entry| entry[3] == 255),
        "no entry is clear"
    );
    let decoded = decode_as(ImageFormat::Gif, &gif, &limits()).expect("decodes");
    assert_eq!(
        shown(decoded.pixels()),
        shown(&picture.to_rgba().expect("memory"))
    );
}

#[test]
fn a_frame_short_of_the_screen_opens_masked_beyond_it() {
    let gif = gif_with(&[&frame_of(1, 0, 1, 2)]);
    let (picture, unkept, _) = encode_fixture::native(ImageFormat::Gif, &gif, &limits());
    assert!(unkept.extras);
    let Pixels::Indexed {
        mask: Some(mask), ..
    } = picture.pixels()
    else {
        panic!("a masked palette picture");
    };
    assert_eq!(mask, &[0, 255, 0, 255]);
    let decoded = decode_as(ImageFormat::Gif, &gif, &limits()).expect("decodes");
    assert_eq!(
        shown(decoded.pixels()),
        shown(&picture.to_rgba().expect("memory")),
        "it opens as it looks"
    );
}

#[test]
fn a_transparent_index_past_the_table_opens_clear_and_no_other_index_past_it_does() {
    // Two entries; the control names index 3 transparent; the frame's pixels
    // are 0, 3, then 1 or 2.
    let frame = |last: u8| {
        let mut gif = b"GIF89a\x04\x00\x01\x00\x80\x00\x00\x00\x00\x00\xFF\xFF\xFF".to_vec();
        gif.extend_from_slice(b"\x21\xF9\x04\x01\x00\x00\x03\x00");
        let picture = indexed(4, 1, colours(4), None, |x, _| [0, 3, 1, last][x as usize]);
        let encoded = encode_gif(&picture, PLAIN).expect("encodes");
        // Past the header, the screen, and the four-entry table, to the frame.
        gif.extend_from_slice(&encoded[6 + 7 + 12..]);
        gif
    };
    let (picture, unkept, _) = encode_fixture::native(ImageFormat::Gif, &frame(1), &limits());
    assert!(unkept.extras, "the clear entry is the file's addition");
    assert_eq!(palette(&picture)[3][3], 0);
    let decoded = decode_as(ImageFormat::Gif, &frame(1), &limits()).expect("decodes");
    assert_eq!(
        shown(decoded.pixels()),
        shown(&picture.to_rgba().expect("memory"))
    );
    let past = frame(2);
    assert!(decode_as(ImageFormat::Gif, &past, &limits()).is_err());
    assert!(crate::open_native(ImageFormat::Gif, &past[..], &limits()).is_err());
}
