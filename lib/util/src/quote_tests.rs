use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{quote, quote_if_needed, Quoted, Style};

fn styled(text: &str, style: Style) -> String {
    format!("{}", Quoted::new(text, style))
}

fn hidden(text: &str, style: Style) -> String {
    format!("{}", Quoted::new(text, style).hiding())
}

/// Each pair as GNU coreutils 9.4's `printf %q` writes it in `C.UTF-8`.
const GNU_SHELL_ESCAPE: &[(&str, &str)] = &[
    ("plain", "plain"),
    ("a b", "'a b'"),
    ("it's", "\"it's\""),
    ("it's a", "\"it's a\""),
    ("it's here!", "'it'\\''s here!'"),
    ("a\"b", "'a\"b'"),
    ("a$b", "'a$b'"),
    ("a\nb", "'a'$'\\n''b'"),
    ("\n\tx", "''$'\\n\\t''x'"),
    ("x\r\n", "'x'$'\\r\\n'"),
    ("é.txt", "é.txt"),
    ("\u{9b}31m", "''$'\\302\\233''31m'"),
    ("\u{1b}[31mred", "''$'\\033''[31mred'"),
    ("~home", "'~home'"),
    ("a~b", "a~b"),
    ("#hash", "'#hash'"),
    ("a#b", "a#b"),
    ("{", "'{'"),
    ("}", "'}'"),
    ("{a}", "{a}"),
    ("a{b", "a{b"),
    ("x]", "x]"),
    ("-dash", "-dash"),
    ("a'b\"c", "'a'\\''b\"c'"),
    ("a*b", "'a*b'"),
    ("[x]", "'[x]'"),
    ("a=b", "'a=b'"),
    ("a\\b", "'a\\b'"),
    ("\u{7f}", "''$'\\177'"),
    ("a,b%c+d@e:f", "a,b%c+d@e:f"),
    ("nb\u{a0}sp", "nb\u{a0}sp"),
    ("\t", "''$'\\t'"),
    ("'", "\"'\""),
    ("''", "\"''\""),
    ("a'b'c", "\"a'b'c\""),
    ("a'b*", "'a'\\''b*'"),
    ("it's\ttab", "'it'\\''s'$'\\t''tab'"),
    ("it's\nX", "'it'\\''s'$'\\n''X'"),
    ("\nit's", "''$'\\n''it'\\''s'"),
    ("a'\tb", "'a'\\'''$'\\t''b'"),
    ("a\tb'", "'a'$'\\t''b'\\'''"),
    ("\t'", "''$'\\t'\\'''"),
    ("'\t", "''\\'''$'\\t'"),
    ("'a\n", "''\\''a'$'\\n'"),
    ("$'", "'$'\\'''"),
    ("'$", "''\\''$'"),
    ("\u{85}nel", "''$'\\302\\205''nel'"),
    ("ab\u{2028}cd", "'ab'$'\\342\\200\\250''cd'"),
    ("{B'", "'{B'\\'''"),
    ("+F'{P", "'+F'\\''{P'"),
    ("d~'FhC", "'d~'\\''FhC'"),
    ("#oJJaI'", "\"#oJJaI'\""),
    ("~a'", "\"~a'\""),
    ("'#", "''\\''#'"),
    ("é'#", "'é'\\''#'"),
    ("a'é", "\"a'é\""),
    ("a'\u{a0}b", "\"a'\u{a0}b\""),
    ("", "''"),
];

#[test]
fn shell_escape_quotes_as_gnu_does() {
    for &(text, expected) in GNU_SHELL_ESCAPE {
        assert_eq!(format!("{}", quote_if_needed(text)), expected, "{text:?}");
    }
}

#[test]
fn a_message_always_quotes_its_name() {
    assert_eq!(format!("{}", quote("plain")), "'plain'");
    assert_eq!(format!("{}", quote("é.txt")), "'é.txt'");
    assert_eq!(format!("{}", quote("a b")), "'a b'");
    assert_eq!(format!("{}", quote("it's")), "\"it's\"");
    assert_eq!(format!("{}", quote("a\nb")), "'a'$'\\n''b'");
    assert_eq!(format!("{}", quote("")), "''");
    for &(text, expected) in GNU_SHELL_ESCAPE {
        let always = format!("{}", quote(text));
        let quoted = expected.starts_with(['\'', '"']);
        assert!(
            if quoted {
                always == expected
            } else {
                always == format!("'{expected}'")
            },
            "{text:?} became {always:?}"
        );
    }
}

#[test]
fn a_name_that_reorders_or_hides_text_is_escaped_where_gnu_leaves_it_raw() {
    assert_eq!(
        format!("{}", quote_if_needed("rlo\u{202e}exe")),
        "'rlo'$'\\342\\200\\256''exe'"
    );
    assert_eq!(
        format!("{}", quote_if_needed("zw\u{200b}sp")),
        "'zw'$'\\342\\200\\213''sp'"
    );
    assert_eq!(styled("soft\u{ad}hy", Style::C), "\"soft\\302\\255hy\"");
}

#[test]
fn a_quote_and_a_closing_escape_take_the_minimal_form_gnu_does_not() {
    // GNU 9.4 writes `'''it'\''s'$'\n'` and `'\n''a'\'''$'\n'`; the second
    // reads back as a backslash and an `n`.
    assert_eq!(format!("{}", quote_if_needed("it's\n")), "'it'\\''s'$'\\n'");
    assert_eq!(
        format!("{}", quote_if_needed("\na'\n")),
        "''$'\\n''a'\\'''$'\\n'"
    );
}

#[test]
fn the_unescaped_shell_styles_quote_only_what_a_shell_splits() {
    assert_eq!(styled("n\nx", Style::Shell), "'n\nx'");
    assert_eq!(styled("t\tx", Style::Shell), "'t\tx'");
    assert_eq!(styled("r\rx", Style::Shell), "'r\rx'");
    assert_eq!(styled("e\u{1b}x", Style::Shell), "e\u{1b}x");
    assert_eq!(styled("c\u{9b}x", Style::Shell), "c\u{9b}x");
    assert_eq!(styled("plain", Style::ShellAlways), "'plain'");
    assert_eq!(styled("it's\n", Style::Shell), "'it'\\''s\n'");
}

#[test]
fn hiding_shows_each_hidden_character_as_one_question_mark() {
    assert_eq!(hidden("n\nx", Style::Shell), "'n?x'");
    assert_eq!(hidden("c\u{9b}x", Style::Shell), "c?x");
    assert_eq!(hidden("e\u{1b}[1m", Style::Shell), "'e?[1m'");
    assert_eq!(hidden("ls\u{2028}x", Style::Literal), "ls?x");
    assert_eq!(hidden("d\u{7f}", Style::Literal), "d?");
    assert_eq!(hidden("it's\u{9b}", Style::Shell), "\"it's?\"");
    // The escaping styles escape, whatever hiding asks.
    assert_eq!(hidden("a\nb", Style::ShellEscape), "'a'$'\\n''b'");
    assert_eq!(hidden("a\nb", Style::C), "\"a\\nb\"");
}

#[test]
fn the_literal_style_writes_the_name_as_it_is() {
    assert_eq!(styled("a\nb \u{1b}", Style::Literal), "a\nb \u{1b}");
}

#[test]
fn the_c_styles_escape_as_gnu_does() {
    let cases: &[(&str, &str, &str)] = &[
        ("a\"b", "\"a\\\"b\"", "a\"b"),
        ("a\\b", "\"a\\\\b\"", "a\\\\b"),
        ("c1\u{9b}x", "\"c1\\302\\233x\"", "c1\\302\\233x"),
        ("del\u{7f}", "\"del\\177\"", "del\\177"),
        ("esc\u{1b}[1m", "\"esc\\033[1m\"", "esc\\033[1m"),
        ("it's", "\"it's\"", "it's"),
        (
            "ls\u{2028}x",
            "\"ls\\342\\200\\250x\"",
            "ls\\342\\200\\250x",
        ),
        ("q??=x", "\"q??=x\"", "q??=x"),
        ("sp ace", "\"sp ace\"", "sp ace"),
        ("é.txt", "\"é.txt\"", "é.txt"),
        ("'\t", "\"'\\t\"", "'\\t"),
        ("a\n", "\"a\\n\"", "a\\n"),
    ];
    for &(text, c, escape) in cases {
        assert_eq!(styled(text, Style::C), c, "{text:?}");
        assert_eq!(styled(text, Style::Escape), escape, "{text:?}");
    }
}

/// A mark takes a backslash in the C styles and forces the shell styles to
/// quote, beside what each style escapes anyway.
#[test]
fn each_marked_character_is_escaped_in_the_c_styles() {
    let marks = " *=>@|";
    for (text, c, escape) in [
        ("a b", "\"a\\ b\"", "a\\ b"),
        ("a=b|c", "\"a\\=b\\|c\"", "a\\=b\\|c"),
        ("a\\*", "\"a\\\\\\*\"", "a\\\\\\*"),
        ("a:b", "\"a:b\"", "a:b"),
    ] {
        assert_eq!(format!("{}", Quoted::new(text, Style::C).marking(marks)), c);
        assert_eq!(
            format!("{}", Quoted::new(text, Style::Escape).marking(marks)),
            escape
        );
    }
    assert_eq!(
        format!("{}", Quoted::new("a@b", Style::ShellEscape).marking(marks)),
        "'a@b'"
    );
}

/// `ls -R` headings as GNU coreutils 9.4 writes them in `C.UTF-8`, in the
/// styles `literal`, `shell`, `shell-always`, `shell-escape`, `c` and `escape`.
#[test]
fn a_marked_character_is_quoted_or_escaped_as_gnu_does() {
    let cases: &[(&str, [&str; 6])] = &[
        (
            "./a:b",
            [
                "./a:b",
                "'./a:b'",
                "'./a:b'",
                "'./a:b'",
                "\"./a\\:b\"",
                "./a\\:b",
            ],
        ),
        (
            "./a b",
            [
                "./a b",
                "'./a b'",
                "'./a b'",
                "'./a b'",
                "\"./a b\"",
                "./a b",
            ],
        ),
        (
            "./h\u{1b}i",
            [
                "./h\u{1b}i",
                "./h\u{1b}i",
                "'./h\u{1b}i'",
                "'./h'$'\\033''i'",
                "\"./h\\033i\"",
                "./h\\033i",
            ],
        ),
        (
            "./plain",
            [
                "./plain",
                "./plain",
                "'./plain'",
                "./plain",
                "\"./plain\"",
                "./plain",
            ],
        ),
    ];
    let styles = [
        Style::Literal,
        Style::Shell,
        Style::ShellAlways,
        Style::ShellEscape,
        Style::C,
        Style::Escape,
    ];
    for &(text, expected) in cases {
        for (style, want) in styles.into_iter().zip(expected) {
            let got = format!("{}", Quoted::new(text, style).marking(":"));
            assert_eq!(got, want, "{text:?} in {style:?}");
        }
    }
}

/// GNU's `shell-always` heading for `./e'f:g` is `"./e'f\:g"`, which a shell
/// reads back with the backslash.
#[test]
fn a_marked_character_inside_double_quotes_reads_back() {
    for style in [Style::Shell, Style::ShellAlways, Style::ShellEscape] {
        let got = format!("{}", Quoted::new("./e'f:g", style).marking(":"));
        assert_eq!(got, "\"./e'f:g\"", "{style:?}");
        assert_eq!(read_back(&got).as_deref(), Some("./e'f:g"));
    }
}

/// A shell word read back: `'…'`, `"…"` (holding only what a double-quoted
/// form may), `$'…'` with C escapes and octal bytes, and a bare run that a
/// shell takes literally. `None` for anything else, a bare character a shell
/// would act on among them.
fn read_back(word: &str) -> Option<String> {
    let bytes = word.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\'' => {
                let end = at + 1 + word[at + 1..].find('\'')?;
                out.extend_from_slice(&bytes[at + 1..end]);
                at = end + 1;
            }
            b'"' => {
                let end = at + 1 + word[at + 1..].find('"')?;
                let inner = &bytes[at + 1..end];
                if inner.iter().any(|b| matches!(b, b'$' | b'`' | b'\\')) {
                    return None;
                }
                out.extend_from_slice(inner);
                at = end + 1;
            }
            b'$' if bytes.get(at + 1) == Some(&b'\'') => {
                at += 2;
                loop {
                    match *bytes.get(at)? {
                        b'\'' => break,
                        b'\\' => {
                            at += 1;
                            let letter = *bytes.get(at)?;
                            let byte = match letter {
                                b'a' => 7,
                                b'b' => 8,
                                b't' => b'\t',
                                b'n' => b'\n',
                                b'v' => 11,
                                b'f' => 12,
                                b'r' => b'\r',
                                b'0'..=b'7' => {
                                    let digits = bytes.get(at..at + 3)?;
                                    let mut value = 0u16;
                                    for &digit in digits {
                                        if !(b'0'..=b'7').contains(&digit) {
                                            return None;
                                        }
                                        value = value * 8 + u16::from(digit - b'0');
                                    }
                                    at += 2;
                                    u8::try_from(value).ok()?
                                }
                                _ => return None,
                            };
                            out.push(byte);
                            at += 1;
                        }
                        other => {
                            out.push(other);
                            at += 1;
                        }
                    }
                }
                at += 1;
            }
            b'\\' => {
                out.push(*bytes.get(at + 1)?);
                at += 2;
            }
            other => {
                let first = at == 0;
                let alone = bytes.len() == 1;
                let acts = match other {
                    b' ' | b'\t' | b'\n' | b'\r' | b'!' | b'"' | b'$' | b'&' | b'(' | b')'
                    | b'*' | b';' | b'<' | b'=' | b'>' | b'?' | b'[' | b'^' | b'`' | b'|' => true,
                    b'#' | b'~' => first,
                    b'{' | b'}' => alone,
                    _ => false,
                };
                if acts {
                    return None;
                }
                out.push(other);
                at += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[test]
fn every_short_name_reads_back_from_both_escaping_styles() {
    const ALPHABET: [char; 14] = [
        'a', ' ', '\'', '"', '$', '\\', '!', '~', '#', '{', '\n', '\u{1b}', '\u{9b}', '\u{202e}',
    ];
    let mut names = Vec::from([String::new()]);
    let mut frontier = names.clone();
    for _ in 0..3 {
        let mut longer = Vec::new();
        for name in &frontier {
            for ch in ALPHABET {
                let mut next = name.clone();
                next.push(ch);
                longer.push(next);
            }
        }
        names.extend(longer.iter().cloned());
        frontier = longer;
    }
    for name in &names {
        for style in [Style::ShellEscape, Style::ShellEscapeAlways] {
            let quoted = styled(name, style);
            assert_eq!(
                read_back(&quoted).as_deref(),
                Some(name.as_str()),
                "{name:?} as {quoted:?}"
            );
            assert!(
                !quoted.chars().any(crate::utf8::is_hidden),
                "{name:?} as {quoted:?} shows a hidden character"
            );
        }
    }
}
