//! A file's structure as a person reads it: heading lines over the records of
//! its container, each at the offset and length it occupies, with the facts it
//! declares beneath it.
//!
//! A report is built from untrusted bytes, so it is bounded: at most
//! [`MAX_REPORT_LINES`] lines of at most [`MAX_LINE_BYTES`] bytes, and text the
//! file carries is shown with its control bytes escaped.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

/// Most lines a report holds. A fixed containment bound: past it the report
/// says it stops.
pub const MAX_REPORT_LINES: usize = 4096;

/// Longest line a report holds, in bytes; a longer one is cut with `…`.
pub const MAX_LINE_BYTES: usize = 192;

/// Most bytes of a text field the file carries a line shows before it is cut.
pub const MAX_TEXT_SHOWN: usize = 96;

/// Columns a record's offset takes, which a fact is indented past so it reads
/// under the record's name.
const OFFSET_COLUMNS: usize = 10;

/// Columns a record's name is given.
const NAME_WIDTH: usize = 12;

/// One line of a report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportLine {
    /// What it says.
    pub text: String,
    /// Whether it heads what follows it: the file's summary, or a block of
    /// facts.
    pub heading: bool,
}

/// A file's structure report.
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

/// A report being written, which refuses lines once it holds as many as it
/// may and then says so once.
#[derive(Debug, Default)]
pub struct Builder {
    lines: Vec<ReportLine>,
    full: bool,
}

impl Builder {
    /// The report written.
    #[must_use]
    pub fn finish(self) -> Report {
        Report { lines: self.lines }
    }

    /// The lines written so far.
    #[must_use]
    pub fn lines(&self) -> &[ReportLine] {
        &self.lines
    }

    /// Whether no more lines fit: a walker stops rather than reading on.
    #[must_use]
    pub const fn full(&self) -> bool {
        self.full
    }

    /// A line heading what follows it, answering whether it fitted.
    pub fn heading(&mut self, text: fmt::Arguments<'_>) -> bool {
        self.push(true, |line| line.write_fmt(text))
    }

    /// One record of the container: where it starts, what it is, how many
    /// bytes it spans, and what it declares.
    pub fn record(
        &mut self,
        offset: usize,
        name: &str,
        length: usize,
        detail: fmt::Arguments<'_>,
    ) -> bool {
        self.push(false, |line| {
            write!(line, "{offset:08x}  ")?;
            push_clean(line, name.as_bytes(), NAME_WIDTH);
            let pad = NAME_WIDTH.saturating_sub(line.len().saturating_sub(OFFSET_COLUMNS));
            for _ in 0..pad {
                line.push(' ');
            }
            write!(line, " {length:>10}  ")?;
            line.write_fmt(detail)
        })
    }

    /// A fact a record states, under it, `depth` levels in.
    pub fn fact(&mut self, depth: usize, text: fmt::Arguments<'_>) -> bool {
        self.push(false, |line| {
            for _ in 0..OFFSET_COLUMNS + depth * 2 {
                line.push(' ');
            }
            line.write_fmt(text)
        })
    }

    /// The walk cannot go on past `offset`, for `why`.
    pub fn stop(&mut self, offset: usize, why: &str) {
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
pub fn push_clean(line: &mut String, bytes: &[u8], most: usize) {
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
#[must_use]
pub fn clean(bytes: &[u8]) -> String {
    let mut text = String::new();
    push_clean(&mut text, bytes, MAX_TEXT_SHOWN);
    text
}

/// `bytes` as at most `most` pairs of hex digits, `…` after when there were
/// more.
#[must_use]
pub fn hex(bytes: &[u8], most: usize) -> String {
    let mut text = String::with_capacity(bytes.len().min(most) * 3 + 3);
    for (index, &byte) in bytes.iter().take(most).enumerate() {
        if index > 0 {
            text.push(' ');
        }
        push_digits(&mut text, byte);
    }
    if bytes.len() > most {
        text.push_str(" …");
    }
    text
}

/// A digest, key or signature as one run of hex digits.
#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        push_digits(&mut text, byte);
    }
    text
}

fn push_digits(text: &mut String, byte: u8) {
    for digit in crate::hex::digits(byte) {
        text.push(char::from(digit));
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::{digest, hex, push_clean, Builder, MAX_LINE_BYTES, MAX_REPORT_LINES};

    #[test]
    fn a_report_that_fills_says_it_stops_and_takes_no_more() {
        let mut out = Builder::default();
        let mut taken = 0;
        while out.fact(0, format_args!("line {taken}")) {
            taken += 1;
        }
        assert_eq!(taken, MAX_REPORT_LINES - 1);
        assert!(out.full());
        assert!(!out.heading(format_args!("after")));
        let last = out.lines().last().expect("lines");
        assert!(last.heading && last.text.contains("stops here"), "{last:?}");
        assert_eq!(out.lines().len(), MAX_REPORT_LINES);
    }

    #[test]
    fn a_long_line_is_cut_at_a_character_and_text_from_the_file_is_escaped() {
        let mut out = Builder::default();
        let long: String = core::iter::repeat_n('\u{e9}', MAX_LINE_BYTES).collect();
        out.fact(0, format_args!("{long}"));
        let line = &out.lines()[0].text;
        assert!(line.len() <= MAX_LINE_BYTES);
        assert!(line.ends_with('\u{2026}'));

        let mut shown = String::new();
        push_clean(&mut shown, b"a\x07b\xffc\n", 64);
        assert_eq!(shown, "a\\x07b\\xffc\\x0a");
        let mut cut = String::new();
        push_clean(&mut cut, &[b'x'; 40], 8);
        assert_eq!(cut, "xxxxxxxx\u{2026}");
    }

    #[test]
    fn a_record_lines_its_offset_name_and_length_up_in_columns() {
        let mut out = Builder::default();
        out.record(0x38, "segments", 120, format_args!("3 segments"));
        out.record(0, "a name longer than its column", 7, format_args!("x"));
        assert_eq!(
            out.lines()[0].text,
            "00000038  segments            120  3 segments"
        );
        assert!(out.lines()[1]
            .text
            .starts_with("00000000  a name longe\u{2026}"));
    }

    #[test]
    fn bytes_are_spelled_as_spaced_pairs_or_one_run() {
        assert_eq!(hex(&[0xde, 0xad, 0xbe], 2), "de ad …");
        assert_eq!(hex(&[0x01], 4), "01");
        assert_eq!(digest(&[0xab, 0x01, 0xff]), "ab01ff");
        assert_eq!(digest(&[]), "");
    }
}
