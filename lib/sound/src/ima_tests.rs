//! The block decoder against Python's `audioop`, an independent IMA ADPCM
//! decoder, over blocks synthesised here: compared at the start and through
//! FNV-1a over the whole block.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::{decode_block, frames_in, HEADER};
use crate::g72x::tests::{fnv, xorshift};
use crate::DecodeError;

fn header(predictor: i16, index: u8) -> [u8; HEADER] {
    let [low, high] = predictor.to_le_bytes();
    [low, high, index, 0]
}

fn mono_block() -> Vec<u8> {
    let mut block = header(1234, 20).to_vec();
    block.extend(xorshift(2044, 0x2545_F491_4F6C_DD1D));
    block
}

fn stereo_block() -> Vec<u8> {
    let mut block = header(-500, 5).to_vec();
    block.extend(header(3000, 60));
    block.extend(xorshift(2040, 0x9E37_79B9_7F4A_7C15));
    block
}

fn decode(block: &[u8], channels: usize, skip: usize, frames: usize) -> Vec<i16> {
    let mut out = vec![0u8; frames * 2 * channels];
    let written = decode_block(block, channels, skip, &mut out).expect("a block");
    out.as_chunks::<2>()
        .0
        .iter()
        .take(written * channels)
        .map(|pair| i16::from_le_bytes(*pair))
        .collect()
}

#[test]
fn blocks_decode_as_audioop_decodes_them() {
    let mono = decode(&mono_block(), 1, 0, 4089);
    assert_eq!(mono.len(), 4089);
    assert_eq!(
        mono[..10],
        [1234, 1191, 1196, 1272, 1261, 1291, 1373, 1318, 1168, 974]
    );
    assert_eq!(fnv(&mono), 0x5e61_d8c9_b21a_52dd);
    let stereo = decode(&stereo_block(), 2, 0, 2041);
    assert_eq!(stereo.len(), 4082);
    assert_eq!(
        stereo[..10],
        [-500, 3000, -519, 3284, -531, -73, -537, -6020, -551, -13314]
    );
    assert_eq!(fnv(&stereo), 0xe8d0_cff8_c8dd_082b);
}

#[test]
fn a_block_is_entered_where_asked_and_ends_where_the_room_does() {
    let whole = decode(&stereo_block(), 2, 0, 2041);
    let entered = decode(&stereo_block(), 2, 101, 50);
    assert_eq!(entered, whole[202..302]);
    let past = decode(&stereo_block(), 2, 2041, 10);
    assert!(past.is_empty(), "nothing past the block");
}

#[test]
fn a_block_holds_its_header_sample_then_eight_a_word() {
    assert_eq!(frames_in(2048, 1), Some(4089));
    assert_eq!(frames_in(2048, 2), Some(2041));
    assert_eq!(frames_in(4, 1), Some(1));
    assert_eq!(frames_in(3, 1), None, "short of its header");
    assert_eq!(frames_in(2050, 2), None, "not whole words");
}

#[test]
fn a_header_past_the_tables_or_short_of_itself_is_refused() {
    let mut out = [0u8; 64];
    let mut block = mono_block();
    block[2] = 89;
    assert_eq!(
        decode_block(&block, 1, 0, &mut out),
        Err(DecodeError::WavAdpcmBlockCorrupt)
    );
    assert_eq!(
        decode_block(&[0, 0, 0], 1, 0, &mut out),
        Err(DecodeError::WavAdpcmBlockCorrupt)
    );
}
