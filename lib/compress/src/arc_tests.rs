//! The ARC family's methods: every one decodes what its archivers write —
//! coded here as they code, by the steps each method names — everything the
//! encoder writes decodes back at every width, through its table filling and
//! clearing, and a stream out of shape is refused.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

/// Bytes a byte at a time.
struct Bytes<'a>(&'a [u8]);

impl Input for Bytes<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(first)
    }
}

/// What an encoder writes, gathered.
#[derive(Default)]
struct Gathered(Vec<u8>);

impl Output for Gathered {
    fn put(&mut self, bytes: &[u8]) -> bool {
        self.0.extend_from_slice(bytes);
        true
    }
}

/// `want` bytes of `stream` decoded as `method` says, read in pieces of
/// `piece`.
fn decode(stream: &[u8], method: Method, want: usize, piece: usize) -> Result<Vec<u8>, Error> {
    let mut decoder = Decoder::new(vec![0u32; DECODER_TABLE], vec![0u8; DECODER_STACK], method)?;
    let mut input = Bytes(stream);
    let mut out = vec![0u8; want];
    let mut made = 0;
    while made < want {
        let end = (made + piece).min(want);
        let got = decoder.read(&mut input, &mut out[made..end])?;
        if got == 0 {
            break;
        }
        made += got;
    }
    out.truncate(made);
    Ok(out)
}

fn encode(plain: &[u8], max_bits: u8) -> Vec<u8> {
    let mut encoder = Encoder::new(vec![0u32; ENCODER_WORK], max_bits).unwrap();
    let mut out = Gathered::default();
    for piece in plain.chunks(1000) {
        encoder.write(piece, &mut out).unwrap();
    }
    encoder.finish(&mut out).unwrap();
    out.0
}

/// Text drawn by a fixed generator, compressible as text is.
fn prose(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&[u8]; 6] = [
        b"spark ",
        b"archive ",
        b"riscos ",
        b"member ",
        b"filer ",
        b"!Boot\n",
    ];
    let mut state = seed;
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(WORDS[(state % 6) as usize]);
        if state.is_multiple_of(5) {
            out.extend_from_slice(&state.to_le_bytes()[..3]);
        }
    }
    out.truncate(len);
    out
}

/// RLE90, as an archiver packs: a run of three or more as the byte, the
/// marker and its count, the marker itself escaped.
fn pack(plain: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < plain.len() {
        let byte = plain[at];
        let run = plain[at..]
            .iter()
            .take(255)
            .take_while(|&&next| next == byte)
            .count();
        if byte == RUN {
            out.extend_from_slice(&[RUN, 0]);
            at += 1;
        } else if run >= 3 {
            out.extend_from_slice(&[byte, RUN, u8::try_from(run).unwrap()]);
            at += run;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    out
}

/// Squeezing, as an archiver squeezes: RLE90, then a Huffman tree built from
/// the packed bytes' counts and the end symbol's, its codes packed least
/// significant bit first.
fn squeeze(plain: &[u8]) -> Vec<u8> {
    let packed = pack(plain);
    let mut counts = vec![0u64; 257];
    for &byte in &packed {
        counts[usize::from(byte)] += 1;
    }
    counts[256] = 1;
    // Trees as (weight, node): a leaf is -(symbol + 1), an inner node its
    // index; inner nodes are numbered root first once all are joined.
    // Each tree as its weight and its root.
    let mut forest: Vec<(u64, i32)> = (0..257)
        .filter(|&symbol| counts[symbol] > 0)
        .map(|symbol| (counts[symbol], -1 - i32::try_from(symbol).unwrap()))
        .collect();
    let mut inner: Vec<(i32, i32)> = Vec::new();
    while forest.len() > 1 {
        forest.sort_by_key(|tree| core::cmp::Reverse(tree.0));
        let (light, lighter) = (forest.pop().unwrap(), forest.pop().unwrap());
        inner.push((lighter.1, light.1));
        let node = i32::try_from(inner.len() - 1).unwrap();
        forest.push((light.0 + lighter.0, node));
    }
    // Renumber so the root, joined last, is node 0.
    let count = inner.len();
    let renumber = |child: i32| {
        if child < 0 {
            child
        } else {
            i32::try_from(count - 1).unwrap() - child
        }
    };
    let nodes: Vec<(i32, i32)> = inner
        .iter()
        .rev()
        .map(|&(zero, one)| (renumber(zero), renumber(one)))
        .collect();
    let mut codes = vec![Vec::new(); 257];
    let mut stack = vec![(0i32, Vec::new())];
    while let Some((node, path)) = stack.pop() {
        let (zero, one) = nodes[usize::try_from(node).unwrap()];
        for (child, bit) in [(zero, false), (one, true)] {
            let mut path = path.clone();
            path.push(bit);
            if child < 0 {
                codes[usize::try_from(-child - 1).unwrap()] = path;
            } else {
                stack.push((child, path));
            }
        }
    }
    let mut out = u16::try_from(count).unwrap().to_le_bytes().to_vec();
    for &(zero, one) in &nodes {
        out.extend_from_slice(&i16::try_from(zero).unwrap().to_le_bytes());
        out.extend_from_slice(&i16::try_from(one).unwrap().to_le_bytes());
    }
    let (mut held, mut used) = (0u8, 0);
    for symbol in packed.iter().map(|&byte| usize::from(byte)).chain([256]) {
        for &bit in &codes[symbol] {
            held |= u8::from(bit) << used;
            used += 1;
            if used == 8 {
                out.push(held);
                (held, used) = (0, 0);
            }
        }
    }
    if used > 0 {
        out.push(held);
    }
    out
}

#[test]
fn every_method_the_family_reads_decodes_what_its_archivers_write() {
    let plain = {
        let mut text = prose(30_000, 11);
        // Runs for RLE90 to take, and its own marker byte.
        text.extend(vec![b'='; 700]);
        text.extend([RUN, RUN, b'x', RUN]);
        text
    };
    let lzw = |max_bits, rle| Method::Lzw { max_bits, rle };
    let cases: [(Vec<u8>, Method); 6] = [
        (pack(&plain), Method::Packed),
        (squeeze(&plain), Method::Squeezed),
        // ARC 5's crunching and ArcFS's: packing, then twelve-bit LZW.
        (encode(&pack(&plain), 12), lzw(12, true)),
        (encode(&pack(&plain), 16), lzw(16, true)),
        // PKARC's squashing; Spark's and ArcFS's compressing.
        (encode(&plain, 13), lzw(13, false)),
        (encode(&plain, 16), lzw(16, false)),
    ];
    for (at, (stream, method)) in cases.into_iter().enumerate() {
        for piece in [1, 7, 4096] {
            assert_eq!(
                decode(&stream, method, plain.len(), piece).unwrap(),
                plain,
                "case {at}"
            );
        }
    }
}

#[test]
fn what_the_encoder_writes_decodes_back_at_every_width() {
    let inputs: [Vec<u8>; 6] = [
        Vec::new(),
        vec![b'x'],
        prose(70_000, 1),
        (0..=255u8).cycle().take(5000).collect(),
        vec![0x90; 3000],
        tairix_test_noise(20_000),
    ];
    for max_bits in LZW_MIN_BITS..=LZW_MAX_BITS {
        for plain in &inputs {
            let stream = encode(plain, max_bits);
            let back = decode(
                &stream,
                Method::Lzw {
                    max_bits,
                    rle: false,
                },
                plain.len(),
                4093,
            )
            .unwrap();
            assert_eq!(&back, plain, "width {max_bits}, {} bytes", plain.len());
        }
    }
}

/// Bytes with no structure to find: a fixed generator's.
fn tairix_test_noise(len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn a_full_table_that_stops_paying_is_cleared_and_the_stream_still_decodes() {
    // Text fills a 9-bit table at once; noise after it compresses worse, so
    // the encoder clears, and the reader must follow it.
    let mut plain = prose(200_000, 7);
    plain.extend(tairix_test_noise(100_000));
    plain.extend(prose(100_000, 9));
    let stream = encode(&plain, 9);
    let clears = {
        let mut bits = Bits::default();
        let mut input = Bytes(&stream);
        let mut count = 0;
        // Codes before the first clear are all nine bits wide.
        while let Some(code) = bits.take(&mut input, 9) {
            if code == CLEAR {
                count += 1;
                break;
            }
        }
        count
    };
    assert_eq!(clears, 1, "the encoder cleared its table");
    let back = decode(
        &stream,
        Method::Lzw {
            max_bits: 9,
            rle: false,
        },
        plain.len(),
        65_536,
    )
    .unwrap();
    assert_eq!(back, plain);
}

#[test]
fn rle90_repeats_the_byte_before_a_run_and_escapes_its_own_marker() {
    // `a`, a run of four in all, a literal 0x90, then a count of one: that
    // byte again, no more.
    let packed = [b'a', RUN, 4, RUN, 0, b'b', RUN, 1, b'c'];
    let plain = decode(&packed, Method::Packed, 64, 64).unwrap();
    assert_eq!(plain, [b'a', b'a', b'a', b'a', RUN, b'b', b'c']);
    // A literal 0x90 is the byte a run after it repeats.
    let plain = decode(&[RUN, 0, RUN, 3], Method::Packed, 64, 64).unwrap();
    assert_eq!(plain, [RUN, RUN, RUN]);
}

/// ARC 4's crunching, encoded the way its table is kept, for a decoder with
/// no archive of the method to hand to be held to.
fn crunch(plain: &[u8], hash: Hash) -> Vec<u8> {
    let mut work = vec![0u32; 2 * CRUNCH_SLOTS];
    for byte in 0..=u8::MAX {
        crunch_insert(&mut work, hash, NO_PREFIX, byte).unwrap();
    }
    let find = |work: &[u32], prefix: u16, byte: u8| -> Option<u16> {
        let want = 1 << 31 | u32::from(prefix) << 8 | u32::from(byte);
        let mut slot = crunch_hash(hash, prefix, byte);
        for _ in 0..CRUNCH_SLOTS {
            if work[slot] == want {
                return u16::try_from(slot).ok();
            }
            match work[CRUNCH_SLOTS + slot] as usize {
                0 => return None,
                next => slot = next,
            }
        }
        None
    };
    let mut codes = Vec::new();
    let mut entries = 256;
    let Some((&first, rest)) = plain.split_first() else {
        return Vec::new();
    };
    let mut string = find(&work, NO_PREFIX, first).unwrap();
    for &byte in rest {
        if let Some(longer) = find(&work, string, byte) {
            string = longer;
            continue;
        }
        codes.push(string);
        if entries < CRUNCH_SLOTS {
            crunch_insert(&mut work, hash, string, byte).unwrap();
            entries += 1;
        }
        string = find(&work, NO_PREFIX, byte).unwrap();
    }
    codes.push(string);
    let mut out = Vec::new();
    for pair in codes.chunks(2) {
        let a = pair[0];
        let byte = |value: u16| u8::try_from(value).unwrap();
        out.push(byte(a >> 4));
        match pair.get(1) {
            Some(&b) => {
                out.push(byte((a & 0xF) << 4 | b >> 8));
                out.push(byte(b & 0xFF));
            }
            None => out.push(byte((a & 0xF) << 4)),
        }
    }
    out
}

#[test]
fn arc_4s_crunching_decodes_through_its_hashed_table() {
    let mut runs = prose(30_000, 3);
    runs.extend(vec![b'z'; 500]);
    for hash in [Hash::Square, Hash::Product] {
        for plain in [&runs[..], b"abababababababab", b"a"] {
            let stream = crunch(plain, hash);
            let method = Method::Crunched { hash, rle: false };
            assert_eq!(
                decode(&stream, method, plain.len(), 333).unwrap(),
                plain,
                "{hash:?}"
            );
        }
    }
}

#[test]
fn a_stream_out_of_shape_is_refused() {
    // A code past the next one the table can make.
    let mut beyond = Vec::new();
    beyond.extend_from_slice(&[b'a', 0b1100_0000, 0b1111_1111, 0]);
    assert_eq!(
        decode(
            &beyond,
            Method::Lzw {
                max_bits: 12,
                rle: false
            },
            16,
            16
        ),
        Err(Error::Corrupt)
    );
    // A first code that names no byte.
    assert_eq!(
        decode(
            &[0xFF, 0x01],
            Method::Lzw {
                max_bits: 12,
                rle: false
            },
            16,
            16
        ),
        Err(Error::Corrupt)
    );
    for max_bits in [0, 8, 17] {
        assert!(matches!(
            Decoder::new(
                vec![0u32; DECODER_TABLE],
                vec![0u8; DECODER_STACK],
                Method::Lzw {
                    max_bits,
                    rle: false
                }
            ),
            Err(Error::Corrupt)
        ));
        assert!(matches!(
            Encoder::new(vec![0u32; ENCODER_WORK], max_bits),
            Err(Error::Corrupt)
        ));
    }
    assert!(matches!(
        Decoder::new(
            vec![0u32; 100],
            Vec::<u8>::new(),
            Method::Lzw {
                max_bits: 12,
                rle: false
            }
        ),
        Err(Error::WorkspaceTooSmall)
    ));
    assert!(matches!(
        Encoder::new(vec![0u32; 100], 12),
        Err(Error::WorkspaceTooSmall)
    ));
}

#[test]
fn a_dictionary_sized_to_its_input_codes_as_the_largest_and_one_too_small_clears() {
    let plain = prose(40_000, 5);
    let full = encode(&plain, 16);
    for len in [0, 1, 500, 40_000, 1 << 20] {
        assert!(encoder_work(len) <= ENCODER_WORK);
        assert!(encoder_work(len) >= encoder_work(0));
    }
    let sized = {
        let mut encoder = Encoder::new(vec![0u32; encoder_work(plain.len() as u64)], 16).unwrap();
        let mut out = Gathered::default();
        encoder.write(&plain, &mut out).unwrap();
        encoder.finish(&mut out).unwrap();
        out.0
    };
    assert_eq!(
        sized, full,
        "room for every string codes as the largest dictionary"
    );
    // The smallest dictionary half fills long before 16-bit codes run out:
    // it clears, and the reader follows.
    let small = {
        let mut encoder = Encoder::new(vec![0u32; encoder_work(0)], 16).unwrap();
        let mut out = Gathered::default();
        encoder.write(&plain, &mut out).unwrap();
        encoder.finish(&mut out).unwrap();
        out.0
    };
    assert_ne!(small, full);
    let back = decode(
        &small,
        Method::Lzw {
            max_bits: 16,
            rle: false,
        },
        plain.len(),
        4096,
    )
    .unwrap();
    assert_eq!(back, plain);
}

#[test]
fn a_squeezed_tree_out_of_shape_is_refused() {
    let tree = |nodes: &[(i16, i16)]| -> Vec<u8> {
        let mut out = u16::try_from(nodes.len()).unwrap().to_le_bytes().to_vec();
        for &(zero, one) in nodes {
            out.extend_from_slice(&zero.to_le_bytes());
            out.extend_from_slice(&one.to_le_bytes());
        }
        out.push(0xFF);
        out
    };
    // A child past the tree, a leaf past the end symbol, a loop, and a tree
    // cut off.
    for bad in [tree(&[(1, -1)]), tree(&[(-258, -1)]), tree(&[(0, 0)])] {
        assert_eq!(decode(&bad, Method::Squeezed, 8, 8), Err(Error::Corrupt));
    }
    assert_eq!(
        decode(&[3, 0, 1], Method::Squeezed, 8, 8),
        Err(Error::UnexpectedEof)
    );
    let mut too_many = 257u16.to_le_bytes().to_vec();
    too_many.extend(vec![0xFF; 2000]);
    assert_eq!(
        decode(&too_many, Method::Squeezed, 8, 8),
        Err(Error::Corrupt)
    );
    // A one-node tree: `a` on a zero bit, the end on a one.
    let plain = decode(
        &tree(&[(-1 - i16::from(b'a'), -257)])[..6],
        Method::Squeezed,
        8,
        8,
    );
    assert_eq!(plain, Ok(Vec::new()));
    let mut two = tree(&[(-1 - i16::from(b'a'), -257)]);
    two.truncate(6);
    two.push(0b0000_1000);
    assert_eq!(decode(&two, Method::Squeezed, 8, 8).unwrap(), b"aaa");
}
