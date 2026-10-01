//! The validation every encoder shares: each refuses a palette its depth
//! cannot hold and an index past the palette alike.

use super::{encode_jpeg, encode_png, encode_sprite_area, EncodeError, JpegOptions};
use crate::{IndexDepth, PictureKind, PictureSource, Rgba8, SpriteInput, SpriteMode};
use crate::{SpriteName, SpritePalette};

/// A two-pixel indexed picture of `depth` over `palette`, its row `row`.
struct Lying {
    depth: IndexDepth,
    palette: &'static [Rgba8],
    row: [u8; 2],
}

impl PictureSource for Lying {
    fn width(&self) -> u32 {
        2
    }

    fn height(&self) -> u32 {
        1
    }

    fn kind(&self) -> PictureKind<'_> {
        PictureKind::Indexed {
            depth: self.depth,
            palette: self.palette,
            masked: false,
        }
    }

    fn read_row(&self, _: u32, samples: &mut [u8], _: &mut [u8]) {
        samples.copy_from_slice(&self.row);
    }
}

/// What each of the three encoders answers for `picture`.
fn every_encoder(picture: &Lying) -> [Result<(), EncodeError>; 3] {
    let jpeg = JpegOptions::new(JpegOptions::DEFAULT_QUALITY, [255; 3]).expect("a quality");
    let sprite = SpriteInput::Picture {
        name: SpriteName::new("lying").expect("a name"),
        mode: SpriteMode::from_value(0).expect("mode 0"),
        palette: &SpritePalette::Full,
        masked: false,
        source: picture,
    };
    [
        encode_png(picture).map(drop),
        encode_jpeg(picture, jpeg).map(drop),
        encode_sprite_area(&[sprite]).map(drop),
    ]
}

const BLACK: Rgba8 = [0, 0, 0, 255];

#[test]
fn an_index_past_the_palette_is_refused_by_every_encoder() {
    let picture = Lying {
        depth: IndexDepth::One,
        palette: &[BLACK],
        row: [0, 1],
    };
    assert_eq!(
        every_encoder(&picture),
        [Err(EncodeError::IndexOutOfRange); 3]
    );
}

#[test]
fn a_picture_with_no_colours_is_refused_by_every_encoder() {
    let picture = Lying {
        depth: IndexDepth::One,
        palette: &[],
        row: [0, 0],
    };
    assert_eq!(
        every_encoder(&picture),
        [Err(EncodeError::InvalidPalette); 3]
    );
}

#[test]
fn a_palette_longer_than_its_depth_is_refused_by_every_encoder() {
    let picture = Lying {
        depth: IndexDepth::One,
        palette: &[BLACK, BLACK, BLACK],
        row: [0, 2],
    };
    assert_eq!(
        every_encoder(&picture),
        [Err(EncodeError::InvalidPalette); 3]
    );
}

#[test]
fn a_palette_its_depth_holds_is_written_by_every_encoder() {
    let picture = Lying {
        depth: IndexDepth::One,
        palette: &[BLACK, [255; 4]],
        row: [0, 1],
    };
    assert_eq!(every_encoder(&picture), [Ok(()); 3]);
}
