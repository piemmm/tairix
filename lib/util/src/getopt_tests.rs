use super::{Arg, Opt, Order, Refusal, Scan, Takes};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Symbolic,
    Force,
    Verbose,
    NoDereference,
    NoTarget,
    Target,
    Recursive,
    Color,
}

/// A slice of `ln`'s options, with a synonym and an optional value besides.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('s', &["symbolic"], Flag::Symbolic),
    Opt::flag('f', &["force"], Flag::Force),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('n', &["no-dereference"], Flag::NoDereference),
    Opt::flag('T', &["no-target-directory"], Flag::NoTarget),
    Opt::valued('t', &["target-directory"], Flag::Target),
    Opt::flag('r', &["recursive", "recurse"], Flag::Recursive),
    Opt::long(&["color"], Takes::Attached, Flag::Color),
];

type Read<'a> = Result<Vec<Arg<'a, Flag>>, Refusal>;

fn scan<'a>(args: &'a [&'a str], order: Order) -> Read<'a> {
    Scan::new(OPTIONS, args, order).collect()
}

fn permuted<'a>(args: &'a [&'a str]) -> Read<'a> {
    scan(args, Order::Permute)
}

#[test]
fn short_options_cluster_and_take_a_value_attached_or_following() {
    assert_eq!(
        permuted(&["-sfv", "-tdir", "-t", "other"]),
        Ok(alloc::vec![
            Arg::Opt(Flag::Symbolic, None),
            Arg::Opt(Flag::Force, None),
            Arg::Opt(Flag::Verbose, None),
            Arg::Opt(Flag::Target, Some("dir")),
            Arg::Opt(Flag::Target, Some("other")),
        ])
    );
    assert_eq!(
        permuted(&["-st", "-f"]),
        Ok(alloc::vec![
            Arg::Opt(Flag::Symbolic, None),
            Arg::Opt(Flag::Target, Some("-f")),
        ]),
        "a value is taken whatever it looks like"
    );
}

#[test]
fn a_long_option_is_read_whole_or_by_any_unambiguous_prefix() {
    assert_eq!(
        permuted(&["--verb", "--target=d", "--target-directory", "e", "--no-t"]),
        Ok(alloc::vec![
            Arg::Opt(Flag::Verbose, None),
            Arg::Opt(Flag::Target, Some("d")),
            Arg::Opt(Flag::Target, Some("e")),
            Arg::Opt(Flag::NoTarget, None),
        ])
    );
    assert_eq!(
        permuted(&["--rec", "--recurse"]),
        Ok(alloc::vec![
            Arg::Opt(Flag::Recursive, None),
            Arg::Opt(Flag::Recursive, None),
        ]),
        "a prefix two synonyms share is either"
    );
    assert_eq!(
        permuted(&["--color", "--color=always", "never"]),
        Ok(alloc::vec![
            Arg::Opt(Flag::Color, None),
            Arg::Opt(Flag::Color, Some("always")),
            Arg::Operand("never"),
        ]),
        "an optional value is only ever attached"
    );
}

#[test]
fn operands_stand_among_the_options_unless_they_end_them() {
    assert_eq!(
        permuted(&["a", "-s", "-", "b", "--", "-f", "--verbose"]),
        Ok(alloc::vec![
            Arg::Operand("a"),
            Arg::Opt(Flag::Symbolic, None),
            Arg::Operand("-"),
            Arg::Operand("b"),
            Arg::Operand("-f"),
            Arg::Operand("--verbose"),
        ])
    );
    assert_eq!(
        scan(&["-s", "a", "-f"], Order::OperandsEnd),
        Ok(alloc::vec![
            Arg::Opt(Flag::Symbolic, None),
            Arg::Operand("a"),
            Arg::Operand("-f"),
        ])
    );
}

/// GNU 9.4's words for each refusal, as `ln` prints them.
#[test]
fn a_refused_argument_is_worded_as_getopt_words_it() {
    let refused = |args: &[&str]| permuted(args).expect_err("refused").to_string();
    assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
    assert_eq!(refused(&["-sx"]), "invalid option -- 'x'");
    assert_eq!(refused(&["--frob=1"]), "unrecognized option '--frob=1'");
    assert_eq!(
        refused(&["--n"]),
        "option '--n' is ambiguous; possibilities: '--no-dereference' '--no-target-directory'"
    );
    assert_eq!(
        refused(&["--no=x"]),
        "option '--no=x' is ambiguous; possibilities: '--no-dereference' '--no-target-directory'"
    );
    assert_eq!(refused(&["-t"]), "option requires an argument -- 't'");
    assert_eq!(
        refused(&["--target"]),
        "option '--target-directory' requires an argument"
    );
    assert_eq!(
        refused(&["--no-t=x"]),
        "option '--no-target-directory' doesn't allow an argument"
    );
    assert_eq!(
        Refusal::UnknownLong(String::from("--a\u{1b}[2J")).to_string(),
        "unrecognized option '--a'$'\\033''[2J'",
        "what the line holds is escaped"
    );
}

/// `seq -5 5` counts from -5: a negative number is an operand, and under
/// [`Order::OperandsEnd`] the first one ends the options.
#[test]
fn a_negative_number_is_an_operand_where_the_tool_says_so() {
    let args = ["-s", "-5", "-.3", "-f", "é", "-é"];
    let read: Read<'_> = Scan::new(OPTIONS, &args, Order::Permute)
        .numbers_are_operands()
        .collect();
    assert_eq!(
        read,
        Err(Refusal::UnknownShort('é')),
        "a dash and anything else is still options"
    );
    let read: Read<'_> = Scan::new(OPTIONS, &args[..5], Order::OperandsEnd)
        .numbers_are_operands()
        .collect();
    assert_eq!(
        read,
        Ok(alloc::vec![
            Arg::Opt(Flag::Symbolic, None),
            Arg::Operand("-5"),
            Arg::Operand("-.3"),
            Arg::Operand("-f"),
            Arg::Operand("é"),
        ])
    );
    assert_eq!(
        permuted(&["-5"]),
        Err(Refusal::UnknownShort('5')),
        "a digit is an option letter unless the tool says otherwise"
    );
}

#[test]
fn the_scan_stops_at_the_first_refusal_and_reads_on_where_it_is_asked() {
    let mut scan = Scan::new(OPTIONS, &["-s", "--frob", "-f"], Order::Permute);
    assert_eq!(scan.next(), Some(Ok(Arg::Opt(Flag::Symbolic, None))));
    assert_eq!(
        scan.next(),
        Some(Err(Refusal::UnknownLong(String::from("--frob"))))
    );
    assert_eq!(scan.next(), Some(Ok(Arg::Opt(Flag::Force, None))));
    assert_eq!(scan.next(), None);
}
