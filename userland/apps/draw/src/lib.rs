//! Draw, the desktop vector editor (`plans/DRAW.md`): the engine a document
//! window runs, host-testable apart from the runtime that hosts it.
//!
//! A window holds one [`document::Document`] — a drawing and its history —
//! seen through a [`viewport::Viewport`], edited by [`edit`]'s operations,
//! and painted from tiles rendered off the event loop ([`tiles`]).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::disallowed_methods)]

extern crate alloc;

pub mod appwindow;
mod bars;
pub mod compute;
pub mod copy;
pub mod document;
pub mod edit;
pub mod glyphs;
pub mod gradient;
pub mod input;
pub mod inspector;
pub mod layout;
pub mod menu;
pub mod paint;
pub mod pdfopen;
pub mod preferences;
pub mod settings;
pub mod snap;
pub mod stops;
pub mod tiles;
pub mod tools;
pub mod view;
pub mod viewport;
pub mod window;

/// `parts` as a sentence lists them: apart by commas, the last after "and".
pub(crate) fn listed<'a>(parts: impl Iterator<Item = &'a str>) -> alloc::string::String {
    let mut parts = parts.peekable();
    let mut list = alloc::string::String::new();
    while let Some(part) = parts.next() {
        if !list.is_empty() {
            list.push_str(if parts.peek().is_some() {
                ", "
            } else {
                " and "
            });
        }
        list.push_str(part);
    }
    list
}

/// What the crate's tests share: glyphs that never arrive, and a text.
#[cfg(test)]
pub(crate) mod testing {
    use alloc::sync::Arc;

    use tairix_bezier::Point;
    use tairix_drawing::text::Glyph;
    use tairix_drawing::{Baseline, Glyphs, SharedSlice, SharedStr, Text, TextAlign};

    /// No glyph is known.
    pub(crate) struct NoGlyphs;

    impl Glyphs for NoGlyphs {
        fn glyph(&self, _: &str, _: u16, _: bool, _: char) -> Option<Arc<Glyph>> {
            None
        }
    }

    /// Every character a square filling its em.
    pub(crate) struct Squares;

    impl Glyphs for Squares {
        fn glyph(&self, _: &str, _: u16, _: bool, _: char) -> Option<Arc<Glyph>> {
            let mut ring = tairix_bezier::Subpath::new(Point::new(0.0, -1.0));
            ring.elements.extend(
                [(1.0, -1.0), (1.0, 0.0), (0.0, 0.0)]
                    .map(|(x, y)| tairix_bezier::PathEl::Line(Point::new(x, y))),
            );
            ring.closed = true;
            let outline = tairix_bezier::Path {
                subpaths: alloc::vec![ring],
            };
            Some(Arc::new(Glyph::new(outline, 1.0)))
        }
    }

    /// `content` set ten units large in the sans-serif from `origin`.
    pub(crate) fn text(content: &str, origin: Point) -> Text {
        Text {
            content: SharedStr::try_from_str(content).expect("held"),
            family: SharedStr::try_from_str(tairix_drawing::DEFAULT_FAMILY).expect("held"),
            size: 10.0,
            weight: 400,
            italic: false,
            letter_spacing: 0.0,
            align: TextAlign::Start,
            baseline: Baseline::Lines {
                origin,
                line_height: tairix_drawing::DEFAULT_LINE_HEIGHT,
            },
            runs: SharedSlice::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::listed;

    #[test]
    fn a_list_reads_as_a_sentence_writes_it() {
        assert_eq!(listed([].into_iter()), "");
        assert_eq!(listed(["masks"].into_iter()), "masks");
        assert_eq!(listed(["masks", "links"].into_iter()), "masks and links");
        assert_eq!(
            listed(["masks", "links", "scripts"].into_iter()),
            "masks, links and scripts"
        );
    }
}
