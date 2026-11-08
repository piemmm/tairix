//! Opening documents: resolution, object streams, repair and the page tree.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Document, Options};
use crate::fixture::{one_page, Builder};
use crate::limits::MAX_PAGE_TREE_DEPTH;
use crate::object::Object;
use crate::PdfError;

const OPTIONS: Options<'static> = Options {
    password: None,
    budget: 1 << 20,
};

fn open(file: &[u8]) -> Result<Document<'_>, PdfError> {
    Document::open(file, &OPTIONS)
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
            .map(alloc::borrow::Cow::into_owned),
        Err(PdfError::Malformed)
    );
    assert_eq!(
        document
            .resolve(&crate::fixture::reference(99))
            .map(alloc::borrow::Cow::into_owned),
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
            .map(alloc::borrow::Cow::into_owned)
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
    let document = Document::open(&file, &options).expect("a document");
    let Ok(Object::Stream(stream)) = document
        .resolve(&crate::fixture::reference(4))
        .map(alloc::borrow::Cow::into_owned)
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
        .map(alloc::borrow::Cow::into_owned)
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
