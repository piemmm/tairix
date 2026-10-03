use super::{parse, Content, XmlError, MAX_DEPTH, MAX_ELEMENTS};

const SPACE: &str = "urn:example";

#[test]
fn elements_attributes_and_text_are_read_in_order() {
    let root = parse(
        r#"<?xml version="1.0"?><!-- a note --><image w="2" h="3"><stack><layer name="a &amp; b" src='x.png'/>tail</stack></image>"#,
        SPACE,
    )
    .expect("a document");
    assert_eq!(root.name, "image");
    assert_eq!(root.attr("w"), Some("2"));
    let stack = root.children().next().expect("a stack");
    let layer = stack.children().next().expect("a layer");
    assert_eq!(layer.attr("name"), Some("a & b"), "entities decoded");
    assert_eq!(layer.attr("src"), Some("x.png"));
    assert_eq!(stack.text(), "tail");
    assert!(matches!(stack.content.last(), Some(Content::Text(_))));
}

#[test]
fn a_prefix_bound_to_the_readers_namespace_is_dropped_and_any_other_kept() {
    let root = parse(
        r#"<x:a xmlns:x="urn:example" xmlns:y="urn:other"><x:b/><y:c/></x:a>"#,
        SPACE,
    )
    .expect("a document");
    assert_eq!(root.name, "a");
    let names: alloc::vec::Vec<&str> = root.children().map(|child| child.name).collect();
    assert_eq!(names, ["b", "y:c"]);
}

#[test]
fn broken_or_oversized_documents_are_refused() {
    assert_eq!(parse("<a><b></a>", SPACE), Err(XmlError::Malformed));
    assert_eq!(parse("<a b=\"open></a>", SPACE), Err(XmlError::Malformed));
    assert_eq!(parse("<!-- never closed", SPACE), Err(XmlError::Malformed));
    assert_eq!(
        parse("<a/><b/>", SPACE),
        Err(XmlError::Malformed),
        "two roots"
    );
    assert_eq!(parse("just text", SPACE), Err(XmlError::MissingRoot));
    let deep = "<a>".repeat(MAX_DEPTH + 1);
    assert_eq!(parse(&deep, SPACE), Err(XmlError::TooComplex));
    let wide = alloc::format!("<a>{}</a>", "<b/>".repeat(MAX_ELEMENTS));
    assert_eq!(parse(&wide, SPACE), Err(XmlError::TooComplex));
}
