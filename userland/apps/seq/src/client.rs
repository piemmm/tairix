//! The sequence engine: generate and write the numbers a [`Job`] names.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_help::{own_short_help, HelpSource};

use core::cmp::Ordering;

use crate::command::{Command, Job};
use crate::error::SeqError;
use crate::exact::Integer;
use crate::format::Format;
use crate::number::{scan_operand, Layout, Operand};

/// The usage banner the short-help switches print when `seq`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "usage: seq [-f format] [-s string] [-w] [first [increment]] last";

/// `seq`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "seq";

pub use tairix_util::output::Output;

/// Flush threshold for the output buffer: one write call delivers many
/// numbers instead of one, without unbounded buffering.
const FLUSH_LEN: usize = 8192;

/// Run one [`Command`], writing the sequence to `out`. `locale` is the
/// user's `LANG` preference, if set; `help` is the tool's own `Help/`
/// tree, read by the short-help switches.
///
/// # Errors
///
/// [`SeqError::Output`] when a write failed, and the number-scanning
/// usage errors ([`SeqError::InvalidNumber`], [`SeqError::NotANumber`],
/// [`SeqError::ZeroIncrement`]) the operand scan raises.
pub fn run(
    command: Command,
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), SeqError> {
    let job = match command {
        Command::Help => {
            let bytes = own_short_help(help, locale, OWN_WORD)
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            return out.write_all(&bytes).map_err(|_| SeqError::Output);
        }
        Command::Print(job) => job,
    };
    print_job(&job, out)
}

/// Print the sequence `job` names.
fn print_job(job: &Job, out: &dyn Output) -> Result<(), SeqError> {
    let spelled = spellings(&job.operands);
    let mut scanned = [Operand {
        value: 1.0,
        layout: Layout::Fixed {
            integer: 1,
            precision: 0,
        },
    }; 3];
    for (at, (slot, text)) in scanned.iter_mut().zip(spelled).enumerate() {
        *slot = scan_operand(text)?;
        // A zero step is refused before LAST is read, as GNU refuses it.
        if at == STEP && slot.value == 0.0 {
            return Err(SeqError::ZeroIncrement(String::from(text)));
        }
    }
    let values = scanned.map(|operand| operand.value);
    let mut emitter = Emitter::new(out, job.separator.as_bytes());
    match (&job.format, Plan::of(job, &spelled, &scanned)) {
        (Some(format), _) => floats(values, format, &mut emitter)?,
        (None, Plan::Integers(first, step, last, width)) => {
            integers(first, &step, &last, width, &mut emitter)?;
        }
        (None, Plan::Floats(format)) => floats(values, &format, &mut emitter)?,
    }
    emitter.finish()
}

/// Where INCREMENT sits among the three operands.
const STEP: usize = 1;

/// The FIRST, INCREMENT and LAST spellings, an omitted FIRST or INCREMENT
/// being `1`.
fn spellings(operands: &[String]) -> [&str; 3] {
    match operands {
        [last] => ["1", "1", last],
        [first, last] => [first, "1", last],
        [first, step, last, ..] => [first, step, last],
        [] => ["1", "1", "1"],
    }
}

/// How the default format prints a sequence.
enum Plan {
    /// Exactly, as integers: FIRST, INCREMENT, the bound LAST sets, and the
    /// `-w` width.
    Integers(Integer, Integer, Integer, usize),
    /// Through a float format.
    Floats(Format),
}

impl Plan {
    /// The default plan: `%g` when an operand shows no fixed point (`-w`
    /// then pads nothing, as GNU's does), otherwise the precision FIRST and
    /// INCREMENT show, padded under `-w` to the wider of FIRST and LAST.
    fn of(job: &Job, spelled: &[&str; 3], scanned: &[Operand; 3]) -> Self {
        let precision_of = |operand: &Operand| match operand.layout {
            Layout::Fixed { precision, .. } => Some(precision),
            Layout::Free => None,
        };
        let [Some(first), Some(step), Some(_)] = scanned.each_ref().map(precision_of) else {
            return Self::Floats(Format::shortest());
        };
        let precision = first.max(step);
        let width = if job.equal_width {
            scanned[0]
                .layout
                .width_at(precision)
                .max(scanned[2].layout.width_at(precision))
        } else {
            0
        };
        if precision == 0 {
            let descending = scanned[1].value < 0.0;
            let exact = Integer::from_spelling(spelled[0])
                .zip(Integer::from_spelling(spelled[1]))
                .zip(Integer::bound(spelled[2], descending));
            if let Some(((first, step), last)) = exact {
                return Self::Integers(first, step, last, width);
            }
        }
        Self::Floats(Format::fixed(precision, width))
    }
}

/// Print FIRST, FIRST + INCREMENT, … through LAST exactly.
fn integers(
    mut value: Integer,
    step: &Integer,
    last: &Integer,
    width: usize,
    emitter: &mut Emitter<'_>,
) -> Result<(), SeqError> {
    let past = if step.is_negative() {
        Ordering::Less
    } else {
        Ordering::Greater
    };
    while value.cmp(last) != past {
        emitter.integer(&value, width)?;
        value.add(step);
    }
    Ok(())
}

/// Print `first + i × step` for `i` = 0, 1, … while it has not passed
/// `last`, each through `format`.
///
/// Each value is the product, never a running sum, so no error accumulates.
/// A value rounding carried just past `last` stands for `last` when its
/// number, as `format` writes it, reads back as `last` exactly and the
/// value before it did not already, as GNU's does; FIRST past `last` never
/// prints.
fn floats(
    [first, step, last]: [f64; 3],
    format: &Format,
    emitter: &mut Emitter<'_>,
) -> Result<(), SeqError> {
    let reads_back_as_last = |x: f64| {
        format
            .rounded(x)
            .is_some_and(|value| value.partial_cmp(&last) == Some(Ordering::Equal))
    };
    let passed = |x: f64| x.is_nan() || if step > 0.0 { x > last } else { x < last };
    let mut previous = None;
    let mut index = 0.0;
    loop {
        let x = if index == 0.0 {
            first
        } else {
            first + index * step
        };
        if passed(x) {
            // LAST's own rendering, so a step landing a hair below zero is
            // never shown as `-0.0`.
            if previous.is_some_and(|before| !reads_back_as_last(before)) && reads_back_as_last(x) {
                emitter.float(format, last)?;
            }
            return Ok(());
        }
        emitter.float(format, x)?;
        previous = Some(x);
        index += 1.0;
    }
}

/// The output stream, buffered: numbers joined by the separator and the
/// last ended by a line break, written in blocks of at most [`FLUSH_LEN`]
/// however wide a number's padding.
struct Emitter<'a> {
    out: &'a dyn Output,
    separator: &'a [u8],
    buffer: Vec<u8>,
    started: bool,
}

impl<'a> Emitter<'a> {
    fn new(out: &'a dyn Output, separator: &'a [u8]) -> Self {
        Self {
            out,
            separator,
            buffer: Vec::new(),
            started: false,
        }
    }

    fn begin(&mut self) -> Result<(), SeqError> {
        if self.started {
            self.buffer.extend_from_slice(self.separator);
        }
        self.started = true;
        self.flush_full()
    }

    /// `x` through `format`, written as it renders so a field of any size
    /// stays within the buffer.
    fn float(&mut self, format: &Format, x: f64) -> Result<(), SeqError> {
        self.begin()?;
        format.render_into(x, self).map_err(|_| SeqError::Output)?;
        self.flush_full()
    }

    /// `value` as `%0WIDTHd` prints it, its padding written in blocks.
    fn integer(&mut self, value: &Integer, width: usize) -> Result<(), SeqError> {
        self.begin()?;
        if value.sign_shown() {
            self.buffer.push(b'-');
        }
        let mut zeros = width.saturating_sub(value.shown_len());
        while zeros > 0 {
            let block = zeros.min(FLUSH_LEN);
            self.buffer.resize(self.buffer.len() + block, b'0');
            zeros -= block;
            self.flush_full()?;
        }
        self.buffer.extend(value.digit_bytes());
        self.flush_full()
    }

    fn flush_full(&mut self) -> Result<(), SeqError> {
        if self.buffer.len() >= FLUSH_LEN {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), SeqError> {
        self.out
            .write_all(&self.buffer)
            .map_err(|_| SeqError::Output)?;
        self.buffer.clear();
        Ok(())
    }

    fn finish(mut self) -> Result<(), SeqError> {
        if self.started {
            self.buffer.push(b'\n');
        }
        self.flush()
    }
}

impl core::fmt::Write for Emitter<'_> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.buffer.extend_from_slice(text.as_bytes());
        self.flush_full().map_err(|_| core::fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use tairix_abi::Errno;
    use tairix_help::{HelpSource, SourceError};

    use super::{run, Output, USAGE};
    use crate::command::parse;
    use crate::error::SeqError;

    /// An [`Output`] recording everything written.
    #[derive(Default)]
    struct Sink {
        written: RefCell<Vec<u8>>,
    }

    impl Output for Sink {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.written.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    /// A help source with no documents, so the short help falls back to
    /// the usage banner.
    struct NoHelp;

    impl HelpSource for NoHelp {
        fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
            Ok(Vec::new())
        }
        fn read(&self, _locale: &str, _document: &str) -> Result<Option<Vec<u8>>, SourceError> {
            Ok(None)
        }
    }

    fn seq(args: &[&str]) -> Result<String, SeqError> {
        let sink = Sink::default();
        run(parse(args)?, None, &NoHelp, &sink)?;
        Ok(String::from_utf8(sink.written.into_inner()).expect("utf-8 output"))
    }

    #[test]
    fn counts_from_one_by_one() {
        assert_eq!(seq(&["5"]).expect("runs"), "1\n2\n3\n4\n5\n");
        assert_eq!(seq(&["1"]).expect("runs"), "1\n");
        assert_eq!(seq(&["2", "5"]).expect("runs"), "2\n3\n4\n5\n");
        assert_eq!(seq(&["1", "2", "10"]).expect("runs"), "1\n3\n5\n7\n9\n");
    }

    #[test]
    fn an_empty_range_prints_nothing() {
        assert_eq!(seq(&["5", "1"]).expect("runs"), "");
        assert_eq!(seq(&["0"]).expect("runs"), "");
        assert_eq!(seq(&["1", "-1", "5"]).expect("runs"), "");
    }

    #[test]
    fn descending_sequences() {
        assert_eq!(seq(&["5", "-1", "1"]).expect("runs"), "5\n4\n3\n2\n1\n");
        assert_eq!(seq(&["3", "-1.5", "0"]).expect("runs"), "3.0\n1.5\n0.0\n");
        assert_eq!(seq(&["-1", "-3"]).expect("runs"), "");
        assert_eq!(seq(&["-1", "-1", "-3"]).expect("runs"), "-1\n-2\n-3\n");
    }

    #[test]
    fn separators_join_numbers() {
        assert_eq!(seq(&["-s", ",", "3"]).expect("runs"), "1,2,3\n");
        // A multi-byte separator leaves the exact path but prints the same
        // integers.
        assert_eq!(seq(&["-s", "::", "3"]).expect("runs"), "1::2::3\n");
        assert_eq!(seq(&["-s", "", "3"]).expect("runs"), "123\n");
    }

    #[test]
    fn fractional_steps_infer_the_precision() {
        assert_eq!(
            seq(&["0.5", "0.5", "2"]).expect("runs"),
            "0.5\n1.0\n1.5\n2.0\n"
        );
        assert_eq!(seq(&["1", "0.5", "2"]).expect("runs"), "1.0\n1.5\n2.0\n");
        assert_eq!(
            seq(&["1", "0.25", "1.5"]).expect("runs"),
            "1.00\n1.25\n1.50\n"
        );
        // The GNU rounding rule: the value one step past LAST prints when
        // it renders as LAST.
        assert_eq!(
            seq(&["0", "0.000001", "0.000003"]).expect("runs"),
            "0.000000\n0.000001\n0.000002\n0.000003\n"
        );
    }

    #[test]
    fn equal_width_pads_with_zeros() {
        assert_eq!(seq(&["-w", "8", "10"]).expect("runs"), "08\n09\n10\n");
        assert_eq!(
            seq(&["-w", "1", "3", "10"]).expect("runs"),
            "01\n04\n07\n10\n"
        );
        assert_eq!(
            seq(&["-w", "0.5", "0.5", "2"]).expect("runs"),
            "0.5\n1.0\n1.5\n2.0\n"
        );
        assert_eq!(
            seq(&["-w", "-2", "2"]).expect("runs"),
            "-2\n-1\n00\n01\n02\n"
        );
    }

    #[test]
    fn formats_shape_the_output() {
        assert_eq!(
            seq(&["-f", "%.2f", "3"]).expect("runs"),
            "1.00\n2.00\n3.00\n"
        );
        assert_eq!(seq(&["-f", "%g", "3"]).expect("runs"), "1\n2\n3\n");
        assert_eq!(
            seq(&["-f", "%.2e", "100", "100", "300"]).expect("runs"),
            "1.00e+02\n2.00e+02\n3.00e+02\n"
        );
        assert_eq!(seq(&["-f", "[%.0f]", "2"]).expect("runs"), "[1]\n[2]\n");
    }

    #[test]
    fn exact_integers_beyond_the_float_mantissa() {
        // 2^64 + 1 and neighbours: exact only through the decimal path.
        assert_eq!(
            seq(&["18446744073709551615", "18446744073709551617"]).expect("runs"),
            "18446744073709551615\n18446744073709551616\n18446744073709551617\n"
        );
        // A carry across every digit.
        assert_eq!(
            seq(&[
                "999999999999999999999999999999",
                "1000000000000000000000000000000"
            ])
            .expect("runs"),
            "999999999999999999999999999999\n1000000000000000000000000000000\n"
        );
        // Steps up to 200 stay exact.
        assert_eq!(
            seq(&["9999999999999999999999", "200", "10000000000000000000399"]).expect("runs"),
            "9999999999999999999999\n10000000000000000000199\n10000000000000000000399\n"
        );
    }

    #[test]
    fn integer_spellings_reach_the_exact_path() {
        assert_eq!(seq(&["1e1"]).expect("runs").lines().count(), 10);
        assert_eq!(seq(&["0x14"]).expect("runs").lines().count(), 20);
        assert_eq!(seq(&["8", "1e1"]).expect("runs"), "8\n9\n10\n");
        assert_eq!(seq(&["007"]).expect("runs"), "1\n2\n3\n4\n5\n6\n7\n");
    }

    #[test]
    fn long_runs_cross_the_flush_boundary_intact() {
        // > 8 KiB of output exercises the buffered flush with the
        // held-back separator/terminator byte.
        use core::fmt::Write;

        let text = seq(&["3000"]).expect("runs");
        let expected: String = (1..=3000).fold(String::new(), |mut acc, n| {
            let _ = writeln!(acc, "{n}");
            acc
        });
        assert_eq!(text, expected);
    }

    #[test]
    fn scan_errors_surface_in_operand_order() {
        assert_eq!(
            seq(&["x", "0", "y"]),
            Err(SeqError::InvalidNumber(String::from("x")))
        );
        // The zero increment is diagnosed before the LAST operand is
        // scanned, as in the GNU tool.
        assert_eq!(
            seq(&["1", "0", "y"]),
            Err(SeqError::ZeroIncrement(String::from("0")))
        );
        assert_eq!(
            seq(&["1", "0.0", "5"]),
            Err(SeqError::ZeroIncrement(String::from("0.0")))
        );
        assert_eq!(
            seq(&["1", "-0", "5"]),
            Err(SeqError::ZeroIncrement(String::from("-0")))
        );
        assert_eq!(
            seq(&["nan"]),
            Err(SeqError::NotANumber(String::from("nan")))
        );
    }

    /// A value rounding carried just past LAST stands for LAST, printed as
    /// LAST is: `0.3 − 3 × 0.1` is a hair below zero in `f64`.
    #[test]
    fn a_value_just_past_last_is_printed_as_last() {
        assert_eq!(
            seq(&["0.3", "-0.1", "0"]).expect("runs"),
            "0.3\n0.2\n0.1\n0.0\n"
        );
        assert_eq!(
            seq(&["-f", "%+.1f", "0.3", "-0.1", "0"]).expect("runs"),
            "+0.3\n+0.2\n+0.1\n+0.0\n"
        );
        assert_eq!(
            seq(&["0", "0.1", "0.3"]).expect("runs"),
            "0.0\n0.1\n0.2\n0.3\n"
        );
        // Not a value merely rendered as LAST, nor one LAST already shows.
        assert_eq!(seq(&["-f", "%.0f", "0", "1", "0.6"]).expect("runs"), "0\n");
        assert_eq!(
            seq(&["-f", "%.0f", "0", "0.6", "1"]).expect("runs"),
            "0\n1\n"
        );
        assert_eq!(
            seq(&["-f", "%.1f", "1.0000000000000002", "1"]).expect("runs"),
            ""
        );
    }

    /// An uppercase hexadecimal exponent is read as C reads it, as a
    /// lowercase one is: GNU lays `0X1P-1` out as an integer, printing
    /// `seq 0X1P-1 3` as 0 1 2 3 and `seq 0 0X1P-1 2` without end.
    #[test]
    fn an_uppercase_hexadecimal_exponent_is_read_as_c_reads_it() {
        assert_eq!(seq(&["0X1P-1", "3"]).expect("runs"), "0.5\n1.5\n2.5\n");
        assert_eq!(
            seq(&["0", "0X1P-1", "2"]).expect("runs"),
            "0\n0.5\n1\n1.5\n2\n"
        );
        assert_eq!(seq(&["0", "0X1P-1", "2"]), seq(&["0", "0x1p-1", "2"]));
    }

    /// A field of any size is written in blocks, never held whole: a huge
    /// precision and a huge `-w` width alike.
    #[test]
    fn a_field_of_any_size_is_written_a_block_at_a_time() {
        #[derive(Default)]
        struct Blocks {
            written: RefCell<usize>,
            largest: RefCell<usize>,
        }
        impl Output for Blocks {
            fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
                *self.written.borrow_mut() += bytes.len();
                let mut largest = self.largest.borrow_mut();
                *largest = (*largest).max(bytes.len());
                Ok(())
            }
        }
        for (args, written) in [
            (&["-f", "%.1000000f", "1", "2"][..], 2 * 1_000_003),
            (&["-w", "0e100000", "1"][..], 2 * 100_002),
        ] {
            let sink = Blocks::default();
            run(parse(args).expect("parses"), None, &NoHelp, &sink).expect("runs");
            assert_eq!(*sink.written.borrow(), written, "{args:?}");
            assert!(*sink.largest.borrow() < 2 * super::FLUSH_LEN, "{args:?}");
        }
    }

    /// The tool against GNU coreutils 9.4's own answers (`LC_ALL=C`), run
    /// over a seeded corpus of operand spellings, formats and separators:
    /// each case is its arguments, its output, its diagnostic and its exit
    /// status, with `\\`, `\\t` and `\\n` escaped. The documented
    /// divergences are not in it.
    #[test]
    fn every_case_answers_as_gnu_seq_answers() {
        let unescape = |text: &str| {
            let mut out = String::new();
            let mut chars = text.chars();
            while let Some(c) = chars.next() {
                out.push(match (c, c == '\\') {
                    (_, true) => match chars.next() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        _ => '\\',
                    },
                    (c, false) => c,
                });
            }
            out
        };
        let corpus = include_str!("gnu_cases.txt");
        let lines: Vec<&str> = corpus.lines().collect();
        assert!(lines.len() > 4000, "the corpus is whole");
        for case in lines.chunks(4) {
            let field = |at: usize, tag: &str| {
                case[at]
                    .strip_prefix(tag)
                    .map(|rest| rest.strip_prefix('\t').unwrap_or(rest))
                    .expect("a well-formed case")
            };
            let args: Vec<String> = field(0, "A").split('\t').map(unescape).collect();
            let words: Vec<&str> = args.iter().map(String::as_str).collect();
            let (out, err, status) = (
                unescape(field(1, "O")),
                unescape(field(2, "E")),
                field(3, "R"),
            );
            match seq(&words) {
                Ok(got) => assert!(
                    status == "0" && got == out,
                    "{words:?}: {got:?}, GNU {out:?} {err:?}"
                ),
                Err(refused) => assert!(
                    status == "1" && refused.to_string() == err,
                    "{words:?}: {refused}, GNU {out:?} {err:?}"
                ),
            }
        }
    }

    #[test]
    fn short_help_falls_back_to_the_usage_banner() {
        assert_eq!(seq(&["-h"]).expect("runs"), format!("{USAGE}\n"));
    }

    /// Every locale's `OPTIONS` section documents exactly the switches
    /// this parser accepts (`plans/APPS.md` §3.1): the flag tokens are
    /// language-neutral, so each translated document must carry the same
    /// keys as the canonical one. The documents are read from the
    /// bundle's own on-disk `Help/` tree — the single source the image
    /// builder plants — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_parser_switches() {
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        let locales = tairix_help::REQUIRED_LOCALES;
        for locale in locales {
            let path = format!("{help_root}/{locale}/seq.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-f, --format <format>`",
                "`-s, --separator <string>`",
                "`-w, --equal-width`",
                "`-h, -?`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/seq.md must document {switch}"
                );
            }
        }
    }
}
