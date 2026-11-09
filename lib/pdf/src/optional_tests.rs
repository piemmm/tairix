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
    let document = Document::open(crate::fixture::kept(file), &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    visibility
        .shows(&document, &reference(num))
        .expect("an answer")
}

fn catalog(document: &Document) -> crate::object::Dict {
    match document
        .resolve(&crate::fixture::reference(1))
        .and_then(crate::object::Resolved::into_owned)
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
    // Each of objects 20 to 24 is four references to the next: unshared, the
    // six levels would be 2389 evaluations.
    let mut objects: alloc::vec::Vec<(u32, alloc::string::String)> = (20..25)
        .map(|num| {
            (
                num,
                format!("[/And {0} 0 R {0} 0 R {0} 0 R {0} 0 R]", num + 1),
            )
        })
        .collect();
    objects.push((25, alloc::string::String::from("[/And 10 0 R]")));
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
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    assert!(visibility
        .shows(&document, &reference(40))
        .expect("an answer"));
    assert_eq!(
        visibility.expressions.len(),
        7,
        "each of 20 to 25, and group 10"
    );
    assert_eq!(
        crate::limits::MAX_EVALUATION_STEPS - document.work().left(),
        22,
        "every meeting charged once: four at each of five levels, two at the last"
    );
}

#[test]
fn whether_an_expression_is_too_complex_never_depends_on_what_was_read_before() {
    // 20 is an `And` of group 10 sixty-three times; 21 an `Or` of 20 sixty-four
    // times, 4097 expressions met.
    let and = format!("[/And {}]", "10 0 R ".repeat(63));
    let or = format!("[/Or {}]", "20 0 R ".repeat(64));
    let file = document(
        "<</OCGs [10 0 R 11 0 R 12 0 R] /D <</OFF [11 0 R]>>>>",
        &[
            (20, and.as_str()),
            (21, or.as_str()),
            (30, "<</Type /OCMD /VE 20 0 R>>"),
            (31, "<</Type /OCMD /VE 21 0 R>>"),
        ],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    assert_eq!(visibility.shows(&document, &reference(30)), Ok(true));
    assert_eq!(
        visibility.shows(&document, &reference(31)),
        Err(crate::PdfError::TooComplex)
    );
}

/// Objects 20 to 36, each an `And` of the next, 36 one of group 10: 20 nests
/// seventeen levels, one past the bound, and each later one a level less.
fn chain() -> alloc::vec::Vec<(u32, alloc::string::String)> {
    let mut objects: alloc::vec::Vec<(u32, alloc::string::String)> = (20..36)
        .map(|num| (num, format!("[/And {} 0 R]", num + 1)))
        .collect();
    objects.push((36, alloc::string::String::from("[/And 10 0 R]")));
    objects
}

/// A document of [`chain`] and the membership dictionaries `governing`.
fn chained(governing: &[(u32, &str)]) -> alloc::vec::Vec<u8> {
    let objects = chain();
    let mut listed: alloc::vec::Vec<(u32, &str)> = objects
        .iter()
        .map(|(num, body)| (*num, body.as_str()))
        .collect();
    listed.extend_from_slice(governing);
    document(
        "<</OCGs [10 0 R 11 0 R 12 0 R] /D <</OFF [11 0 R]>>>>",
        &listed,
    )
}

/// Whether an expression nests too deep is judged where it is met: one read
/// near the top is refused deeper down, whichever was met first.
#[test]
fn how_deep_an_expression_nests_is_judged_where_it_is_met() {
    let file = chained(&[
        (50, "<</Type /OCMD /VE 22 0 R>>"),
        (51, "<</Type /OCMD /VE [/And [/And 22 0 R]]>>"),
    ]);
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    assert_eq!(visibility.shows(&document, &reference(50)), Ok(true));
    assert_eq!(
        visibility.shows(&document, &reference(51)),
        Err(crate::PdfError::Malformed),
        "22 nests fifteen levels, two down"
    );
}

/// An expression refused is remembered as refused, so content it governs
/// never walks it again.
#[test]
fn an_expression_refused_is_not_walked_again() {
    let file = chained(&[(50, "<</Type /OCMD /VE 20 0 R>>")]);
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    let refused = Err(crate::PdfError::Malformed);
    assert_eq!(visibility.shows(&document, &reference(50)), refused);
    assert!(matches!(
        visibility.expressions.outcome(20),
        Some(crate::memo::Outcome::Deeper(16))
    ));
    assert_eq!(visibility.shows(&document, &reference(50)), refused);
    assert_eq!(visibility.governed.borrow().len(), 1, "answered once");
}

/// An expression that contains itself is refused where it meets itself.
#[test]
fn an_expression_that_contains_itself_is_refused() {
    let file = document(
        "<</OCGs [10 0 R 11 0 R 12 0 R] /D <</OFF [11 0 R]>>>>",
        &[
            (20, "[/Or 10 0 R 20 0 R]"),
            (50, "<</Type /OCMD /VE 20 0 R>>"),
        ],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let visibility = Visibility::of(&document, &catalog(&document)).expect("a visibility");
    assert_eq!(
        visibility.shows(&document, &reference(50)),
        Err(crate::PdfError::Malformed)
    );
}
