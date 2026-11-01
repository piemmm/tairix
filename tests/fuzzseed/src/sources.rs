//! A crate's own sources, read for the host tests that pin code against them:
//! the one walk every port's assembly and register-write pins share.

use std::io;
use std::path::Path;

/// Whether `name` names an assembly source rather than a Rust one.
#[must_use]
pub fn is_assembly(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension == "s")
}

/// Whether a source named `name` holding `text` carries assembly: an assembly
/// file, or Rust holding an `asm!`, `global_asm!` or `naked_asm!` block. A
/// file whose only assembly is a template another file splices in is not
/// seen, so a pin over these alone is not total.
fn carries_assembly(name: &str, text: &str) -> bool {
    is_assembly(name) || text.contains("asm!(")
}

/// One source file of a crate.
pub struct Source {
    relative: String,
    text: String,
}

impl Source {
    /// Its path under the walked root, `/`-separated.
    #[must_use]
    pub fn relative(&self) -> &str {
        &self.relative
    }

    /// Its file name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.relative.rsplit('/').next().unwrap_or(&self.relative)
    }

    /// What it holds.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether it carries assembly ([`carries_assembly`]).
    fn carries_assembly(&self) -> bool {
        carries_assembly(self.name(), &self.text)
    }
}

/// Every `.s` file and every `.rs` file but a `*_tests.rs` module under
/// `root`, recursively, sorted by relative path. The test modules are left
/// out, so a needle a pin spells there never matches itself.
///
/// # Errors
///
/// The first directory or file that cannot be read.
pub fn crate_sources(root: &Path) -> io::Result<Vec<Source>> {
    let mut sources = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let rust = Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "rs")
                && !name.ends_with("_tests.rs");
            if !rust && !is_assembly(name) {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            sources.push(Source {
                relative,
                text: std::fs::read_to_string(&path)?,
            });
        }
    }
    sources.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(sources)
}

/// The relative paths of the sources under `root` that carry assembly and
/// whose file name no `listed` pair names.
///
/// # Errors
///
/// As [`crate_sources`].
pub fn unlisted_assembly(root: &Path, listed: &[(&str, &str)]) -> io::Result<Vec<String>> {
    Ok(crate_sources(root)?
        .into_iter()
        .filter(|source| {
            source.carries_assembly() && !listed.iter().any(|(name, _)| *name == source.name())
        })
        .map(|source| source.relative)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{carries_assembly, crate_sources, is_assembly, unlisted_assembly, Source};
    use crate::scratch::Scratch;

    #[test]
    fn assembly_is_a_dot_s_file_or_rust_holding_an_asm_block() {
        assert!(is_assembly("boot.s") && is_assembly("nested/boot.s"));
        assert!(!is_assembly("boot.rs") && !is_assembly("s") && !is_assembly("boot.S.rs"));
        assert!(carries_assembly("trap.s", ""));
        for block in ["asm!(", "global_asm!(", "naked_asm!("] {
            assert!(carries_assembly(
                "a.rs",
                &format!("core::arch::{block}\"nop\")")
            ));
        }
        assert!(!carries_assembly("a.rs", "fn main() {}"));
    }

    #[test]
    fn crate_sources_reads_what_builds_and_not_the_tests() {
        let tree = Scratch::under(&std::env::temp_dir(), "tairix-fuzzseed-sources");
        std::fs::create_dir(tree.path().join("nested")).expect("the tree");
        for (path, text) in [
            ("a.rs", "core::arch::asm!(\"nop\");"),
            ("a_tests.rs", "core::arch::asm!(\"nop\");"),
            ("b.s", "nop"),
            ("nested/c.rs", "fn c() {}"),
            ("notes.md", "asm!("),
        ] {
            std::fs::write(tree.path().join(path), text).expect("a file");
        }
        let sources = crate_sources(tree.path()).expect("readable");
        let relative: Vec<&str> = sources.iter().map(Source::relative).collect();
        assert_eq!(relative, ["a.rs", "b.s", "nested/c.rs"]);
        let carrying: Vec<&str> = sources
            .iter()
            .filter(|source| source.carries_assembly())
            .map(Source::name)
            .collect();
        assert_eq!(carrying, ["a.rs", "b.s"]);
        assert_eq!(sources[2].name(), "c.rs");
        assert_eq!(
            unlisted_assembly(tree.path(), &[("a.rs", "")]).expect("readable"),
            ["b.s"]
        );
    }
}
