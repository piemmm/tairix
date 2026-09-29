//! Unit tests for the format set, every lexer, detection and validation.

extern crate std;

use alloc::string::String;
use alloc::vec::Vec;

use tairix_theme::SyntaxRole::{self, *};

use crate::{
    format_for_head, lex_line, store_for_name, validate, Format, LineState, Severity, Span,
    MAX_LEX_LINE,
};

/// Every span of `line`, as its text and role, checked against the output
/// contract on the way.
fn classify(
    format: Format,
    state: LineState,
    line: &str,
) -> (Vec<(String, SyntaxRole)>, LineState) {
    let mut spans = Vec::new();
    let next = lex_line(format, state, line.as_bytes(), &mut spans);
    assert_contract(&spans, line.len());
    let pairs = spans
        .iter()
        .map(|span| {
            (
                String::from(&line[span.start as usize..span.end as usize]),
                span.role,
            )
        })
        .collect();
    (pairs, next)
}

/// The spans of each line of `text`, the state carried from line to line.
fn document(format: Format, text: &str) -> Vec<Vec<(String, SyntaxRole)>> {
    let mut state = LineState::START;
    text.lines()
        .map(|line| {
            let (pairs, next) = classify(format, state, line);
            state = next;
            pairs
        })
        .collect()
}

/// The spans of one line from the start of a document.
fn line(format: Format, text: &str) -> Vec<(String, SyntaxRole)> {
    classify(format, LineState::START, text).0
}

/// Whether `pairs` holds `text` classified as `role`.
fn has(pairs: &[(String, SyntaxRole)], text: &str, role: SyntaxRole) -> bool {
    pairs.iter().any(|(t, r)| t == text && *r == role)
}

/// The role `text` was given, if a span is exactly it.
fn role_of(pairs: &[(String, SyntaxRole)], text: &str) -> Option<SyntaxRole> {
    pairs.iter().find(|(t, _)| t == text).map(|(_, r)| *r)
}

fn assert_contract(spans: &[Span], len: usize) {
    let mut last = 0;
    for span in spans {
        assert!(span.start < span.end, "empty span {span:?}");
        assert!(span.start >= last, "overlap at {span:?}");
        assert!(
            span.end as usize <= len.min(MAX_LEX_LINE),
            "past the line: {span:?}"
        );
        assert_ne!(span.role, Plain, "a plain span is a gap, never emitted");
        last = span.end;
    }
}

#[test]
fn the_format_set_is_closed_and_its_indices_round_trip() {
    for (at, format) in Format::ALL.iter().enumerate() {
        assert_eq!(usize::from(format.index()), at);
        assert_eq!(Format::from_index(format.index()), Some(*format));
    }
    let past = u8::try_from(Format::COUNT).expect("the set fits a byte");
    assert_eq!(Format::from_index(past), None);
    let mut labels: Vec<&str> = Format::ALL.iter().map(|f| f.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), Format::COUNT, "every label names one format");
    assert!(Format::SystemConfig.is_store() && !Format::Rust.is_store());
    assert_eq!(Format::Rust.line_comment(), Some("//"));
    assert_eq!(Format::Python.line_comment(), Some("#"));
    assert_eq!(Format::Html.line_comment(), None);
}

#[test]
fn a_word_of_every_table_is_found() {
    // A word the binary search misses would come back plain.
    for (format, word, role) in [
        (Format::Rust, "where", Keyword),
        (Format::Rust, "usize", Type),
        (Format::C, "_Static_assert", Keyword),
        (Format::C, "uintptr_t", Type),
        (Format::Java, "yield", Keyword),
        (Format::Java, "boolean", Type),
        (Format::JavaScript, "undefined", Keyword),
        (Format::Python, "yield", Keyword),
        (Format::Python, "tuple", Type),
        (Format::Shell, "while", Keyword),
        (Format::Java, "non-sealed", Keyword),
    ] {
        assert_eq!(
            role_of(&line(format, word), word),
            Some(role),
            "{format:?} {word}"
        );
    }
    let subtraction = line(Format::Java, "non-x - sealed");
    assert_eq!(
        role_of(&subtraction, "non"),
        None,
        "a hyphen joins only a keyword: {subtraction:?}"
    );
    assert_eq!(role_of(&subtraction, "sealed"), Some(Keyword));
    assert_eq!(
        role_of(&line(Format::C, "non-sealed"), "non-sealed"),
        None,
        "only Java joins"
    );
}

#[test]
fn rust_classifies_words_literals_and_calls() {
    let pairs = line(
        Format::Rust,
        "pub fn main() -> u32 { let x: Vec<u8> = vec![0x1F_u8]; 1.5e-3 }",
    );
    assert!(has(&pairs, "pub", Keyword));
    assert!(has(&pairs, "fn", Keyword));
    assert!(has(&pairs, "main", Function));
    assert!(has(&pairs, "u32", Type));
    assert!(has(&pairs, "Vec", Type));
    assert!(has(&pairs, "vec!", Function));
    assert!(has(&pairs, "0x1F_u8", Number));
    assert!(has(&pairs, "1.5e-3", Number));
    let range = line(Format::Rust, "for i in 0..10 {}");
    assert!(
        has(&range, "0", Number) && has(&range, "10", Number),
        "{range:?}"
    );
}

#[test]
fn rust_tells_lifetimes_from_characters() {
    let pairs = line(
        Format::Rust,
        "fn f<'a>(s: &'a str) -> char { 'x' } let q = '\\'';",
    );
    assert!(has(&pairs, "'a", Type));
    assert!(has(&pairs, "'x'", String));
    assert!(pairs
        .iter()
        .any(|(t, r)| t.contains("\\'") && *r == Escape || t == "'\\''"));
}

#[test]
fn rust_strings_escapes_and_raw_strings_span_lines() {
    let pairs = line(Format::Rust, r#"let s = "a\nb";"#);
    assert!(has(&pairs, "\"a", String) && has(&pairs, "\\n", Escape) && has(&pairs, "b\"", String));
    let lines = document(Format::Rust, "let r = r#\"one\n\"two\"\nthree\"# + 1;");
    assert!(has(&lines[0], "r#\"one", String), "{:?}", lines[0]);
    assert!(
        has(&lines[1], "\"two\"", String),
        "a lone quote does not close r#: {:?}",
        lines[1]
    );
    assert!(has(&lines[2], "three\"#", String));
    assert!(
        has(&lines[2], "1", Number),
        "code resumes after the close: {:?}",
        lines[2]
    );
}

#[test]
fn rust_block_comments_nest_across_lines() {
    let lines = document(Format::Rust, "a /* one /* two */\nstill */ b\nc");
    assert!(has(&lines[0], "/* one /* two */", Comment));
    assert!(has(&lines[1], "still */", Comment));
    assert!(
        !lines[2].iter().any(|(_, r)| *r == Comment),
        "{:?}",
        lines[2]
    );
}

#[test]
fn rust_attributes_are_directives() {
    let pairs = line(Format::Rust, "#[derive(Debug, Clone)] struct S;");
    assert!(has(&pairs, "#[derive(Debug, Clone)]", Directive));
    assert!(has(&pairs, "struct", Keyword));
}

#[test]
fn c_preprocessor_lines_continue_with_a_backslash() {
    let lines = document(
        Format::C,
        "#define MAX(a, b) \\\n  ((a) > (b))\nint x = 'q';",
    );
    assert!(has(&lines[0], "#define MAX(a, b) \\", Directive));
    assert!(has(&lines[1], "  ((a) > (b))", Directive));
    assert!(has(&lines[2], "int", Type));
    assert!(has(&lines[2], "'q'", String));
}

#[test]
fn a_c_string_ends_at_the_line_without_a_continuation() {
    let lines = document(Format::C, "char *s = \"open\nint y;");
    assert!(has(&lines[1], "int", Type), "{:?}", lines[1]);
}

#[test]
fn java_text_blocks_span_lines() {
    let lines = document(Format::Java, "String s = \"\"\"\n  body\n  \"\"\"; int n;");
    assert!(has(&lines[1], "  body", String));
    assert!(has(&lines[2], "  \"\"\"", String));
    assert!(has(&lines[2], "int", Type));
}

#[test]
fn javascript_templates_span_lines_and_single_quotes_are_strings() {
    let lines = document(Format::JavaScript, "const t = `a\nb` + 'c' + undefined;");
    assert!(has(&lines[0], "`a", String));
    assert!(has(&lines[1], "b`", String));
    assert!(has(&lines[1], "'c'", String), "{:?}", lines[1]);
    assert!(has(&lines[1], "undefined", Keyword));
    assert!(has(
        &line(Format::JavaScript, "fetch(url)"),
        "fetch",
        Function
    ));
}

#[test]
fn json_tells_keys_from_values_and_refuses_what_it_cannot_hold() {
    let pairs = line(
        Format::Json,
        r#"{"name": "edit", "n": -1.5e3, "ok": true, "x": null}"#,
    );
    assert!(has(&pairs, "\"name\"", Key));
    assert!(has(&pairs, "\"edit\"", String));
    assert!(has(&pairs, "-1.5e3", Number));
    assert!(has(&pairs, "true", Keyword) && has(&pairs, "null", Keyword));
    assert!(has(
        &line(Format::Json, "// no comments"),
        "// no comments",
        Error
    ));
    assert!(has(&line(Format::Json, "{bare: 1}"), "bare", Error));
}

#[test]
fn yaml_keys_lists_comments_and_block_scalars() {
    let lines = document(
        Format::Yaml,
        "---\nname: edit # the app\nlist:\n  - one\n  - key: 2\ntext: |\n  kept\n  as is\nnext: yes\n",
    );
    assert!(has(&lines[0], "---", Directive));
    assert!(has(&lines[1], "name", Key) && has(&lines[1], "# the app", Comment));
    assert!(has(&lines[4], "key", Key) && has(&lines[4], "2", Number));
    assert!(has(&lines[6], "kept", String) && has(&lines[7], "as is", String));
    assert!(
        has(&lines[8], "next", Key) && has(&lines[8], "yes", Keyword),
        "{:?}",
        lines[8]
    );
}

#[test]
fn toml_tables_keys_values_and_multiline_strings() {
    let lines = document(
        Format::Toml,
        "[package]\nname = \"tairix\" # why\nversion.workspace = true\nwhen = 1979-05-27T07:32:00Z\ntext = \"\"\"\nbody\n\"\"\"\n[[bin]]",
    );
    assert!(has(&lines[0], "[package]", Directive));
    assert!(
        has(&lines[1], "name", Key)
            && has(&lines[1], "\"tairix\"", String)
            && has(&lines[1], "# why", Comment)
    );
    assert!(
        has(&lines[2], "version", Key)
            && has(&lines[2], "workspace", Key)
            && has(&lines[2], "true", Keyword)
    );
    assert!(
        has(&lines[3], "1979-05-27T07:32:00Z", Number),
        "{:?}",
        lines[3]
    );
    assert!(has(&lines[5], "body", String));
    assert!(has(&lines[7], "[[bin]]", Directive));
}

#[test]
fn python_definitions_decorators_and_triple_strings() {
    let lines = document(
        Format::Python,
        "@property\ndef area(self) -> float:\n    \"\"\"Doc\n    more\"\"\"\n    return rb'\\x00' + f\"{x}\"  # note\nclass Shape: pass",
    );
    assert!(has(&lines[0], "@property", Directive));
    assert!(
        has(&lines[1], "def", Keyword)
            && has(&lines[1], "area", Function)
            && has(&lines[1], "float", Type)
    );
    assert!(has(&lines[3], "    more\"\"\"", String));
    assert!(has(&lines[4], "return", Keyword));
    assert!(
        lines[4]
            .iter()
            .any(|(t, r)| t.starts_with("rb'") && *r == String),
        "{:?}",
        lines[4]
    );
    assert!(has(&lines[4], "# note", Comment));
    assert!(has(&lines[5], "Shape", Type));
}

#[test]
fn shell_words_expansions_comments_and_here_documents() {
    let lines = document(
        Format::Shell,
        "#!/bin/sh\nname=value # set\nif [ -n \"$HOME\" ]; then echo ${x:-y}; fi\ncat <<-EOF | grep a#b\n\tbody $not\n\tEOF\necho done",
    );
    assert!(has(&lines[0], "#!/bin/sh", Directive));
    assert!(has(&lines[1], "name", Attribute) && has(&lines[1], "# set", Comment));
    assert!(has(&lines[2], "if", Keyword) && has(&lines[2], "$HOME", Attribute));
    assert!(has(&lines[2], "echo", Function) && has(&lines[2], "${x:-y}", Attribute));
    assert!(has(&lines[3], "EOF", Directive));
    assert!(
        !lines[3].iter().any(|(_, r)| *r == Comment),
        "a # inside a word is not a comment"
    );
    assert!(has(&lines[4], "\tbody $not", String), "{:?}", lines[4]);
    assert!(has(&lines[5], "\tEOF", Directive));
    assert!(has(&lines[6], "echo", Function));
}

#[test]
fn html_tags_attributes_entities_and_comments() {
    let lines = document(
        Format::Html,
        "<!DOCTYPE html>\n<a href=\"/x\" class=big>&amp; &#x41; &nope</a>\n<!-- one\ntwo -->",
    );
    assert!(has(&lines[0], "<!DOCTYPE html>", Directive));
    assert!(has(&lines[1], "a", Tag) && has(&lines[1], "href", Attribute));
    assert!(has(&lines[1], "\"/x\"", String) && has(&lines[1], "big", String));
    assert!(has(&lines[1], "&amp;", Escape) && has(&lines[1], "&#x41;", Escape));
    assert!(
        !lines[1].iter().any(|(t, _)| t.contains("nope")),
        "a bare & is text"
    );
    assert!(has(&lines[2], "<!-- one", Comment) && has(&lines[3], "two -->", Comment));
}

#[test]
fn html_script_and_style_bodies_are_lexed_in_their_own_language_across_lines() {
    let lines = document(
        Format::Html,
        "<script type=\"module\">\nlet s = `a\nb`; // </b>\n</script><style>\np { color: #fff; }\n</style>",
    );
    assert!(has(&lines[1], "let", Keyword) && has(&lines[1], "`a", String));
    assert!(
        has(&lines[2], "b`", String),
        "the template carried: {:?}",
        lines[2]
    );
    assert!(has(&lines[2], "// </b>", Comment));
    assert!(has(&lines[3], "script", Tag) && has(&lines[3], "style", Tag));
    assert!(
        has(&lines[4], "p", Tag)
            && has(&lines[4], "color", Attribute)
            && has(&lines[4], "#fff", Number)
    );
    assert!(has(&lines[5], "style", Tag));
}

#[test]
fn xml_prolog_cdata_and_no_embedding() {
    let lines = document(
        Format::Xml,
        "<?xml version=\"1.0\"?>\n<svg:g><![CDATA[x <y>\nz]]></svg:g>\n<script>let a</script>",
    );
    assert!(has(&lines[0], "<?xml version=\"1.0\"?>", Directive));
    assert!(has(&lines[1], "svg:g", Tag) && has(&lines[1], "x <y>", String));
    assert!(has(&lines[2], "z", String) && has(&lines[2], "]]>", Directive));
    assert!(
        !has(&lines[3], "let", Keyword),
        "XML embeds no script: {:?}",
        lines[3]
    );
}

#[test]
fn css_selectors_properties_values_and_media_nesting() {
    let lines = document(
        Format::Css,
        "@media (min-width: 40em) {\n  .a > h1:hover, #id { margin: 0 auto !important; }\n}\n/* note\n */ p { width: calc(100% - 2px); background: url(x.png) }",
    );
    assert!(has(&lines[0], "@media", Directive));
    assert!(
        has(&lines[1], ".a", Attribute)
            && has(&lines[1], "h1", Tag)
            && has(&lines[1], ":hover", Keyword)
    );
    assert!(
        has(&lines[1], "margin", Attribute)
            && has(&lines[1], "0", Number)
            && has(&lines[1], "!important", Keyword)
    );
    assert!(has(&lines[3], "/* note", Comment));
    assert!(has(&lines[4], " */", Comment) && has(&lines[4], "p", Tag));
    assert!(
        has(&lines[4], "calc", Function)
            && has(&lines[4], "100%", Number)
            && has(&lines[4], "2px", Number)
    );
    assert!(has(&lines[4], "x.png", String), "{:?}", lines[4]);
}

#[test]
fn a_vendor_prefixed_group_rule_holds_rules() {
    let lines = document(
        Format::Css,
        "@-webkit-keyframes spin {\n  from { color: red }\n}",
    );
    assert!(
        has(&lines[0], "@-webkit-keyframes", Directive),
        "{:?}",
        lines[0]
    );
    assert!(
        has(&lines[1], "from", Tag),
        "a keyframe selector: {:?}",
        lines[1]
    );
    assert!(has(&lines[1], "color", Attribute));
}

#[test]
fn markdown_blocks_and_inlines() {
    let lines = document(
        Format::Markdown,
        "# Title\n- item with `code` and **bold** and [a link](http://x)\n```rust\nlet x = 1;\n```\n---\nsnake_case_name stays plain\n> quoted \\*",
    );
    assert!(has(&lines[0], "# Title", Heading));
    assert!(has(&lines[1], "- ", Punctuation) && has(&lines[1], "`code`", Code));
    assert!(has(&lines[1], "**bold**", Emphasis) && has(&lines[1], "[a link](http://x)", Link));
    assert!(
        has(&lines[2], "```rust", Code)
            && has(&lines[3], "let x = 1;", Code)
            && has(&lines[4], "```", Code)
    );
    assert!(has(&lines[5], "---", Punctuation));
    assert!(
        lines[6].is_empty(),
        "underscores inside a word are not emphasis: {:?}",
        lines[6]
    );
    assert!(has(&lines[7], ">", Punctuation) && has(&lines[7], "\\*", Escape));
}

#[test]
fn an_unclosed_braced_escape_never_swallows_its_quote() {
    let pairs = line(Format::Rust, r#"let s = "\u{" + x;"#);
    assert!(has(&pairs, "\\u{", Escape), "{pairs:?}");
    assert_eq!(role_of(&pairs, "+"), Some(Punctuation), "{pairs:?}");
    assert!(has(
        &line(Format::Rust, r#""\u{1_F600}""#),
        "\\u{1_F600}",
        Escape
    ));
    let long = line(Format::JavaScript, r#""\u{1234567}""#);
    assert!(
        has(&long, "\\u{123456", Escape),
        "six digits at most: {long:?}"
    );
}

#[test]
fn a_code_span_closes_only_at_a_run_of_its_own_length() {
    assert!(has(&line(Format::Markdown, "a `b``c` d"), "`b``c`", Code));
    let unclosed = line(Format::Markdown, "``a` b");
    assert!(
        unclosed.iter().all(|(_, role)| *role != Code),
        "{unclosed:?}"
    );
    let longest = "`".repeat(32);
    assert!(has(
        &line(Format::Markdown, &alloc::format!("{longest}x{longest}")),
        &alloc::format!("{longest}x{longest}"),
        Code
    ));
    let longer = "`".repeat(33);
    let text = line(Format::Markdown, &alloc::format!("{longer}x{longer}"));
    assert!(
        text.iter().all(|(_, role)| *role != Code),
        "past the bound is text"
    );
}

#[test]
fn autolinks_tags_and_links_close_where_they_should() {
    let pairs = line(Format::Markdown, "<http://x> <b class=\"k\"> < c> [t](u");
    assert!(has(&pairs, "<http://x>", Link) && has(&pairs, "<b class=\"k\">", Tag));
    assert!(
        has(&pairs, "[t]", Link),
        "an unclosed target leaves the text: {pairs:?}"
    );
    assert!(!pairs.iter().any(|(text, _)| text.starts_with("< c")));
}

#[test]
fn a_shell_assignment_value_is_never_a_command_or_a_keyword() {
    let pairs = line(Format::Shell, "a=if b=2 echo $x");
    assert!(has(&pairs, "a", Attribute) && has(&pairs, "b", Attribute));
    assert_eq!(
        role_of(&pairs, "if"),
        None,
        "a value, not a keyword: {pairs:?}"
    );
    assert!(has(&pairs, "2", Number));
    assert!(
        has(&pairs, "echo", Function),
        "the command after assignments"
    );
    let expanded = line(Format::Shell, "x=$HOME echo; $(printf y)");
    assert!(has(&expanded, "echo", Function), "{expanded:?}");
    assert!(has(&expanded, "printf", Function), "`$(` opens a command");
    let chain = line(Format::Shell, "a=b=c");
    assert!(has(&chain, "a", Attribute) && role_of(&chain, "b=c").is_none());
}

#[test]
fn a_toml_dotted_key_is_decided_for_the_whole_chain() {
    let keyed = line(Format::Toml, "a.\"b c\".d = 1");
    assert!(has(&keyed, "a", Key) && has(&keyed, "\"b c\"", Key) && has(&keyed, "d", Key));
    let value = line(Format::Toml, "x = a.b");
    assert!(
        has(&value, "x", Key) && role_of(&value, "b").is_none(),
        "{value:?}"
    );
    let inline = line(Format::Toml, "t = { a.b = 1, c = 2 }");
    assert!(has(&inline, "a", Key) && has(&inline, "b", Key) && has(&inline, "c", Key));
}

#[test]
fn a_directive_opens_only_at_the_start_of_its_line() {
    assert!(has(
        &line(Format::C, "  #define X"),
        "  #define X",
        Directive
    ));
    let later = line(Format::C, "  x # y");
    assert!(
        later.iter().all(|(_, role)| *role != Directive),
        "{later:?}"
    );
    assert!(has(&line(Format::Python, "  @dec"), "@dec", Directive));
    let infix = line(Format::Python, "a @ b");
    assert!(
        infix.iter().all(|(_, role)| *role != Directive),
        "{infix:?}"
    );
}

#[test]
fn lines_built_to_rescan_themselves_lex_as_they_should() {
    let half = MAX_LEX_LINE / 2;
    let brackets = "[".repeat(MAX_LEX_LINE);
    assert!(line(Format::Markdown, &alloc::format!("a{brackets}")).is_empty());
    let mut falling = String::from("a");
    let mut run = 180;
    while run > 0 && falling.len() + run < MAX_LEX_LINE {
        falling.push_str(&"`".repeat(run));
        falling.push('x');
        run -= 1;
    }
    let spans = line(Format::Markdown, &falling);
    assert!(spans.iter().all(|(_, role)| *role != Code));
    let angles = alloc::format!("a{}>", "< ".repeat(half - 1));
    assert!(line(Format::Markdown, &angles).is_empty());
    let parens = alloc::format!("a{}", "[t](".repeat(MAX_LEX_LINE / 4));
    assert!(line(Format::Markdown, &parens)
        .iter()
        .all(|(_, role)| *role == Link));
    let stars = alloc::format!("a{}", "*".repeat(MAX_LEX_LINE - 1));
    line(Format::Markdown, &stars);

    let chain = line(Format::Shell, &"a=".repeat(half));
    assert_eq!(
        chain.iter().filter(|(_, role)| *role == Attribute).count(),
        1
    );
    let dotted = line(Format::Toml, &"a.".repeat(half));
    assert!(dotted.iter().all(|(_, role)| *role != Key));
    let keyed = line(
        Format::Toml,
        &alloc::format!("{}a = 1", "a.".repeat(half - 4)),
    );
    assert_eq!(
        keyed.iter().filter(|(_, role)| *role == Key).count(),
        half - 3,
        "every part of the chain is a key"
    );

    let hashes = alloc::format!("{}x{}", " ".repeat(half), "#".repeat(half - 1));
    assert!(line(Format::C, &hashes)
        .iter()
        .all(|(_, role)| *role != Directive));
    let ats = alloc::format!("{}x{}", " ".repeat(half), "@".repeat(half - 1));
    assert!(line(Format::Python, &ats)
        .iter()
        .all(|(_, role)| *role != Directive));
    let escapes = alloc::format!("\"{}", "\\u{".repeat(MAX_LEX_LINE / 3));
    assert!(line(Format::Rust, &escapes)
        .iter()
        .all(|(_, role)| matches!(role, String | Escape)));
}

#[test]
fn a_key_with_no_value_is_the_error_its_store_reports() {
    assert!(has(
        &line(Format::SystemConfig, "os.loginType"),
        "os.loginType",
        Error
    ));
    assert!(has(
        &line(Format::NetworkConfig, "wan.kind # none"),
        "wan.kind",
        Error
    ));
    assert!(has(
        &line(Format::ServiceOverrides, "timed"),
        "timed",
        Error
    ));
}

#[test]
fn the_system_store_colours_what_its_registry_admits() {
    let good = line(Format::SystemConfig, "os.loginType text # boot to text");
    assert!(
        has(&good, "os.loginType", Key)
            && has(&good, "text", Keyword)
            && has(&good, "# boot to text", Comment)
    );
    let bad = line(Format::SystemConfig, "os.loginType desktop");
    assert!(has(&bad, "desktop", Error));
    assert!(has(
        &line(Format::SystemConfig, "os.nothing x"),
        "os.nothing",
        Error
    ));
    assert!(has(
        &line(Format::SystemConfig, "time.servers a.example"),
        "a.example",
        String
    ));
}

#[test]
fn the_network_store_colours_interface_key_and_value() {
    let pairs = line(Format::NetworkConfig, "eth0.ipv4.method dhcp");
    assert!(
        has(&pairs, "eth0", Key)
            && has(&pairs, "ipv4.method", Attribute)
            && has(&pairs, "dhcp", Keyword)
    );
    assert!(has(
        &line(Format::NetworkConfig, "eth0.mtu 99"),
        "99",
        Error
    ));
    assert!(has(
        &line(Format::NetworkConfig, "0bad.kind ethernet"),
        "0bad.kind",
        Error
    ));
}

#[test]
fn the_override_store_colours_names_and_dispositions() {
    let pairs = line(Format::ServiceOverrides, "timed disabled # not today");
    assert!(
        has(&pairs, "timed", Key)
            && has(&pairs, "disabled", Keyword)
            && has(&pairs, "# not today", Comment)
    );
    assert!(has(
        &line(Format::ServiceOverrides, "Timed sometimes"),
        "Timed",
        Error
    ));
    assert!(has(
        &line(Format::ServiceOverrides, "timed sometimes"),
        "sometimes",
        Error
    ));
}

#[test]
fn an_app_settings_document_colours_through_its_engine() {
    let pairs = line(Format::AppSettings, "title = \"a\\tb # c\" # note");
    assert!(has(&pairs, "title", Key) && has(&pairs, "=", Punctuation));
    assert!(has(&pairs, "\\t", Escape) && has(&pairs, "# note", Comment));
    assert!(has(
        &line(Format::AppSettings, "no separator here"),
        "no separator here",
        Error
    ));
    assert!(has(
        &line(Format::ProgramLibrary, "  # comment"),
        "# comment",
        Comment
    ));
}

#[test]
fn the_databases_expect_their_header_then_records() {
    let users = document(Format::UsersDb, "tairix-users-v1\n# admins\nbad record");
    assert!(has(&users[0], "tairix-users-v1", Directive));
    assert!(has(&users[1], "# admins", Comment));
    assert!(has(&users[2], "bad record", Error));
    let groups = document(Format::GroupsDb, "tairix-groups-v1\nwheel:0\nwheel:x");
    assert!(
        has(&groups[1], "wheel", Key)
            && has(&groups[1], ":", Punctuation)
            && has(&groups[1], "0", Number)
    );
    assert!(has(&groups[2], "wheel:x", Error));
    assert!(has(
        &document(Format::GroupsDb, "wrong header")[0],
        "wrong header",
        Error
    ));
    let crlf = classify(Format::UsersDb, LineState::START, "tairix-users-v1\r");
    assert!(has(&crlf.0, "tairix-users-v1\r", Directive), "{:?}", crlf.0);
    let long = alloc::format!("{}:0", "g".repeat(tairix_users::MAX_GROUP_LINE_LEN));
    let refused = document(
        Format::GroupsDb,
        &alloc::format!("tairix-groups-v1\n{long}"),
    );
    assert!(has(&refused[1], &long, Error));
}

#[test]
fn a_font_manifest_colours_through_its_reader() {
    let pairs = line(Format::FontFamily, "face = Mono-Regular.ttf");
    assert!(
        has(&pairs, "face", Key)
            && has(&pairs, "=", Punctuation)
            && has(&pairs, "Mono-Regular.ttf", String)
    );
    assert!(has(
        &line(Format::FontFamily, "face Mono.ttf"),
        "face Mono.ttf",
        Error
    ));
}

#[test]
fn a_store_line_that_is_not_utf8_is_an_error() {
    let mut spans = Vec::new();
    lex_line(
        Format::SystemConfig,
        LineState::START,
        b"os.loginType \xff",
        &mut spans,
    );
    assert_eq!(
        spans,
        [Span {
            start: 0,
            end: 14,
            role: Error
        }]
    );
}

#[test]
fn a_line_past_the_bound_is_classified_only_within_it() {
    let long = "x".repeat(MAX_LEX_LINE * 2) + " /* tail */";
    let mut spans = Vec::new();
    lex_line(Format::C, LineState::START, long.as_bytes(), &mut spans);
    assert_contract(&spans, long.len());
    assert!(spans.iter().all(|span| span.end as usize <= MAX_LEX_LINE));
}

#[test]
fn spans_already_in_the_buffer_are_left_alone() {
    let mut spans = alloc::vec![Span {
        start: 0,
        end: 4,
        role: Comment
    }];
    lex_line(Format::C, LineState::START, b"// all", &mut spans);
    assert_eq!(spans.len(), 2, "another line's comment is not merged into");
    assert_eq!(
        spans[1],
        Span {
            start: 0,
            end: 6,
            role: Comment
        }
    );
}

#[test]
fn any_state_word_is_safe_for_every_format() {
    for format in Format::ALL {
        for raw in [0, 1, 2, 3, 7, 0xff, 0x100, 0xdead_beef, u32::MAX] {
            let mut spans = Vec::new();
            let text = b"a \"b /* c <d e='f' -->] */ \\";
            lex_line(format, LineState::from_raw(raw), text, &mut spans);
            assert_contract(&spans, text.len());
        }
    }
}

#[test]
fn stores_are_found_by_their_fixed_names() {
    assert_eq!(store_for_name("system.conf"), Some(Format::SystemConfig));
    assert_eq!(store_for_name("network.conf"), Some(Format::NetworkConfig));
    assert_eq!(store_for_name("library.conf"), Some(Format::ProgramLibrary));
    assert_eq!(store_for_name("overrides"), Some(Format::ServiceOverrides));
    assert_eq!(store_for_name("settings.conf"), Some(Format::AppSettings));
    assert_eq!(store_for_name("FontFamily"), Some(Format::FontFamily));
    assert_eq!(store_for_name("notes.conf"), None);
}

#[test]
fn a_head_names_its_format_when_it_can() {
    assert_eq!(
        format_for_head(b"tairix-users-v1\nroot:..."),
        Some(Format::UsersDb)
    );
    assert_eq!(
        format_for_head(b"tairix-groups-v1\r\nwheel:0"),
        Some(Format::GroupsDb)
    );
    assert_eq!(format_for_head(b"#!/bin/sh\necho"), Some(Format::Shell));
    assert_eq!(
        format_for_head(b"#!/usr/bin/env -S python3 -u\n"),
        Some(Format::Python)
    );
    assert_eq!(
        format_for_head(b"#!/usr/bin/env node\n"),
        Some(Format::JavaScript)
    );
    assert_eq!(
        format_for_head(b"\xef\xbb\xbf  <!DOCTYPE HTML>\n<html>"),
        Some(Format::Html)
    );
    assert_eq!(
        format_for_head(b"<?xml version=\"1.0\"?><svg/>"),
        Some(Format::Xml)
    );
    assert_eq!(format_for_head(b"<svg xmlns=\"...\">"), Some(Format::Xml));
    assert_eq!(format_for_head(b"#!/usr/bin/perl\n"), None);
    assert_eq!(format_for_head(b"plain words"), None);
    assert_eq!(format_for_head(b""), None);
}

#[test]
fn validation_reports_the_system_refusal_at_its_line() {
    let refused = validate(
        Format::SystemConfig,
        b"# c\nos.loginType text\ncache.all maybe\n",
    );
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].line, Some(3));
    assert_eq!(refused[0].severity, Severity::Error);
    assert!(validate(Format::SystemConfig, b"os.loginType text\n").is_empty());
    let net = validate(Format::NetworkConfig, b"eth0.kind ethernet\neth0.mtu 5\n");
    assert_eq!(net.first().map(|d| d.line), Some(Some(2)));
    let overrides = validate(Format::ServiceOverrides, b"timed disabled\ntimed enabled\n");
    assert_eq!(overrides.first().map(|d| d.line), Some(Some(2)));
    let groups = validate(Format::GroupsDb, b"tairix-groups-v1\nwheel:0\nstaff:0\n");
    assert_eq!(groups.first().map(|d| d.line), Some(Some(3)));
    let users = validate(Format::UsersDb, b"wrong\n");
    assert_eq!(users.first().map(|d| d.line), Some(Some(1)));
    let font = validate(Format::FontFamily, b"label = Mono\nkind = monospace\n");
    assert_eq!(
        font.first().map(|d| d.line),
        Some(None),
        "no face is a whole-document refusal"
    );
    assert!(validate(
        Format::FontFamily,
        b"label = Mono\nkind = monospace\nface = M.ttf\n"
    )
    .is_empty());
}

#[test]
fn a_tolerant_store_warns_at_every_line_it_ignores() {
    let warned = validate(Format::AppSettings, b"a = 1\nnot a setting\nB = 2\n");
    let lines: Vec<(Option<u32>, Severity)> = warned.iter().map(|d| (d.line, d.severity)).collect();
    assert_eq!(
        lines,
        [(Some(2), Severity::Warning), (Some(3), Severity::Warning)]
    );
    let library = validate(
        Format::ProgramLibrary,
        b"editor.name = Editor\neditor.colour = mauve\n",
    );
    assert_eq!(
        library.first().map(|d| (d.line, d.severity)),
        Some((Some(2), Severity::Error))
    );
}

#[test]
fn validation_names_the_line_a_non_utf8_store_breaks_on() {
    let refused = validate(Format::SystemConfig, b"os.loginType text\n\nbad \xc3\n");
    assert_eq!(refused.first().map(|d| d.line), Some(Some(3)));
}

#[test]
fn a_format_that_is_not_a_store_is_not_validated() {
    assert!(validate(Format::Rust, b"this is not rust at all {{{").is_empty());
    assert!(validate(Format::PlainText, &[0xff; 8]).is_empty());
}
