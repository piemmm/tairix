//! Optional content: the default configuration, membership dictionaries and
//! visibility expressions.

use super::Visibility;
use crate::document::Document;
use crate::fixture::{reference, Builder, OPTIONS};
use alloc::format;

/// A document whose optional content properties are `properties`, with
/// groups 10 to 12 and `objects`.
fn document(properties: &str, objects: &[(u32, &str)]) -> alloc::vec::Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(
        1,
        &alloc::format!("<</Type /Catalog /Pages 2 0 R /OCProperties {properties}>>"),
    )
    .object(2, "<</Type /Pages /Kids [3 0 R]>>")
    .object(3, "<</Type /Page>>");
    for num in 10..=12 {
        pdf.object(num, "<</Type /OCG /Name (g)>>");
    }
    for &(num, body) in objects {
        pdf.object(num, body);
    }
    pdf.tabled(1, "")
}

fn shows(file: &[u8], num: u32) -> bool {
    let document = Document::open(file, &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    visibility
        .shows(&document, &reference(num))
        .expect("an answer")
}

fn catalog(document: &Document<'_>) -> crate::object::Dict {
    match document
        .resolve(&crate::fixture::reference(1))
        .map(alloc::borrow::Cow::into_owned)
    {
        Ok(crate::object::Object::Dict(catalog)) => catalog,
        _ => panic!("a catalog"),
    }
}

#[test]
fn the_default_configuration_hides_what_it_turns_off() {
    let file = document("<</OCGs [10 0 R 11 0 R 12 0 R] /D <</OFF [11 0 R]>>>>", &[]);
    assert!(shows(&file, 10));
    assert!(!shows(&file, 11));
    let file = document(
        "<</OCGs [10 0 R 11 0 R 12 0 R] /D <</BaseState /OFF /ON [12 0 R]>>>>",
        &[],
    );
    assert!(!shows(&file, 10));
    assert!(shows(&file, 12));
}

#[test]
fn a_membership_dictionary_follows_its_policy() {
    let properties = "<</OCGs [10 0 R 11 0 R 12 0 R] /D <</OFF [11 0 R]>>>>";
    let file = document(
        properties,
        &[
            (20, "<</Type /OCMD /OCGs [10 0 R 11 0 R]>>"),
            (21, "<</Type /OCMD /OCGs [10 0 R 11 0 R] /P /AllOn>>"),
            (22, "<</Type /OCMD /OCGs [10 0 R 11 0 R] /P /AnyOff>>"),
            (23, "<</Type /OCMD /OCGs 11 0 R /P /AllOff>>"),
            (
                24,
                "<</Type /OCMD /OCGs [10 0 R] /VE [/And 10 0 R [/Not 11 0 R]]>>",
            ),
            (25, "<</Type /OCMD /VE [/Or 11 0 R [/And 10 0 R 11 0 R]]>>"),
        ],
    );
    assert!(shows(&file, 20));
    assert!(!shows(&file, 21));
    assert!(shows(&file, 22));
    assert!(shows(&file, 23));
    assert!(shows(&file, 24));
    assert!(!shows(&file, 25));
}

#[test]
fn a_document_without_optional_content_shows_everything() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page>>")
        .object(10, "<</Type /OCG>>");
    let file = pdf.tabled(1, "");
    assert!(shows(&file, 10));
}

#[test]
fn an_expression_shared_many_times_over_is_evaluated_once() {
    // Each of objects 20 to 30 is four references to the next: unshared,
    // eleven levels of it would be four million evaluations.
    let mut objects: alloc::vec::Vec<(u32, alloc::string::String)> = (20..30)
        .map(|num| {
            (
                num,
                format!("[/And {0} 0 R {0} 0 R {0} 0 R {0} 0 R]", num + 1),
            )
        })
        .collect();
    objects.push((30, alloc::string::String::from("[/And 10 0 R]")));
    objects.push((
        40,
        alloc::string::String::from("<</Type /OCMD /VE 20 0 R>>"),
    ));
    let listed: alloc::vec::Vec<(u32, &str)> = objects
        .iter()
        .map(|(num, body)| (*num, body.as_str()))
        .collect();
    let file = document(
        "<</OCGs [10 0 R 11 0 R 12 0 R] /D <</OFF [11 0 R]>>>>",
        &listed,
    );
    let document = Document::open(&file, &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    assert!(visibility
        .shows(&document, &reference(40))
        .expect("an answer"));
    let held = visibility.expressions.borrow();
    assert_eq!(held.len(), 12, "each of 20 to 30, and group 10");
}
