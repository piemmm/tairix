//! Filename generation, brace expansion and the empty-word rule, end to end.
//!
//! The tree replicates the one these expectations were taken from by running
//! bash 5.2 (with `LC_ALL=C`): every `assert` here is bash's own output for
//! the same line, except where a comment names a deliberate difference.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::{Errno, FileKind};

use super::{Budget, ExpandError, Expander, MAX_EXPANSION_BYTES};
use crate::complete::{DirEntryInfo, DirLister};
use crate::env::Environment;
use crate::error::ParseError;
use crate::lexer::{tokenize, Token, Word};
use crate::options::{ShellOption, ShellOptions};
use crate::pathexp::{GlobFs, Ignore};
use crate::shell::Shell;
use crate::test_support::{MemFs, RecordingConsole, ScriptedHost};
use tairix_fuzzseed::meter::{metered, refusing_above};
use tairix_glob::{Options, Pattern};

/// bash's probe tree: two plain directories, a nested one, a hidden
/// directory and file, files of both cases, a name with a space, a link to a
/// directory and a dangling link.
fn tree() -> MemFs {
    MemFs::new("/work")
        .file("a/z")
        .file("a-b/c")
        .file("d/x")
        .file("d/e/y")
        .file("d/e/f/w")
        .dir(".hid")
        .file(".dot")
        .file("b.txt")
        .file("c.TXT")
        .file("A.txt")
        .file("Makefile")
        .file("sp ace")
        .link("linkd", "d")
        .link("dangle", "nowhere")
}

fn words(line: &str) -> Vec<Word> {
    tokenize(line)
        .expect("lexes")
        .into_iter()
        .map(|token| match token {
            Token::Word(word) => word,
            other => panic!("only words: {other:?}"),
        })
        .collect()
}

fn expand_with(
    fs: &MemFs,
    options: ShellOptions,
    env: &Environment,
    line: &str,
) -> Result<Vec<String>, ExpandError> {
    Expander::new(env, &options, fs, &Ignore::default())
        .fields(&words(line), &mut Budget::new(MAX_EXPANSION_BYTES))
}

fn expand(options: ShellOptions, line: &str) -> Vec<String> {
    expand_with(&tree(), options, &Environment::new(), line).expect("expands")
}

fn default(line: &str) -> Vec<String> {
    expand(ShellOptions::default(), line)
}

/// The defaults with `option` on.
fn only(option: ShellOption) -> ShellOptions {
    ShellOptions::default().with(option)
}

const GLOBSTAR: ShellOptions = ShellOptions::NONE.with(ShellOption::Globstar);

const EXTGLOB: ShellOptions = ShellOptions::NONE.with(ShellOption::Extglob);

#[test]
fn a_star_matches_every_visible_name_sorted_bytewise() {
    assert_eq!(
        default("*"),
        ["A.txt", "Makefile", "a", "a-b", "b.txt", "c.TXT", "d", "dangle", "linkd", "sp ace"]
    );
    assert_eq!(default("cp *.txt /tmp"), ["cp", "A.txt", "b.txt", "/tmp"]);
}

#[test]
fn a_pattern_matching_nothing_stays_as_written() {
    assert_eq!(default("cp *.tmp /tmp"), ["cp", "*.tmp", "/tmp"]);
    assert_eq!(default("[z]*"), ["[z]*"]);
}

#[test]
fn a_leading_dot_needs_a_dot() {
    assert_eq!(default(".*"), [".dot", ".hid"]);
    let dotglob = only(ShellOption::Dotglob);
    assert_eq!(
        expand(dotglob, "*")[..2],
        [".dot".to_string(), ".hid".to_string()]
    );
}

#[test]
fn wildcards_brackets_and_classes() {
    assert_eq!(default("?.txt"), ["A.txt", "b.txt"]);
    assert_eq!(default("[ab]*"), ["a", "a-b", "b.txt"]);
    assert_eq!(default("[!a]*").first().map(String::as_str), Some("A.txt"));
    assert_eq!(default("[[:upper:]]*"), ["A.txt", "Makefile"]);
    assert_eq!(default("[a-]*"), ["a", "a-b"]);
    assert_eq!(default(r"[a\-c]*"), ["a", "a-b", "c.TXT"]);
    assert_eq!(default("sp?ace"), ["sp ace"]);
}

#[test]
fn quoting_makes_pattern_characters_literal() {
    assert_eq!(
        default(r#""*".txt \*.txt b"*" "b"*"#),
        ["*.txt", "*.txt", "b*", "b.txt"]
    );
    assert_eq!(
        default("'['ab]* [b]* [* [ ]"),
        ["[ab]*", "b.txt", "[*", "[", "]"]
    );
}

#[test]
fn a_variable_s_value_is_never_a_pattern() {
    let mut env = Environment::new();
    env.set("pat", "*.txt");
    env.set("dir", "d");
    let fs = tree();
    let options = ShellOptions::default();
    // zsh's rule: the value is data, so only the typed `*` is a wildcard.
    assert_eq!(
        expand_with(&fs, options, &env, "$pat").expect("expands"),
        ["*.txt"]
    );
    assert_eq!(
        expand_with(&fs, options, &env, "$dir/*").expect("expands"),
        ["d/e", "d/x"]
    );
}

#[test]
fn components_walk_directories_and_links_to_them() {
    assert_eq!(
        default("*/*"),
        ["a-b/c", "a/z", "d/e", "d/x", "linkd/e", "linkd/x"]
    );
    assert_eq!(default("*/"), ["a-b/", "a/", "d/", "linkd/"]);
    assert_eq!(default("*/x"), ["d/x", "linkd/x"]);
    assert_eq!(default("linkd/*"), ["linkd/e", "linkd/x"]);
    assert_eq!(default("d/e/*/"), ["d/e/f/"]);
    // A dangling link is a name like any other.
    assert_eq!(default("dang*"), ["dangle"]);
}

#[test]
fn the_word_s_own_spelling_of_its_prefix_is_kept() {
    assert_eq!(default("./*.txt"), ["./A.txt", "./b.txt"]);
    assert_eq!(default("d/../b*"), ["d/../b.txt"]);
    assert_eq!(default("/work/b*"), ["/work/b.txt"]);
    let fs = tree().alias("Home", "/work");
    assert_eq!(
        expand_with(
            &fs,
            ShellOptions::default(),
            &Environment::new(),
            "Home:/d/*"
        )
        .expect("expands"),
        ["Home:/d/e", "Home:/d/x"]
    );
}

/// Every root spelling the grammar has is generated over and kept as
/// written: the expanded alias and the volume identity as well as `Name:/`.
#[test]
fn every_root_spelling_is_generated_over() {
    const VOLUME: &str = "b7f2e4e6-8d7a-4ef8-a13e-d3b84d4e8001";
    let fs = tree().alias("Home", "/work").volume(VOLUME, "/work");
    let env = Environment::new();
    let fields =
        |line: &str| expand_with(&fs, ShellOptions::default(), &env, line).expect("expands");
    assert_eq!(
        fields("alias::Home/d/*"),
        ["alias::Home/d/e", "alias::Home/d/x"]
    );
    let id = alloc::format!("id::{VOLUME}");
    assert_eq!(
        fields(&alloc::format!("{id}/d/*")),
        [alloc::format!("{id}/d/e"), alloc::format!("{id}/d/x")]
    );
    assert_eq!(
        fields(&alloc::format!("{id}/[[:upper:]]*")),
        [
            alloc::format!("{id}/A.txt"),
            alloc::format!("{id}/Makefile")
        ]
    );
}

/// A `:` inside a bracket expression is pattern syntax: only one in the
/// literal text before the first `/` can spell a root.
#[test]
fn a_colon_in_a_bracket_expression_is_no_root() {
    assert_eq!(default("[[:digit:][:upper:]]*"), ["A.txt", "Makefile"]);
    let fs = tree().alias("Home", "/work");
    assert_eq!(
        expand_with(
            &fs,
            ShellOptions::default(),
            &Environment::new(),
            "Home:/[[:upper:]]*"
        )
        .expect("expands"),
        ["Home:/A.txt", "Home:/Makefile"]
    );
}

#[test]
fn a_literal_last_component_is_checked_without_a_listing() {
    let fs = tree();
    let env = Environment::new();
    assert_eq!(
        expand_with(&fs, ShellOptions::default(), &env, "*/y").expect("expands"),
        ["*/y"]
    );
    let before = fs.listings();
    assert_eq!(
        expand_with(&fs, ShellOptions::default(), &env, "d/*/y").expect("expands"),
        ["d/e/y"]
    );
    // Only `d` is listed; `d/e/y` is probed, not read.
    assert_eq!(fs.listings() - before, 1);
    // A directory is needed only before a `/`.
    assert_eq!(
        expand_with(&fs, ShellOptions::default(), &env, "*/x/").expect("expands"),
        ["*/x/"]
    );
}

#[test]
fn without_globstar_a_double_star_is_a_star() {
    assert_eq!(default("**"), default("*"));
    assert_eq!(default("**/x"), ["d/x", "linkd/x"]);
}

#[test]
fn globstar_matches_any_number_of_levels_never_through_a_link() {
    assert_eq!(
        expand(GLOBSTAR, "**"),
        [
            "A.txt", "Makefile", "a", "a-b", "a-b/c", "a/z", "b.txt", "c.TXT", "d", "d/e", "d/e/f",
            "d/e/f/w", "d/e/y", "d/x", "dangle", "linkd", "sp ace"
        ]
    );
    assert_eq!(
        expand(GLOBSTAR, "**/"),
        ["a-b/", "a/", "d/", "d/e/", "d/e/f/", "linkd/"]
    );
    assert_eq!(
        expand(GLOBSTAR, "d/**"),
        ["d/", "d/e", "d/e/f", "d/e/f/w", "d/e/y", "d/x"]
    );
    assert_eq!(expand(GLOBSTAR, "d/**/"), ["d/", "d/e/", "d/e/f/"]);
    assert_eq!(expand(GLOBSTAR, "**/y"), ["d/e/y"]);
    assert_eq!(expand(GLOBSTAR, "**/x"), ["d/x"]);
    assert_eq!(expand(GLOBSTAR, "**/**/y"), ["d/e/y"]);
    assert_eq!(expand(GLOBSTAR, "**/b.txt"), ["b.txt"]);
    assert_eq!(expand(GLOBSTAR, "d/**/e"), ["d/e"]);
    assert_eq!(
        expand(GLOBSTAR, "**/*/"),
        ["a-b/", "a/", "d/", "d/e/", "d/e/f/", "linkd/"]
    );
    assert_eq!(expand(GLOBSTAR, "linkd/**")[0], "linkd/");
    assert_eq!(expand(GLOBSTAR, ".hid/**"), [".hid/"]);
    assert_eq!(expand(GLOBSTAR, "nonexist/**"), ["nonexist/**"]);
    assert_eq!(expand(GLOBSTAR, "d/x/**"), ["d/x/**"]);
    // Within a component `**` is an ordinary star.
    assert_eq!(expand(GLOBSTAR, "a**"), ["a", "a-b"]);
    let dotted = GLOBSTAR.with(ShellOption::Dotglob);
    assert_eq!(expand(dotted, "**/")[0], ".hid/");
}

/// bash spells the level `**` adds nothing to differently in two places
/// (`d/**` gives `d/`, but `**/e/**` gives `d/e`); the shell spells it one
/// way, the prefix and its separator, every time.
#[test]
fn a_globstar_s_empty_level_is_spelled_one_way() {
    assert_eq!(
        expand(GLOBSTAR, "**/e/**"),
        ["d/e/", "d/e/f", "d/e/f/w", "d/e/y"]
    );
}

#[test]
fn a_run_of_double_stars_stands_for_one() {
    assert_eq!(expand(GLOBSTAR, "**/**"), expand(GLOBSTAR, "**"));
    assert_eq!(
        expand(GLOBSTAR, "**/**/"),
        ["a-b/", "a/", "d/", "d/e/", "d/e/f/", "linkd/"]
    );
    // bash spells this run's empty level `d`, as it does no lone `d/**`.
    assert_eq!(expand(GLOBSTAR, "d/**/**"), expand(GLOBSTAR, "d/**"));
    assert_eq!(expand(GLOBSTAR, "**/**/**/y"), ["d/e/y"]);
}

/// A directory holding one directory, without end: the cycle a hostile
/// volume could present.
struct Endless;

impl DirLister for Endless {
    fn list_dir(&self, _dir: &str) -> Result<Vec<DirEntryInfo>, Errno> {
        Ok(alloc::vec![DirEntryInfo {
            name: "n".to_string(),
            kind: FileKind::Directory,
        }])
    }
}

impl GlobFs for Endless {
    fn read_dir(
        &self,
        _dir: &str,
        each: &mut dyn FnMut(&str, FileKind) -> core::ops::ControlFlow<()>,
    ) -> Result<(), Errno> {
        let _ = each("n", FileKind::Directory);
        Ok(())
    }

    fn kind(&self, _path: &str, _follow: bool) -> Result<FileKind, Errno> {
        Ok(FileKind::Directory)
    }
}

#[test]
fn a_walk_ends_where_a_path_could_no_longer_be_spelled() {
    let walk = |line| {
        Expander::new(&Environment::new(), &GLOBSTAR, &Endless, &Ignore::default())
            .fields(&words(line), &mut Budget::new(MAX_EXPANSION_BYTES))
            .expect("expands")
    };
    for line in ["**", "**/", "**/*", "**/n"] {
        assert_eq!(walk(line).len(), tairix_path::MAX_COMPONENTS, "{line}");
    }
    // The bound is the path's, wherever the walk begins.
    let paths = walk("n/n/**");
    assert_eq!(paths.len(), tairix_path::MAX_COMPONENTS - 1);
    assert!(paths.iter().all(
        |path| path.split('/').filter(|part| !part.is_empty()).count()
            <= tairix_path::MAX_COMPONENTS
    ));
}

#[test]
fn a_wildcard_after_a_double_star_reads_each_directory_once() {
    let fs = tree();
    let env = Environment::new();
    let before = fs.listings();
    assert_eq!(
        expand_with(&fs, GLOBSTAR, &env, "**/[xy]").expect("expands"),
        ["d/e/y", "d/x"]
    );
    // The working directory, `a`, `a-b`, `d`, `d/e` and `d/e/f`.
    assert_eq!(fs.listings() - before, 6);
    assert_eq!(
        expand_with(&fs, GLOBSTAR, &env, "**/e*/").expect("expands"),
        ["d/e/"]
    );
    // A hidden name matches a `.`, though a hidden directory is no level.
    assert_eq!(
        expand_with(&fs, GLOBSTAR, &env, "**/.*").expect("expands"),
        [".dot", ".hid"]
    );
}

#[test]
fn nullglob_and_failglob() {
    let nullglob = only(ShellOption::Nullglob);
    assert_eq!(expand(nullglob, "x *.nomatch y"), ["x", "y"]);
    // `[` and `]` open no expression, so they are not patterns at all.
    assert_eq!(expand(nullglob, "[ -f x ]"), ["[", "-f", "x", "]"]);
    let failglob = only(ShellOption::Failglob);
    let err = expand_with(&tree(), failglob, &Environment::new(), "ls *.nomatch")
        .expect_err("matches nothing");
    assert_eq!(err.error, ParseError::NoMatch);
    assert_eq!(err.subject.as_deref(), Some("*.nomatch"));
}

#[test]
fn nocaseglob_folds_patterns_but_not_literal_components() {
    let nocase = only(ShellOption::Nocaseglob);
    assert_eq!(expand(nocase, "*.txt"), ["A.txt", "b.txt", "c.TXT"]);
    assert_eq!(expand(nocase, "[B-C]*"), ["b.txt", "c.TXT"]);
    assert_eq!(expand(nocase, "d/../makef*"), ["d/../Makefile"]);
    assert_eq!(expand(nocase, "*/X"), ["*/X"]);
    assert_eq!(expand(nocase, "makefile"), ["makefile"]);
    assert_eq!(expand(nocase, "[[:upper:]]*"), ["A.txt", "Makefile"]);
}

#[test]
fn extended_patterns() {
    assert_eq!(
        expand(EXTGLOB, "!(*.txt|*.TXT)"),
        ["Makefile", "a", "a-b", "d", "dangle", "linkd", "sp ace"]
    );
    assert_eq!(expand(EXTGLOB, "*.@(txt|TXT)"), ["A.txt", "b.txt", "c.TXT"]);
    assert_eq!(expand(EXTGLOB, "+([a-c])"), ["a"]);
    assert_eq!(expand(EXTGLOB, "@(.dot|b.txt)"), [".dot", "b.txt"]);
    assert_eq!(expand(EXTGLOB, "!(.foo)")[0], "A.txt", "a dot needs a dot");
    assert_eq!(expand(EXTGLOB, "@(sp ace)"), ["sp ace"]);
    assert_eq!(expand(EXTGLOB, "@(a|*b)/*"), ["a-b/c", "a/z"]);
    assert_eq!(expand(EXTGLOB, "a!(-b)"), ["a"]);
    assert_eq!(expand(EXTGLOB, "!(*)"), ["!(*)"]);
}

/// A `/` inside a group belongs to it, as bash 5.2 reads it: the group is one
/// component, and an alternative holding a `/` matches no name.
#[test]
fn a_slash_inside_a_group_keeps_the_group_one_component() {
    assert_eq!(expand(EXTGLOB, "@(a|d/x)"), ["a"]);
    assert_eq!(expand(EXTGLOB, "!(d/x)"), default("*"));
    assert_eq!(expand(EXTGLOB, "@(a|d/x)/*"), ["a/z"]);
    assert_eq!(expand(EXTGLOB, "@(d/x)"), ["@(d/x)"]);
}

/// Deliberately unlike bash: an entry is compared a component at a time, so
/// one whose group holds a `/`, which bash lets reach across one, is refused
/// rather than left hiding less than it names.
#[test]
fn a_globignore_group_holding_a_slash_is_refused() {
    let mut env = Environment::new();
    env.set("GLOBIGNORE", "@(b.txt|d/x)");
    let err = expand_with(&tree(), EXTGLOB, &env, "b*").expect_err("refused");
    assert_eq!(err.error, ParseError::PatternGroupHoldsSlash);
    assert_eq!(err.subject.as_deref(), Some("GLOBIGNORE"));
    env.set("GLOBIGNORE", "@(b.txt|d)/x");
    assert_eq!(
        expand_with(&tree(), EXTGLOB, &env, "d/*").expect("expands"),
        ["d/e"]
    );
}

#[test]
fn globignore_removes_what_it_names_and_shows_dotfiles() {
    let mut env = Environment::new();
    env.set("GLOBIGNORE", "b.*:*.TXT");
    let all = expand_with(&tree(), ShellOptions::default(), &env, "*").expect("expands");
    assert_eq!(all[..2], [".dot".to_string(), ".hid".to_string()]);
    assert!(!all.iter().any(|name| name == "b.txt" || name == "c.TXT"));
    env.set("GLOBIGNORE", "*");
    assert_eq!(
        expand_with(&tree(), ShellOptions::default(), &env, "* .*").expect("expands"),
        ["*", ".*"]
    );
    env.set("GLOBIGNORE", "d/*");
    assert_eq!(
        expand_with(&tree(), ShellOptions::default(), &env, "d/*").expect("expands"),
        ["d/*"]
    );
    // While it filters, a path ending in `.` or `..` is never kept.
    assert_eq!(default("*/."), ["a-b/.", "a/.", "d/.", "linkd/."]);
    env.set("GLOBIGNORE", "zz");
    for line in ["*/.", "d/*/.."] {
        assert_eq!(
            expand_with(&tree(), ShellOptions::default(), &env, line).expect("expands"),
            [line]
        );
    }
    // bash ignores nothing for an entry that is no pattern; refusing keeps a
    // typo from exposing what it was meant to hide. Only a line whose
    // pattern matched anything needs the list, so no other line fails.
    env.set("GLOBIGNORE", "[z-a]");
    let err = expand_with(&tree(), ShellOptions::default(), &env, "b*").expect_err("bad entry");
    assert_eq!(err.subject.as_deref(), Some("GLOBIGNORE"));
    assert_eq!(
        expand_with(&tree(), ShellOptions::default(), &env, "echo *.nomatch").expect("expands"),
        ["echo", "*.nomatch"]
    );
}

/// A match the heap cannot hold fails its line, never the shell.
#[test]
fn a_match_the_heap_cannot_hold_fails_only_its_line() {
    let fs = MemFs::new("/work").file(&"a".repeat(255));
    let line = "@(a)".repeat(16);
    let err = refusing_above(32 * 1024, || {
        expand_with(&fs, EXTGLOB, &Environment::new(), &line)
    })
    .expect_err("the match needs more than the heap gives");
    assert_eq!(err.error, ParseError::OutOfMemory);
}

#[test]
fn a_malformed_globignore_leaves_other_commands_running() {
    let fs = tree();
    let (host, console, status) = shell_run(
        &fs,
        &["GLOBIGNORE='[z-a]'", "ls b*", "unset GLOBIGNORE", "ls b*"],
    );
    assert_eq!(status, 0, "{}", console.stderr());
    assert!(console.stderr().contains("GLOBIGNORE"));
    assert_eq!(launched_argv(&host), [["ls", "b.txt"]]);
}

#[test]
fn noglob_turns_filename_generation_off() {
    let noglob = only(ShellOption::Noglob);
    assert_eq!(expand(noglob, "*.txt {a,b}"), ["*.txt", "a", "b"]);
}

#[test]
fn braces_expand_first_and_each_result_globs_on_its_own() {
    assert_eq!(default("{b,a}*"), ["b.txt", "a", "a-b"]);
    assert_eq!(default("*.{txt,TXT}"), ["A.txt", "b.txt", "c.TXT"]);
    assert_eq!(default("a{b,c}/*"), ["ab/*", "ac/*"]);
    assert_eq!(default("{a,d}/*"), ["a/z", "d/e", "d/x"]);
    // An empty unquoted result is no word; a quoted one is.
    assert_eq!(default("x{,}y {,a} {\"\",a}"), ["xy", "xy", "a", "", "a"]);
}

#[test]
fn an_empty_unquoted_word_is_dropped() {
    let mut env = Environment::new();
    env.set("EMPTY", "");
    let fs = tree();
    let fields = |line| expand_with(&fs, ShellOptions::default(), &env, line).expect("expands");
    assert_eq!(fields("echo a $EMPTY b"), ["echo", "a", "b"]);
    assert_eq!(fields("echo \"$EMPTY\" ''"), ["echo", "", ""]);
    assert_eq!(fields("echo $EMPTY\"\""), ["echo", ""]);
    assert_eq!(fields("$EMPTY"), Vec::<String>::new());
}

#[test]
fn export_s_assignment_operands_are_not_globbed() {
    let fs = tree().file("x=foo");
    let env = Environment::new();
    let fields = |line| expand_with(&fs, ShellOptions::default(), &env, line).expect("expands");
    assert_eq!(fields("export x=*"), ["export", "x=*"]);
    assert_eq!(fields("echo x=*"), ["echo", "x=foo"]);
    // Braces still expand there, as bash's do.
    assert_eq!(fields("export x={a,b}"), ["export", "x=a", "x=b"]);
}

/// A listing that fails once it has begun handed over only part of its
/// directory, whatever the failure, so the line stops rather than use it.
#[test]
fn a_listing_that_fails_part_way_is_never_taken_as_whole() {
    let fs = tree().interrupted("d", Errno::NotFound);
    let err = expand_with(&fs, ShellOptions::default(), &Environment::new(), "d/*")
        .expect_err("a partial listing");
    assert_eq!(err.error, ParseError::UnreadableDirectory(Errno::NotFound));
    assert_eq!(err.subject.as_deref(), Some("d"));
}

/// A probe that fails other than as an absence is reported as the lookup it
/// was, never as a directory that could not be read.
#[test]
fn a_failing_probe_names_the_path_it_looked_up() {
    let fs = tree().unresolvable("d/x", Errno::DeviceFault);
    let err = expand_with(&fs, ShellOptions::default(), &Environment::new(), "[d]/x")
        .expect_err("a failing probe");
    assert_eq!(err.error, ParseError::UnresolvablePath(Errno::DeviceFault));
    assert_eq!(err.subject.as_deref(), Some("d/x"));
    assert!(alloc::format!("{}", err.error).starts_with("cannot look up path: "));
}

#[test]
fn a_reference_shaped_word_names_nothing_on_disk() {
    assert_eq!(default("cat sys:*"), ["cat", "sys:*"]);
}

#[test]
fn listing_failures_mean_nothing_there_unless_the_result_would_be_partial() {
    let denied = tree().failing("d", Errno::PermissionDenied);
    let env = Environment::new();
    assert_eq!(
        expand_with(&denied, ShellOptions::default(), &env, "d/*").expect("expands"),
        ["d/*"]
    );
    // A path no directory could have, such as one deeper than the kernel
    // resolves, is just as empty.
    let deep = tree().failing("d/e", Errno::OutOfRange);
    assert_eq!(
        expand_with(&deep, ShellOptions::default(), &env, "d/e/*").expect("expands"),
        ["d/e/*"]
    );
    // A failing device, a volume gone, or a listing the ABI does not
    // describe, is not.
    for errno in [Errno::DeviceFault, Errno::NotAttached, Errno::BadMagic] {
        let faulty = tree().failing("d/e", errno);
        let err = expand_with(&faulty, GLOBSTAR, &env, "**").expect_err("an unreadable directory");
        assert_eq!(err.error, ParseError::UnreadableDirectory(errno));
        assert_eq!(err.subject.as_deref(), Some("d/e"));
    }
}

#[test]
fn a_malformed_pattern_is_refused_and_named() {
    let err = expand_with(
        &tree(),
        ShellOptions::default(),
        &Environment::new(),
        "[[:nope:]]*",
    )
    .expect_err("unknown class");
    assert!(matches!(err.error, ParseError::Pattern(_)));
    assert_eq!(err.subject.as_deref(), Some("[[:nope:]]*"));
}

#[test]
fn an_expansion_is_bounded() {
    let err = expand_with(
        &tree(),
        ShellOptions::default(),
        &Environment::new(),
        "{1..100000000}",
    )
    .expect_err("too large");
    assert_eq!(err.error, ParseError::ExpansionTooLarge);
    // The bound is the argument block's, so a hundred thousand short words
    // fit within it.
    assert_eq!(default("{1..100000}").len(), 100_000);
}

// --- Through the interpreter -----------------------------------------------

fn shell_run(fs: &MemFs, lines: &[&str]) -> (ScriptedHost, RecordingConsole, i32) {
    let host = ScriptedHost::new();
    let console = RecordingConsole::new();
    let status = {
        let mut shell = Shell::new(&host, &console).with_filesystem(fs);
        for line in lines {
            let _ = shell.run_line(line);
        }
        shell.environment().last_status()
    };
    (host, console, status)
}

fn launched_argv(host: &ScriptedHost) -> Vec<Vec<String>> {
    host.launches()
        .into_iter()
        .map(|launch| launch.commands[0].argv.clone())
        .collect()
}

#[test]
fn a_launched_command_receives_the_matches() {
    let fs = tree();
    let (host, _, status) = shell_run(&fs, &["cp *.txt d/*/ dest"]);
    assert_eq!(status, 0);
    assert_eq!(
        launched_argv(&host),
        [["cp", "A.txt", "b.txt", "d/e/", "dest"]]
    );
}

#[test]
fn the_command_word_globs_too() {
    let fs = MemFs::new("/work").file("runme");
    let (host, _, _) = shell_run(&fs, &["run* arg"]);
    assert_eq!(launched_argv(&host), [["runme", "arg"]]);
}

#[test]
fn shopt_changes_how_the_next_line_globs() {
    let fs = tree();
    let (host, console, status) =
        shell_run(&fs, &["shopt -s nullglob globstar", "ls *.nomatch **/y"]);
    assert_eq!(status, 0, "{}", console.stderr());
    assert_eq!(launched_argv(&host), [["ls", "d/e/y"]]);
}

#[test]
fn failglob_aborts_the_line_with_status_one() {
    let fs = tree();
    let (host, console, status) =
        shell_run(&fs, &["shopt -s failglob", "ls *.nomatch; echo same-line"]);
    assert_eq!(status, 1);
    assert_eq!(console.stderr(), "shell: '*.nomatch': no match\n");
    assert!(host.launches().is_empty());
    assert!(!console.stdout().contains("same-line"));
}

#[test]
fn an_ambiguous_redirect_fails_only_its_command() {
    let fs = tree();
    let (host, console, status) = shell_run(&fs, &["cat < *.txt; echo $?"]);
    assert_eq!(console.stderr(), "shell: '*.txt': ambiguous redirect\n");
    assert!(host.launches().is_empty());
    assert_eq!(console.stdout(), "1\n");
    assert_eq!(status, 0);
    let (_, console, _) = shell_run(&fs, &["cat > {x,y}"]);
    assert!(console.stderr().contains("{x,y}: ambiguous redirect"));
    // One match is the target.
    let (host, _, _) = shell_run(&fs, &["cat < b.*"]);
    let launches = host.launches();
    assert_eq!(
        launches[0].commands[0].redirections[0].action,
        crate::host::RedirAction::Open {
            mode: crate::parser::OpenMode::Read,
            target: crate::host::RedirTarget::Path("b.txt".into()),
        }
    );
}

#[test]
fn failglob_in_a_redirect_fails_only_its_command() {
    let fs = tree();
    let (_, console, _) = shell_run(&fs, &["shopt -s failglob", "cat < *.nomatch; echo $?"]);
    assert_eq!(console.stderr(), "shell: '*.nomatch': no match\n");
    assert_eq!(console.stdout(), "1\n");
}

#[test]
fn extglob_syntax_is_refused_while_the_option_is_off() {
    let fs = tree();
    let (host, console, status) = shell_run(&fs, &["shopt -s extglob; ls @(a|b)"]);
    // As bash reads the line before `shopt` runs, the whole line is refused.
    assert_eq!(status, 2);
    assert!(console.stderr().contains("needs `shopt -s extglob`"));
    assert!(host.launches().is_empty());
    let (host, _, _) = shell_run(&fs, &["shopt -s extglob", "ls @(a|b.txt)"]);
    assert_eq!(launched_argv(&host), [["ls", "a", "b.txt"]]);
}

#[test]
fn a_command_that_expands_to_nothing() {
    let fs = tree();
    let (host, console, status) = shell_run(&fs, &["false_status=$?", "$EMPTY"]);
    assert_eq!(status, 0);
    assert!(host.launches().is_empty());
    assert_eq!(console.stderr(), "");
    // Its assignments then bind in the shell, as bash's do.
    let (_, console, _) = shell_run(&fs, &["X=1 $EMPTY", "echo $X"]);
    assert_eq!(console.stdout(), "1\n");
    // bash runs both and returns 0; the host cannot apply a redirection or
    // join a pipeline without a program, so the shell refuses them.
    for line in ["$EMPTY > out", "$EMPTY | cat"] {
        let (host, console, status) = shell_run(&fs, &[line]);
        assert_eq!(status, 1, "{line}");
        assert!(console.stderr().contains("command expanded to nothing"));
        assert!(host.launches().is_empty());
    }
}

#[test]
fn cd_refuses_a_pattern_naming_several_directories() {
    let fs = tree();
    let (_, console, status) = shell_run(&fs, &["cd [ad]"]);
    assert_eq!(status, 1);
    assert_eq!(console.stderr(), "cd: too many arguments\n");
}

#[test]
fn an_assignment_value_is_not_globbed() {
    let fs = tree();
    let (_, console, _) = shell_run(&fs, &["x=*", "echo \"$x\" $x"]);
    assert_eq!(console.stdout(), "* *\n");
}

// --- Names a listing returns -----------------------------------------------

/// A volume written elsewhere may hold a directory named `Home:`, which,
/// leading a relative path, the grammar reads as the alias root.
fn stick() -> MemFs {
    MemFs::new("/stick")
        .file("Home:/stick-file")
        .dir("Home:/Documents")
        .file("B")
        .file("-x")
        .alias("Home", "/Users/me")
        .file("/Users/me/home-file")
        .dir("/Users/me/Documents")
}

#[test]
fn a_listed_name_holding_a_colon_stays_relative() {
    let fs = stick();
    let env = Environment::new();
    let fields = |line| expand_with(&fs, ShellOptions::default(), &env, line).expect("expands");
    assert_eq!(fields("*/Documents"), ["./Home:/Documents"]);
    assert_eq!(fields("*/*"), ["./Home:/Documents", "./Home:/stick-file"]);
    // Sorted by name, as bash spells them.
    assert_eq!(fields("*"), ["-x", "B", "./Home:"]);
    // A root the word spells itself is kept as written.
    assert_eq!(fields("Home:/*"), ["Home:/Documents", "Home:/home-file"]);
}

#[test]
fn globignore_reads_a_listed_name_as_bash_spells_it() {
    let fs = stick();
    let mut env = Environment::new();
    env.set("GLOBIGNORE", "Home?");
    assert_eq!(
        expand_with(&fs, ShellOptions::default(), &env, "*").expect("expands"),
        ["-x", "B"]
    );
}

/// `GLOBIGNORE` is compiled once for each value it takes, charged to the line
/// that first needs it at what it holds, rather than once per command.
#[test]
fn globignore_is_compiled_once_for_each_value_and_charged() {
    let fs = tree();
    let ignore = Ignore::default();
    let spent = |value: &str| {
        let mut env = Environment::new();
        env.set("GLOBIGNORE", value);
        let mut budget = Budget::new(MAX_EXPANSION_BYTES);
        let fields = Expander::new(&env, &ShellOptions::default(), &fs, &ignore)
            .fields(&words("*.txt"), &mut budget)
            .expect("expands");
        (fields, MAX_EXPANSION_BYTES - budget.remaining())
    };
    for (value, kept) in [("b.*", ["A.txt"]), ("A.*", ["b.txt"])] {
        let (fields, compiling) = spent(value);
        assert_eq!(fields, kept, "{value}");
        let (_, compiled) = spent(value);
        let held = Pattern::with_options(value, Options::PATHNAME)
            .expect("compiles")
            .heap_bytes();
        // The value's copy, its one pattern, and that pattern's slot.
        assert_eq!(
            compiling - compiled,
            value.len() + held + core::mem::size_of::<Pattern>(),
            "{value}"
        );
    }
    let (host, _, _) = shell_run(
        &fs,
        &[
            "GLOBIGNORE='b.*'",
            "ls *.txt",
            "GLOBIGNORE='A.*'",
            "ls *.txt",
        ],
    );
    assert_eq!(launched_argv(&host), [["ls", "A.txt"], ["ls", "b.txt"]]);
}

#[test]
fn a_command_is_handed_the_listed_name_never_the_alias() {
    let fs = stick();
    let (host, _, status) = shell_run(&fs, &["rm -r */Documents"]);
    assert_eq!(status, 0);
    assert_eq!(launched_argv(&host), [["rm", "-r", "./Home:/Documents"]]);
}

// --- One bound per line ----------------------------------------------------

#[test]
fn a_pipeline_s_members_share_one_bound() {
    let fs = tree();
    let member = alloc::format!("cmd x{{1..20000}}{}", "y".repeat(200));
    let (host, _, status) = shell_run(&fs, &[member.as_str()]);
    assert_eq!(status, 0, "one member fits");
    assert_eq!(host.launches().len(), 1);
    let line = alloc::format!("{member} | {member}");
    let (host, console, status) = shell_run(&fs, &[line.as_str()]);
    assert_eq!(status, 1);
    assert!(console
        .stderr()
        .contains("expansion exceeds the argument size limit"));
    assert!(host.launches().is_empty());
}

#[test]
fn assignments_on_one_line_share_the_bound() {
    let fs = tree();
    let seed = alloc::format!("x={}", "0123456789abcdef".repeat(64));
    let doubling = ["x=$x$x"; 14].join("; ");
    let (_, console, status) = shell_run(&fs, &[seed.as_str(), doubling.as_str()]);
    assert_eq!(status, 1);
    assert!(console
        .stderr()
        .contains("expansion exceeds the argument size limit"));
    // The next line has a bound of its own, and a value nearly the bound's
    // size passes through it whole.
    let (host, _, status) = shell_run(&fs, &[seed.as_str(), doubling.as_str(), "cmd $x"]);
    assert_eq!(status, 0);
    assert_eq!(launched_argv(&host)[0][1].len(), 1024 << 13);
}

/// What an expansion holds is what it is charged — the braced words made on
/// the way and the fields kept — never bookkeeping per character.
#[test]
fn an_expansion_holds_no_more_than_it_is_charged() {
    let fs = tree();
    let mut env = Environment::new();
    env.set("x", "y".repeat(1 << 20));
    let options = ShellOptions::default();
    let ignore = Ignore::default();
    for line in [
        alloc::format!("cmd x{{1..4000}}{}", "y".repeat(1000)),
        String::from("{1..100000}"),
        String::from("cmd $x$x"),
    ] {
        let words = words(&line);
        let mut budget = Budget::new(MAX_EXPANSION_BYTES);
        let (fields, metering) =
            metered(|| Expander::new(&env, &options, &fs, &ignore).fields(&words, &mut budget));
        assert!(fields.is_ok(), "{line}");
        let charged = MAX_EXPANSION_BYTES - budget.remaining();
        assert!(
            metering.peak <= charged + (64 << 10),
            "{line}: {} held for {charged} charged",
            metering.peak
        );
    }
}

/// The line once refused only after holding eight times the bound is
/// refused holding no more than the bound.
#[test]
fn a_line_past_the_bound_is_refused_holding_no_more_than_it() {
    let fs = tree();
    let line = alloc::format!("cmd x{{1..4000}}{}", "y".repeat(4070));
    let host = ScriptedHost::new();
    let console = RecordingConsole::new();
    let mut shell = Shell::new(&host, &console).with_filesystem(&fs);
    let (refused, metering) = metered(|| shell.run_line(&line));
    assert_eq!(refused, Err(ParseError::ExpansionTooLarge));
    assert!(
        metering.peak <= MAX_EXPANSION_BYTES + (1 << 20),
        "{} bytes held",
        metering.peak
    );
    assert!(host.launches().is_empty());
}

/// Memory refused part way through an expansion fails the line, and the
/// session goes on.
#[test]
fn memory_refused_mid_expansion_fails_only_the_line() {
    let fs = tree();
    let host = ScriptedHost::new();
    let console = RecordingConsole::new();
    let mut shell = Shell::new(&host, &console).with_filesystem(&fs);
    shell
        .run_line(&alloc::format!("y={}", "y".repeat(1 << 20)))
        .expect("assigns");
    for line in ["x=$y$y", "cmd x{1..4000}"] {
        assert_eq!(
            refusing_above(64 << 10, || shell.run_line(line)),
            Err(ParseError::OutOfMemory),
            "{line}"
        );
        assert_eq!(shell.environment().last_status(), 1, "{line}");
    }
    assert_eq!(shell.environment().get("x"), None);
    assert!(host.launches().is_empty());
    shell
        .run_line("x=$y$y")
        .expect("assigns once memory allows it");
    assert_eq!(shell.environment().get("x").map(str::len), Some(2 << 20));
}
