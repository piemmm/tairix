extern crate std;

use std::vec;
use std::vec::Vec;

use super::writer::RangeEncoder;
use super::{Coder, RangeDecoder};

/// One symbol of each kind the decoder reads.
#[derive(Clone, Copy, Debug)]
enum Symbol {
    Frequency { low: u32, high: u32, total: u32 },
    Binary { low: u32, high: u32, bits: u32 },
    Bit { set: bool, logp: u32 },
    Inverse { symbol: usize },
    Uniform { value: u32, total: u32 },
    Raw { value: u32, count: u32 },
}

const ICDF: [u8; 5] = [200, 140, 60, 12, 0];

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound
    }
}

fn symbols(seed: u64, count: usize) -> Vec<Symbol> {
    let mut rng = Xorshift(seed);
    (0..count)
        .map(|_| match rng.below(6) {
            0 => {
                let total = 2 + rng.below(30000);
                let low = rng.below(total);
                let high = low + 1 + rng.below(total - low);
                Symbol::Frequency { low, high, total }
            }
            1 => {
                let bits = 1 + rng.below(15);
                let low = rng.below(1 << bits);
                let high = low + 1 + rng.below((1 << bits) - low);
                Symbol::Binary { low, high, bits }
            }
            2 => Symbol::Bit {
                set: rng.below(3) == 0,
                logp: 1 + rng.below(15),
            },
            3 => Symbol::Inverse {
                symbol: rng.below(u32::try_from(ICDF.len()).expect("a short table")) as usize,
            },
            4 => {
                let total = 2 + if rng.below(2) == 0 {
                    rng.below(255)
                } else {
                    rng.next() >> 1
                };
                Symbol::Uniform {
                    value: rng.below(total),
                    total,
                }
            }
            _ => {
                let count = 1 + rng.below(24);
                Symbol::Raw {
                    value: rng.next() & ((1 << count) - 1),
                    count,
                }
            }
        })
        .collect()
}

fn write(symbols: &[Symbol]) -> (Vec<u8>, u32) {
    let mut frame = vec![0u8; 4096];
    let mut encoder = RangeEncoder::new(&mut frame);
    for &symbol in symbols {
        match symbol {
            Symbol::Frequency { low, high, total } => encoder.take(low, high, total),
            Symbol::Binary { low, high, bits } => encoder.take(low, high, 1 << bits),
            Symbol::Bit { set, logp } => {
                encoder.bit_logp(set, logp);
            }
            Symbol::Inverse { symbol } => {
                encoder.icdf(symbol, &ICDF, 8);
            }
            Symbol::Uniform { value, total } => {
                encoder.uint(value, total);
            }
            Symbol::Raw { value, count } => {
                encoder.bits(value, count);
            }
        }
    }
    let range = encoder.range();
    assert!(encoder.finish(), "fits");
    (frame, range)
}

#[test]
fn every_symbol_kind_reads_back_as_written_and_the_final_states_agree() {
    for seed in 1..40 {
        let written = symbols(seed, 300);
        let (frame, range) = write(&written);
        let mut decoder = RangeDecoder::new(&frame);
        for (index, &symbol) in written.iter().enumerate() {
            match symbol {
                Symbol::Frequency { low, high, total } => {
                    let f = decoder.decode(total);
                    assert!((low..high).contains(&f), "seed {seed} symbol {index}");
                    decoder.update(low, high, total);
                }
                Symbol::Binary { low, high, bits } => {
                    let f = decoder.decode_bin(bits);
                    assert!((low..high).contains(&f), "seed {seed} symbol {index}");
                    decoder.update(low, high, 1 << bits);
                }
                Symbol::Bit { set, logp } => assert_eq!(decoder.bit_logp(logp), set),
                Symbol::Inverse { symbol } => assert_eq!(decoder.icdf(&ICDF, 8), symbol),
                Symbol::Uniform { value, total } => assert_eq!(decoder.uint(total), value),
                Symbol::Raw { value, count } => assert_eq!(decoder.bits(count), value),
            }
        }
        assert_eq!(decoder.range(), range, "seed {seed}");
        assert!(decoder.tell() <= i32::try_from(8 * frame.len()).expect("a short frame"));
    }
}

#[test]
fn the_fractional_count_brackets_the_whole_one() {
    let written = symbols(7, 400);
    let (frame, _) = write(&written);
    let mut decoder = RangeDecoder::new(&frame);
    assert_eq!(decoder.tell(), 1);
    for _ in 0..200 {
        decoder.bit_logp(3);
        let (whole, eighths) = (decoder.tell(), decoder.tell_frac());
        assert!(
            eighths <= whole * 8 && eighths > (whole - 1) * 8,
            "{whole} {eighths}"
        );
    }
}

#[test]
fn a_read_past_the_frame_yields_zeros_and_a_value_out_of_range_reads_as_the_largest() {
    let mut empty = RangeDecoder::new(&[]);
    assert_eq!(empty.bits(16), 0);
    assert!(!empty.bit_logp(1));
    for _ in 0..64 {
        empty.icdf(&ICDF, 8);
    }
    assert!(empty.tell() > 0);

    let mut frame = [0u8; 8];
    let mut encoder = RangeEncoder::new(&mut frame);
    encoder.take(255, 256, 256);
    encoder.bits(0x3FF, 10);
    assert!(encoder.finish(), "fits");
    let mut decoder = RangeDecoder::new(&frame);
    assert_eq!(decoder.uint(0x3_FF00), 0x3_FEFF);
}

/// A frame coded right up to its last bit, its final raw bits sharing the
/// range coder's last byte, still finishes and reads back.
#[test]
fn a_frame_filled_to_its_last_bit_finishes_and_reads_back() {
    let mut rng = Xorshift(11);
    for len in 1..48 {
        let mut frame = vec![0u8; len];
        let mut encoder = RangeEncoder::new(&mut frame);
        let budget = 8 * i32::try_from(len).expect("a short frame");
        let mut written = Vec::new();
        loop {
            let raw = rng.below(2) == 0;
            let count = if raw { 1 + rng.below(3) } else { 1 };
            if encoder.tell() + i32::try_from(count).expect("a few bits") > budget {
                break;
            }
            let value = rng.next() & ((1 << count) - 1);
            if raw {
                encoder.bits(value, count);
            } else {
                encoder.bit_logp(value != 0, 1);
            }
            written.push((raw, count, value));
        }
        assert!(encoder.finish(), "a frame of {len}");
        let mut decoder = RangeDecoder::new(&frame);
        for &(raw, count, value) in &written {
            let read = if raw {
                decoder.bits(count)
            } else {
                u32::from(decoder.bit_logp(1))
            };
            assert_eq!(read, value, "a frame of {len}");
        }
    }
}

#[test]
fn symbols_outrunning_their_frame_are_refused() {
    let mut frame = [0u8; 2];
    let mut encoder = RangeEncoder::new(&mut frame);
    for _ in 0..64 {
        encoder.bit_logp(true, 15);
    }
    assert!(!encoder.finish());
}
