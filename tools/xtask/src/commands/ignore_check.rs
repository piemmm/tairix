//! `cargo xtask ignore-check`: no test the gate builds is ignored.
//!
//! libtest reports an ignored test as skipped, not failed, so a `#[ignore]`
//! leaves a test unrun under a green gate. A test proven wrong is replaced,
//! never kept and ignored, so this check refuses every spelling of one: the
//! attribute, the attribute made conditional, and a doc example fenced to be
//! ignored, which the doctest pass reports the same way. It also refuses a
//! `compile_fail` example that names no error code, which passes whatever
//! error stopped it, as an unused import would.

use std::path::Path;

use super::source_tree::{self, Finding};
use crate::children::Children;

/// Every ignored test in the workspace rooted at `root`.
pub fn scan(root: &Path, children: &Children) -> Result<Vec<Finding>, String> {
    let mut out = Vec::new();
    source_tree::for_each_file(root, children, "ignore-check", |path, rel, name| {
        if source_tree::is_rust(name) {
            let src = std::fs::read_to_string(path)
                .map_err(|e| format!("ignore-check: cannot read {}: {e}", path.display()))?;
            scan_source(rel, &src, &mut out);
        }
        Ok(())
    })?;
    source_tree::sort(&mut out);
    Ok(out)
}

fn scan_source(rel: &str, src: &str, out: &mut Vec<Finding>) {
    let mut start = 0;
    for (index, line) in src.split_inclusive('\n').enumerate() {
        let code = line.trim_start();
        let at = start + (line.len() - code.len());
        if attributes_ignore(&src[at..]) || doc_fence_proves_nothing(code) {
            out.push(Finding::at(rel, index, line));
        }
        start += line.len();
    }
}

/// Whether one of the attributes `text` opens with ignores a test. An
/// attribute may run on past its line, as a long `cfg_attr` does once
/// formatted, and several may share one.
fn attributes_ignore(mut text: &str) -> bool {
    while let Some((body, rest)) = attribute(text) {
        if attribute_ignores(body) {
            return true;
        }
        text = rest.trim_start_matches([' ', '\t']);
    }
    false
}

/// The body of the attribute `text` opens with, `#[` to its closing `]`, and
/// what follows it; `None` where `text` opens with none, or it never closes.
fn attribute(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_prefix("#[")?;
    let close = top_level(body, ']').next()?;
    Some((&body[..close], &body[close + 1..]))
}

/// `ignore`, `ignore = "why"`, or a `cfg_attr` one of whose attributes is.
fn attribute_ignores(body: &str) -> bool {
    let body = body.trim();
    word_at(body, "ignore")
        || body
            .strip_prefix("cfg_attr")
            .and_then(|rest| rest.trim_start().strip_prefix('('))
            .and_then(|rest| rest.trim_end().strip_suffix(')'))
            .is_some_and(|arguments| {
                let mut from = 0;
                let mut items = Vec::new();
                for comma in top_level(arguments, ',') {
                    items.push(&arguments[from..comma]);
                    from = comma + 1;
                }
                items.push(&arguments[from..]);
                items.into_iter().skip(1).any(attribute_ignores)
            })
}

/// The byte offsets of each `wanted` in `text` outside any string literal
/// and any bracket nesting `text` opens, stopping at a bracket that closes
/// more than it opened.
fn top_level(text: &str, wanted: char) -> impl Iterator<Item = usize> + '_ {
    let mut chars = text.char_indices();
    let mut depth = 0usize;
    core::iter::from_fn(move || {
        while let Some((at, c)) = chars.next() {
            match c {
                c if c == wanted && depth == 0 => return Some(at),
                '"' => loop {
                    match chars.next()? {
                        (_, '\\') => {
                            chars.next()?;
                        }
                        (_, '"') => break,
                        _ => {}
                    }
                },
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.checked_sub(1)?,
                _ => {}
            }
        }
        None
    })
}

/// A `///` or `//!` line opening a fence that ignores its example, through
/// `ignore` or rustdoc's `ignore-<target>`, or marks it `compile_fail` without
/// the error code it proves, which would pass whatever error stopped it.
fn doc_fence_proves_nothing(code: &str) -> bool {
    let Some(info) = code
        .strip_prefix("///")
        .or_else(|| code.strip_prefix("//!"))
        .and_then(|doc| doc.trim_start().strip_prefix("```"))
    else {
        return false;
    };
    let tags: Vec<&str> = info
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|tag| !tag.is_empty())
        .collect();
    let error_code = |tag: &&str| {
        tag.strip_prefix('E')
            .is_some_and(|digits| digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()))
    };
    tags.iter()
        .any(|tag| *tag == "ignore" || tag.starts_with("ignore-"))
        || (tags.contains(&"compile_fail") && !tags.iter().any(error_code))
}

/// Whether `text` opens with `word` as a whole identifier.
fn word_at(text: &str, word: &str) -> bool {
    text.strip_prefix(word)
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// Run the check, failing with every ignored test found.
pub fn run(root: &Path, children: &Children) -> Result<(), String> {
    source_tree::refuse(
        "ignore-check: the gate runs every test, so none may be ignored or pass \
         whatever it proves; fix or replace each one, fence as `text` an example \
         that is not Rust or that the host cannot build, and pin a `compile_fail` \
         example to the error it proves (`compile_fail,E0277`):",
        &scan(root, children)?,
    )
}

#[cfg(test)]
mod tests {
    use super::{scan, scan_source};
    use crate::children::Children;

    #[test]
    fn the_workspace_ignores_no_test() {
        let mut root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        root.pop();
        root.pop();
        let violations = scan(&root, &Children::default()).expect("scan");
        assert!(violations.is_empty(), "ignored tests: {violations:#?}");
    }

    #[test]
    fn every_spelling_of_an_ignored_test_is_found() {
        for source in [
            "#[ignore]",
            "    #[ignore = \"slow\"]",
            "#[ ignore ]",
            "#[test] #[ignore]",
            "#[cfg_attr(miri, ignore)]",
            "#[cfg_attr(all(unix, not(miri)), ignore = \"why, [then] more\")]",
            "#[cfg_attr(\n    all(feature = \"x\", not(miri)),\n    ignore = \"a reason\"\n)]",
            "#[cfg_attr(unix, cfg_attr(miri, ignore))]",
            "/// ```ignore",
            "    //! ```rust,ignore",
            "/// ``` ignore",
            "/// ```ignore-x86_64",
            "/// ```compile_fail",
            "    //! ```rust,compile_fail",
        ] {
            let mut out = Vec::new();
            scan_source("a.rs", source, &mut out);
            assert_eq!(out.len(), 1, "{source}");
        }
    }

    #[test]
    fn what_ignores_no_test_is_not_found() {
        for source in [
            "#[test]",
            "#[ignored_by_nothing]",
            "#[cfg_attr(test, derive(Debug))]",
            "#[cfg_attr(feature = \"ignore\", test)]",
            "#[cfg_attr(any(unix, ignore), test)]",
            "#[doc = \"#[ignore]\"]",
            "#[cfg_attr(",
            "/// ```text",
            "/// ```rust,no_run",
            "/// ```compile_fail,E0277",
            "/// ```compile_fail,E0080,edition2021",
            "/// a word about how to ignore a reply",
            "// #[ignore]",
            "fn ignore() {}",
            "let ignore = true;",
            "let attribute = \"#[ignore]\";",
        ] {
            let mut out = Vec::new();
            scan_source("a.rs", source, &mut out);
            assert!(out.is_empty(), "{source}");
        }
    }

    #[test]
    fn each_finding_names_the_line_its_attribute_opens_on() {
        let mut out = Vec::new();
        let source = "#[test]\n#[ignore]\nfn t() {}\n#[test]\n#[cfg_attr(\n    miri,\n    ignore\n)]\nfn u() {}\n";
        scan_source("a.rs", source, &mut out);
        let found: Vec<_> = out.iter().map(|v| (v.path.as_str(), v.line)).collect();
        assert_eq!(found, [("a.rs", 2), ("a.rs", 5)]);
    }
}
