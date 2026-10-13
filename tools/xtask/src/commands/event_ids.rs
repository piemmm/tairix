//! `cargo xtask event-ids` implementation.
//!
//! Every component takes its event identifiers from its own range in
//! `tairix_log::registry`, so one number never carries two meanings in the
//! log. This stage reads every identifier the workspace's crates define and
//! fails when a crate defines one outside its range, when a crate defines any
//! with no range registered, or when two crates define one value.
//!
//! A definition is an `EventId(<integer literal>)`, or an arm of a catalogue
//! spelled `EventId(match self { … => <integer literal>, … })`; an `EventId`
//! built from anything else is a value read back, not a definition. Comments,
//! string literals and every item compiled only under `cfg(test)` are not
//! read: a test fixture names whatever identifier suits it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tairix_log::registry::{Owner, OWNERS};
use tairix_log::EventId;

/// The trees whose crates emit events.
const SCANNED: &[&str] = &["kernel", "lib", "drivers", "userland"];

/// One identifier a crate defines.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Definition {
    crate_path: String,
    file: String,
    line: usize,
    id: u32,
}

/// Run the check over the workspace at `root`.
///
/// # Errors
///
/// Every violation found, one per line, or a failure to read the tree.
pub fn run(root: &Path) -> Result<(), String> {
    let definitions = scan(root)?;
    let violations = check(&definitions, OWNERS);
    if violations.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} event-id violation(s); every identifier comes from its crate's range in \
         lib/log/src/registry.rs:\n{}",
        violations.len(),
        violations.join("\n")
    ))
}

fn scan(root: &Path) -> Result<Vec<Definition>, String> {
    let mut files = Vec::new();
    for tree in SCANNED {
        collect(&root.join(tree), &mut files)?;
    }
    files.sort();
    let mut definitions = Vec::new();
    for path in files {
        let rel = relative(root, &path);
        if is_test_file(&rel) {
            continue;
        }
        let Some(crate_path) = crate_of(root, &path) else {
            continue;
        };
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))?;
        for (line, id) in definitions_in(&text) {
            definitions.push(Definition {
                crate_path: crate_path.clone(),
                file: rel.clone(),
                line,
                id,
            });
        }
    }
    Ok(definitions)
}

pub(super) fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "target" && name != "tests" && name != "benches" {
                collect(&path, out)?;
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

pub(super) fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(super) fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs" || name.ends_with("_tests.rs")
}

/// The workspace path of the crate `path` belongs to: its nearest ancestor
/// holding a manifest.
fn crate_of(root: &Path, path: &Path) -> Option<String> {
    let mut dir = path.parent()?;
    while dir.starts_with(root) && dir != root {
        if dir.join("Cargo.toml").is_file() {
            return Some(relative(root, dir));
        }
        dir = dir.parent()?;
    }
    None
}

/// Every definition in `source`, with the line it is on.
fn definitions_in(source: &str) -> Vec<(usize, u32)> {
    let text = without_tests(&blank_comments_and_strings(source));
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("EventId(") {
        let start = from + at;
        let open = start + "EventId(".len();
        from = open;
        if start > 0 && is_ident_byte(bytes[start - 1]) {
            continue;
        }
        let rest = text[open..].trim_start();
        let offset = open + (text[open..].len() - rest.len());
        if let Some((id, len)) = integer_literal(rest) {
            if rest[len..].trim_start().starts_with(')') {
                found.push((line_of(&text, offset), id));
            }
        } else if rest.starts_with("match") {
            if let Some(body_open) = text[offset..].find('{').map(|i| offset + i) {
                let body_end = matching_brace(bytes, body_open).unwrap_or(text.len());
                found.extend(arms(&text, body_open, body_end));
                from = body_end;
            }
        }
    }
    found
}

/// Every `=> <integer literal>` arm between `open` and `end`.
fn arms(text: &str, open: usize, end: usize) -> Vec<(usize, u32)> {
    let mut found = Vec::new();
    let mut from = open;
    while let Some(at) = text[from..end].find("=>") {
        let after = from + at + 2;
        from = after;
        let rest = text[after..end].trim_start();
        let offset = after + (text[after..end].len() - rest.len());
        if let Some((id, len)) = integer_literal(rest) {
            let tail = rest[len..].trim_start();
            if tail.starts_with(',') || tail.starts_with('}') || tail.is_empty() {
                found.push((line_of(text, offset), id));
            }
        }
    }
    found
}

fn integer_literal(text: &str) -> Option<(u32, usize)> {
    let len = text
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'_')
        .count();
    if len == 0 || !text.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let mut end = len;
    if text[end..].starts_with("u32") {
        end += 3;
    }
    if text[end..].bytes().next().is_some_and(is_ident_byte) {
        return None;
    }
    text[..len]
        .replace('_', "")
        .parse()
        .ok()
        .map(|id| (id, end))
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

pub(super) fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

/// `source` with every comment and string literal's contents replaced by
/// spaces, line breaks kept, so offsets still name the original lines.
pub(super) fn blank_comments_and_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut at = 0;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                let end = source[at..].find('\n').map_or(bytes.len(), |i| at + i);
                blank(&mut out, at, end);
                at = end;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                let mut i = at;
                while i < bytes.len() {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, at, i);
                at = i;
            }
            b'r' if raw_string_hashes(&bytes[at..]).is_some() => {
                let hashes = raw_string_hashes(&bytes[at..]).unwrap_or(0);
                let body = at + 2 + hashes;
                let closing = format!("\"{}", "#".repeat(hashes));
                let end = source[body..]
                    .find(&closing)
                    .map_or(bytes.len(), |i| body + i + closing.len());
                blank(&mut out, body, end.saturating_sub(closing.len()).max(body));
                at = end;
            }
            b'"' => {
                let mut i = at + 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                blank(&mut out, at + 1, i.min(bytes.len()));
                at = i + 1;
            }
            b'\'' if char_literal_len(&bytes[at..]).is_some() => {
                let len = char_literal_len(&bytes[at..]).unwrap_or(2);
                blank(&mut out, at + 1, at + len - 1);
                at += len;
            }
            _ => at += 1,
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// The `#` count of a raw string literal starting at `bytes`, if one does.
fn raw_string_hashes(bytes: &[u8]) -> Option<usize> {
    let hashes = bytes[1..].iter().take_while(|b| **b == b'#').count();
    (bytes.get(1 + hashes) == Some(&b'"')).then_some(hashes)
}

/// The length of a character literal starting at `bytes`, telling it from a
/// lifetime.
fn char_literal_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(1) == Some(&b'\\') {
        let close = bytes[2..].iter().position(|b| *b == b'\'')?;
        return Some(close + 3);
    }
    let width = std::str::from_utf8(&bytes[1..bytes.len().min(5)])
        .ok()
        .or_else(|| std::str::from_utf8(&bytes[1..2]).ok())?
        .chars()
        .next()?
        .len_utf8();
    (bytes.get(1 + width) == Some(&b'\'')).then_some(width + 2)
}

/// `text` with every item an attribute compiles only under `test` blanked.
pub(super) fn without_tests(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut from = 0;
    while let Some(at) = text[from..].find("#[cfg(") {
        let start = from + at;
        let Some(close) = matching_bracket(bytes, start + 1) else {
            break;
        };
        from = close + 1;
        let predicate = text[start + "#[cfg(".len()..close].trim_end();
        if !names_test(predicate.strip_suffix(')').unwrap_or(predicate)) {
            continue;
        }
        let end = item_end(bytes, close + 1);
        for byte in &mut out[start..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        from = end;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Whether a `cfg` predicate admits its item only under `test`: `test`
/// itself, or an `all(…)` naming it. An `any(…)` or `not(…)` leaves the item
/// compiled outside tests.
fn names_test(predicate: &str) -> bool {
    let predicate = predicate.trim();
    if predicate == "test" {
        return true;
    }
    predicate.strip_prefix("all(").is_some_and(|inner| {
        inner
            .split(|c: char| c == ',' || c == ')' || c.is_whitespace())
            .any(|word| word == "test")
    })
}

/// The end of the item that starts after `from`: past its body's closing
/// brace, its terminating semicolon, or the comma that ends it in a list; an
/// unmatched closer ends the list it was the last of.
fn item_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b';' | b',' => return i + 1,
            b'}' | b')' | b']' => return i,
            b'(' => i = matching(bytes, i, b'(', b')').map_or(bytes.len(), |c| c + 1),
            b'[' => i = matching_bracket(bytes, i).map_or(bytes.len(), |c| c + 1),
            b'{' => {
                let close = matching_brace(bytes, i).unwrap_or(bytes.len() - 1);
                let after = close + 1;
                let next = bytes[after..]
                    .iter()
                    .position(|b| !b.is_ascii_whitespace())
                    .map(|p| after + p);
                return match next {
                    Some(semi) if bytes[semi] == b';' => semi + 1,
                    _ => after,
                };
            }
            b'#' if bytes.get(i + 1) == Some(&b'[') => {
                i = matching_bracket(bytes, i + 1).map_or(bytes.len(), |c| c + 1);
            }
            _ => i += 1,
        }
    }
    bytes.len()
}

fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
    matching(bytes, open, b'{', b'}')
}

fn matching_bracket(bytes: &[u8], open: usize) -> Option<usize> {
    matching(bytes, open, b'[', b']')
}

fn matching(bytes: &[u8], open: usize, up: u8, down: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (i, byte) in bytes.iter().enumerate().skip(open) {
        if *byte == up {
            depth += 1;
        } else if *byte == down {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Every way `definitions` break the registry `owners`.
fn check(definitions: &[Definition], owners: &[Owner]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut unregistered = BTreeSet::new();
    let mut by_id: BTreeMap<u32, BTreeMap<&str, &Definition>> = BTreeMap::new();
    for definition in definitions {
        let owner = owners
            .iter()
            .find(|owner| owner.crates.contains(&definition.crate_path.as_str()));
        match owner {
            None => {
                if unregistered.insert(definition.crate_path.as_str()) {
                    violations.push(format!(
                        "{}:{}: `{}` defines event id {} but has no range in the registry",
                        definition.file, definition.line, definition.crate_path, definition.id
                    ));
                }
            }
            Some(owner) if !owner.range.contains(EventId(definition.id)) => {
                violations.push(format!(
                    "{}:{}: event id {} lies outside `{}`'s range {}..{}",
                    definition.file,
                    definition.line,
                    definition.id,
                    definition.crate_path,
                    owner.range.start(),
                    owner.range.end()
                ));
            }
            Some(_) => {}
        }
        by_id
            .entry(definition.id)
            .or_default()
            .entry(definition.crate_path.as_str())
            .or_insert(definition);
    }
    for (id, crates) in by_id {
        if crates.len() > 1 {
            let sites: Vec<String> = crates
                .values()
                .map(|d| format!("{}:{}", d.file, d.line))
                .collect();
            violations.push(format!(
                "event id {id} is defined by {} crates: {}",
                crates.len(),
                sites.join(", ")
            ));
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_log::registry::EventRange;

    fn ids(source: &str) -> Vec<u32> {
        definitions_in(source)
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    #[test]
    fn literal_and_catalogue_definitions_are_read_with_their_lines() {
        let source = "const A: EventId = EventId(4_101);\n\
                      fn id(self) -> EventId {\n    EventId(match self {\n        Self::X => 4100,\n        Self::Y => 4_133,\n    })\n}\n";
        assert_eq!(
            definitions_in(source),
            vec![(1, 4101), (4, 4100), (5, 4133)]
        );
    }

    #[test]
    fn a_value_read_back_is_no_definition() {
        assert!(ids("let e = EventId(decoded.event_id()); let EventId(raw) = e;").is_empty());
        assert!(ids("let x = MyEventId(5);").is_empty());
    }

    #[test]
    fn comments_strings_and_test_items_are_not_read() {
        let source = r#"
/// `EventId(5000)` is the syscall record.
// EventId(1)
/* EventId(2) */
const S: &str = "EventId(3)";
#[cfg(test)]
mod tests {
    const T: EventId = EventId(4);
}
#[cfg(all(test, not(target_os = "none")))]
mod more {
    fn f() { let _ = EventId(5); }
}
#[cfg(test)]
const U: EventId = EventId(6);
#[cfg(any(test, feature = "x"))]
const KEPT: EventId = EventId(7);
struct S {
    #[cfg(test)]
    probe: [EventId; 2],
    open: u8,
}
#[cfg(test)]
const PAIR: [EventId; 2] = [EventId(9), EventId(10)];
#[cfg(test)]
fn braces() -> char { '{' }
const LIFETIME: &'static str = "x"; const C: char = '"'; const V: EventId = EventId(8);
"#;
        assert_eq!(ids(source), vec![7, 8]);
    }

    fn definition(crate_path: &str, id: u32) -> Definition {
        Definition {
            crate_path: crate_path.into(),
            file: format!("{crate_path}/src/lib.rs"),
            line: 1,
            id,
        }
    }

    const FIXTURE: &[Owner] = &[
        Owner {
            range: EventRange::new(4_000, 5_000),
            crates: &["kernel/core", "kernel/tairix-kernel"],
        },
        Owner {
            range: EventRange::new(27_000, 27_200),
            crates: &["drivers/bus/usb/xhci"],
        },
    ];

    #[test]
    fn ids_in_range_and_unique_pass() {
        let defs = [
            definition("kernel/core", 4_100),
            definition("kernel/tairix-kernel", 4_260),
            definition("drivers/bus/usb/xhci", 27_001),
        ];
        assert!(check(&defs, FIXTURE).is_empty());
    }

    #[test]
    fn a_collision_an_id_out_of_range_and_an_unregistered_crate_are_each_refused() {
        let defs = [
            definition("kernel/core", 4_101),
            definition("kernel/tairix-kernel", 4_101),
            definition("drivers/bus/usb/xhci", 4_101),
            definition("drivers/storage/raid", 4_191),
        ];
        let violations = check(&defs, FIXTURE);
        assert_eq!(violations.len(), 3, "{violations:#?}");
        assert!(violations
            .iter()
            .any(|v| v.contains("outside `drivers/bus/usb/xhci`'s range")));
        assert!(violations
            .iter()
            .any(|v| v.contains("`drivers/storage/raid` defines event id 4191 but has no range")));
        assert!(violations
            .iter()
            .any(|v| v.contains("event id 4101 is defined by 3 crates")));
    }

    #[test]
    fn one_crate_repeating_an_id_is_not_a_collision() {
        let defs = [
            definition("kernel/tairix-kernel", 4_139),
            definition("kernel/tairix-kernel", 4_139),
        ];
        assert!(check(&defs, FIXTURE).is_empty());
    }
}
