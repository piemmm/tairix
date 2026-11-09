//! The cross-reference: tables, streams, hybrids, updates and repair.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;
use core::fmt::Write;

use super::{chained, object_head, repair, Entry, ObjectStream, Source};
use crate::fixture::{flate, Builder};
use crate::limits::MAX_XREF_SECTIONS;
use crate::object::{Name, Object};
use crate::parse;
use crate::parse::Endstreams;
use crate::PdfError;

const ROOM: u64 = 1 << 20;

fn budget() -> Cell<u64> {
    Cell::new(ROOM)
}

/// The cross-reference `file` states, read within a test's budget.
fn stated(file: &[u8]) -> Result<super::Xref, PdfError> {
    let (budget, ends) = (budget(), Endstreams::default());
    chained(Source {
        file,
        budget: &budget,
        ends: &ends,
    })
}

#[test]
fn a_table_places_every_object_it_lists() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog>>").object(2, "(two)");
    let (one, two) = (pdf.offset(1), pdf.offset(2));
    let file = pdf.tabled(1, "");
    let xref = stated(&file).expect("a cross-reference");
    assert_eq!(xref.entry(1), Entry::Offset(one));
    assert_eq!(xref.entry(2), Entry::Offset(two));
    assert_eq!(xref.entry(0), Entry::Free);
    assert_eq!(xref.entry(3), Entry::Free);
    let trailer = xref.trailer.expect("a trailer");
    assert_eq!(trailer.get(b"Size"), Some(&Object::Int(3)));
}

#[test]
fn an_update_redefines_objects_and_keeps_the_older_trailers_keys() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog>>").object(2, "(old)");
    let first = pdf.table(1, "/Info 2 0 R");
    pdf.out
        .extend_from_slice(format!("startxref\n{first}\n%%EOF\n").as_bytes());
    let updated = pdf.out.len();
    pdf.out.extend_from_slice(b"2 0 obj\n(new)\nendobj\n");
    let update = pdf.out.len();
    pdf.out.extend_from_slice(
        format!(
            "xref\n2 1\n{updated:010} 00000 n \ntrailer\n<</Size 3 /Root 1 0 R /Prev {first}>>\n"
        )
        .as_bytes(),
    );
    let file = pdf.finish(update);
    let xref = stated(&file).expect("a cross-reference");
    assert_eq!(xref.entry(2), Entry::Offset(updated));
    let trailer = xref.trailer.expect("a trailer");
    assert!(
        trailer.get(b"Info").is_some(),
        "the older trailer's key is kept"
    );
}

#[test]
fn a_prev_loop_is_malformed() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog>>");
    let at = pdf.out.len();
    pdf.out.extend_from_slice(
        format!("xref\n0 1\n0000000000 65535 f \ntrailer\n<</Size 2 /Root 1 0 R /Prev {at}>>\n")
            .as_bytes(),
    );
    let file = pdf.finish(at);
    assert!(matches!(stated(&file), Err(PdfError::Malformed)));
}

#[test]
fn a_stream_places_direct_and_compressed_objects() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 3 0 R>>")
        .object_stream(2, &[(3, "<</Type /Pages /Kids []>>"), (4, "(four)")]);
    let one = pdf.offset(1);
    let at = pdf.xref_stream(5, "/Root 1 0 R", &[(3, 2, 0), (4, 2, 1)]);
    let file = pdf.finish(at);
    let xref = stated(&file).expect("a cross-reference");
    assert_eq!(xref.entry(1), Entry::Offset(one));
    assert_eq!(
        xref.entry(3),
        Entry::Compressed {
            stream: 2,
            index: 0
        }
    );
    assert_eq!(
        xref.entry(4),
        Entry::Compressed {
            stream: 2,
            index: 1
        }
    );
    assert_eq!(xref.entry(5), Entry::Offset(at));
    let trailer = xref.trailer.expect("a trailer");
    assert!(trailer.is_type(b"XRef"));
}

#[test]
fn a_hybrid_table_yields_its_free_entries_to_its_stream() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 3 0 R>>")
        .object_stream(2, &[(3, "<</Type /Pages /Kids []>>")]);
    let stream = pdf.xref_stream(5, "", &[(3, 2, 0)]);
    let (one, two) = (pdf.offset(1), pdf.offset(2));
    let at = pdf.out.len();
    pdf.out.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{one:010} 00000 n \n{two:010} 00000 n \n0000000000 00001 f \ntrailer\n<</Size 6 /Root 1 0 R /XRefStm {stream}>>\n"
        )
        .as_bytes(),
    );
    let file = pdf.finish(at);
    let xref = stated(&file).expect("a cross-reference");
    assert_eq!(
        xref.entry(3),
        Entry::Compressed {
            stream: 2,
            index: 0
        }
    );
    assert_eq!(xref.entry(1), Entry::Offset(one));
    assert_eq!(xref.entry(0), Entry::Free);
}

#[test]
fn a_missing_startxref_or_a_bad_offset_is_malformed() {
    assert!(matches!(
        stated(b"%PDF-1.7\n1 0 obj 1 endobj"),
        Err(PdfError::Malformed)
    ));
    assert!(matches!(
        stated(b"%PDF-1.7\nstartxref\n999\n%%EOF"),
        Err(PdfError::Malformed)
    ));
}

#[test]
fn a_subsection_past_the_object_limit_is_damage_a_repair_rebuilds() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog>>");
    let at = pdf.out.len();
    pdf.out.extend_from_slice(b"xref\n8388600 100\n");
    let file = pdf.finish(at);
    assert!(matches!(stated(&file), Err(PdfError::Malformed)));
}

#[test]
fn repair_finds_every_object_the_last_of_a_number_winning() {
    let mut file = b"%PDF-1.4\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n".to_vec();
    file.extend_from_slice(b"2 0 obj (old) endobj\n");
    let newer = file.len();
    file.extend_from_slice(b"2 0 obj (new) endobj\n");
    file.extend_from_slice(b"trailer <</Root 1 0 R>>\n");
    let xref = repair(&file).expect("a repaired cross-reference");
    assert_eq!(xref.entry(1), Entry::Offset(9));
    assert_eq!(xref.entry(2), Entry::Offset(newer));
    let trailer = xref.trailer.expect("a trailer");
    assert!(trailer.get(b"Root").is_some());
}

#[test]
fn repair_takes_the_last_trailer_or_cross_reference_stream_naming_a_catalog() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog>>")
        .object(7, "<</Type /Catalog>>");
    let first = pdf.table(1, "");
    pdf.out
        .extend_from_slice(format!("startxref\n{first}\n%%EOF\n").as_bytes());
    let later = pdf.xref_stream(9, "/Root 7 0 R", &[]);
    let file = pdf.finish(later);
    let xref = repair(&file).expect("a repaired cross-reference");
    let trailer = xref.trailer.expect("a trailer");
    assert_eq!(
        trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .map(|root| root.num),
        Some(7)
    );
}

#[test]
fn repair_lists_object_streams_for_later_indexing() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog>>")
        .object_stream(4, &[(5, "5")]);
    let file = pdf.tabled(1, "");
    let xref = repair(&file).expect("a repaired cross-reference");
    assert_eq!(xref.unindexed, [4]);
}

#[test]
fn an_object_head_needs_a_number_and_a_generation_before_obj() {
    let at = |text: &[u8]| object_head(text, parse::rfind(text, b"obj").expect("an obj"));
    assert_eq!(at(b"12 0 obj"), Some((12, 0)));
    assert_eq!(at(b"\n 12  3 obj"), Some((12, 2)));
    assert_eq!(at(b"12 0obj"), None);
    assert_eq!(at(b"0 obj"), None);
    assert_eq!(at(b"x12 0 obj"), None);
    assert_eq!(at(b"12345678901 0 obj"), None);
    assert_eq!(at(b"9999999 0 obj"), None, "past the object limit");
}

#[test]
fn an_object_stream_finds_an_object_by_number_where_its_index_is_wrong() {
    let data = b"10 0 11 2 1 (b)".to_vec();
    let dict = crate::object::Dict::from_written(alloc::vec![
        (Name(b"N".to_vec()), Object::Int(2)),
        (Name(b"First".to_vec()), Object::Int(10)),
    ])
    .expect("room");
    let room = budget();
    let objects = ObjectStream::new(&dict, data, &room).expect("an object stream");
    assert_eq!(objects.object(0, 10, &room), Ok(Object::Int(1)));
    assert_eq!(
        objects.object(0, 11, &room),
        Ok(Object::String(b"b".to_vec()))
    );
    assert_eq!(
        objects.object(9, 11, &room),
        Ok(Object::String(b"b".to_vec()))
    );
    assert_eq!(objects.object(0, 12, &room), Err(PdfError::Malformed));
    let numbers: Vec<u32> = objects.numbers().collect();
    assert_eq!(numbers, [10, 11]);
}

#[test]
fn an_object_stream_past_its_bounds_is_refused() {
    let dict = |count: i64, first: i64| {
        crate::object::Dict::from_written(alloc::vec![
            (Name(b"N".to_vec()), Object::Int(count)),
            (Name(b"First".to_vec()), Object::Int(first)),
        ])
        .expect("room")
    };
    assert!(matches!(
        ObjectStream::new(&dict(1, 99), b"1 0 2".to_vec(), &budget()),
        Err(PdfError::Malformed)
    ));
    assert!(matches!(
        ObjectStream::new(&dict(1, 4), b"1 50 2".to_vec(), &budget()),
        Err(PdfError::Malformed)
    ));
    assert!(matches!(
        ObjectStream::new(&dict(100_001, 0), Vec::new(), &budget()),
        Err(PdfError::TooComplex)
    ));
}

#[test]
fn heads_within_an_object_read_already_are_its_bytes() {
    // Every head after the first lies in a comment of the first object's
    // dictionary: read as objects, each would read the rest of the line again.
    let mut file = b"%PDF-1.7\n1 0 obj << ".to_vec();
    for num in 2..4000 {
        file.extend_from_slice(format!("%{num} 0 obj << ").as_bytes());
    }
    file.extend_from_slice(b"\n/A 1 >> endobj\n");
    let xref = repair(&file).expect("a repair");
    assert_eq!(xref.entry(1), Entry::Offset(9));
    assert_eq!(xref.entry(2), Entry::Free);
    assert_eq!(xref.entry(3999), Entry::Free);
}

#[test]
fn a_long_chain_of_updates_merges_its_trailers_once() {
    // The newest trailer holds many keys and the oldest one it lacks: merged
    // once, not rebuilt for every section.
    let mut file = b"%PDF-1.7\n".to_vec();
    let mut prev = None;
    for _ in 1..MAX_XREF_SECTIONS {
        let at = file.len();
        let entries = prev.map_or_else(
            || String::from("/Info 9 0 R"),
            |prev| format!("/Prev {prev}"),
        );
        file.extend_from_slice(format!("xref\ntrailer\n<<{entries}>>\n").as_bytes());
        prev = Some(at);
    }
    let prev = prev.expect("an older section");
    let newest = file.len();
    let keys = (0..50_000).fold(String::new(), |mut keys, key| {
        write!(keys, "/K{key} {key} ").expect("a string takes any text");
        keys
    });
    file.extend_from_slice(
        format!("xref\ntrailer\n<<{keys}/Root 1 0 R /Prev {prev}>>\nstartxref\n{newest}\n%%EOF\n")
            .as_bytes(),
    );
    let trailer = stated(&file)
        .expect("a cross-reference")
        .trailer
        .expect("a trailer");
    assert_eq!(trailer.get(b"K49999"), Some(&Object::Int(49_999)));
    assert!(
        trailer.get(b"Info").is_some(),
        "the oldest trailer's key is kept"
    );
    assert_eq!(
        trailer.get(b"Prev").and_then(Object::as_int),
        i64::try_from(prev).ok(),
        "the newest trailer's value wins"
    );
}

#[test]
fn what_a_cross_reference_streams_rows_decode_to_is_charged() {
    // A hundred thousand one-byte rows compress to a few hundred bytes, and
    // the entries they place take far more than they decode to.
    let mut pdf = Builder::new();
    let at = pdf.out.len();
    pdf.stream(
        1,
        "/Type /XRef /Size 100000 /W [1 0 0] /Filter /FlateDecode",
        &flate(&alloc::vec![0; 100_000]),
    );
    let file = pdf.finish(at);
    assert!(matches!(stated(&file), Err(PdfError::TooComplex)));
}

#[test]
fn an_object_streams_index_is_charged_to_the_budget() {
    let dict = crate::object::Dict::from_written(alloc::vec![
        (Name(b"N".to_vec()), Object::Int(2)),
        (Name(b"First".to_vec()), Object::Int(10)),
    ])
    .expect("room");
    let data = b"10 0 11 2 1 (b)";
    assert!(matches!(
        ObjectStream::new(&dict, data.to_vec(), &Cell::new(1)),
        Err(PdfError::TooComplex)
    ));
    let room = budget();
    assert!(ObjectStream::new(&dict, data.to_vec(), &room).is_ok());
    assert!(room.get() < ROOM, "its index is charged");
}

#[test]
fn a_repair_reading_past_trailer_keywords_is_held_to_its_allowance() {
    // Each keyword is followed by the rest of one unbroken run, as a keyword
    // or an unterminated string, so lexing what follows each for free would
    // cost the square of the run.
    for run in [b"trailer".as_slice(), b"trailer(".as_slice()] {
        let mut file = b"%PDF-1.7\n".to_vec();
        file.extend(run.repeat(5000));
        assert!(matches!(repair(&file), Err(PdfError::TooComplex)));
    }
}

#[test]
fn a_repair_refuses_an_object_nested_past_the_bound_rather_than_passing_it_over() {
    // Passed over, the object would be missing from what the repair rebuilt:
    // a different file, where reading it whole refuses it.
    let mut file = b"%PDF-1.7\n1 0 obj\n".to_vec();
    file.extend(b"<</A ".repeat(80));
    file.extend(b">>".repeat(80));
    file.extend(b"\nendobj\ntrailer\n<</Root 1 0 R>>\n");
    assert!(matches!(repair(&file), Err(PdfError::TooComplex)));
}

#[test]
fn repair_completes_the_last_trailer_from_earlier_ones() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog>>")
        .object(9, "<</Filter /Standard>>");
    pdf.table(1, "/Encrypt 9 0 R");
    pdf.out
        .extend_from_slice(b"trailer\n<</Root 1 0 R /Size 20>>\n");
    let trailer = repair(&pdf.out)
        .expect("a repair")
        .trailer
        .expect("a trailer");
    assert_eq!(
        trailer
            .get(b"Encrypt")
            .and_then(Object::as_reference)
            .map(|encrypt| encrypt.num),
        Some(9),
        "the earlier trailer's key is kept"
    );
    assert_eq!(
        trailer.get(b"Size"),
        Some(&Object::Int(20)),
        "the later trailer's value wins"
    );
}

#[test]
fn repair_with_no_trailer_left_names_the_encryption_dictionary_it_finds() {
    let cut = b"%PDF-1.4\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        9 0 obj <</Filter /Standard /V 2 /R 3 /O <00> /U <00> /P -4>> endobj\n";
    let trailer = repair(cut)
        .expect("a repair")
        .trailer
        .expect("a trailer naming it");
    assert_eq!(
        trailer
            .get(b"Encrypt")
            .and_then(Object::as_reference)
            .map(|encrypt| encrypt.num),
        Some(9)
    );
    let signed = b"%PDF-1.4\n1 0 obj <</Type /Sig /Filter /Adobe.PPKLite \
        /SubFilter /adbe.pkcs7.detached /Contents <00> /ByteRange [0 1 2 3]>> endobj\n";
    assert!(
        repair(signed).expect("a repair").trailer.is_none(),
        "a signature is no encryption dictionary"
    );
}
