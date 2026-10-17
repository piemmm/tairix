//! What a picture file is made of, as a person reads it: each record of its
//! container in file order, at the offset and length it occupies, with what it
//! declares — dimensions, colour model, density, palette, frames, colour
//! profile, text — and every EXIF tag by name with its value.
//!
//! The file is untrusted, so the report is built in a parser sandbox, never in
//! the process that draws it, and it is bounded: at most [`MAX_REPORT_LINES`]
//! lines of at most [`MAX_LINE_BYTES`] bytes. A record that will not read ends
//! the walk with where and why, rather than a guess at what follows; text the
//! file carries is shown with its control bytes escaped.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use crate::{bmp, gif, ico, jpeg, ora, png, sprite, tiff, webp, ImageFormat};

/// Most lines a report holds. A fixed containment bound: past it the report
/// says it stops.
pub const MAX_REPORT_LINES: usize = 4096;

/// Longest line a report holds, in bytes; a longer one is cut with `…`.
pub const MAX_LINE_BYTES: usize = 192;

/// Most bytes of a text field the file carries a line shows before it is cut.
pub(crate) const MAX_TEXT_SHOWN: usize = 96;

/// One line of a report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportLine {
    /// What it says.
    pub text: String,
    /// Whether it heads what follows it: the file's summary, or a block of
    /// facts such as an EXIF directory.
    pub heading: bool,
}

/// A picture file's structure report.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    lines: Vec<ReportLine>,
}

impl Report {
    /// The report's lines, first first.
    #[must_use]
    pub fn lines(&self) -> &[ReportLine] {
        &self.lines
    }

    /// The report, taken apart into its lines.
    #[must_use]
    pub fn into_lines(self) -> Vec<ReportLine> {
        self.lines
    }
}

/// The structure report of `bytes`, read as `format`.
#[must_use]
pub fn inspect(bytes: &[u8], format: ImageFormat) -> Report {
    let mut report = Builder::default();
    match format {
        ImageFormat::Png => png::inspect::report(bytes, &mut report),
        ImageFormat::Jpeg => jpeg::inspect::report(bytes, &mut report),
        ImageFormat::Gif => gif::inspect::report(bytes, &mut report),
        ImageFormat::Bmp => bmp::inspect::report(bytes, &mut report),
        ImageFormat::Ico => ico::inspect::report(bytes, &mut report),
        ImageFormat::Sprite => sprite::inspect::report(bytes, &mut report),
        ImageFormat::Tiff => tiff::inspect::report(bytes, &mut report),
        ImageFormat::Webp => webp::inspect::report(bytes, &mut report),
        ImageFormat::OpenRaster => ora::inspect::report(bytes, &mut report),
    }
    report.finish()
}

/// A report being written, which refuses lines once it holds as many as it
/// may and then says so once.
#[derive(Default)]
pub(crate) struct Builder {
    lines: Vec<ReportLine>,
    full: bool,
}

/// Columns a record's offset, name and length take, which a fact is indented
/// past so it reads under the record's details.
const FACT_INDENT: usize = 10;

impl Builder {
    /// The report written.
    pub(crate) fn finish(self) -> Report {
        Report { lines: self.lines }
    }

    /// Whether no more lines fit: a walker stops rather than reading on.
    pub(crate) const fn full(&self) -> bool {
        self.full
    }

    /// A line heading what follows it.
    pub(crate) fn heading(&mut self, text: fmt::Arguments<'_>) -> bool {
        self.push(true, |line| line.write_fmt(text))
    }

    /// One record of the container: where it starts, what it is, how many
    /// bytes it spans, and what it declares.
    pub(crate) fn record(
        &mut self,
        offset: usize,
        name: &str,
        length: usize,
        detail: fmt::Arguments<'_>,
    ) -> bool {
        self.push(false, |line| {
            write!(line, "{offset:08x}  ")?;
            push_clean(line, name.as_bytes(), 12);
            let pad = 12usize.saturating_sub(line.len().saturating_sub(10));
            for _ in 0..pad {
                line.push(' ');
            }
            write!(line, " {length:>10}  ")?;
            line.write_fmt(detail)
        })
    }

    /// A fact a record states, under it, `depth` levels in.
    pub(crate) fn fact(&mut self, depth: usize, text: fmt::Arguments<'_>) -> bool {
        self.push(false, |line| {
            for _ in 0..FACT_INDENT + depth * 2 {
                line.push(' ');
            }
            line.write_fmt(text)
        })
    }

    /// The walk cannot go on past `offset`, for `why`.
    pub(crate) fn stop(&mut self, offset: usize, why: &str) {
        self.push(true, |line| write!(line, "Stopped at {offset:08x}: {why}"));
    }

    /// Write one line through `write`, cut to [`MAX_LINE_BYTES`]; once the
    /// report holds all but one of the lines it may, the last says it stops.
    fn push(&mut self, heading: bool, write: impl FnOnce(&mut String) -> fmt::Result) -> bool {
        if self.full {
            return false;
        }
        if self.lines.len() + 1 >= MAX_REPORT_LINES {
            self.full = true;
            self.lines.push(ReportLine {
                text: alloc::format!("The report stops here: it holds {MAX_REPORT_LINES} lines"),
                heading: true,
            });
            return false;
        }
        let mut text = String::new();
        // Writing to a `String` cannot fail.
        let _ = write(&mut text);
        cut(&mut text, MAX_LINE_BYTES);
        self.lines.push(ReportLine { text, heading });
        true
    }
}

/// Cut `text` to at most `most` bytes at a character boundary, ending it in
/// `…` when anything was cut.
fn cut(text: &mut String, most: usize) {
    if text.len() <= most {
        return;
    }
    let mut end = most.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push('…');
}

/// Append `bytes` — text the file carries, in no encoding it promises — to
/// `line`: valid UTF-8 as it is, every control character and stray byte as
/// `\xNN`, and at most `most` bytes of it before `…`.
pub(crate) fn push_clean(line: &mut String, bytes: &[u8], most: usize) {
    let start = line.len();
    for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            if ch.is_control() {
                let _ = write!(line, "\\x{:02x}", u32::from(ch) & 0xff);
            } else {
                line.push(ch);
            }
            if line.len() - start > most {
                cut_from(line, start, most);
                return;
            }
        }
        for byte in chunk.invalid() {
            let _ = write!(line, "\\x{byte:02x}");
            if line.len() - start > most {
                cut_from(line, start, most);
                return;
            }
        }
    }
}

/// Cut what was appended to `line` from `start` to `most` bytes, with `…`.
fn cut_from(line: &mut String, start: usize, most: usize) {
    let mut end = start + most;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.truncate(end);
    line.push('…');
}

/// `bytes` as text a line can show, cut at [`MAX_TEXT_SHOWN`].
pub(crate) fn clean(bytes: &[u8]) -> String {
    let mut text = String::new();
    push_clean(&mut text, bytes, MAX_TEXT_SHOWN);
    text
}

/// `bytes` as at most `most` pairs of hex digits, `…` after when there were
/// more.
pub(crate) fn hex(bytes: &[u8], most: usize) -> String {
    let mut text = String::with_capacity(bytes.len().min(most) * 3 + 3);
    for (index, byte) in bytes.iter().take(most).enumerate() {
        if index > 0 {
            text.push(' ');
        }
        let _ = write!(text, "{byte:02x}");
    }
    if bytes.len() > most {
        text.push_str(" …");
    }
    text
}

/// `pixels` per metre as the dots per inch it amounts to, to the nearest.
pub(crate) fn dpi_of_ppm(pixels: u32) -> u64 {
    // An inch is 0.0254 m, so a metre's pixels × 127 / 5000 is an inch's.
    (u64::from(pixels) * 127 + 2500) / 5000
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
