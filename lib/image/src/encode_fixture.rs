//! Pictures the encoder tests build and the readings they compare.

use alloc::vec::Vec;

use crate::{
    open_native, DecodeLimits, ImageFormat, NativeDocument, Picture, Rgba8, Unkept, Written,
};

/// A `width` by `height` RGBA picture, each pixel `pixel(x, y)`.
pub(crate) fn rgba(width: u32, height: u32, mut pixel: impl FnMut(u32, u32) -> Rgba8) -> Picture {
    let mut bytes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            bytes.extend_from_slice(&pixel(x, y));
        }
    }
    Picture::rgba(width, height, bytes).expect("valid")
}

/// A small deterministic generator, so a noise picture is the same every run.
pub(crate) struct Noise(pub(crate) u64);

impl Noise {
    pub(crate) fn next(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        u8::try_from(self.0 >> 56).expect("the top byte")
    }
}

/// `rgba` as it looks: a fully transparent pixel shows nothing, whatever
/// colour it was left holding.
pub(crate) fn shown(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&pixel| if pixel[3] == 0 { [0; 4] } else { pixel })
        .collect()
}

/// `bytes` opened as `format`'s one picture, with what it did not keep and
/// how it was written.
pub(crate) fn native(
    format: ImageFormat,
    bytes: &[u8],
    limits: &DecodeLimits,
) -> (Picture, Unkept, Written) {
    match open_native(format, bytes, limits).expect("the file opens") {
        NativeDocument::Picture {
            picture,
            unkept,
            written,
            ..
        } => (picture, unkept, written),
        NativeDocument::Pages { .. }
        | NativeDocument::Sprites(_)
        | NativeDocument::Layers { .. } => panic!("one picture"),
    }
}
