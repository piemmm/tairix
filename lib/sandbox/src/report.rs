//! A structure report on the wire: its line count, then each line's heading
//! flag and text.
//!
//! Read back as though the worker had been taken over: the count and every
//! line are held to the report's own bounds, and a line that is not printable
//! text refuses the whole reply.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::report::{ReportLine, MAX_LINE_BYTES, MAX_REPORT_LINES};

use crate::wire::{Reader, Writer};

/// Write `lines` after a reply's tag.
pub(crate) fn write_lines(w: &mut Writer, lines: &[ReportLine]) {
    w.u32(u32::try_from(lines.len()).unwrap_or(u32::MAX));
    for line in lines {
        w.u8(u8::from(line.heading));
        w.bytes(line.text.as_bytes());
    }
}

/// Why a reply's lines were not taken.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum LinesFault {
    /// They broke their grammar or their bounds: they cannot be believed.
    Malformed,
    /// There was no memory to hold them.
    NoMemory,
}

/// The lines a reply carries from where `r` stands, refused whole when any of
/// them breaks its bounds.
pub(crate) fn read_lines(r: &mut Reader<'_>) -> Result<Vec<ReportLine>, LinesFault> {
    let count = usize::try_from(r.u32().map_err(malformed)?).map_err(malformed)?;
    if count > MAX_REPORT_LINES {
        return Err(LinesFault::Malformed);
    }
    let mut lines = Vec::new();
    lines
        .try_reserve_exact(count)
        .map_err(|_| LinesFault::NoMemory)?;
    for _ in 0..count {
        let heading = match r.u8() {
            Ok(0) => false,
            Ok(1) => true,
            _ => return Err(LinesFault::Malformed),
        };
        let text = r.bytes(MAX_LINE_BYTES).map_err(malformed)?;
        let text = core::str::from_utf8(text).map_err(malformed)?;
        if text.chars().any(char::is_control) {
            return Err(LinesFault::Malformed);
        }
        lines.push(ReportLine {
            text: String::from(text),
            heading,
        });
    }
    Ok(lines)
}

/// Any failure to read a line: it cannot be believed.
fn malformed<E>(_: E) -> LinesFault {
    LinesFault::Malformed
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;

    use tairix_util::report::{ReportLine, MAX_LINE_BYTES, MAX_REPORT_LINES};

    use super::{read_lines, write_lines, LinesFault};
    use crate::wire::{Reader, Writer};

    fn line(text: &str, heading: bool) -> ReportLine {
        ReportLine {
            text: String::from(text),
            heading,
        }
    }

    #[test]
    fn lines_round_trip_with_their_headings() {
        let lines = vec![line("A file", true), line("  a fact", false)];
        let mut w = Writer::new();
        write_lines(&mut w, &lines);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(read_lines(&mut r), Ok(lines));
        assert!(r.is_exhausted());
    }

    #[test]
    fn a_line_past_its_bounds_or_holding_a_control_refuses_the_reply() {
        let refused = |write: &dyn Fn(&mut Writer)| {
            let mut w = Writer::new();
            write(&mut w);
            let bytes = w.finish();
            read_lines(&mut Reader::new(&bytes))
        };
        let too_many = u32::try_from(MAX_REPORT_LINES + 1).expect("small");
        assert_eq!(refused(&|w| w.u32(too_many)), Err(LinesFault::Malformed));
        let too_long = vec![b'x'; MAX_LINE_BYTES + 1];
        let long = |w: &mut Writer| {
            w.u32(1);
            w.u8(0);
            w.bytes(&too_long);
        };
        assert_eq!(refused(&long), Err(LinesFault::Malformed));
        let control = |w: &mut Writer| {
            w.u32(1);
            w.u8(0);
            w.bytes(b"bell\x07");
        };
        assert_eq!(refused(&control), Err(LinesFault::Malformed));
        let flag = |w: &mut Writer| {
            w.u32(1);
            w.u8(2);
            w.bytes(b"x");
        };
        assert_eq!(refused(&flag), Err(LinesFault::Malformed));
        let short = |w: &mut Writer| w.u32(2);
        assert_eq!(refused(&short), Err(LinesFault::Malformed));
    }
}
