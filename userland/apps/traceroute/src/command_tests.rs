use super::*;

fn config(args: &[&str]) -> Config {
    match parse(args) {
        Ok(Command::Run(config)) => config,
        other => panic!("expected a run: {other:?}"),
    }
}

#[test]
fn a_bare_target_takes_the_linux_defaults() {
    let config = config(&["example.test"]);
    assert_eq!(config.host, "example.test");
    assert_eq!(config.family, None);
    assert_eq!(config.first_hop.get(), 1);
    assert_eq!(config.max_hops.get(), 30);
    assert_eq!(config.queries.get(), 3);
    assert_eq!(config.sim_queries.get(), 16);
    assert_eq!(config.wait_ns, 5_000_000_000);
    assert!(!config.numeric);
    assert_eq!(config.packet_len, None);
}

#[test]
fn every_value_option_takes_its_value_attached_or_separate() {
    let config = config(&[
        "-f3", "-m", "12", "-q", "2", "-N4", "-w", "0.5", "-n", "host",
    ]);
    assert_eq!(config.first_hop.get(), 3);
    assert_eq!(config.max_hops.get(), 12);
    assert_eq!(config.queries.get(), 2);
    assert_eq!(config.sim_queries.get(), 4);
    assert_eq!(config.wait_ns, 500_000_000);
    assert!(config.numeric);
}

#[test]
fn long_options_and_the_equals_form() {
    let config = config(&[
        "--first=2",
        "--max-hops",
        "9",
        "--queries=1",
        "--sim-queries=8",
        "--wait=2",
        "--icmp",
        "-6n",
        "host",
    ]);
    assert_eq!(config.first_hop.get(), 2);
    assert_eq!(config.max_hops.get(), 9);
    assert_eq!(config.queries.get(), 1);
    assert_eq!(config.sim_queries.get(), 8);
    assert_eq!(config.wait_ns, 2_000_000_000);
    assert_eq!(config.family, Some(NetAddrFamily::V6));
    assert!(config.numeric);
}

#[test]
fn a_second_operand_is_the_packet_length() {
    let config = config(&["-4", "host", "100"]);
    assert_eq!(config.family, Some(NetAddrFamily::V4));
    assert_eq!(config.packet_len, Some(100));
    assert!(matches!(
        parse(&["host", "100", "extra"]),
        Err(ParseError::ExtraOperand(_))
    ));
    assert!(matches!(
        parse(&["host", "huge"]),
        Err(ParseError::InvalidValue { .. })
    ));
}

#[test]
fn help_wins_from_either_spelling() {
    assert_eq!(parse(&["-?"]), Ok(Command::Help));
    assert_eq!(parse(&["host", "--help"]), Ok(Command::Help));
}

#[test]
fn another_probe_method_is_refused_by_name() {
    for args in [["-U", "host"], ["-T", "host"], ["--tcp", "host"]] {
        assert!(
            matches!(parse(&args), Err(ParseError::UnsupportedMethod(_))),
            "{args:?}"
        );
    }
}

#[test]
fn out_of_range_values_fail_closed() {
    for args in [
        ["-m", "0", "host"],
        ["-m", "256", "host"],
        ["-q", "11", "host"],
        ["-N", "65", "host"],
        ["-w", "0", "host"],
        ["-w", "-1", "host"],
    ] {
        assert!(
            matches!(parse(&args), Err(ParseError::InvalidValue { .. })),
            "{args:?}"
        );
    }
    assert!(
        matches!(
            parse(&["-f", "10", "-m", "5", "host"]),
            Err(ParseError::InvalidValue { .. })
        ),
        "the first hop may not lie beyond the last"
    );
}

#[test]
fn a_missing_value_or_target_fails_closed() {
    assert_eq!(
        parse(&["host", "-m"]),
        Err(ParseError::Option(Refusal::MissingShortValue('m')))
    );
    assert_eq!(parse(&["-n"]), Err(ParseError::MissingTarget));
    assert_eq!(
        parse(&["-x", "host"]),
        Err(ParseError::Option(Refusal::UnknownShort('x')))
    );
}

/// Every locale's `OPTIONS` documents the switches this parser accepts, read
/// from the bundle's own on-disk `Help/` tree; each documented spelling must
/// parse, so the list cannot name a switch the parser does not know.
#[test]
fn help_documents_the_parser_switches() {
    extern crate std;

    const SWITCHES: [(&str, Option<&str>); 10] = [
        ("`-4`", None),
        ("`-6`", None),
        ("`-I, --icmp`", None),
        ("`-f, --first`", Some("1")),
        ("`-m, --max-hops`", Some("1")),
        ("`-q, --queries`", Some("1")),
        ("`-N, --sim-queries`", Some("1")),
        ("`-w, --wait`", Some("1")),
        ("`-n`", None),
        ("`-?, --help`", None),
    ];
    for (key, value) in SWITCHES {
        for spelling in key.trim_matches('`').split(", ") {
            let argv: &[&str] = match value {
                Some(value) => &[spelling, value, "host"],
                None => &[spelling, "host"],
            };
            assert!(parse(argv).is_ok(), "{spelling} must parse");
        }
    }
    let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
    for locale in tairix_help::REQUIRED_LOCALES {
        let path = format!("{help_root}/{locale}/traceroute.md");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        for (key, _) in SWITCHES {
            assert!(
                text.contains(key),
                "{locale}/traceroute.md must document {key}"
            );
        }
    }
}

/// GNU's words for each refusal.
fn refused(args: &[&str]) -> String {
    parse(args).expect_err("refused").to_string()
}

#[test]
fn a_refused_option_is_worded_as_getopt_words_it() {
    assert_eq!(refused(&["-Q", "host"]), "invalid option -- 'Q'");
    assert_eq!(refused(&["-nQ", "host"]), "invalid option -- 'Q'");
    assert_eq!(refused(&["--frob", "host"]), "unrecognized option '--frob'");
    assert_eq!(
        refused(&["host", "-q"]),
        "option requires an argument -- 'q'"
    );
    assert_eq!(
        refused(&["host", "--wait"]),
        "option '--wait' requires an argument"
    );
    assert_eq!(
        refused(&["--icmp=1", "host"]),
        "option '--icmp' doesn't allow an argument"
    );
}

#[test]
fn a_long_option_may_be_abbreviated() {
    let config = config(&["--max=9", "--sim", "8", "--ic", "host"]);
    assert_eq!(config.max_hops.get(), 9);
    assert_eq!(config.sim_queries.get(), 8);
    assert_eq!(parse(&["--he"]), Ok(Command::Help));
    assert_eq!(
        refused(&["--m", "9", "host"]),
        "option '--m' is ambiguous; possibilities: '--module' '--max-hops'"
    );
}
