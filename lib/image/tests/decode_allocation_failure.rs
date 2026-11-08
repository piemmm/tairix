//! Pictures decoded as the heap runs out.
//!
//! The shared metered allocator refuses, on the calling thread only, every
//! request past a chosen count, so a decode can be made to fail at each
//! request it makes in turn: each refusal is answered as
//! [`DecodeError::OutOfMemory`] — never an abort, which is what one
//! allocation that cannot refuse would make of it — and nothing the decode
//! held outlives it.

use tairix_fuzzseed::meter::refused_at_each;
use tairix_image::{
    decode, decode_sprite, encode_jpeg, encode_png, encode_sprite, DecodeError, DecodeLimits,
    JpegOptions, Picture, SpriteInput, SpriteMode, SpriteName, SpritePalette,
};

#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

const LIMITS: DecodeLimits = DecodeLimits::new(256, 256, 1 << 16, 1 << 20);

/// A picture of translucent colours, so every encoder has work to do.
fn picture() -> Picture {
    let mut rgba = Vec::new();
    for y in 0..9_u8 {
        for x in 0..13_u8 {
            rgba.extend_from_slice(&[x * 19, y * 27, 140, 40 + x * 10]);
        }
    }
    Picture::rgba(13, 9, rgba).expect("a picture")
}

fn out_of_memory(error: &DecodeError) -> bool {
    *error == DecodeError::OutOfMemory
}

#[test]
fn a_png_a_jpeg_and_a_sprite_decoded_as_the_heap_runs_out_are_refused() {
    let picture = picture();
    let png = encode_png(&picture).expect("a png");
    let options = JpegOptions::new(80, [255, 255, 255]).expect("options");
    let jpeg = encode_jpeg(&picture, options).expect("a jpeg");
    let sprite = encode_sprite(&SpriteInput::Picture {
        name: SpriteName::new("picture").expect("a name"),
        mode: SpriteMode::truecolour((1, 1), true),
        palette: &SpritePalette::Implied,
        masked: true,
        source: &picture,
    })
    .expect("a sprite");
    refused_at_each(|| decode(&png, &LIMITS), out_of_memory);
    refused_at_each(|| decode(&jpeg, &LIMITS), out_of_memory);
    refused_at_each(|| decode_sprite(&sprite, &LIMITS), out_of_memory);
}
