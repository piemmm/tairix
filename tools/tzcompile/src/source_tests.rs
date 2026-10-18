//! Host tests of the source reader: fields, words, times and lines read as
//! `zic` 2026e reads them.

use tairix_tz::rule::{DayRule, Moment, TimeBase};

use super::{byword, fields, hms, last_weekday, moment, parse, saving, Era, Link, SyntaxError};

fn split(line: &str) -> Vec<String> {
    fields(line).expect("the line splits")
}

#[test]
fn fields_split_at_white_space_up_to_a_comment() {
    assert_eq!(
        split("R  EU 1981\tma -  Mar  # a comment"),
        ["R", "EU", "1981", "ma", "", "Mar"]
    );
    assert_eq!(
        split("Z \"A B\" 1 - x#y"),
        ["Z", "A B", "1", "", "x"],
        "quoted and cut"
    );
    assert_eq!(split("   "), Vec::<String>::new());
    assert_eq!(split("# all comment"), Vec::<String>::new());
    assert_eq!(
        split("a\"-\" \"-\""),
        ["a-", ""],
        "a lone dash, quoted or not, is empty"
    );
    assert_eq!(fields("a \"b"), Err("Odd number of quotation marks"));
    assert_eq!(fields(&"x ".repeat(11)), Err("Too many input fields"));
}

#[test]
fn a_word_matches_exactly_or_as_the_prefix_of_one_entry() {
    let table = [("January", 1), ("June", 6), ("July", 7)];
    assert_eq!(byword("june", &table), Some(6), "exact, any case");
    assert_eq!(byword("Ja", &table), Some(1));
    assert_eq!(byword("Jun", &table), Some(6));
    assert_eq!(byword("Ju", &table), None, "ambiguous");
    assert_eq!(byword("Juno", &table), None);
    assert_eq!(byword("", &table), None);
    assert_eq!(last_weekday("lastSun"), Some(0));
    assert_eq!(last_weekday("lastsa"), Some(6));
    assert_eq!(last_weekday("last-Fri"), Some(5), "the older spelling");
    assert_eq!(last_weekday("last"), None);
    assert_eq!(last_weekday("lastS"), None, "Sunday or Saturday");
    assert_eq!(last_weekday("Sun"), None);
}

#[test]
fn a_time_reads_hours_minutes_and_seconds_with_a_sign() {
    for (text, seconds) in [
        ("", Some(0)),
        ("2", Some(7200)),
        ("-1", Some(-3600)),
        ("1:30", Some(5400)),
        ("-0:01:15", Some(-75)),
        ("24", Some(86_400)),
        ("26:00", Some(93_600)),
        ("0:59:60", Some(3600)),
        ("1:60", None),
        ("1:00:61", None),
        ("1:", None),
        ("1:00:00x", None),
        ("x", None),
        ("--1", None),
    ] {
        assert_eq!(hms(text), seconds, "{text:?}");
    }
}

#[test]
fn a_fraction_of_a_second_rounds_as_zic_rounds_it() {
    // What `zic` 2026e compiles each to, half rounding to even, and a fraction
    // with a nonzero digit after its first stopping its scan there.
    for (text, seconds) in [
        ("0:00:00.5", 0),
        ("0:00:01.5", 2),
        ("0:00:00.51", 0),
        ("0:00:00.501", 1),
        ("0:00:00.6", 1),
        ("-0:00:00.5", 0),
        ("0:00:02.50", 2),
        ("0:00:03.5", 4),
        ("0:00:59.9", 60),
    ] {
        assert_eq!(hms(text), Some(seconds), "{text:?}");
    }
    // `zic`'s scan stops at the first character after the tenths that is not
    // a zero, so what follows it goes unread; after a zero it must be a digit.
    assert_eq!(hms("0:00:01.5x"), Some(2));
    assert_eq!(hms("0:00:00.50x"), None);
}

#[test]
fn a_saving_counts_as_daylight_saving_when_it_is_nonzero_or_says_so() {
    assert_eq!(saving(""), Some((0, false)));
    assert_eq!(saving("1"), Some((3600, true)));
    assert_eq!(saving("-1"), Some((-3600, true)), "negative saving");
    assert_eq!(saving("0d"), Some((0, true)));
    assert_eq!(saving("1s"), Some((3600, false)));
    assert_eq!(saving("Eire"), None, "a rule set's name is no saving");
}

#[test]
fn a_moment_reads_its_month_day_and_clock() {
    let read = |month, day, time| moment(month, day, time).expect("a moment");
    assert_eq!(
        read("Mar", "lastSu", "1u"),
        Moment {
            month: 3,
            day: DayRule::OnOrBefore {
                weekday: 0,
                day: 31
            },
            at: 3600,
            base: TimeBase::Universal,
        }
    );
    assert_eq!(
        read("O", "Su>=8", "2"),
        Moment {
            month: 10,
            day: DayRule::OnOrAfter { weekday: 0, day: 8 },
            at: 7200,
            base: TimeBase::Wall,
        }
    );
    assert_eq!(
        read("Ap", "Sa<=24", "24s"),
        Moment {
            month: 4,
            day: DayRule::OnOrBefore {
                weekday: 6,
                day: 24
            },
            at: 86_400,
            base: TimeBase::Standard,
        }
    );
    assert_eq!(
        read("F", "lastSu", "0").day,
        DayRule::OnOrBefore {
            weekday: 0,
            day: 29
        }
    );
    assert_eq!(read("F", "29", "0z").base, TimeBase::Universal);
    assert_eq!(read("Jun", "1", "2g").base, TimeBase::Universal);
    assert_eq!(moment("Smarch", "1", "0"), Err("invalid month name"));
    assert_eq!(moment("Ap", "31", "0"), Err("invalid day of month"));
    assert_eq!(moment("Ap", "0", "0"), Err("invalid day of month"));
    assert_eq!(moment("Ap", "Su>8", "0"), Err("invalid day of month"));
    assert_eq!(moment("Ap", "Xy>=8", "0"), Err("invalid weekday name"));
    assert_eq!(moment("Ap", "1", "2:61"), Err("invalid time of day"));
}

const SOURCE: &str = "\
# A rule set, a zone of three eras, and a link.
R T 1990 ma - Mar lastSu 1u 1 S
R T 1990 o - O lastSu 1u 0 -
R T 1991 ma - O lastSu 1u 0 -
Z Test/Zone -0:5 - LMT 1900 Jul 1
0 - GMT 1990
0 T G%sT
L Test/Zone Test/Alias
";

#[test]
fn a_source_reads_its_rules_zones_and_links() {
    let source = parse(SOURCE).expect("the source parses");
    assert_eq!(source.rules.len(), 3);
    let first = &source.rules[0];
    assert_eq!(
        (first.from, first.to, first.to_is_year),
        (1990, i64::MAX, false)
    );
    assert_eq!(
        (first.save, first.daylight, first.letters.as_str()),
        (3600, true, "S")
    );
    let only = &source.rules[1];
    assert_eq!((only.from, only.to, only.to_is_year), (1990, 1990, false));
    assert_eq!(only.letters, "", "a dash names no letters");
    let [zone] = &source.zones[..] else {
        panic!("one zone");
    };
    assert_eq!(zone.name, "Test/Zone");
    let lmt = &zone.eras[0];
    assert_eq!(
        (lmt.stdoff, lmt.saving.as_str(), lmt.format.as_str()),
        (-300, "", "LMT")
    );
    let until = lmt.until.as_ref().expect("an end");
    assert_eq!((until.year, until.moment.month), (1900, 7));
    assert_eq!(
        zone.eras[2],
        Era {
            stdoff: 0,
            saving: String::from("T"),
            format: String::from("G%sT"),
            until: None,
        }
    );
    assert_eq!(
        source.links,
        [Link {
            target: String::from("Test/Zone"),
            name: String::from("Test/Alias"),
        }]
    );
}

#[test]
fn a_malformed_line_is_refused_with_its_number() {
    let refused = |text: &str| parse(text).map(|_| ()).err();
    let at = |line, reason| Some(SyntaxError { line, reason });
    assert_eq!(refused("Z A/B 0 - X"), at(1, "unterminated line"));
    assert_eq!(refused("\n\nQ x\n"), at(3, "input line of unknown type"));
    assert_eq!(
        refused("R T 1990 ma - Mar 1 0\n"),
        at(1, "wrong number of fields on Rule line")
    );
    assert_eq!(
        refused("R 9T 1990 o - Mar 1 0 1 S\n"),
        at(1, "invalid rule name")
    );
    assert_eq!(
        refused("R T 1991 1990 - Mar 1 0 1 S\n"),
        at(1, "starting year greater than ending year")
    );
    assert_eq!(
        refused("R T 1990 o x Mar 1 0 1 S\n"),
        at(1, "year type is unsupported; use \"-\" instead")
    );
    assert_eq!(
        refused("Z A/B 0 - X 1990\n"),
        at(1, "expected continuation line not found")
    );
    assert_eq!(
        refused("Z A/B 0 - X 1990\n0 - Y 1980\n0 - Z\n"),
        at(
            2,
            "Zone continuation line end time is not after end time of previous line"
        )
    );
    assert_eq!(
        refused("Z A/B 0 - X\nZ A/B 0 - Y\n"),
        at(2, "duplicate zone name")
    );
    assert_eq!(
        refused("Z A//B 0 - X\n"),
        at(1, "file name has an empty component")
    );
    assert_eq!(
        refused("Z A/../B 0 - X\n"),
        at(1, "file name contains a '.' or '..' component")
    );
    assert_eq!(
        refused("Z A/B 0 - %d\n"),
        at(1, "invalid abbreviation format")
    );
    assert_eq!(
        refused("Z A/B 0 - %s/%s\n"),
        at(1, "invalid abbreviation format")
    );
    assert_eq!(
        refused("Z A/B 0 - X 1990 F 29\n0 - Y\n"),
        at(1, "use of 2/29 in non leap-year")
    );
    assert_eq!(
        refused("Z A/B 0 - X 999999999999\n0 - Y\n"),
        at(1, "time overflow")
    );
    assert_eq!(
        refused("L A/B\n"),
        at(1, "wrong number of fields on Link line")
    );
    assert_eq!(
        refused("L \"\" A/B\n"),
        at(1, "blank TARGET field on Link line")
    );
    assert_eq!(refused("Z A/B 0 - X\0\n"), at(1, "NUL input byte"));
    assert_eq!(
        refused(&format!("# {}\n", "x".repeat(2046))),
        at(1, "line too long")
    );
    assert_eq!(
        parse(&format!("# {}\n", "x".repeat(2045))).map(|_| ()),
        Ok(())
    );
}
