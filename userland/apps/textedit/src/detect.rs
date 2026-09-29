//! What a freshly opened document is: text or binary, the format its name
//! implies, its line-ending convention, and the indentation it uses.
//!
//! Nothing here parses the document. The binary check counts byte classes the
//! grid already decodes; the format comes from the name through the one
//! extension registry. A format only the document's opening bytes reveal is
//! asked of the sandbox.

use core::ops::ControlFlow;

use tairix_browse::{media_for_name, MediaType};
use tairix_syntax::{store_for_name, Format};

use crate::document::{Document, Source};
use crate::text::{Decoder, Glyph};

/// Most of a document's head the binary check reads.
pub const HEAD_BYTES: usize = 64 * 1024;

/// Lines the indentation check looks at.
const INDENT_SAMPLE_LINES: usize = 1000;

/// A line-ending convention.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum LineEnding {
    /// `\n`.
    #[default]
    Lf,
    /// `\r\n`.
    CrLf,
}

impl LineEnding {
    /// The bytes a line break in this convention is.
    #[must_use]
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }

    /// What the status band calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
        }
    }
}

/// What one level of indentation is.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Indent {
    /// A tab.
    #[default]
    Tab,
    /// This many spaces.
    Spaces(u8),
}

/// Whether the first [`HEAD_BYTES`] of `source` look like binary data
/// rather than text: they hold a NUL, or more than one unit in ten is a
/// control byte text does not use or a byte that is not UTF-8.
#[must_use]
pub fn looks_binary(source: &impl Source) -> bool {
    let mut units = 0usize;
    let mut odd = 0usize;
    let mut nul = false;
    let mut count = |_: usize, _: usize, glyph: Glyph| {
        match glyph {
            Glyph::Control(0) => {
                nul = true;
                return ControlFlow::Break(());
            }
            Glyph::Control(b'\n' | b'\r' | 0x0c | 0x1b)
            | Glyph::Tab
            | Glyph::Char(_)
            | Glyph::Hidden(_) => {}
            Glyph::Control(_) | Glyph::Invalid(_) => odd += 1,
        }
        units += 1;
        ControlFlow::Continue(())
    };
    let mut decoder = Decoder::new(0);
    let mut read = 0;
    source.walk(0, |slice| {
        let slice = &slice[..slice.len().min(HEAD_BYTES - read)];
        read += slice.len();
        if decoder.feed(slice, &mut count).is_break() || read == HEAD_BYTES {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    // A sequence the head cuts is not held against it.
    nul || odd * 10 > units
}

/// The format a document named `name` is written in, when the name says.
///
/// A settings store is known by its fixed file name first, because two stores
/// of different grammars share an extension; then the extension decides.
#[must_use]
pub fn format_for_name(name: &str) -> Option<Format> {
    let leaf = name.rsplit_once('/').map_or(name, |(_, leaf)| leaf);
    store_for_name(leaf).or_else(|| media_for_name(leaf).and_then(format_for_media))
}

/// The format a content type is written in, for the types that have one.
#[must_use]
pub const fn format_for_media(media: MediaType) -> Option<Format> {
    match media {
        MediaType::TextHtml => Some(Format::Html),
        MediaType::Xml | MediaType::ImageSvg => Some(Format::Xml),
        MediaType::TextCss => Some(Format::Css),
        MediaType::TextJavaScript => Some(Format::JavaScript),
        MediaType::Json => Some(Format::Json),
        MediaType::Yaml => Some(Format::Yaml),
        MediaType::Toml => Some(Format::Toml),
        MediaType::TextMarkdown => Some(Format::Markdown),
        MediaType::TextRust => Some(Format::Rust),
        MediaType::TextC => Some(Format::C),
        MediaType::TextJava => Some(Format::Java),
        MediaType::TextPython => Some(Format::Python),
        MediaType::ShellScript => Some(Format::Shell),
        MediaType::TextPlain | MediaType::TextCsv => Some(Format::PlainText),
        _ => None,
    }
}

/// The convention of the document's first line break; LF when it has none.
#[must_use]
pub fn line_ending(document: &Document) -> LineEnding {
    let first = document.line_bounds(0);
    if first.next - first.end == 2 {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

/// The indentation the document uses, judged from its first lines: a tab if
/// an indented line starts with one first, else the narrowest run of
/// leading spaces seen, else a tab.
#[must_use]
pub fn indentation(document: &Document) -> Indent {
    let mut narrowest: Option<usize> = None;
    for line in 0..document.line_count().min(INDENT_SAMPLE_LINES) {
        let start = document.line_start(line);
        let mut spaces = 0usize;
        let mut tab = false;
        document.walk(start, |slice| {
            for &byte in slice {
                match byte {
                    b' ' if spaces < 8 => spaces += 1,
                    b'\t' if spaces == 0 => {
                        tab = true;
                        return ControlFlow::Break(());
                    }
                    _ => return ControlFlow::Break(()),
                }
            }
            ControlFlow::Continue(())
        });
        if tab && narrowest.is_none() {
            return Indent::Tab;
        }
        if spaces >= 2 {
            narrowest = Some(narrowest.map_or(spaces, |seen| seen.min(spaces)));
        }
    }
    narrowest.map_or(Indent::Tab, |spaces| {
        Indent::Spaces(u8::try_from(spaces).unwrap_or(8))
    })
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod tests;
