//! The document's XML, scanned by the shared `lib/xml` scanner with the SVG
//! namespace's elements named by their local names.

pub use tairix_xml::{Content, Element, MAX_ELEMENTS};

use crate::error::SvgError;

/// The namespace a prefixed element must resolve to before it is treated as
/// SVG. An unprefixed element is SVG whatever the document declares, so an
/// asset written without an `xmlns` still renders.
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

/// Parse `input` into the document's root element.
///
/// # Errors
/// [`SvgError::Malformed`], [`SvgError::MissingRoot`] and
/// [`SvgError::TooComplex`], as the scanner refuses the document.
pub fn parse(input: &str) -> Result<Element<'_>, SvgError> {
    tairix_xml::parse(input, SVG_NAMESPACE).map_err(|refusal| match refusal {
        tairix_xml::XmlError::Malformed => SvgError::Malformed,
        tairix_xml::XmlError::MissingRoot => SvgError::MissingRoot,
        tairix_xml::XmlError::TooComplex => SvgError::TooComplex,
    })
}

/// `element`'s reference to another element, in either the SVG 2 spelling
/// or the SVG 1.1 `xlink` one.
#[must_use]
pub fn href<'e>(element: &'e Element<'_>) -> Option<&'e str> {
    element.attr("href").or_else(|| element.attr("xlink:href"))
}
