//! Where the program bundles' source crates live, and how their manifest
//! source reads.
//!
//! One walk and one line grammar, compiled into this crate's build script
//! (which embeds each bundle's `Help/` and `Resources/`) and exported to the
//! bundle composer (which builds each bundle's `Run` and signs its
//! `AppInfo`), so the payload a bundle carries and the bundle itself are
//! found and named by one rule and can never describe different programs.
//! The kernel's build script walks the driver tree for its bootstrap floor's
//! manifests with the same walk and reads them with the same grammar.

use std::io;
use std::path::{Path, PathBuf};
use std::vec::Vec;

/// The manifest source whose presence makes a crate a program bundle.
pub const MANIFEST_SOURCE: &str = "AppInfo.toml";

/// Each line of `manifest` that is neither blank nor a `#` comment, numbered
/// from one, with the trimmed `(key, value)` it spells, or `None` for a line
/// that is not `key = value`.
///
/// The grammar is flat, so a table header is such a line.
pub fn manifest_entries(
    manifest: &str,
) -> impl Iterator<Item = (usize, Option<(&str, &str)>)> + '_ {
    manifest.lines().enumerate().filter_map(|(index, raw)| {
        let line = raw.trim();
        (!line.is_empty() && !line.starts_with('#')).then(|| {
            let entry = line
                .split_once('=')
                .map(|(key, value)| (key.trim(), value.trim()));
            (index + 1, entry)
        })
    })
}

/// The text of a double-quoted manifest string, or `None` for a value that is
/// not one or that holds a quote or backslash, which the grammar gives no
/// meaning.
#[must_use]
pub fn quoted_string(value: &str) -> Option<&str> {
    let text = value.strip_prefix('"')?.strip_suffix('"')?;
    (!text.contains(['"', '\\'])).then_some(text)
}

/// Whether `name` may be a bundle's `name`: a plain command word of ASCII
/// letters, digits, `-` and `_`. It names the word a shell resolves and the
/// bundle's own directory in its store, where a `/` or a `..` would plant the
/// bundle outside it.
#[must_use]
pub fn is_command_word(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The string `manifest` gives `key`, from the entries above its first line
/// that is not one; `None` where the first of them naming `key` exactly does
/// not give it a quoted string, or none does.
#[must_use]
pub fn string_entry<'a>(manifest: &'a str, key: &str) -> Option<&'a str> {
    manifest_entries(manifest)
        .map_while(|(_, entry)| entry)
        .find(|&(name, _)| name == key)
        .and_then(|(_, value)| quoted_string(value))
}

/// Every crate under `root` holding the manifest file `manifest`, in a
/// deterministic order: the program crates under `userland` hold
/// [`MANIFEST_SOURCE`].
///
/// Such a crate sits at `<class>/<crate>`, or at `<class>/<group>/<crate>`
/// beneath a grouping directory that is not itself a crate — a game's own
/// subtree, a bus's drivers. `read` is told every directory whose listing the
/// answer depends on, so a build script can rerun when a crate is added
/// anywhere the walk looks.
///
/// # Errors
///
/// A directory the walk cannot list.
pub fn manifest_crates(
    root: &Path,
    manifest: &str,
    mut read: impl FnMut(&Path),
) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for class in sorted_dirs(root, &mut read)? {
        for member in sorted_dirs(&class, &mut read)? {
            if member.join("Cargo.toml").is_file() {
                if member.join(manifest).is_file() {
                    found.push(member);
                }
                continue;
            }
            for member_crate in sorted_dirs(&member, &mut read)? {
                if member_crate.join(manifest).is_file() {
                    found.push(member_crate);
                }
            }
        }
    }
    Ok(found)
}

/// The `[package] name` a crate's `Cargo.toml` text gives, or `None` where
/// it gives none as a plain quoted string.
#[must_use]
pub fn package_name(cargo_toml: &str) -> Option<&str> {
    let mut in_package = false;
    for raw in cargo_toml.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "name" {
                return quoted_string(value.trim());
            }
        }
    }
    None
}

/// Whether `name` is a file a host's file manager leaves beside the files it
/// shows — Finder's `.DS_Store` and `._` resource forks, Explorer's
/// `Thumbs.db` and `desktop.ini` — which belong to no payload and are passed
/// over, so browsing a source folder neither breaks the build nor ships one.
#[must_use]
pub fn is_host_metadata(name: &str) -> bool {
    matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini") || name.starts_with("._")
}

/// The immediate subdirectories of `root`, sorted, so the walk is the same
/// on every filesystem.
fn sorted_dirs(root: &Path, read: &mut impl FnMut(&Path)) -> io::Result<Vec<PathBuf>> {
    read(root);
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::{
        is_command_word, is_host_metadata, manifest_crates, manifest_entries, package_name,
        quoted_string, string_entry,
    };

    /// The files a host's file manager drops beside the ones it shows are
    /// passed over; a name that merely resembles one is not.
    #[test]
    fn host_metadata_is_passed_over_and_nothing_else_is() {
        for name in [".DS_Store", "._icon.png", "._", "Thumbs.db", "desktop.ini"] {
            assert!(is_host_metadata(name), "{name}");
        }
        for name in [
            "DS_Store",
            "_icon.png",
            "icon.png",
            ".hidden.md",
            "thumbs.db.md",
        ] {
            assert!(!is_host_metadata(name), "{name}");
        }
    }

    /// Blank and comment lines are skipped, entries are numbered by their
    /// line and trimmed, and a line that is not `key = value` says so.
    #[test]
    fn entries_are_every_line_that_is_not_blank_or_a_comment() {
        let manifest = "# a program\n\nname = \"ls\"\n  kind=\"command\"  \n[table]\n";
        let entries: Vec<_> = manifest_entries(manifest).collect();
        assert_eq!(
            entries,
            vec![
                (3, Some(("name", "\"ls\""))),
                (4, Some(("kind", "\"command\""))),
                (5, None),
            ]
        );
    }

    /// Only a whole double-quoted string with nothing inside it the grammar
    /// cannot mean is a string.
    #[test]
    fn a_quoted_string_is_exactly_one_pair_of_quotes() {
        assert_eq!(quoted_string("\"ls\""), Some("ls"));
        assert_eq!(quoted_string("\"\""), Some(""));
        for refused in ["ls", "\"ls", "ls\"", "\"", "\"l\"s\"", "\"l\\s\"", "'ls'"] {
            assert_eq!(quoted_string(refused), None, "{refused}");
        }
    }

    /// A quoted manifest string admits `/` and `..`, and the payload walk
    /// once planted a bundle directory named from one unchecked.
    #[test]
    fn a_bundle_name_is_a_plain_command_word() {
        for name in ["ls", "wintersun", "sys-info", "a_b", "X9"] {
            assert!(is_command_word(name), "{name}");
        }
        for name in ["", "..", "a/b", "../etc", ".hidden", "a b", "a.app", "é"] {
            assert!(!is_command_word(name), "{name:?}");
        }
    }

    /// The name is the `[package]` table's own, never another table's or a
    /// key that merely starts with it.
    #[test]
    fn a_package_name_is_the_package_tables_name() {
        let manifest = "[workspace]\nname = \"no\"\n[package]\nnamespace = \"no\"\n\
                        name = \"tairix-x\"\n[dependencies]\nname = \"no\"\n";
        assert_eq!(package_name(manifest), Some("tairix-x"));
        assert_eq!(package_name("[package]\nversion = \"1\"\n"), None);
        assert_eq!(package_name("[package]\nname = tairix\n"), None);
        assert_eq!(package_name("[dependencies]\nname = \"no\"\n"), None);
    }

    /// The walk finds a crate holding the manifest at either depth, never one
    /// without it, and never looks inside a crate for another.
    #[test]
    fn the_walk_finds_each_crate_holding_the_manifest() {
        let root =
            std::env::temp_dir().join(std::format!("tairix-syshelp-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, manifest) in [
            ("class/held", true),
            ("class/bare", false),
            ("class/group/nested", true),
            ("class/held/inner", true),
        ] {
            let dir = root.join(dir);
            std::fs::create_dir_all(&dir).expect("a scratch tree");
            std::fs::write(dir.join("Cargo.toml"), "").expect("a crate");
            if manifest {
                std::fs::write(dir.join("Floor.toml"), "").expect("a manifest");
            }
        }
        let found = manifest_crates(&root, "Floor.toml", |_| {}).expect("walks");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            found,
            vec![root.join("class/group/nested"), root.join("class/held")]
        );
    }

    /// A key is matched exactly, before the first line that is not an entry,
    /// and its first entry decides.
    #[test]
    fn a_string_entry_is_the_first_exact_top_level_match() {
        let manifest = "names = \"no\"\nname = \"ls\"\nname = \"cat\"\n";
        assert_eq!(string_entry(manifest, "name"), Some("ls"));
        assert_eq!(string_entry(manifest, "nam"), None);
        assert_eq!(string_entry("[table]\nname = \"ls\"\n", "name"), None);
        assert_eq!(string_entry("name = ls\nname = \"ls\"\n", "name"), None);
        assert_eq!(string_entry("kind = \"command\"\n", "name"), None);
    }
}
