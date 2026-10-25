//! The shapes of a parsed `tail` command line, and their parser.
//!
//! The grammar is the GNU `tail` surface: `-c`/`--bytes` and `-n`/`--lines`
//! (each taking a count with an optional leading `+` meaning "start at unit
//! N" or `-` meaning "the last N units", plus the GNU multiplier suffix
//! alphabet), `-q`/`-v` header control, `-z` NUL line delimiters, the
//! reserved `-h`/`-?`/`--help` short-help switches, `--` end-of-options, and
//! the obsolete `{+,-}COUNT[bcl]` form accepted only as the first argument.
//! The follow family (`-f`/`-F`/`--follow`/`--retry`/`--pid`/
//! `--sleep-interval`/`--max-unchanged-stats`, and the obsolete trailing
//! `f`) is supported: `tail` blocks off-CPU on the kernel's file-change
//! wait source and re-emits appended data as each source grows, never a
//! busy poll.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_util::argv::choose;
use tairix_util::cnum::scan_double;
use tairix_util::count::{parse_decimal, parse_suffixed};
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};

use crate::error::TailError;

/// The default `--sleep-interval`, one second, in nanoseconds — the GNU
/// default `--pid`/rotation re-check cadence.
pub const DEFAULT_SLEEP_NS: u64 = 1_000_000_000;

/// The default `--max-unchanged-stats`: after this many consecutive
/// re-check cycles with no change, a `--follow=name` source is reopened by
/// name (the GNU default is 5).
pub const DEFAULT_MAX_UNCHANGED: u64 = 5;

/// How `tail` follows a source after the initial output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Follow {
    /// `-f`/`--follow`/`--follow=descriptor`: follow the open file by
    /// descriptor — a rename or move keeps following the same node.
    Descriptor,
    /// `-F`/`--follow=name` (and `--follow --retry`): follow the *name* — a
    /// rotation (the name replaced by a new file) reopens the new file.
    Name,
}

/// The `--follow` modes by the words naming them, in GNU's order.
pub(crate) const FOLLOW_MODES: [(&str, Follow); 2] =
    [("descriptor", Follow::Descriptor), ("name", Follow::Name)];

impl Follow {
    /// The mode `text` names: its word, or the only one it is a prefix of.
    fn parse(text: &str) -> Result<Self, TailError> {
        choose(&FOLLOW_MODES, text).map_err(|miss| TailError::BadFollow(miss, String::from(text)))
    }
}

/// One input of a `tail` run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// Standard input: the `-` operand, and the default when no operand is
    /// given.
    Stdin,
    /// A named file, read through the filesystem seam.
    Path(String),
}

/// What is counted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Count {
    /// Lines (delimited by newline, or NUL under `-z`).
    Lines(u64),
    /// Bytes.
    Bytes(u64),
}

/// When the `==> name <==` header precedes each source's output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderMode {
    /// The default: headers only when more than one source is named.
    MultipleFiles,
    /// `-q`/`--quiet`/`--silent`: never.
    Never,
    /// `-v`/`--verbose`: always.
    Always,
}

/// A parsed, runnable `tail` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    /// How much of each source to emit.
    pub count: Count,
    /// `true` when the count came with a leading `+`: emit from unit `count`
    /// (1-based) to the end, rather than the last `count` units.
    pub from_start: bool,
    /// `-z`: lines are NUL-delimited instead of newline-delimited.
    pub zero: bool,
    /// Header policy for multi-source output.
    pub headers: HeaderMode,
    /// Follow mode, if any (`-f`/`-F`); [`None`] for a one-shot `tail`.
    pub follow: Option<Follow>,
    /// `--retry`: keep trying to open a source that is initially
    /// inaccessible. Implied by `-F`.
    pub retry: bool,
    /// `--pid=PID`: with a follow mode, stop once process `PID` dies.
    pub pid: Option<u64>,
    /// `--sleep-interval` in nanoseconds: the cadence at which a follow with
    /// `--pid` re-checks the process, and a `--follow=name` source with no
    /// wake source (an inaccessible name) is retried.
    pub sleep_ns: u64,
    /// `--max-unchanged-stats`: consecutive unchanged re-check cycles before
    /// a `--follow=name` source is reopened by name.
    pub max_unchanged: u64,
    /// The inputs, in command-line order. Never empty (the no-operand
    /// default is a single [`Source::Stdin`]).
    pub sources: Vec<Source>,
}

/// One thing the `tail` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Emit the selected part of each source.
    Tail(Job),
    /// Render `tail`'s own short help (`-h`/`-?`/`--help`).
    Help,
}

/// The default line count when no `-n`/`-c` is given, as in the GNU tool.
const DEFAULT_LINES: u64 = 10;

/// What each of `tail`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Bytes,
    Lines,
    Follow,
    FollowName,
    Retry,
    Pid,
    SleepInterval,
    MaxUnchanged,
    Quiet,
    Verbose,
    Zero,
    Help,
}

/// `tail`'s options, as GNU's `tail` spells them, and the reserved help
/// switches. `-f` is `--follow` without its value.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::valued('c', &["bytes"], Flag::Bytes),
    Opt::letter('f', Takes::Nothing, Flag::Follow),
    Opt::long(&["follow"], Takes::Attached, Flag::Follow),
    Opt::letter('F', Takes::Nothing, Flag::FollowName),
    Opt::valued('n', &["lines"], Flag::Lines),
    Opt::long(&["max-unchanged-stats"], Takes::Value, Flag::MaxUnchanged),
    Opt::long(&["pid"], Takes::Value, Flag::Pid),
    Opt::flag('q', &["quiet", "silent"], Flag::Quiet),
    Opt::long(&["retry"], Takes::Nothing, Flag::Retry),
    Opt::long(&["sleep-interval"], Takes::Value, Flag::SleepInterval),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('z', &["zero-terminated"], Flag::Zero),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The obsolete `{+,-}COUNT[bcl]` form is honoured only as the first
/// argument, exactly as in the GNU tool; the rest is read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands,
/// clustered, a long one by any unambiguous prefix, `--` ending them, and a
/// lone `-` the standard-input operand. The help switches (`-h`, `-?`,
/// `--help`) answer at once, and the last `-c`/`-n` wins.
///
/// # Errors
///
/// * [`TailError::Option`] — an option `getopt` refuses, in its words.
/// * [`TailError::InvalidTrailing`] — a letter the obsolete form does not
///   take, or a digit in a later option cluster, as GNU diagnoses both.
/// * [`TailError::BadFollow`] — a `--follow` value naming no one mode.
/// * [`TailError::InvalidLines`] / [`TailError::InvalidBytes`] — a count that
///   is not one.
/// * [`TailError::InvalidPid`], [`TailError::InvalidSleep`],
///   [`TailError::InvalidMaxUnchanged`] — a follow-family value that is not
///   one.
pub fn parse(args: &[&str]) -> Result<Command, TailError> {
    let mut count: Option<Count> = None;
    let mut from_start = false;
    let mut zero = false;
    let mut headers = HeaderMode::MultipleFiles;
    let mut follow: Option<Follow> = None;
    let mut retry = false;
    let mut pid: Option<u64> = None;
    let mut sleep_ns = DEFAULT_SLEEP_NS;
    let mut max_unchanged = DEFAULT_MAX_UNCHANGED;
    let mut sources: Vec<Source> = Vec::new();

    let mut rest = args;
    if let Some(first) = args.first() {
        if let Some(parsed) = parse_obsolete(first, &mut from_start, &mut follow)? {
            count = Some(parsed);
            rest = &args[1..];
        }
    }

    for arg in Scan::new(OPTIONS, rest, Order::Permute) {
        match arg.map_err(option_error)? {
            Arg::Operand("-") => sources.push(Source::Stdin),
            Arg::Operand(path) => sources.push(Source::Path(String::from(path))),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Quiet, _) => headers = HeaderMode::Never,
            Arg::Opt(Flag::Verbose, _) => headers = HeaderMode::Always,
            Arg::Opt(Flag::Zero, _) => zero = true,
            Arg::Opt(Flag::Follow, None) => follow = Some(follow.unwrap_or(Follow::Descriptor)),
            Arg::Opt(Flag::Follow, Some(mode)) => follow = Some(Follow::parse(mode)?),
            Arg::Opt(Flag::FollowName, _) => {
                follow = Some(Follow::Name);
                retry = true;
            }
            Arg::Opt(Flag::Retry, _) => retry = true,
            Arg::Opt(Flag::Pid, value) => pid = Some(parse_pid(value.unwrap_or_default())?),
            Arg::Opt(Flag::SleepInterval, value) => {
                sleep_ns = parse_sleep(value.unwrap_or_default())?;
            }
            Arg::Opt(Flag::MaxUnchanged, value) => {
                max_unchanged = parse_max(value.unwrap_or_default())?;
            }
            Arg::Opt(flag @ (Flag::Bytes | Flag::Lines), value) => {
                let text = value.unwrap_or_default();
                count = Some(parse_value(text, flag == Flag::Lines, &mut from_start)?);
            }
        }
    }

    if sources.is_empty() {
        sources.push(Source::Stdin);
    }
    // `-F` is `--follow=name --retry`: the name form implies retry.
    if follow == Some(Follow::Name) {
        retry = true;
    }
    Ok(Command::Tail(Job {
        count: count.unwrap_or(Count::Lines(DEFAULT_LINES)),
        from_start,
        zero,
        headers,
        follow,
        retry,
        pid,
        sleep_ns,
        max_unchanged,
        sources,
    }))
}

/// Parse a `--pid` value: a plain non-negative decimal process id.
fn parse_pid(text: &str) -> Result<u64, TailError> {
    parse_decimal(text).ok_or_else(|| TailError::InvalidPid(String::from(text)))
}

/// Parse a `--sleep-interval` value: a non-negative C-locale number of
/// seconds (fractional allowed, as in the GNU tool), converted to whole
/// nanoseconds. The whole token must be consumed — trailing junk is invalid.
fn parse_sleep(text: &str) -> Result<u64, TailError> {
    let invalid = || TailError::InvalidSleep(String::from(text));
    let (secs, used) = scan_double(text).ok_or_else(invalid)?;
    if used != text.len() || !secs.is_finite() || secs < 0.0 {
        return Err(invalid());
    }
    // Convert seconds to whole nanoseconds. The float conversions are
    // deliberate: sub-nanosecond precision is meaningless for a poll
    // interval (floored), the value is already checked non-negative, and an
    // interval too large for `u64` nanoseconds saturates rather than wraps.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let nanos = {
        let scaled = secs * 1_000_000_000.0;
        if scaled >= u64::MAX as f64 {
            u64::MAX
        } else {
            scaled as u64
        }
    };
    Ok(nanos)
}

/// Parse a `--max-unchanged-stats` value: a plain non-negative decimal.
fn parse_max(text: &str) -> Result<u64, TailError> {
    parse_decimal(text).ok_or_else(|| TailError::InvalidMaxUnchanged(String::from(text)))
}

/// The error `refusal` stands for: a digit in a later cluster is GNU's
/// obsolete count out of place, its "invalid trailing option".
fn option_error(refusal: Refusal) -> TailError {
    match refusal {
        Refusal::UnknownShort(digit) if digit.is_ascii_digit() => TailError::InvalidTrailing(digit),
        refusal => TailError::Option(refusal),
    }
}

/// Parse the obsolete `{+,-}COUNT[bcl]` first argument. `+` selects the
/// from-start posture, `-` the last-N posture; `b` counts 512-byte blocks
/// (bytes), `c` counts bytes, `l` counts lines (the default). Returns
/// `Ok(None)` when `first` is not that form, so the caller falls through to
/// ordinary option parsing.
fn parse_obsolete(
    first: &str,
    from_start: &mut bool,
    follow: &mut Option<Follow>,
) -> Result<Option<Count>, TailError> {
    let sign = match first.chars().next() {
        Some('+') => true,
        Some('-') => false,
        _ => return Ok(None),
    };
    let body = &first[1..];
    if !body.starts_with(|c: char| c.is_ascii_digit()) {
        return Ok(None);
    }

    let digits_end = body
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(body.len());
    let digits = &body[..digits_end];
    let mut lines = true;
    let mut multiplier: u64 = 1;
    for letter in body[digits_end..].chars() {
        match letter {
            'c' => {
                lines = false;
                multiplier = 1;
            }
            'b' => {
                lines = false;
                multiplier = 512;
            }
            'l' => {
                lines = true;
                multiplier = 1;
            }
            // The historical BSD `tail -Nf` form: a trailing `f` on the
            // obsolete first argument requests follow-by-descriptor.
            'f' => *follow = Some(follow.unwrap_or(Follow::Descriptor)),
            _ => return Err(TailError::InvalidTrailing(letter)),
        }
    }

    let value = parse_decimal(digits).ok_or_else(|| invalid(digits, lines))?;
    let units = value.saturating_mul(multiplier);
    *from_start = sign;
    Ok(Some(if lines {
        Count::Lines(units)
    } else {
        Count::Bytes(units)
    }))
}

/// Parse a `-c`/`-n` value: an optional leading `+` (start at unit N,
/// recorded in `from_start`) or `-` (the last N), decimal digits, and an
/// optional multiplier suffix from the GNU alphabet.
///
/// `from_start` is reassigned on every call — the last `-c`/`-n` decides, as
/// in the GNU tool.
fn parse_value(text: &str, lines: bool, from_start: &mut bool) -> Result<Count, TailError> {
    let unsigned = if let Some(rest) = text.strip_prefix('+') {
        *from_start = true;
        rest
    } else if let Some(rest) = text.strip_prefix('-') {
        *from_start = false;
        rest
    } else {
        *from_start = false;
        text
    };
    let units = parse_suffixed(unsigned).ok_or_else(|| invalid(text, lines))?;
    Ok(if lines {
        Count::Lines(units)
    } else {
        Count::Bytes(units)
    })
}

/// The invalid-count error for the unit being parsed.
fn invalid(text: &str, lines: bool) -> TailError {
    if lines {
        TailError::InvalidLines(String::from(text))
    } else {
        TailError::InvalidBytes(String::from(text))
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::{
        parse, Command, Count, Follow, HeaderMode, Job, Source, DEFAULT_MAX_UNCHANGED,
        DEFAULT_SLEEP_NS,
    };
    use crate::error::TailError;
    use tairix_util::argv::WordMiss;

    fn job(
        count: Count,
        from_start: bool,
        zero: bool,
        headers: HeaderMode,
        sources: &[Source],
    ) -> Command {
        Command::Tail(Job {
            count,
            from_start,
            zero,
            headers,
            follow: None,
            retry: false,
            pid: None,
            sleep_ns: DEFAULT_SLEEP_NS,
            max_unchanged: DEFAULT_MAX_UNCHANGED,
            sources: sources.to_vec(),
        })
    }

    /// The [`Job`] inside a parsed `Command::Tail`, for the follow-family
    /// tests that assert on the follow fields directly.
    fn tail_job(args: &[&str]) -> Job {
        match parse(args).expect("fixture parses") {
            Command::Tail(job) => job,
            Command::Help => panic!("expected a tail job, got help"),
        }
    }

    fn path(name: &str) -> Source {
        Source::Path(String::from(name))
    }

    #[test]
    fn defaults_to_ten_lines_of_stdin() {
        assert_eq!(
            parse(&[]),
            Ok(job(
                Count::Lines(10),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[Source::Stdin]
            ))
        );
    }

    #[test]
    fn named_files_and_dash_stdin() {
        assert_eq!(
            parse(&["a", "-", "b"]),
            Ok(job(
                Count::Lines(10),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("a"), Source::Stdin, path("b")]
            ))
        );
    }

    #[test]
    fn lines_and_bytes_options() {
        assert_eq!(
            parse(&["-n", "3", "f"]),
            Ok(job(
                Count::Lines(3),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        assert_eq!(
            parse(&["-c5", "f"]),
            Ok(job(
                Count::Bytes(5),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        assert_eq!(
            parse(&["--lines=2", "f"]),
            Ok(job(
                Count::Lines(2),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        assert_eq!(
            parse(&["--bytes", "7", "f"]),
            Ok(job(
                Count::Bytes(7),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
    }

    #[test]
    fn plus_prefix_selects_from_start() {
        assert_eq!(
            parse(&["-n", "+5", "f"]),
            Ok(job(
                Count::Lines(5),
                true,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        assert_eq!(
            parse(&["-c", "+5", "f"]),
            Ok(job(
                Count::Bytes(5),
                true,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
    }

    #[test]
    fn leading_minus_is_the_last_n_posture() {
        // Both `-n 2` and `-n -2` mean "the last two lines".
        assert_eq!(parse(&["-n", "2"]), parse(&["-n", "-2"]));
        assert_eq!(
            parse(&["-n", "-2"]),
            Ok(job(
                Count::Lines(2),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[Source::Stdin]
            ))
        );
    }

    #[test]
    fn the_last_count_decides_the_posture() {
        // Each `-n`/`-c` reassigns the from-start flag.
        assert_eq!(
            parse(&["-n", "+3", "-n", "4", "f"]),
            Ok(job(
                Count::Lines(4),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
    }

    #[test]
    fn multiplier_suffixes() {
        assert_eq!(parse(&["-c", "1b"]), parse(&["-c", "512"]));
        assert_eq!(parse(&["-c", "2K"]), parse(&["-c", "2048"]));
        assert_eq!(parse(&["-c", "1kB"]), parse(&["-c", "1000"]));
        assert_eq!(parse(&["-c", "1MiB"]), parse(&["-c", "1048576"]));
        // A count beyond u64 saturates: larger than any possible input.
        assert_eq!(
            parse(&["-c", "99Y"]),
            parse(&["-c", "18446744073709551615"])
        );
    }

    #[test]
    fn invalid_counts_are_diagnosed_per_unit() {
        assert_eq!(
            parse(&["-n", "x"]),
            Err(TailError::InvalidLines(String::from("x")))
        );
        assert_eq!(
            parse(&["-c", "5J"]),
            Err(TailError::InvalidBytes(String::from("5J")))
        );
        assert_eq!(
            parse(&["-c", ""]),
            Err(TailError::InvalidBytes(String::new()))
        );
    }

    #[test]
    fn header_and_zero_flags() {
        assert_eq!(
            parse(&["-q", "a", "b"]),
            Ok(job(
                Count::Lines(10),
                false,
                false,
                HeaderMode::Never,
                &[path("a"), path("b")]
            ))
        );
        assert_eq!(
            parse(&["-vz", "a"]),
            Ok(job(
                Count::Lines(10),
                false,
                true,
                HeaderMode::Always,
                &[path("a")]
            ))
        );
        assert_eq!(
            parse(&["--silent", "--zero-terminated"]),
            Ok(job(
                Count::Lines(10),
                false,
                true,
                HeaderMode::Never,
                &[Source::Stdin]
            ))
        );
    }

    #[test]
    fn bundled_count_takes_the_token_tail() {
        assert_eq!(
            parse(&["-qn3", "f"]),
            Ok(job(
                Count::Lines(3),
                false,
                false,
                HeaderMode::Never,
                &[path("f")]
            ))
        );
    }

    #[test]
    fn obsolete_first_argument_form() {
        assert_eq!(
            parse(&["-5", "f"]),
            Ok(job(
                Count::Lines(5),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        assert_eq!(
            parse(&["+5", "f"]),
            Ok(job(
                Count::Lines(5),
                true,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        assert_eq!(
            parse(&["-5c", "f"]),
            Ok(job(
                Count::Bytes(5),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        assert_eq!(
            parse(&["+5c", "f"]),
            Ok(job(
                Count::Bytes(5),
                true,
                false,
                HeaderMode::MultipleFiles,
                &[path("f")]
            ))
        );
        // `b` counts 512-byte blocks (bytes); `l` restores line counting.
        assert_eq!(parse(&["-2b"]), parse(&["-c", "1024"]));
        assert_eq!(parse(&["-3l"]), parse(&["-n", "3"]));
        // The historical BSD `-Nf` form: last N lines and follow.
        let job = tail_job(&["-5f"]);
        assert_eq!(job.count, Count::Lines(5));
        assert_eq!(job.follow, Some(Follow::Descriptor));
        // A letter the obsolete form does not accept is still invalid.
        assert_eq!(parse(&["-5x"]), Err(TailError::InvalidTrailing('x')));
    }

    #[test]
    fn obsolete_form_is_first_argument_only() {
        // Not the first argument: `+5` is a file operand and a digit in a
        // cluster is the GNU "invalid trailing option" diagnostic.
        assert_eq!(
            parse(&["f", "+5"]),
            Ok(job(
                Count::Lines(10),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("f"), path("+5")]
            ))
        );
        assert_eq!(parse(&["-q", "-5"]), Err(TailError::InvalidTrailing('5')));
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(
            parse(&["--", "-n"]),
            Ok(job(
                Count::Lines(10),
                false,
                false,
                HeaderMode::MultipleFiles,
                &[path("-n")]
            ))
        );
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-q?"]), Ok(Command::Help));
    }

    #[test]
    fn follow_family_is_parsed() {
        // `-f` / `--follow` / `--follow=descriptor` follow by descriptor.
        for args in [
            &["-f"][..],
            &["--follow"][..],
            &["--follow=descriptor"][..],
            &["--follow=d"][..],
        ] {
            let job = tail_job(args);
            assert_eq!(job.follow, Some(Follow::Descriptor), "{args:?}");
            assert!(!job.retry, "{args:?}");
        }
        // `-F` / `--follow=name` follow by name and imply `--retry`.
        for args in [&["-F"][..], &["--follow=name"][..], &["--follow=n"][..]] {
            let job = tail_job(args);
            assert_eq!(job.follow, Some(Follow::Name), "{args:?}");
            assert!(job.retry, "name-follow implies retry ({args:?})");
        }
        // `--follow --retry` upgrades to the name form's retry without
        // changing the descriptor mode.
        let job = tail_job(&["--follow", "--retry"]);
        assert_eq!(job.follow, Some(Follow::Descriptor));
        assert!(job.retry);
        // `-fF` clusters to the stronger name form.
        let job = tail_job(&["-fF"]);
        assert_eq!(job.follow, Some(Follow::Name));
        assert!(job.retry);
    }

    #[test]
    fn pid_sleep_and_max_unchanged_parse() {
        let job = tail_job(&["-f", "--pid=1234"]);
        assert_eq!(job.pid, Some(1234));
        // Space-separated and `=`-joined forms agree.
        assert_eq!(tail_job(&["-f", "--pid", "9"]).pid, Some(9));
        // Fractional seconds convert to nanoseconds.
        assert_eq!(
            tail_job(&["-f", "--sleep-interval=0.5"]).sleep_ns,
            500_000_000
        );
        assert_eq!(
            tail_job(&["-f", "--sleep-interval", "2"]).sleep_ns,
            2_000_000_000
        );
        assert_eq!(
            tail_job(&["-F", "--max-unchanged-stats=3"]).max_unchanged,
            3
        );
        // Defaults when unspecified.
        let job = tail_job(&["-f"]);
        assert_eq!(job.sleep_ns, DEFAULT_SLEEP_NS);
        assert_eq!(job.max_unchanged, DEFAULT_MAX_UNCHANGED);
    }

    #[test]
    fn follow_family_rejects_bad_values() {
        assert_eq!(
            parse(&["-f", "--pid=x"]),
            Err(TailError::InvalidPid(String::from("x")))
        );
        assert_eq!(
            parse(&["-f", "--sleep-interval=-1"]),
            Err(TailError::InvalidSleep(String::from("-1")))
        );
        assert_eq!(
            parse(&["-f", "--sleep-interval=1x"]),
            Err(TailError::InvalidSleep(String::from("1x")))
        );
        assert_eq!(
            parse(&["-F", "--max-unchanged-stats=y"]),
            Err(TailError::InvalidMaxUnchanged(String::from("y")))
        );
        assert_eq!(
            refused(&["-f", "--pid"]),
            "option '--pid' requires an argument"
        );
        assert_eq!(
            parse(&["--follow=x"]),
            Err(TailError::BadFollow(WordMiss::Unknown, String::from("x")))
        );
        assert_eq!(
            refused(&["--follow="]),
            "ambiguous argument '' for '--follow'\nValid arguments are:\n  - 'descriptor'\n  - 'name'"
        );
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
        assert_eq!(refused(&["-fx"]), "invalid option -- 'x'");
        assert_eq!(refused(&["-n"]), "option requires an argument -- 'n'");
        assert_eq!(
            refused(&["--bytes"]),
            "option '--bytes' requires an argument"
        );
        assert_eq!(
            refused(&["--retry=x"]),
            "option '--retry' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `tail --verb --li 3 f` as `tail --verbose --lines 3 f`,
    /// and refuses `--s`, which begins both `--silent` and `--sleep-interval`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--verb", "--li", "3", "f"]),
            Ok(job(
                Count::Lines(3),
                false,
                false,
                HeaderMode::Always,
                &[path("f")]
            ))
        );
        let followed = tail_job(&["--fol=n", "--sl=2", "--max=3", "--pi", "9"]);
        assert_eq!(followed.follow, Some(Follow::Name));
        assert_eq!(followed.sleep_ns, 2_000_000_000);
        assert_eq!(followed.max_unchanged, 3);
        assert_eq!(followed.pid, Some(9));
        assert_eq!(
            refused(&["--s"]),
            "option '--s' is ambiguous; possibilities: '--silent' '--sleep-interval'"
        );
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn options_and_operands_interleave() {
        assert_eq!(
            parse(&["f", "-q"]),
            Ok(job(
                Count::Lines(10),
                false,
                false,
                HeaderMode::Never,
                &[path("f")]
            ))
        );
    }
}
