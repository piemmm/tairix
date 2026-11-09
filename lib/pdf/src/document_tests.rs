//! Opening documents: resolution, object streams, repair and the page tree.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Document, Options};
use crate::fixture::{one_page, Builder};
use crate::limits::{MAX_PAGE_TREE_DEPTH, MAX_RESOLVE_DEPTH};
use crate::object::Object;
use crate::PdfError;

const OPTIONS: Options<'static> = Options {
    password: None,
    budget: 1 << 20,
};

fn open(file: &[u8]) -> Result<Document, PdfError> {
    Document::open(crate::fixture::kept(file), &OPTIONS).map_err(PdfError::from)
}

/// A catalog and a page tree whose root lists `kids`, then `pages`.
fn tree(kids: &str, pages: &[(u32, &str)]) -> Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, &format!("<</Type /Pages /Kids [{kids}]>>"));
    for &(num, body) in pages {
        pdf.object(num, body);
    }
    pdf.tabled(1, "")
}

#[test]
fn a_one_page_document_opens() {
    let file = one_page(b"0 0 m 10 10 l S", "<<>>");
    let document = open(&file).expect("a document");
    assert_eq!(document.page_count(), 1);
    assert!(!document.encrypted());
    assert_eq!(document.page(0).expect("a page").size(), (200.0, 100.0));
    assert_eq!(document.page(1).err(), Some(PdfError::NoSuchPage));
}

#[test]
fn a_file_that_is_not_a_pdf_is_malformed() {
    assert_eq!(open(b"GIF89a").err(), Some(PdfError::Malformed));
    assert_eq!(open(b"").err(), Some(PdfError::Malformed));
    let mut late = alloc::vec![b' '; 2000];
    late.extend_from_slice(&one_page(b"", "<<>>"));
    assert_eq!(
        open(&late).err(),
        Some(PdfError::Malformed),
        "the header is too far in"
    );
}

#[test]
fn junk_before_the_header_is_repaired_past() {
    let mut file = b"From: mail\r\n\r\n".to_vec();
    file.extend_from_slice(&one_page(b"", "<<>>"));
    let document = open(&file).expect("a repaired document");
    assert_eq!(document.page_count(), 1);
}

#[test]
fn a_damaged_cross_reference_is_rebuilt() {
    let mut file = one_page(b"", "<<>>");
    let at = crate::parse::rfind(&file, b"xref").expect("a table");
    file[at..].fill(b' ');
    assert_eq!(open(&file).expect("a repaired document").page_count(), 1);
}

#[test]
fn objects_in_object_streams_resolve() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object_stream(
            10,
            &[
                (2, "<</Type /Pages /Kids [3 0 R]>>"),
                (3, "<</Type /Page /Parent 2 0 R /MediaBox [0 0 50 60]>>"),
            ],
        );
    let at = pdf.xref_stream(11, "/Root 1 0 R", &[(2, 10, 0), (3, 10, 1)]);
    let file = pdf.finish(at);
    let document = open(&file).expect("a document");
    assert_eq!(document.page(0).expect("a page").size(), (50.0, 60.0));
}

#[test]
fn a_repaired_file_indexes_its_object_streams() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object_stream(
            10,
            &[
                (2, "<</Type /Pages /Kids [3 0 R]>>"),
                (3, "<</Type /Page /MediaBox [0 0 70 80]>>"),
            ],
        );
    let mut file = pdf.out.clone();
    file.extend_from_slice(b"trailer <</Root 1 0 R>>\n");
    let document = open(&file).expect("a repaired document");
    assert_eq!(document.page(0).expect("a page").size(), (70.0, 80.0));
}

#[test]
fn a_repair_without_a_trailer_finds_the_catalog() {
    let file = b"%PDF-1.3\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R]>> endobj\n\
        3 0 obj <</Type /Page /MediaBox [0 0 30 40]>> endobj\n";
    assert_eq!(
        open(file)
            .expect("a document")
            .page(0)
            .expect("a page")
            .size(),
        (30.0, 40.0)
    );
}

#[test]
fn a_reference_cycle_is_malformed() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .object(4, "5 0 R")
        .object(5, "4 0 R");
    let file = pdf.tabled(1, "");
    let document = open(&file).expect("a document");
    assert_eq!(
        document
            .resolve(&crate::fixture::reference(4))
            .and_then(crate::object::Resolved::into_owned),
        Err(PdfError::Malformed)
    );
    assert_eq!(
        document
            .resolve(&crate::fixture::reference(99))
            .and_then(crate::object::Resolved::into_owned),
        Ok(Object::Null)
    );
}

#[test]
fn a_streams_length_may_be_indirect_or_wrong() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .raw(
            4,
            b"4 0 obj\n<</Length 5 0 R>>\nstream\nq Q\nendstream\nendobj\n",
        )
        .object(5, "3")
        .raw(
            6,
            b"6 0 obj\n<</Length 99>>\nstream\nshort\nendstream\nendobj\n",
        );
    let file = pdf.tabled(1, "");
    let document = open(&file).expect("a document");
    for (num, expected) in [(4, &b"q Q"[..]), (6, b"short")] {
        let Ok(Object::Stream(stream)) = document
            .resolve(&crate::fixture::reference(num))
            .and_then(crate::object::Resolved::into_owned)
        else {
            panic!("a stream");
        };
        assert_eq!(document.decode(&stream).expect("its bytes").bytes, expected);
    }
}

#[test]
fn a_page_tree_node_reached_twice_is_read_once() {
    let file = tree(
        "3 0 R 4 0 R 3 0 R 2 0 R",
        &[
            (3, "<</Type /Page>>"),
            (4, "<</Type /Pages /Kids [3 0 R 5 0 R]>>"),
            (5, "<</Type /Page>>"),
        ],
    );
    assert_eq!(open(&file).expect("a document").page_count(), 2);
}

#[test]
fn a_page_tree_too_deep_is_too_complex() {
    let mut pages = Vec::new();
    let bodies: Vec<String> = (0..=u32::try_from(MAX_PAGE_TREE_DEPTH).expect("a small depth"))
        .map(|level| format!("<</Type /Pages /Kids [{} 0 R]>>", level + 4))
        .collect();
    for (level, body) in bodies.iter().enumerate() {
        pages.push((
            u32::try_from(level).expect("a small level") + 3,
            body.as_str(),
        ));
    }
    let file = tree("3 0 R", &pages);
    assert_eq!(open(&file).err(), Some(PdfError::TooComplex));
}

#[test]
fn a_document_with_no_pages_is_malformed() {
    assert_eq!(open(&tree("", &[])).err(), Some(PdfError::Malformed));
}

#[test]
fn a_kid_whose_object_cannot_be_read_is_passed_over() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R 4 0 R]>>")
        .object(3, "<</Type /Page>>");
    // Object 4's entry names where object 3 begins.
    let wrong = pdf.offset(3);
    let at = pdf.out.len();
    pdf.out.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n{wrong:010} 00000 n \ntrailer\n<</Size 5 /Root 1 0 R>>\n",
            pdf.offset(1),
            pdf.offset(2),
            pdf.offset(3)
        )
        .as_bytes(),
    );
    let file = pdf.finish(at);
    assert_eq!(open(&file).expect("a document").page_count(), 1);
}

#[test]
fn kids_that_are_not_pages_are_passed_over() {
    let file = tree("null 7 0 R (x) 3 0 R", &[(3, "<</Type /Page>>")]);
    assert_eq!(open(&file).expect("a document").page_count(), 1);
}

#[test]
fn decoding_past_the_budget_is_too_complex() {
    let content = crate::fixture::flate(&alloc::vec![b' '; 4096]);
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /Contents 4 0 R>>")
        .stream(4, "/Filter /FlateDecode", &content);
    let file = pdf.tabled(1, "");
    let options = Options {
        password: None,
        budget: 1000,
    };
    let document = Document::open(crate::fixture::kept(&file), &options).expect("a document");
    let Ok(Object::Stream(stream)) = document
        .resolve(&crate::fixture::reference(4))
        .and_then(crate::object::Resolved::into_owned)
    else {
        panic!("a stream");
    };
    assert_eq!(document.decode(&stream).err(), Some(PdfError::TooComplex));
}

#[test]
fn a_stream_in_another_file_is_unsupported() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .stream(4, "/F (elsewhere.bin)", b"");
    let file = pdf.tabled(1, "");
    let document = open(&file).expect("a document");
    let Ok(Object::Stream(stream)) = document
        .resolve(&crate::fixture::reference(4))
        .and_then(crate::object::Resolved::into_owned)
    else {
        panic!("a stream");
    };
    assert_eq!(document.decode(&stream).err(), Some(PdfError::Unsupported));
}

#[test]
fn a_catalog_search_rejects_an_entry_at_its_head() {
    // Number 5 is placed where object 4 is written, nested past the bound:
    // read through rather than rejected at its head, it would refuse the file.
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>");
    let nested = pdf.out.len();
    pdf.out
        .extend_from_slice(format!("4 0 obj\n{}\nendobj\n", "[".repeat(70)).as_bytes());
    let (one, two, three) = (pdf.offset(1), pdf.offset(2), pdf.offset(3));
    let at = pdf.out.len();
    pdf.out.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{one:010} 00000 n \n{two:010} 00000 n \n{three:010} 00000 n \n5 1\n{nested:010} 00000 n \ntrailer\n<</Size 6>>\n"
        )
        .as_bytes(),
    );
    let file = pdf.finish(at);
    assert_eq!(open(&file).expect("a document").page_count(), 1);
}

#[test]
fn a_repair_takes_the_later_of_a_direct_object_and_one_in_an_object_stream() {
    let repaired = |stream_last: bool| {
        let mut pdf = Builder::new();
        pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
            .object(2, "<</Type /Pages /Kids [3 0 R]>>")
            .object(3, "<</Type /Page /MediaBox 5 0 R>>");
        if stream_last {
            pdf.object(5, "[0 0 10 10]")
                .object_stream(10, &[(5, "[0 0 70 80]")]);
        } else {
            pdf.object_stream(10, &[(5, "[0 0 70 80]")])
                .object(5, "[0 0 10 10]");
        }
        let mut file = pdf.out.clone();
        file.extend_from_slice(b"trailer <</Root 1 0 R>>\n");
        let document = open(&file).expect("a repaired document");
        let size = document.page(0).expect("a page").size();
        size
    };
    assert_eq!(repaired(true), (70.0, 80.0));
    assert_eq!(repaired(false), (10.0, 10.0));
}

#[test]
fn a_page_whose_box_cannot_be_read_is_letter_and_its_document_still_lists() {
    let file = tree(
        "3 0 R 4 0 R",
        &[
            (3, "<</Type /Page /MediaBox 9 0 R>>"),
            (4, "<</Type /Page /MediaBox [0 0 20 30]>>"),
            (9, "[0 0 ("),
        ],
    );
    let document = open(&file).expect("a document");
    assert_eq!(document.page(0).expect("a page").size(), (612.0, 792.0));
    assert_eq!(document.page(1).expect("a page").size(), (20.0, 30.0));
}

#[test]
fn a_page_is_always_a_size_a_number_holds() {
    let file = tree(
        "3 0 R 4 0 R",
        &[
            (
                3,
                "<</Type /Page /MediaBox [0 0 1e-30 1e-30] /UserUnit 1e-300>>",
            ),
            (4, "<</Type /Page /MediaBox [0 0 1e-320 5]>>"),
        ],
    );
    let document = open(&file).expect("a document");
    let (width, height) = document.page(0).expect("a page").size();
    assert!(
        width.is_normal() && height.is_normal(),
        "{width} by {height}"
    );
    assert_eq!(document.page(1).expect("a page").size(), (612.0, 792.0));
}

/// A one-page document whose catalog adds `catalog` and whose trailer adds
/// `trailer`, `objects` written from 10.
fn about(catalog: &str, trailer: &str, objects: &[(u32, &str)]) -> crate::About {
    let mut pdf = Builder::new();
    pdf.object(1, &format!("<</Type /Catalog /Pages 2 0 R {catalog}>>"))
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>");
    for &(num, body) in objects {
        pdf.object(num, body);
    }
    let file = pdf.tabled(1, trailer);
    open(&file)
        .expect("a document")
        .about()
        .expect("its information")
}

#[test]
fn a_document_states_its_title_and_subject_and_whether_it_holds_more() {
    let drawn = about(
        "",
        "/Info 10 0 R",
        &[(10, "<</Producer (TAIRiX Draw) /Title <FEFF0050006C0061006E> /Subject (A \\(rough\\) plan) /CreationDate (D:20260101)>>")],
    );
    assert_eq!(
        drawn,
        crate::About {
            title: Some("Plan".into()),
            subject: Some("A (rough) plan".into()),
            more_metadata: false,
            more_parts: false,
        }
    );
    assert!(about("", "/Info 10 0 R", &[(10, "<</Author (Ann)>>")]).more_metadata);
    assert!(about("/Metadata 10 0 R", "", &[(10, "<<>>")]).more_metadata);
    assert!(about("/Outlines 10 0 R", "", &[(10, "<<>>")]).more_parts);
    assert!(about("", "/Info 10 0 R", &[(10, "(not a dictionary)")]).more_metadata);
}

#[test]
fn an_object_is_read_once_however_often_it_is_asked_for() {
    let body = format!("[{}\n1 2 3]", "%".repeat(4096));
    let file = crate::fixture::with_objects(&[(10, body.as_str())], &[]);
    let document = open(&file).expect("a document");
    let lexing = document.lexing.get();
    for _ in 0..8 {
        assert_eq!(
            document
                .resolve(&crate::fixture::reference(10))
                .and_then(crate::object::Resolved::into_owned),
            Ok(Object::Array(alloc::vec![
                Object::Int(1),
                Object::Int(2),
                Object::Int(3)
            ]))
        );
    }
    let lexed = lexing - document.lexing.get();
    assert!(lexed < 2 * 4096, "read again: {lexed} bytes lexed");
}

#[test]
fn kids_reached_twice_are_walked_once() {
    // Three nodes share an array of kids holding a page written directly,
    // the third through a reference to the reference.
    let file = tree(
        "4 0 R 5 0 R 6 0 R",
        &[
            (4, "<</Type /Pages /Kids 7 0 R>>"),
            (5, "<</Type /Pages /Kids 7 0 R>>"),
            (6, "<</Type /Pages /Kids 8 0 R>>"),
            (7, "[<</Type /Page /MediaBox [0 0 10 20]>>]"),
            (8, "7 0 R"),
        ],
    );
    let document = open(&file).expect("a document");
    assert_eq!(document.page_count(), 1);
    assert_eq!(document.page(0).expect("a page").size(), (10.0, 20.0));
}

#[test]
fn the_catalog_is_looked_for_once() {
    let file = b"%PDF-1.3\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R]>> endobj\n\
        3 0 obj <</Type /Page>> endobj\n4 0 obj (never needed) endobj\n";
    let document = open(file).expect("a document");
    let lexing = document.lexing.get();
    assert!(!document.about().expect("its information").more_parts);
    assert_eq!(
        document.lexing.get(),
        lexing,
        "its objects were looked through again"
    );
}

#[test]
fn an_object_stream_that_cannot_be_read_is_decoded_once() {
    let data = alloc::vec![b' '; 1 << 14];
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        // It decodes, but its objects would begin past its end.
        .stream(
            10,
            "/Type /ObjStm /N 1 /First 99999 /Filter /FlateDecode",
            &crate::fixture::flate(&data),
        );
    let placed: Vec<(u32, u32, u32)> = (0..10).map(|index| (20 + index, 10, index)).collect();
    let at = pdf.xref_stream(11, "/Root 1 0 R", &placed);
    let file = pdf.finish(at);
    let document = open(&file).expect("a document");
    let budget = document.budget.get();
    for num in 20..30 {
        assert_eq!(
            document
                .resolve(&crate::fixture::reference(num))
                .and_then(crate::object::Resolved::into_owned),
            Err(PdfError::Malformed)
        );
    }
    let spent = budget - document.budget.get();
    assert!(spent < 2 * (1 << 14), "decoded again: {spent} spent");
}

#[test]
fn a_bound_met_finding_a_length_is_not_taken_for_no_length() {
    // Object 4's length ends a chain of references longer than a resolution
    // follows: past the bound, never read as stating no length.
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .raw(
            4,
            b"4 0 obj\n<</Length 10 0 R>>\nstream\nq Q\nendstream\nendobj\n",
        );
    let bound = u32::try_from(MAX_RESOLVE_DEPTH).expect("a small bound");
    for num in 10..10 + bound {
        pdf.object(num, &format!("{} 0 R", num + 1));
    }
    pdf.object(10 + bound, "3");
    let file = pdf.tabled(1, "");
    let document = open(&file).expect("a document");
    assert_eq!(
        document
            .resolve(&crate::fixture::reference(4))
            .and_then(crate::object::Resolved::into_owned),
        Err(PdfError::TooComplex)
    );
}

#[test]
fn a_length_in_a_cycle_is_read_as_unknown() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .raw(
            4,
            b"4 0 obj\n<</Length 5 0 R>>\nstream\nq Q\nendstream\nendobj\n",
        )
        .object(5, "6 0 R")
        .object(6, "5 0 R")
        .raw(
            7,
            b"7 0 obj\n<</Length 7 0 R>>\nstream\nB*\nendstream\nendobj\n",
        );
    let file = pdf.tabled(1, "");
    let document = open(&file).expect("a document");
    for (num, expected) in [(4, &b"q Q"[..]), (7, b"B*")] {
        let Ok(Object::Stream(stream)) = document
            .resolve(&crate::fixture::reference(num))
            .and_then(crate::object::Resolved::into_owned)
        else {
            panic!("a stream");
        };
        assert_eq!(document.decode(&stream).expect("its bytes").bytes, expected);
    }
}

/// A document whose object stream 10 holds object 11, its own length.
fn own_length_inside() -> Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .raw(
            10,
            b"10 0 obj\n<</Type /ObjStm /N 1 /First 5 /Length 11 0 R>>\nstream\n11 0 6\nendstream\nendobj\n",
        );
    let at = pdf.xref_stream(99, "/Root 1 0 R", &[(11, 10, 0)]);
    pdf.finish(at)
}

#[test]
fn an_object_whose_reading_met_one_under_way_is_read_afresh_later() {
    // Reached through stream 10's length, object 11 meets 10 under way; read
    // on its own once 10 is read, it is found.
    let file = own_length_inside();
    let document = open(&file).expect("a document");
    let (stream, length) = (crate::fixture::reference(10), crate::fixture::reference(11));
    let read = document.resolve(&stream).expect("the stream");
    assert!(matches!(&*read, Object::Stream(_)));
    assert_eq!(
        document.resolve(&length).map(|object| object.as_int()),
        Ok(Some(6))
    );
}

#[test]
fn damage_met_under_way_holds_for_that_read_alone() {
    let objects = super::Objects::within(1 << 20);
    let outer = objects.mark(1).expect("room");
    let inner = objects.mark(2).expect("room");
    assert!(
        matches!(objects.get(1), Some(Err(PdfError::Malformed))),
        "1 is under way"
    );
    objects.settle(2, inner, &Err(PdfError::Malformed));
    assert!(
        matches!(objects.get(2), Some(Err(PdfError::Malformed))),
        "the read under way never walks 2 again"
    );
    let object = alloc::sync::Arc::new(Object::Int(7));
    objects.settle(1, outer, &Ok(object));
    assert!(objects.get(2).is_none(), "a later read reads 2 afresh");
    let unrelated = objects.mark(3).expect("room");
    objects.settle(3, unrelated, &Err(PdfError::Malformed));
    assert!(
        matches!(objects.get(3), Some(Err(PdfError::Malformed))),
        "damage met alone holds for every read"
    );
}

#[test]
fn object_streams_that_need_each_other_are_refused_without_fanning_out() {
    // Each object stream's filter is a stream whose length lies in the next,
    // named nine times over: read again each time it is named, the eleven
    // would take 9^10 decodings.
    let mut pdf = Builder::new();
    let mut placed = Vec::new();
    for level in 0..=10u32 {
        let (stream, filter, length) = (100 + level, 200 + level, 300 + level);
        let params = alloc::vec![format!("{filter} 0 R"); 8].join(" ");
        pdf.stream(
            stream,
            &format!("/Type /ObjStm /N 1 /First 4 /Filter {filter} 0 R /DecodeParms [{params}]"),
            b"1 0 null",
        )
        .raw(
            filter,
            format!("{filter} 0 obj\n<</Length {length} 0 R>>\nstream\nx\nendstream\nendobj\n")
                .as_bytes(),
        );
        if level < 10 {
            placed.push((length, stream + 1, 0));
        }
    }
    placed.push((1, 100, 0));
    let at = pdf.xref_stream(999, "/Root 1 0 R", &placed);
    let file = pdf.finish(at);
    assert_eq!(open(&file).err(), Some(PdfError::TooComplex));
}

#[test]
fn an_encrypted_file_cut_before_its_trailer_is_never_read_as_plaintext() {
    let zeros = "00".repeat(32);
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .object(
            9,
            &format!("<</Filter /Standard /V 2 /R 3 /Length 128 /P -4 /O <{zeros}> /U <{zeros}>>>"),
        );
    let at = pdf.table(1, "/Encrypt 9 0 R");
    let whole = pdf.finish(at);
    assert_eq!(open(&whole).err(), Some(PdfError::NeedsPassword));
    assert_eq!(
        open(&whole[..at]).err(),
        Some(PdfError::NeedsPassword),
        "its trailer cut off, it is still encrypted"
    );
}

#[test]
fn an_object_kept_is_shared_never_copied_to_be_read() {
    // Within the share of the budget objects are kept in.
    let body = format!("[{}]", "1 ".repeat(1_000));
    let file = crate::fixture::with_objects(&[(10, body.as_str())], &[]);
    let document = open(&file).expect("a document");
    let held = crate::fixture::reference(10);
    let (Ok(crate::object::Resolved::Shared(first)), Ok(crate::object::Resolved::Shared(again))) =
        (document.resolve(&held), document.resolve(&held))
    else {
        panic!("objects read from the file are shared");
    };
    assert!(alloc::sync::Arc::ptr_eq(&first, &again));
}
