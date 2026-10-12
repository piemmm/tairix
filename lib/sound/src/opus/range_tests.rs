extern crate std;

use std::vec::Vec;

use super::writer::RangeEncoder;
use super::RangeDecoder;

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
    let mut encoder = RangeEncoder::new();
    for &symbol in symbols {
        match symbol {
            Symbol::Frequency { low, high, total } => encoder.encode(low, high, total),
            Symbol::Binary { low, high, bits } => encoder.encode_bin(low, high, bits),
            Symbol::Bit { set, logp } => encoder.bit_logp(set, logp),
            Symbol::Inverse { symbol } => encoder.icdf(symbol, &ICDF, 8),
            Symbol::Uniform { value, total } => encoder.uint(value, total),
            Symbol::Raw { value, count } => encoder.bits(value, count),
        }
    }
    let range = encoder.range();
    (encoder.finish(4096).expect("fits"), range)
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

    let mut encoder = RangeEncoder::new();
    encoder.encode(255, 256, 256);
    encoder.bits(0x3FF, 10);
    let frame = encoder.finish(8).expect("fits");
    let mut decoder = RangeDecoder::new(&frame);
    assert_eq!(decoder.uint(0x3_FF00), 0x3_FEFF);
}
