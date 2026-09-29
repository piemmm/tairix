//! What a document is, from its name and from its opening bytes.

use crate::Format;

/// The store a document is by its file name alone, for the stores whose
/// file name is fixed. Each name is the store's own path constant's leaf, so
/// a store that moves cannot leave detection behind.
#[must_use]
pub fn store_for_name(name: &str) -> Option<Format> {
    let leaf = |path: &'static str| path.rsplit_once('/').map_or(path, |(_, leaf)| leaf);
    [
        (leaf(tairix_sysconfig::CONFIG_PATH), Format::SystemConfig),
        (leaf(tairix_netconfig::CONFIG_PATH), Format::NetworkConfig),
        (leaf(tairix_proglib::LIBRARY_PATH), Format::ProgramLibrary),
        (
            leaf(tairix_abi::SERVICE_OVERRIDES_PATH),
            Format::ServiceOverrides,
        ),
        (
            tairix_abi::appdata_ipc::APPDATA_SETTINGS_FILE,
            Format::AppSettings,
        ),
        (tairix_fontface::FAMILY_MANIFEST, Format::FontFamily),
    ]
    .into_iter()
    .find(|(fixed, _)| *fixed == name)
    .map(|(_, format)| format)
}

/// What a document's opening bytes say it is, when they say: a store's
/// mandatory header, an interpreter line, a markup prolog.
#[must_use]
pub fn format_for_head(head: &[u8]) -> Option<Format> {
    let head = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let first = head.split(|&b| b == b'\n').next().unwrap_or(head);
    let first = first.strip_suffix(b"\r").unwrap_or(first);
    if first == tairix_users::FORMAT_HEADER.as_bytes() {
        return Some(Format::UsersDb);
    }
    if first == tairix_users::GROUPS_FORMAT_HEADER.as_bytes() {
        return Some(Format::GroupsDb);
    }
    if let Some(interpreter) = first.strip_prefix(b"#!") {
        return interpreted(interpreter);
    }
    let start = head.trim_ascii_start();
    let opens = |prefix: &[u8]| {
        start
            .get(..prefix.len())
            .is_some_and(|at| at.eq_ignore_ascii_case(prefix))
    };
    if opens(b"<!doctype html") || opens(b"<html") {
        Some(Format::Html)
    } else if opens(b"<?xml") || opens(b"<svg") {
        Some(Format::Xml)
    } else {
        None
    }
}

/// The format of a script run by the interpreter an `#!` line names.
fn interpreted(line: &[u8]) -> Option<Format> {
    let mut words = line
        .split(u8::is_ascii_whitespace)
        .filter(|word| !word.is_empty());
    let mut program = base_name(words.next()?);
    if program == b"env" {
        program = base_name(words.find(|word| !word.starts_with(b"-"))?);
    }
    if program.starts_with(b"python") {
        return Some(Format::Python);
    }
    match program {
        b"sh" | b"bash" | b"dash" | b"ksh" | b"zsh" | b"ash" | b"mksh" | b"elsh" => {
            Some(Format::Shell)
        }
        b"node" | b"deno" => Some(Format::JavaScript),
        _ => None,
    }
}

/// The last component of a path.
fn base_name(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}
