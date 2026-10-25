//! Host tests for the `usermod` command-line grammar.

use super::{parse, Command, Lock, UsageError};
use alloc::string::{String, ToString};

/// The change set a successfully parsed line asks for.
fn changes(args: &[&str]) -> super::Changes {
    match parse(args) {
        Ok(Command::Modify { changes, .. }) => changes,
        other => panic!("expected a modify, got {other:?}"),
    }
}

#[test]
fn every_identity_switch_is_read_attached_and_detached() {
    let detached = changes(&[
        "-c",
        "Ada",
        "-d",
        "/Users/ada",
        "-s",
        "/bin/sh",
        "-g",
        "1000",
        "-G",
        "10,20",
        "ada",
    ]);
    assert_eq!(detached.comment.as_deref(), Some("Ada"));
    assert_eq!(detached.home.as_deref(), Some("/Users/ada"));
    assert_eq!(detached.shell.as_deref(), Some("/bin/sh"));
    assert_eq!(detached.primary_gid, Some(1000));
    assert_eq!(detached.supplementary_gids.as_deref(), Some(&[10, 20][..]));
    assert!(detached.touches_identity());

    let attached = changes(&[
        "-cAda",
        "--home=/Users/ada",
        "--shell=/bin/sh",
        "--gid=1000",
        "--groups=10,20",
        "ada",
    ]);
    assert_eq!(attached, detached);
}

#[test]
fn an_empty_group_list_clears_the_set_while_an_empty_element_is_malformed() {
    assert_eq!(
        changes(&["--groups=", "ada"]).supplementary_gids,
        Some(alloc::vec![])
    );
    assert_eq!(parse(&["--groups=1,,2", "ada"]), Err(UsageError::Usage));
    assert_eq!(parse(&["-g", "notanumber", "ada"]), Err(UsageError::Usage));
}

#[test]
fn the_lock_switches_are_exclusive_and_idempotent() {
    assert_eq!(changes(&["-L", "ada"]).lock, Some(Lock::Locked));
    assert_eq!(changes(&["--unlock", "ada"]).lock, Some(Lock::Unlocked));
    // Naming the same state twice is the same request, not an error.
    assert_eq!(changes(&["-L", "--lock", "ada"]).lock, Some(Lock::Locked));
    // Naming both is a contradiction, refused rather than guessed at.
    assert_eq!(parse(&["-L", "-U", "ada"]), Err(UsageError::Usage));
    assert_eq!(parse(&["-LU", "ada"]), Err(UsageError::Usage));
    assert_eq!(
        parse(&["--lock", "--unlock", "ada"]),
        Err(UsageError::Usage)
    );
}

#[test]
fn the_grant_ceiling_is_long_only_so_it_cannot_collide_with_shadow_utils() {
    assert_eq!(
        changes(&["--grants", "CAP_USER_ADMIN", "ada"])
            .grants
            .as_deref(),
        Some("CAP_USER_ADMIN")
    );
    // An empty ceiling is a legitimate request.
    assert_eq!(changes(&["--grants=", "ada"]).grants.as_deref(), Some(""));
}

#[test]
fn the_help_switches_win_immediately() {
    for flag in ["-h", "-?", "--help"] {
        assert_eq!(parse(&[flag]), Ok(Command::Help));
        assert_eq!(parse(&[flag, "-L", "ada"]), Ok(Command::Help));
    }
}

#[test]
fn end_of_options_lets_a_leading_dash_be_a_name() {
    match parse(&["-L", "--", "-odd"]) {
        Ok(Command::Modify { name, .. }) => assert_eq!(name, String::from("-odd")),
        other => panic!("expected a modify, got {other:?}"),
    }
}

#[test]
fn a_line_that_asks_for_nothing_is_a_usage_error_not_a_no_op_run() {
    assert_eq!(parse(&["ada"]), Err(UsageError::Usage));
}

#[test]
fn a_wrong_operand_count_fails_closed() {
    assert_eq!(parse(&["-L"]), Err(UsageError::Usage));
    assert_eq!(parse(&["-L", "ada", "bob"]), Err(UsageError::Usage));
}

/// `getopt`'s words for each refusal.
fn refused(args: &[&str]) -> String {
    parse(args).expect_err("refused").to_string()
}

#[test]
fn a_refused_option_is_worded_as_getopt_words_it() {
    assert_eq!(refused(&["-x", "ada"]), "invalid option -- 'x'");
    assert_eq!(refused(&["--frob", "ada"]), "unrecognized option '--frob'");
    assert_eq!(refused(&["-c"]), "option requires an argument -- 'c'");
    assert_eq!(
        refused(&["ada", "--shell"]),
        "option '--shell' requires an argument"
    );
    assert_eq!(
        refused(&["--lock=x", "ada"]),
        "option '--lock' doesn't allow an argument"
    );
}

/// `getopt_long` reads `--lo` as `--lock` and `--gra` as `--grants`, and
/// refuses a prefix two long names share.
#[test]
fn a_long_option_may_be_abbreviated() {
    let asked = changes(&["--lo", "--gra=CAP_USER_ADMIN", "ada"]);
    assert_eq!(asked.lock, Some(Lock::Locked));
    assert_eq!(asked.grants.as_deref(), Some("CAP_USER_ADMIN"));
    assert_eq!(
        refused(&["--gr", "1", "ada"]),
        "option '--gr' is ambiguous; possibilities: '--groups' '--grants'"
    );
}
