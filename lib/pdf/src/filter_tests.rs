//! Stream filters: each decoder, the predictors, the chain, and the budget.

use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;

use super::{chain, decode, extend, Decoded, ImageCodec};
use crate::fixture::flate;
use crate::object::{Dict, Name, Object};
use crate::PdfError;

const ROOM: u64 = 1 << 20;

fn name(text: &str) -> Object {
    Object::Name(Name(text.as_bytes().to_vec()))
}

fn params(entries: &[(&str, i64)]) -> Dict {
    let entries = entries
        .iter()
        .map(|&(key, value)| (Name(key.as_bytes().to_vec()), Object::Int(value)))
        .collect();
    Dict::from_written(entries).expect("room")
}

fn through(data: &[u8], filters: &[&str], params: Option<&Dict>) -> Result<Decoded, PdfError> {
    let budget = Cell::new(ROOM);
    let links: Vec<(&[u8], Option<&Dict>)> = filters
        .iter()
        .map(|filter| (filter.as_bytes(), params))
        .collect();
    decode(alloc::borrow::Cow::Borrowed(data), &links, &budget)
}

fn bytes(data: &[u8], filters: &[&str]) -> Vec<u8> {
    through(data, filters, None)
        .expect("a decodable stream")
        .bytes
}

#[test]
fn no_filter_passes_the_bytes_on() {
    assert_eq!(bytes(b"plain", &[]), b"plain");
}

#[test]
fn flate_inflates_a_zlib_stream_and_a_raw_one() {
    let text = b"BT /F1 12 Tf (Hello) Tj ET".repeat(20);
    let zlib = flate(&text);
    assert_eq!(bytes(&zlib, &["FlateDecode"]), text);
    // Without its two-byte header the body still inflates as raw DEFLATE.
    assert_eq!(bytes(&zlib[2..], &["Fl"]), text);
}

#[test]
fn a_truncated_flate_stream_keeps_what_it_decoded() {
    // Bytes no repetition shortens, so half the stream decodes to some.
    let text: Vec<u8> = (0u32..4000)
        .map(|at| (at.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let zlib = flate(&text);
    let cut = through(&zlib[..zlib.len() / 2], &["FlateDecode"], None).expect("a partial stream");
    assert!(cut.damaged);
    assert!(!cut.bytes.is_empty() && text.starts_with(&cut.bytes));
    let garbage = through(b"\xff\xff\xff", &["FlateDecode"], None).expect("a damaged stream");
    assert!(garbage.damaged && garbage.bytes.is_empty());
    assert!(
        !through(&zlib, &["FlateDecode"], None)
            .expect("a whole stream")
            .damaged
    );
}

#[test]
fn lzw_decodes_the_specifications_example() {
    // ISO 32000-2 §7.4.4.2's worked example.
    let coded = [0x80, 0x0b, 0x60, 0x50, 0x22, 0x0c, 0x0c, 0x85, 0x01];
    assert_eq!(
        bytes(&coded, &["LZWDecode"]),
        b"\x2d\x2d\x2d\x2d\x2d\x41\x2d\x2d\x2d\x42"
    );
}

#[test]
fn png_predictors_undo_each_rows_own_filter() {
    // Two pixels of three components a row: rows filtered None, Sub, Up,
    // Average and Paeth in turn.
    let rows: [[u8; 7]; 5] = [
        [0, 10, 20, 30, 40, 50, 60],
        [1, 1, 2, 3, 1, 1, 1],
        [2, 5, 5, 5, 5, 5, 5],
        [3, 0, 0, 0, 0, 0, 0],
        [4, 1, 1, 1, 1, 1, 1],
    ];
    let data = flate(&rows.concat());
    let params = params(&[("Predictor", 15), ("Colors", 3), ("Columns", 2)]);
    let decoded = through(&data, &["FlateDecode"], Some(&params)).expect("a predicted stream");
    assert_eq!(
        decoded.bytes,
        [
            [10, 20, 30, 40, 50, 60],
            [1, 2, 3, 2, 3, 4],
            [6, 7, 8, 7, 8, 9],
            [3, 3, 4, 5, 5, 6],
            [4, 4, 5, 6, 6, 7],
        ]
        .concat()
    );
}

#[test]
fn the_tiff_predictor_sums_each_sample_with_the_one_before() {
    let data = flate(&[1, 2, 3, 1, 1, 1, 10, 0, 0, 1, 255, 255]);
    let eight = params(&[("Predictor", 2), ("Colors", 3), ("Columns", 2)]);
    let decoded = through(&data, &["FlateDecode"], Some(&eight)).expect("a predicted stream");
    assert_eq!(decoded.bytes, [1, 2, 3, 2, 3, 4, 10, 0, 0, 11, 255, 255]);
    let sixteen = params(&[("Predictor", 2), ("BitsPerComponent", 16), ("Columns", 3)]);
    let data = flate(&[0x00, 0x01, 0x00, 0x02, 0xff, 0xff]);
    let decoded = through(&data, &["FlateDecode"], Some(&sixteen)).expect("a predicted stream");
    assert_eq!(decoded.bytes, [0x00, 0x01, 0x00, 0x03, 0x00, 0x02]);
}

#[test]
fn the_tiff_predictor_sums_samples_narrower_than_a_byte() {
    let four = params(&[("Predictor", 2), ("BitsPerComponent", 4), ("Columns", 4)]);
    let decoded = through(&flate(&[0x12, 0x34]), &["FlateDecode"], Some(&four));
    assert_eq!(decoded.map(|decoded| decoded.bytes), Ok(vec![0x13, 0x6a]));
}

#[test]
fn a_tiff_predicted_row_said_to_be_wider_than_its_bytes_is_undone_at_once() {
    // A trillion columns over two decoded bytes: one short row, undone as far
    // as it goes rather than walked a trillion samples.
    let wide = params(&[
        ("Predictor", 2),
        ("BitsPerComponent", 16),
        ("Columns", 1_000_000_000_000),
    ]);
    let decoded = through(&flate(&[0x00, 0x01]), &["FlateDecode"], Some(&wide));
    assert_eq!(decoded.map(|decoded| decoded.bytes), Ok(vec![0x00, 0x01]));
}

#[test]
fn a_predictor_with_impossible_parameters_is_refused() {
    let data = flate(&[0, 1]);
    for bad in [
        params(&[("Predictor", 12), ("Colors", 0)]),
        params(&[("Predictor", 12), ("BitsPerComponent", 3)]),
        params(&[("Predictor", 12), ("Columns", 0)]),
    ] {
        assert_eq!(
            through(&data, &["FlateDecode"], Some(&bad)),
            Err(PdfError::Malformed)
        );
    }
    assert_eq!(
        through(&data, &["FlateDecode"], Some(&params(&[("Predictor", 7)]))),
        Err(PdfError::Unsupported)
    );
}

#[test]
fn ascii_hex_reads_pairs_up_to_its_end_marker() {
    assert_eq!(
        bytes(b"48 65 6c6C6f>ignored", &["ASCIIHexDecode"]),
        b"Hello"
    );
    assert_eq!(bytes(b"7>", &["AHx"]), [0x70]);
    assert_eq!(through(b"4G>", &["AHx"], None), Err(PdfError::Malformed));
}

#[test]
fn ascii85_reads_groups_zero_runs_and_a_short_tail() {
    assert_eq!(
        bytes(b"<~87cURD]i,\"Ebo80~>", &["ASCII85Decode"]),
        b"Hello World!"
    );
    assert_eq!(bytes(b"z~>", &["A85"]), [0, 0, 0, 0]);
    assert_eq!(bytes(b"87cURDZ~>", &["A85"]), b"Hello");
    assert_eq!(through(b"8~>", &["A85"], None), Err(PdfError::Malformed));
    assert_eq!(
        through(b"s8W-\"~>", &["A85"], None),
        Err(PdfError::Malformed)
    );
}

#[test]
fn run_length_reads_literals_and_repeats_up_to_its_end() {
    let coded = [2, b'a', b'b', b'c', 254, b'x', 128, b'z'];
    assert_eq!(bytes(&coded, &["RunLengthDecode"]), b"abcxxx");
    assert_eq!(through(&[5, b'a'], &["RL"], None), Err(PdfError::Malformed));
}

#[test]
fn filters_apply_in_order() {
    let inner = flate(b"chained");
    let hex: Vec<u8> = inner
        .iter()
        .flat_map(|byte| alloc::format!("{byte:02x}").into_bytes())
        .collect();
    assert_eq!(bytes(&hex, &["ASCIIHexDecode", "FlateDecode"]), b"chained");
}

#[test]
fn an_image_codec_ends_the_chain_undecoded() {
    let decoded = through(b"\xff\xd8jpeg", &["DCTDecode"], None).expect("an image stream");
    assert_eq!(decoded.bytes, b"\xff\xd8jpeg");
    assert!(matches!(decoded.image, Some(ImageCodec::Dct)));
    assert_eq!(
        through(b"x", &["DCTDecode", "FlateDecode"], None),
        Err(PdfError::Unsupported)
    );
}

#[test]
fn a_crypt_filter_anywhere_but_first_is_damage() {
    assert_eq!(bytes(b"clear", &["Crypt"]), b"clear");
    assert_eq!(
        through(b"41>", &["ASCIIHexDecode", "Crypt"], None),
        Err(PdfError::Malformed)
    );
}

#[test]
fn bytes_already_decoded_pass_an_empty_chain_uncopied() {
    let budget = Cell::new(ROOM);
    let owned = vec![1u8, 2, 3];
    let at = owned.as_ptr();
    let decoded = decode(alloc::borrow::Cow::Owned(owned), &[], &budget).expect("no filter");
    assert_eq!(decoded.bytes.as_ptr(), at);
    assert_eq!(budget.get(), ROOM, "charged nothing more");
}

#[test]
fn an_unknown_filter_is_unsupported() {
    assert_eq!(
        through(b"x", &["Rot13Decode"], None),
        Err(PdfError::Unsupported)
    );
}

#[test]
fn decoding_past_the_budget_is_too_complex() {
    let zeros = flate(&vec![0; 4096]);
    let budget = Cell::new(4095);
    assert_eq!(
        decode(
            alloc::borrow::Cow::Borrowed(&zeros),
            &[(&b"FlateDecode"[..], None)],
            &budget
        ),
        Err(PdfError::TooComplex)
    );
    let budget = Cell::new(4096);
    assert!(decode(
        alloc::borrow::Cow::Borrowed(&zeros),
        &[(&b"FlateDecode"[..], None)],
        &budget
    )
    .is_ok());
    assert_eq!(budget.get(), 0);
}

#[test]
fn a_chain_pairs_each_filter_with_its_parameters() {
    let one = params(&[("Predictor", 12)]);
    let (flate, held) = (name("FlateDecode"), Object::Dict(one.clone()));
    let links = chain(Some(&flate), Some(&held)).expect("a chain");
    assert_eq!(links, [(&b"FlateDecode"[..], Some(&one))]);
    let filters = Object::Array(vec![name("ASCII85Decode"), name("FlateDecode")]);
    let params = Object::Array(vec![Object::Null, Object::Dict(one.clone())]);
    let links = chain(Some(&filters), Some(&params)).expect("a chain");
    assert_eq!(
        links,
        [
            (&b"ASCII85Decode"[..], None),
            (&b"FlateDecode"[..], Some(&one))
        ]
    );
    assert_eq!(chain(None, None).expect("no chain"), []);
    assert_eq!(chain(Some(&Object::Int(1)), None), Err(PdfError::Malformed));
    let long = Object::Array(vec![name("FlateDecode"); 9]);
    assert_eq!(chain(Some(&long), None), Err(PdfError::TooComplex));
}

#[test]
fn the_png_predictors_undo_in_place_and_charge_nothing_more() {
    // Two rows of three behind their filter bytes, Sub then Up: eight bytes
    // inflated, all the budget there is.
    let data = flate(&[1, 1, 2, 3, 2, 1, 1, 1]);
    let params = params(&[("Predictor", 11), ("Columns", 3)]);
    let budget = Cell::new(8);
    let decoded = decode(
        alloc::borrow::Cow::Borrowed(&data),
        &[(&b"FlateDecode"[..], Some(&params))],
        &budget,
    )
    .expect("within the budget");
    assert_eq!(decoded.bytes, [1, 3, 6, 2, 4, 7]);
    assert_eq!(budget.get(), 0);
}

#[test]
fn a_png_predictor_reaches_back_a_whole_pixel_of_small_components() {
    // Three four-bit components are a byte and a half, so Sub reaches back two.
    let data = flate(&[1, 0x12, 0x34, 0x01]);
    let params = params(&[
        ("Predictor", 11),
        ("Colors", 3),
        ("BitsPerComponent", 4),
        ("Columns", 2),
    ]);
    let decoded = through(&data, &["FlateDecode"], Some(&params)).expect("a predicted stream");
    assert_eq!(decoded.bytes, [0x12, 0x34, 0x13]);
}

#[test]
fn output_grows_geometrically_however_small_each_piece() {
    let budget = Cell::new(ROOM);
    let mut out = Vec::new();
    let mut grown = 0;
    for _ in 0..4096 {
        let held = out.capacity();
        extend(&mut out, &[0], &budget).expect("room");
        grown += usize::from(out.capacity() != held);
    }
    assert!(grown <= 16, "{grown} reallocations for 4096 bytes");
}
