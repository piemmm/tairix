//! The workspace walk the source scans share.

use std::path::Path;

/// Visit every regular file under `root` outside the build output and the
/// repository database, passing its path, its workspace-relative path and its
/// file name.
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
            if file_type.is_dir() {
                if name != "target" && name != ".git" {
                    dirs.push(path);
                }
            } else if file_type.is_file() {
                visit(&path, &relative(root, &path), &name)?;
            }
        }
    }
    Ok(())
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
    use super::is_rust;

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
