use alloc::borrow::Cow;
use core::fmt::Write as _;
use core::mem::size_of;

use super::{
    parse, parse_peak_bytes, Content, Element, Scope, XmlError, MAX_ATTRIBUTES, MAX_DEPTH,
    MAX_ELEMENTS,
};

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
fn every_element_knows_its_namespace() {
    let root = parse(
        concat!(
            r#"<e:Envelope xmlns:e="urn:env" xmlns="urn:default">"#,
            r#"<Body><w:Hello xmlns:w="urn:wsd"/><plain xmlns=""/><u:stray/></Body>"#,
            r#"</e:Envelope>"#,
        ),
        "",
    )
    .expect("a document");
    assert!(root.is("urn:env", "Envelope"));
    assert_eq!(
        root.name, "e:Envelope",
        "a reader naming no namespace keeps the prefix"
    );
    let body = root.children().next().expect("a body");
    assert!(body.is("urn:default", "Body"), "the default namespace");
    let names: alloc::vec::Vec<_> = body
        .children()
        .map(|child| (child.namespace, child.local_name()))
        .collect();
    assert_eq!(
        names,
        [(Some("urn:wsd"), "Hello"), (None, "plain"), (None, "stray"),],
        "an empty default undeclares it, and an unbound prefix is in no namespace"
    );
    let referenced = parse(r#"<x:a xmlns:x="urn:&#120;"/>"#, "").expect("a document");
    assert_eq!(
        referenced.namespace, None,
        "a referenced namespace matches nothing"
    );
}

#[test]
fn a_qualified_name_in_text_resolves_where_it_stands() {
    let root = parse(
        concat!(
            r#"<a xmlns:p="urn:outer" xmlns="urn:default">"#,
            r#"<b xmlns:p="urn:inner"><t>p:Device Computer q:Other :bad</t></b>"#,
            r#"</a>"#,
        ),
        "",
    )
    .expect("a document");
    let outer = Scope::root(&root);
    let b = root.children().next().expect("b");
    let at_b = outer.enter(b);
    let t = b.children().next().expect("t");
    let at_t = at_b.enter(t);
    let text = t.text();
    let mut names = text.split_whitespace().map(|qname| at_t.resolve(qname));
    assert_eq!(
        names.next(),
        Some(Some((Some("urn:inner"), "Device"))),
        "shadowed"
    );
    assert_eq!(names.next(), Some(Some((Some("urn:default"), "Computer"))));
    assert_eq!(names.next(), Some(None), "an unbound prefix");
    assert_eq!(names.next(), Some(None), "an empty prefix");
    assert_eq!(
        outer.resolve("p:Device"),
        Some((Some("urn:outer"), "Device"))
    );
    assert_eq!(
        at_t.namespace("xml"),
        Some("http://www.w3.org/XML/1998/namespace")
    );
    assert_eq!(at_t.element().local_name(), "t");
}

#[test]
fn an_attribute_given_twice_or_too_many_is_refused() {
    assert_eq!(
        parse(r#"<a b="1" b="2"/>"#, SPACE),
        Err(XmlError::Malformed)
    );
    assert_eq!(
        parse(r#"<a xmlns:p="urn:x" xmlns:p="urn:y"/>"#, SPACE),
        Err(XmlError::Malformed),
        "a prefix bound twice on one element"
    );
    assert_eq!(parse(r#"<a :b="1"/>"#, SPACE), Err(XmlError::Malformed));
    assert_eq!(parse("<:a/>", SPACE), Err(XmlError::Malformed));
    assert_eq!(parse("<a:/>", SPACE), Err(XmlError::Malformed));
    let attributes = |count: usize| -> alloc::string::String {
        let mut names = alloc::string::String::new();
        for i in 0..count {
            let _ = write!(names, "c{i}=\"\" ");
        }
        alloc::format!("<a {names}/>")
    };
    assert!(parse(&attributes(MAX_ATTRIBUTES), SPACE).is_ok());
    assert_eq!(
        parse(&attributes(MAX_ATTRIBUTES + 1), SPACE),
        Err(XmlError::TooComplex)
    );
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

/// What a parsed tree holds once its scan is done: every vector's capacity
/// and every decoded string's.
fn held(element: &Element<'_>) -> u64 {
    let owned = |text: &Cow<'_, str>| match text {
        Cow::Owned(text) => text.capacity() as u64,
        Cow::Borrowed(_) => 0,
    };
    let attrs = (element.attrs.capacity() * size_of::<(&str, Cow<'_, str>)>()) as u64
        + element
            .attrs
            .iter()
            .map(|(_, value)| owned(value))
            .sum::<u64>();
    let content = (element.content.capacity() * size_of::<Content<'_>>()) as u64;
    let below: u64 = element
        .content
        .iter()
        .map(|node| match node {
            Content::Element(child) => held(child),
            Content::Text(run) => owned(run),
        })
        .sum();
    attrs + content + below
}

/// What a parse holds stays within the bound read from its length alone, for
/// documents shaped to hold the most: as deep, as wide, as many attributes
/// and as much decoded text as their length allows.
#[test]
fn a_parse_holds_no_more_than_its_length_bounds() {
    let deep = alloc::format!("{}{}", "<a>".repeat(MAX_DEPTH), "</a>".repeat(MAX_DEPTH));
    let wide = alloc::format!("<a>{}</a>", "<b/>".repeat(MAX_ELEMENTS - 1));
    let mut full = alloc::string::String::new();
    for name in ('\u{4e00}'..).take(MAX_ATTRIBUTES) {
        let _ = write!(full, "{name}=\"\" ");
    }
    let attributed = alloc::format!("<a>{}</a>", alloc::format!("<b {full}/>").repeat(4));
    let decoded = alloc::format!("<a>{}</a>", "&amp;x<b/>".repeat(256));
    let runs = alloc::format!("<a>{}</a>", "x<b/>".repeat(1024));
    for document in [deep, wide, attributed, decoded, runs] {
        let root = parse(&document, SPACE).expect("a document");
        let (held, bound) = (held(&root), parse_peak_bytes(document.len()));
        assert!(held <= bound, "{held} past {bound} for {}", &document[..16]);
    }
    assert!(parse_peak_bytes(10) <= parse_peak_bytes(11));
}
