//! Unit tests for the `mkdir` parse and creation engine, over in-memory
//! seams (no kernel).

use super::*;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::cell::RefCell;
use tairix_fscopy::fixture::Tree;
use tairix_fscopy::tree::Kind;
use tairix_help::SourceError;

extern crate std;

/// The umask a fixture tree starts with.
const UMASK: u32 = 0o022;

/// Whether `path` names a directory in `fs`.
fn created(fs: &Tree, path: &str) -> bool {
    fs.kind(path) == Some(Kind::Directory)
}

/// The nodes a run set the mode of, by where each stood when it was set.
fn modes_set(fs: &Tree) -> Vec<String> {
    fs.calls()
        .into_iter()
        .filter_map(|line| line.strip_prefix("set_mode ").map(String::from))
        .collect()
}

/// A terminal fixture capturing everything written.
#[derive(Default)]
struct MockOut {
    text: RefCell<String>,
    fail: bool,
}

impl Output for MockOut {
    fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
        if self.fail {
            return Err(Errno::NotImplemented);
        }
        self.text
            .borrow_mut()
            .push_str(core::str::from_utf8(bytes).unwrap_or("<non-utf8>"));
        Ok(())
    }
}

/// A [`HelpSource`] with no documents, so `run` falls back to [`USAGE`].
struct NoHelp;

impl HelpSource for NoHelp {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(Vec::new())
    }

    fn read(&self, _locale_dir: &str, _file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        Ok(None)
    }
}

fn make(options: Options, paths: &[&str]) -> Command {
    Command::Make {
        options,
        mode: None,
        paths: paths.iter().map(|p| (*p).to_string()).collect(),
    }
}

/// `make` with `-m spec`.
fn make_with(options: Options, spec: &str, paths: &[&str]) -> Command {
    Command::Make {
        options,
        mode: Mode::parse(spec),
        paths: paths.iter().map(|p| (*p).to_string()).collect(),
    }
}

const PARENTS: Options = Options {
    parents: true,
    verbose: false,
};

// --- parse ---------------------------------------------------------------

#[test]
fn a_single_operand_parses() {
    assert_eq!(parse(&["d"]), Ok(make(Options::default(), &["d"])));
}

#[test]
fn no_operand_is_usage() {
    assert_eq!(parse(&[]), Err(MkdirError::Usage));
    assert_eq!(parse(&["-p"]), Err(MkdirError::Usage));
}

#[test]
fn switches_and_clusters_parse() {
    let options = Options {
        parents: true,
        verbose: true,
    };
    assert_eq!(parse(&["-p", "-v", "d"]), Ok(make(options, &["d"])));
    assert_eq!(parse(&["-pv", "d"]), Ok(make(options, &["d"])));
    assert_eq!(
        parse(&["--parents", "--verbose", "d"]),
        Ok(make(options, &["d"]))
    );
}

#[test]
fn help_spellings_parse() {
    assert_eq!(parse(&["-h"]), Ok(Command::Help));
    assert_eq!(parse(&["-?"]), Ok(Command::Help));
    assert_eq!(parse(&["--help"]), Ok(Command::Help));
}

/// GNU 9.4's words for each refusal.
fn refused(args: &[&str]) -> String {
    parse(args).expect_err("refused").to_string()
}

#[test]
fn a_refused_option_is_worded_as_getopt_words_it() {
    assert_eq!(refused(&["-x", "d"]), "invalid option -- 'x'");
    assert_eq!(refused(&["d", "-m"]), "option requires an argument -- 'm'");
    assert_eq!(
        refused(&["d", "--mode"]),
        "option '--mode' requires an argument"
    );
    assert_eq!(refused(&["-px", "d"]), "invalid option -- 'x'");
    assert_eq!(
        refused(&["--parents=1", "d"]),
        "option '--parents' doesn't allow an argument"
    );
}

/// GNU 9.4 reads `mkdir --par --verb d` as `mkdir --parents --verbose d`.
#[test]
fn a_long_option_may_be_abbreviated() {
    let options = Options {
        parents: true,
        verbose: true,
    };
    assert_eq!(parse(&["--par", "--verb", "d"]), Ok(make(options, &["d"])));
    assert_eq!(parse(&["--he"]), Ok(Command::Help));
}

#[test]
fn double_dash_ends_options_and_dash_is_an_operand() {
    assert_eq!(parse(&["--", "-p"]), Ok(make(Options::default(), &["-p"])));
    assert_eq!(parse(&["-"]), Ok(make(Options::default(), &["-"])));
}

/// GNU 9.4 spells the mode attached, as the next argument, or by a long
/// option's prefix, and keeps the last one given.
#[test]
fn a_mode_parses_in_every_spelling_and_the_last_wins() {
    let want = Ok(make_with(Options::default(), "700", &["d"]));
    for args in [
        &["-m", "700", "d"][..],
        &["-m700", "d"],
        &["--mode=700", "d"],
        &["--mode", "700", "d"],
        &["--mo=700", "d"],
        &["d", "-m", "700"],
        &["-m", "bogus", "-m", "700", "d"],
    ] {
        assert_eq!(parse(args), want, "{args:?}");
    }
    assert_eq!(
        parse(&["-pm", "700", "e/f"]),
        Ok(make_with(PARENTS, "700", &["e/f"]))
    );
}

/// GNU 9.4: `mkdir: invalid mode 'bogus'`, nothing made; with no operand the
/// missing operand is the refusal.
#[test]
fn a_bad_mode_is_refused_naming_it() {
    assert_eq!(
        parse(&["-m", "700", "-m", "bogus", "d"]),
        Err(MkdirError::BadMode("bogus".to_string()))
    );
    assert_eq!(refused(&["-m", "8", "d"]), "invalid mode '8'");
    assert_eq!(refused(&["-m", "", "d"]), "invalid mode ''");
    assert_eq!(parse(&["-m", "bogus"]), Err(MkdirError::Usage));
}

// --- engine --------------------------------------------------------------

/// The engine over `fs` under `umask`, answering what it reported when it did
/// not run clean: a refused operand is reported and the next still made.
fn run_in(fs: &Tree, umask: u32, command: Command) -> Result<(), String> {
    reported(|err| run(command, None, fs, umask, &NoHelp, &MockOut::default(), err))
}

fn run_make(fs: &Tree, options: Options, paths: &[&str]) -> Result<(), String> {
    run_in(fs, UMASK, make(options, paths))
}

/// What a run showed on its error stream, when it did not run clean.
fn reported(go: impl FnOnce(&MockOut) -> Result<bool, MkdirError>) -> Result<(), String> {
    let err = MockOut::default();
    match go(&err) {
        Ok(true) => Ok(()),
        Ok(false) => Err(err.text.borrow().clone()),
        Err(fatal) => Err(format!("mkdir: {fatal}\n")),
    }
}

/// `failure` as the tool reports it.
fn shown(failure: &MkdirError) -> Result<(), String> {
    Err(format!("mkdir: {failure}\n"))
}

fn create_refused(path: &str, errno: Errno) -> Result<(), String> {
    shown(&MkdirError::Create {
        path: path.to_string(),
        errno,
    })
}

#[test]
fn a_plain_creation_creates_exactly_the_operand() {
    let fs = Tree::new().dir("/Users/me");
    run_make(&fs, Options::default(), &["/Users/me/new"]).expect("create");
    assert!(created(&fs, "/Users/me/new"));
    run_make(&fs, Options::default(), &["/Users/me/trailing/"]).expect("create");
    assert!(created(&fs, "/Users/me/trailing"));
}

/// A name's control bytes are escaped where GNU escapes them, never written
/// raw to the terminal.
#[test]
fn a_name_holding_a_control_byte_is_escaped_in_every_message() {
    let fs = Tree::new();
    let out = MockOut::default();
    let verbose = Options {
        parents: false,
        verbose: true,
    };
    run(
        make(verbose, &["/n\u{1b}l"]),
        None,
        &fs,
        UMASK,
        &NoHelp,
        &out,
        &MockOut::default(),
    )
    .expect("create");
    assert_eq!(
        out.text.borrow().as_str(),
        "mkdir: created directory '/n'$'\\033''l'\n"
    );
    let refused = MkdirError::Create {
        path: "/a\u{1b}[2J".to_string(),
        errno: Errno::AlreadyExists,
    };
    assert_eq!(
        refused.to_string(),
        format!(
            "cannot create directory '/a'$'\\033''[2J': {}",
            Errno::AlreadyExists
        )
    );
}

#[test]
fn an_existing_name_is_already_exists() {
    let fs = Tree::new().dir("/d").file("/f", b"");
    assert_eq!(
        run_make(&fs, Options::default(), &["/d"]),
        create_refused("/d", Errno::AlreadyExists)
    );
    assert_eq!(
        run_make(&fs, Options::default(), &["/f"]),
        create_refused("/f", Errno::AlreadyExists)
    );
    // A root, `.` or `..` names an existing directory, or why there is none.
    assert_eq!(
        run_make(&fs, Options::default(), &["/"]),
        create_refused("/", Errno::AlreadyExists)
    );
    assert_eq!(
        run_make(&fs, Options::default(), &["/absent/.."]),
        create_refused("/absent/..", Errno::NotFound)
    );
}

/// GNU 9.4: `mkdir x/y z` names the refused one, still makes `z` and exits
/// 1.
#[test]
fn a_refused_operand_is_reported_and_the_next_still_made() {
    let fs = Tree::new().refuse("make_dir /b", Errno::PermissionDenied);
    assert_eq!(
        run_make(&fs, Options::default(), &["/a", "/b", "/c"]),
        create_refused("/b", Errno::PermissionDenied)
    );
    assert!(created(&fs, "/a"));
    assert!(created(&fs, "/c"));
}

#[test]
fn parents_creates_every_missing_ancestor() {
    let fs = Tree::new();
    run_make(&fs, PARENTS, &["/a/b/c"]).expect("create chain");
    assert!(created(&fs, "/a"));
    assert!(created(&fs, "/a/b"));
    assert!(created(&fs, "/a/b/c"));
}

#[test]
fn parents_tolerates_existing_directories() {
    let fs = Tree::new().dir("/a/b");
    run_make(&fs, PARENTS, &["/a/b/c"]).expect("only the leaf is new");
    assert!(created(&fs, "/a/b/c"));
}

#[test]
fn parents_tolerates_an_operand_that_is_already_a_directory() {
    let fs = Tree::new().dir("/a");
    run_make(&fs, PARENTS, &["/a"]).expect("existing directory is not an error under -p");
    run_make(&fs, PARENTS, &["/a/.."]).expect("nor is one a dot-dot reaches");
}

#[test]
fn parents_fails_when_a_prefix_exists_as_a_file() {
    let fs = Tree::new().file("/a", b"");
    assert_eq!(
        run_make(&fs, PARENTS, &["/a/b"]),
        create_refused("/a", Errno::AlreadyExists)
    );
}

/// A link among an operand's existing levels is passed through, as a path
/// walk passes through one, and a `..` leaves the directory it reached.
#[test]
fn parents_walks_through_an_existing_link_and_dot_dots_as_reached() {
    let fs = Tree::new().dir("/real").link("/via", "/real");
    run_make(&fs, PARENTS, &["/via/a/../b"]).expect("create through the link");
    assert!(created(&fs, "/real/a"));
    assert!(created(&fs, "/real/b"));
    assert_eq!(fs.kind("/via"), Some(Kind::Link), "the link itself stays");
}

#[test]
fn parents_on_a_bare_root_is_a_silent_success() {
    let fs = Tree::new();
    run_make(&fs, PARENTS, &["/"]).expect("a bare root always exists");
    run_make(&fs, PARENTS, &["."]).expect("and so does the working directory");
}

/// Run `mkdir` with `-m spec`, under `options`, over `fs` under `umask`.
fn run_mode(
    fs: &Tree,
    umask: u32,
    options: Options,
    spec: &str,
    paths: &[&str],
) -> Result<(), String> {
    run_in(fs, umask, make_with(options, spec, paths))
}

/// GNU 9.4 under umask `022`: the new directory gets exactly the mode named,
/// read from `a=rwx` as `chmod` reads it, the umask aside.
#[test]
fn a_mode_is_given_exactly_whatever_the_umask() {
    for (spec, want) in [
        ("777", 0o777),
        ("2755", 0o2755),
        ("755", 0o755),
        ("g-w", 0o757),
        ("-w", 0o577),
        ("=r", 0o444),
        ("u=rwx,g=rx,o=", 0o750),
        ("u=rwx,g=u", 0o777),
    ] {
        let fs = Tree::new();
        run_mode(&fs, UMASK, Options::default(), spec, &["/d"]).expect("create");
        assert_eq!(fs.mode("/d"), Some(want), "-m {spec}");
    }
}

/// The mode is read back and set only where the umask or a special bit left
/// it short.
#[test]
fn a_mode_the_umask_left_alone_is_not_set_again() {
    let fs = Tree::new();
    run_mode(&fs, UMASK, Options::default(), "755", &["/d"]).expect("create");
    assert!(modes_set(&fs).is_empty());
    run_mode(&fs, UMASK, Options::default(), "777", &["/e"]).expect("create");
    assert_eq!(modes_set(&fs), ["/e"]);
}

/// GNU 9.4 in a set-group-ID directory: the new directory keeps the bit it
/// inherits unless the mode names it.
#[test]
fn a_mode_keeps_an_inherited_set_group_id_bit_it_does_not_name() {
    for (spec, want) in [
        ("755", 0o2755),
        ("777", 0o2777),
        ("700", 0o2700),
        ("00755", 0o755),
        ("g-s", 0o777),
        ("u=rwx,go=rx", 0o2755),
    ] {
        let fs = Tree::new().dir("/shared").with_mode("/shared", 0o2775);
        run_mode(&fs, UMASK, Options::default(), spec, &["/shared/x"]).expect("create");
        assert_eq!(fs.mode("/shared/x"), Some(want), "-m {spec}");
    }
}

/// GNU 9.4: under umask `0777`, `mkdir -p p/q/r` makes the parents `300` and
/// the leaf `000`; under umask `0277` the parent is `700` and the leaf `500`.
#[test]
fn a_missing_parent_keeps_its_owners_write_and_search() {
    let fs = Tree::new().with_umask(0o777);
    run_in(&fs, 0o777, make(PARENTS, &["/p/q/r"])).expect("create chain");
    assert_eq!(fs.mode("/p"), Some(0o300));
    assert_eq!(fs.mode("/p/q"), Some(0o300));
    assert_eq!(fs.mode("/p/q/r"), Some(0o000));
    let fs = Tree::new().with_umask(0o277);
    run_in(&fs, 0o277, make(PARENTS, &["/s/t"])).expect("create chain");
    assert_eq!(fs.mode("/s"), Some(0o700));
    assert_eq!(fs.mode("/s/t"), Some(0o500));
    let fs = Tree::new();
    run_make(&fs, PARENTS, &["/u/v"]).expect("create chain");
    assert!(
        modes_set(&fs).is_empty(),
        "umask 022 leaves the owner whole"
    );
}

/// GNU 9.4: under umask `0`, `mkdir -p -m 700 x/y` makes `x` `777` and `y`
/// `700`; an operand already there keeps its mode.
#[test]
fn parents_take_the_umask_and_the_operand_the_mode() {
    let fs = Tree::new().with_umask(0);
    run_mode(&fs, 0, PARENTS, "700", &["/x/y"]).expect("create chain");
    assert_eq!(fs.mode("/x"), Some(0o777));
    assert_eq!(fs.mode("/x/y"), Some(0o700));
    let fs = Tree::new().dir("/g/h").with_mode("/g/h", 0o2755);
    run_mode(&fs, UMASK, PARENTS, "700", &["/g/h"]).expect("an existing directory");
    assert_eq!(fs.mode("/g/h"), Some(0o2755));
    assert!(modes_set(&fs).is_empty());
}

/// The mode `-m` names reaches the directory made even when another process
/// moves it aside and plants a link at its name straight after the
/// creation: the mode is set through the new directory's descriptor, never
/// its name.
#[test]
fn a_mode_reaches_the_directory_made_not_what_its_name_holds_next() {
    let fs = Tree::new().dir("/victim").with_mode("/victim", 0o700);
    fs.before_line(
        "stat /d",
        alloc::boxed::Box::new(|fs: &Tree| {
            fs.rename("/d", "/d.moved");
            fs.relink("/d", "/victim");
        }),
    );
    run_mode(&fs, UMASK, Options::default(), "777", &["/d"]).expect("create");
    assert_eq!(fs.mode("/d.moved"), Some(0o777), "the directory made");
    assert_eq!(
        fs.mode("/victim"),
        Some(0o700),
        "what the link names, untouched"
    );
}

/// A parent `-p` makes under a umask taking its owner's write and search has
/// them put back through its own descriptor, never through its name.
#[test]
fn a_parent_s_owner_bits_reach_the_parent_made() {
    let fs = Tree::new()
        .dir("/victim")
        .with_mode("/victim", 0o700)
        .with_umask(0o777);
    fs.before_line(
        "stat /p",
        alloc::boxed::Box::new(|fs: &Tree| {
            fs.rename("/p", "/p.moved");
            fs.relink("/p", "/victim");
        }),
    );
    run_in(&fs, 0o777, make(PARENTS, &["/p/q"])).expect("create chain");
    assert_eq!(fs.mode("/p.moved"), Some(0o300), "the parent made");
    assert_eq!(
        fs.mode("/victim"),
        Some(0o700),
        "what the link names, untouched"
    );
}

/// The next level is made in the ancestor `-p` made, held, so a link planted
/// at the ancestor's name between the two cannot carry the operand elsewhere.
///
/// Regression: each level was made by its whole path, so the link planted
/// at `/p` made `q` in what it named.
#[test]
fn a_level_is_made_in_the_ancestor_made_not_in_what_its_name_holds_next() {
    let fs = Tree::new().dir("/victim");
    fs.before_line(
        "make_dir /p/q",
        alloc::boxed::Box::new(|fs: &Tree| {
            fs.rename("/p", "/p.moved");
            fs.relink("/p", "/victim");
        }),
    );
    run_make(&fs, PARENTS, &["/p/q"]).expect("create chain");
    assert!(created(&fs, "/p.moved/q"), "made in the ancestor made");
    assert!(!fs.exists("/victim/q"), "never in what the link names");
}

#[test]
fn verbose_reports_each_created_directory_in_gnu_wording() {
    let fs = Tree::new().dir("/a");
    let out = MockOut::default();
    run(
        make(
            Options {
                parents: true,
                verbose: true,
            },
            &["/a/b/./c"],
        ),
        None,
        &fs,
        UMASK,
        &NoHelp,
        &out,
        &MockOut::default(),
    )
    .expect("create chain");
    // Only the directories actually created are reported, outermost first,
    // each as the operand spelled it.
    assert_eq!(
        out.text.borrow().as_str(),
        "mkdir: created directory '/a/b'\nmkdir: created directory '/a/b/./c'\n"
    );
}

#[test]
fn a_failed_verbose_write_is_an_output_error() {
    let fs = Tree::new();
    let out = MockOut {
        fail: true,
        ..MockOut::default()
    };
    assert_eq!(
        run(
            make(
                Options {
                    parents: false,
                    verbose: true,
                },
                &["/d"],
            ),
            None,
            &fs,
            UMASK,
            &NoHelp,
            &out,
            &MockOut::default(),
        ),
        Err(MkdirError::Output(Errno::NotImplemented))
    );
}

#[test]
fn help_falls_back_to_the_usage_banner() {
    let out = MockOut::default();
    run(
        Command::Help,
        None,
        &Tree::new(),
        UMASK,
        &NoHelp,
        &out,
        &MockOut::default(),
    )
    .expect("help");
    assert_eq!(out.text.borrow().as_str(), USAGE);
}

/// The banner is complete as written, so a consumer emits it verbatim.
///
/// The usage-error path used to hand it to the line-appending stderr writer,
/// which followed it with a blank line GNU `mkdir` never prints while the
/// help path above wrote the same constant unchanged.
#[test]
fn the_usage_banner_ends_in_exactly_one_newline() {
    assert!(USAGE.ends_with('\n'));
    assert!(!USAGE.ends_with("\n\n"));
}

/// Every locale's `OPTIONS` section documents exactly the switches this
/// parser accepts (`plans/APPS.md` §3.1): the flag tokens are
/// language-neutral, so each translated document must carry the same keys
/// as the canonical one. The documents are read from the bundle's own
/// on-disk `Help/` tree — the single source the image builder plants —
/// never a copy embedded in this crate.
#[test]
fn help_documents_the_parser_switches() {
    use std::fs;

    let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
    let locales = tairix_help::REQUIRED_LOCALES;
    for locale in locales {
        let path = format!("{help_root}/{locale}/mkdir.md");
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        for switch in [
            "`-m, --mode=MODE`",
            "`-p, --parents`",
            "`-v, --verbose`",
            "`-h, -?`",
        ] {
            assert!(
                text.contains(switch),
                "{locale}/mkdir.md must document {switch}"
            );
        }
    }
}
