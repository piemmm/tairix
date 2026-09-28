//! The grammar accepts exactly what the Help documents say it does.

extern crate std;

use std::vec::Vec;

use super::{
    drawn_seed_record, parse, CliError, Launch, HELP_SWITCHES, PRODUCER, REFERENCE_SCENE, SEED,
    SEED_RECORD_BYTES, USAGE,
};

#[test]
fn no_arguments_plays_a_new_world() {
    assert_eq!(parse(&[]), Ok(Launch::Play(None)));
}

#[test]
fn a_seed_names_the_world_to_open_in_either_spelling() {
    assert_eq!(parse(&["--seed", "42"]), Ok(Launch::Play(Some(42))));
    assert_eq!(parse(&["--seed=42"]), Ok(Launch::Play(Some(42))));
    assert_eq!(parse(&["--seed", "0"]), Ok(Launch::Play(Some(0))));
    assert_eq!(
        parse(&["--seed", "18446744073709551615"]),
        Ok(Launch::Play(Some(u64::MAX)))
    );
    // A repeated option's last value stands.
    assert_eq!(
        parse(&["--seed", "1", "--seed=2"]),
        Ok(Launch::Play(Some(2)))
    );
    assert_eq!(parse(&["--seed", "7", "--"]), Ok(Launch::Play(Some(7))));
}

#[test]
fn a_seed_is_decimal_digits_and_nothing_else() {
    for seed in [
        "",
        "+5",
        "-5",
        "0x10",
        "1e3",
        " 5",
        "5 ",
        "1_000",
        "18446744073709551616",
    ] {
        assert_eq!(parse(&["--seed", seed]), Err(CliError::Usage), "{seed:?}");
        let joined = std::format!("--seed={seed}");
        assert_eq!(parse(&[joined.as_str()]), Err(CliError::Usage), "{seed:?}");
    }
    // The value is the next argument, even when it looks like an option.
    assert_eq!(parse(&["--seed", "-h"]), Err(CliError::Usage));
    assert_eq!(parse(&["--seed"]), Err(CliError::Usage));
}

#[test]
fn the_reference_scene_is_one_realm_and_takes_no_seed() {
    assert_eq!(
        parse(&["--reference-scene", "--seed", "3"]),
        Err(CliError::Usage)
    );
    assert_eq!(
        parse(&["--seed=3", "--reference-scene"]),
        Err(CliError::Usage)
    );
}

#[test]
fn a_drawn_seed_is_left_on_stdinfo_to_open_the_world_again() {
    let mut out = [0u8; SEED_RECORD_BYTES];
    let n = drawn_seed_record(u64::MAX, &mut out).expect("the record fits");
    let line = core::str::from_utf8(&out[..n]).expect("utf-8");
    assert!(
        line.ends_with('\n') && line.matches('\n').count() == 1,
        "one line"
    );
    assert!(line.starts_with("{\"version\":1,"));
    assert!(line.contains(&std::format!("\"producer\":\"{PRODUCER}\"")));
    assert!(line.contains("\"kind\":\"context\""));
    assert!(line.contains("\"code\":\"world.seed_drawn\""));
    assert!(line.contains("\"severity\":\"info\""));
    assert!(line.contains("\"seed\":\"18446744073709551615\""));
    assert!(line.contains(&std::format!(
        "\"argv\":[\"{PRODUCER}\",\"{SEED}\",\"18446744073709551615\"]"
    )));
    assert!(line.contains("\"safe_to_autorun\":false"));
    // The suggestion is the command line the parser takes back.
    let suggested = std::format!("{SEED} {}", u64::MAX);
    let words: Vec<&str> = suggested.split(' ').collect();
    assert_eq!(parse(&words), Ok(Launch::Play(Some(u64::MAX))));
    assert!(line.contains(&suggested));
    // Braces balance: the structured payload is one JSON object.
    let opens = line.matches('{').count();
    assert_eq!(opens, line.matches('}').count());
    assert!(
        drawn_seed_record(1, &mut [0u8; 16]).is_err(),
        "a short buffer is refused"
    );
}

#[test]
fn the_reference_scene_is_asked_for_by_its_one_option() {
    assert_eq!(parse(&["--reference-scene"]), Ok(Launch::ReferenceScene));
}

/// The usage banner names, as a whole word, every switch the parser takes.
#[test]
fn the_usage_banner_names_every_switch() {
    let words: Vec<&str> = USAGE
        .split(|c: char| c.is_whitespace() || "[|]".contains(c))
        .collect();
    for switch in HELP_SWITCHES.iter().chain([&REFERENCE_SCENE, &SEED]) {
        assert!(words.contains(switch), "the usage banner omits {switch}");
    }
}

/// A help switch wins wherever it is reached, as it does for every command
/// app and for GNU's: after the other option as much as before it.
#[test]
fn the_help_switches_win_where_they_are_reached() {
    for switch in HELP_SWITCHES {
        assert_eq!(parse(&[switch]), Ok(Launch::Help));
        assert_eq!(parse(&[switch, "--reference-scene"]), Ok(Launch::Help));
        assert_eq!(parse(&["--reference-scene", switch]), Ok(Launch::Help));
        assert_eq!(parse(&[switch, "--frob"]), Ok(Launch::Help));
    }
}

/// A flag given twice asks for the same thing twice, and `--` ends the
/// options without taking an operand.
#[test]
fn a_repeated_flag_and_the_end_of_options_are_accepted() {
    assert_eq!(
        parse(&["--reference-scene", "--reference-scene"]),
        Ok(Launch::ReferenceScene)
    );
    assert_eq!(parse(&["--"]), Ok(Launch::Play(None)));
    assert_eq!(
        parse(&["--reference-scene", "--"]),
        Ok(Launch::ReferenceScene)
    );
}

/// No operands and no other options: a line outside the grammar is refused
/// whole rather than half-applied, including a help switch it only reaches
/// after the error.
#[test]
fn anything_else_is_a_usage_error() {
    for line in [
        &["--frob"][..],
        &["operand"],
        &["--frob", "-h"],
        &["--reference-scene=1"],
        &["--", "-h"],
        &["--", "operand"],
        &["--reference-scene", "operand"],
    ] {
        assert_eq!(parse(line), Err(CliError::Usage), "{line:?}");
    }
}

/// Every locale's `OPTIONS` documents the switches this parser accepts, read
/// from the bundle's own `Help/` tree — the one source the image plants.
#[test]
fn every_locale_documents_the_parser_switches() {
    use std::{format, fs};

    let keys = [
        format!("`{}`", HELP_SWITCHES.join(", ")),
        format!("`{REFERENCE_SCENE}`"),
        format!("`{SEED} SEED, {SEED}=SEED`"),
    ];
    for locale in tairix_help::REQUIRED_LOCALES {
        let path = format!("{}/Help/{locale}/wintersun.md", env!("CARGO_MANIFEST_DIR"));
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        for key in &keys {
            assert!(text.contains(key.as_str()), "{locale} must document {key}");
        }
    }
}
