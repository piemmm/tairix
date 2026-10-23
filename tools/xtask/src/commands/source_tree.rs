//! The workspace walk the source scans share.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Visit every regular file under `root` a change could carry, passing its
/// path, its workspace-relative path and its file name: everything but the
/// build output, the repository database, and the untracked paths git ignores
/// (fetched tooling, generated books and images).
///
/// # Errors
///
/// The first directory or entry that cannot be read, prefixed with `stage`,
/// or the first error `visit` returns.
pub(super) fn for_each_file(
    root: &Path,
    stage: &str,
    mut visit: impl FnMut(&Path, &str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let ignored = ignored(root);
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("{stage}: cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{stage}: dir entry: {e}"))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("{stage}: file type {}: {e}", path.display()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let rel = relative(root, &path);
            if ignored.contains(&rel) {
                continue;
            }
            if file_type.is_dir() {
                if name != "target" && name != ".git" {
                    dirs.push(path);
                }
            } else if file_type.is_file() {
                visit(&path, &rel, &name)?;
            }
        }
    }
    Ok(())
}

/// The untracked paths git ignores under `root`, relative to it, a wholly
/// ignored directory once. Empty where git cannot say, so the walk then scans
/// more rather than less; a tracked file is never among them.
fn ignored(root: &Path) -> BTreeSet<String> {
    let listed = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ])
        .current_dir(root)
        .output();
    match listed {
        Ok(listed) if listed.status.success() => listed
            .stdout
            .split(|&byte| byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                String::from_utf8_lossy(path)
                    .trim_end_matches('/')
                    .to_owned()
            })
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// Whether `name` is a Rust source file. Any case counts: a `#[path]` module
/// may be spelled `.RS`, and a scan that skipped it would let it escape.
pub(super) fn is_rust(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

/// `path` relative to `base`, with `/` separators.
pub(super) fn relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::{for_each_file, is_rust};

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    /// What git ignores and does not track is passed over; a tracked file is
    /// visited even where an ignore pattern names it.
    #[test]
    fn the_walk_passes_over_what_git_ignores_and_does_not_track() {
        let root = std::env::temp_dir().join(format!("source-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("scratch tree");
        std::fs::create_dir_all(root.join("fetched/deep")).expect("scratch tree");
        git(&root, &["init", "-q"]);
        // Only this repository's own patterns, never the machine's.
        git(&root, &["config", "core.excludesFile", "/dev/null"]);
        std::fs::write(
            root.join(".gitignore"),
            "fetched/\ngenerated.rs\nforced.json\n",
        )
        .expect("write");
        for file in [
            "src/lib.rs",
            "fetched/deep/pkg.json",
            "generated.rs",
            "forced.json",
        ] {
            std::fs::write(root.join(file), b"x").expect("write");
        }
        git(&root, &["add", "-f", "forced.json"]);
        let mut seen = Vec::new();
        let walked = for_each_file(&root, "test", |_, rel, _| {
            seen.push(rel.to_owned());
            Ok(())
        });
        let _ = std::fs::remove_dir_all(&root);
        walked.expect("walk");
        seen.sort();
        assert_eq!(seen, [".gitignore", "forced.json", "src/lib.rs"]);
    }

    #[test]
    fn a_rust_file_is_known_by_its_extension_in_any_case() {
        for rust in ["lib.rs", "Path.RS", "a.b.rs"] {
            assert!(is_rust(rust), "{rust}");
        }
        for other in ["lib.rs.bak", "rs", ".rs", "notes.md"] {
            assert!(!is_rust(other), "{other}");
        }
    }
}
