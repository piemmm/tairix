//! `cargo xtask source-text` implementation.
//!
//! Source is text: a control byte in it is a typo the eye cannot see, and one
//! that makes tools misread the whole file — `grep` takes a file holding a NUL
//! for binary and skips every line of it, so a census of the tree silently
//! misses whatever that file defines. The scan refuses every C0 control byte
//! but tab and newline, and DEL, in the hand-written source and document
//! types; a carriage return is refused with them, since no source here is
//! CRLF. Vendored assets and generated build output are not source and are
//! not scanned, and neither are the planning and charter documents: a change
//! confined to those runs no gate, so no stage may read them.

use std::path::Path;

use super::source_tree;

/// The file types scanned: code, build and CI glue, and the documents.
const SOURCE_SUFFIXES: [&str; 11] = [
    ".rs", ".s", ".S", ".ld", ".toml", ".json", ".sh", ".yml", ".yaml", ".md", ".h",
];

/// The planning and charter documents, which no stage reads.
fn is_planning_document(rel: &str) -> bool {
    matches!(rel, "PLAN.md" | "AGENTS.md" | "CLAUDE.md")
        || rel.strip_prefix("plans/").is_some_and(|name| {
            !name.contains('/')
                && std::path::Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension == "md")
        })
}

/// One control byte in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub line: usize,
    pub byte: u8,
}

/// Whether `byte` may appear in source text.
const fn is_text(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n') || (byte >= 0x20 && byte != 0x7f)
}

/// Every control byte in `bytes`, by 1-based line.
fn scan_bytes(rel: &str, bytes: &[u8], out: &mut Vec<Violation>) {
    let mut line = 1;
    for &byte in bytes {
        if !is_text(byte) {
            out.push(Violation {
                path: rel.to_string(),
                line,
                byte,
            });
        }
        if byte == b'\n' {
            line += 1;
        }
    }
}

/// Scan the workspace rooted at `root` and return every control byte found.
pub fn scan(root: &Path) -> Result<Vec<Violation>, String> {
    let mut out = Vec::new();
    source_tree::for_each_file(root, "source-text", |path, rel, name| {
        if SOURCE_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) && !is_planning_document(rel)
        {
            let bytes = std::fs::read(path)
                .map_err(|e| format!("source-text: cannot read {}: {e}", path.display()))?;
            scan_bytes(rel, &bytes, &mut out);
        }
        Ok(())
    })?;
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    Ok(out)
}

/// Run the check, printing each control byte and failing when any remains.
pub fn run(root: &Path) -> Result<(), String> {
    let violations = scan(root)?;
    if violations.is_empty() {
        return Ok(());
    }
    for v in &violations {
        eprintln!(
            "xtask: [source-text] {}:{}: control byte 0x{:02x}; write it as an escape",
            v.path, v.line, v.byte
        );
    }
    Err(format!(
        "source-text: {} control byte(s) in tracked source",
        violations.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::{is_planning_document, scan, scan_bytes, Violation};

    fn violations(bytes: &[u8]) -> Vec<(usize, u8)> {
        let mut out = Vec::new();
        scan_bytes("f.rs", bytes, &mut out);
        out.into_iter().map(|v| (v.line, v.byte)).collect()
    }

    #[test]
    fn plain_text_with_tabs_and_newlines_passes() {
        assert!(violations(b"fn main() {\n\tlet s = \"\\0\";\n}\n").is_empty());
        assert!(violations("// é — ünïcode\n".as_bytes()).is_empty());
    }

    #[test]
    fn a_raw_nul_is_refused_on_its_line() {
        assert_eq!(violations(b"one\ntwo \0 two\nthree\n"), [(2, 0)]);
    }

    #[test]
    fn carriage_return_delete_and_other_controls_are_refused() {
        assert_eq!(
            violations(b"a\r\nb\x7f\nc\x0c\nd\x1b[0m\n"),
            [(1, b'\r'), (2, 0x7f), (3, 0x0c), (4, 0x1b)]
        );
    }

    #[test]
    fn only_the_planning_and_charter_documents_are_exempt() {
        for exempt in ["PLAN.md", "AGENTS.md", "CLAUDE.md", "plans/APPS.md"] {
            assert!(is_planning_document(exempt), "{exempt}");
        }
        for scanned in [
            "README.md",
            "docs/src/index.md",
            "plans/sub/X.md",
            "lib/x/plans/Y.md",
            "plans/notes.txt.rs",
            "plans/SHOUTED.MD",
            "plans/.md",
        ] {
            assert!(!is_planning_document(scanned), "{scanned}");
        }
    }

    /// The scan finds a planted control byte through the real walk, and skips
    /// build output and file types that are not source.
    #[test]
    fn the_walk_scans_source_and_skips_the_rest() {
        let root = std::env::temp_dir().join(format!("source-text-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("scratch tree");
        std::fs::create_dir_all(root.join("target")).expect("scratch tree");
        std::fs::write(root.join("src/lib.rs"), b"const S: &str = \"\0\";\n").expect("write");
        std::fs::write(root.join("target/out.rs"), b"\0").expect("write");
        std::fs::write(root.join("LICENCE.txt"), b"vendored\r\n").expect("write");
        std::fs::create_dir_all(root.join("plans")).expect("scratch tree");
        std::fs::write(root.join("plans/X.md"), b"\0").expect("write");
        let found = scan(&root).expect("scan");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            found,
            [Violation {
                path: "src/lib.rs".to_string(),
                line: 1,
                byte: 0,
            }]
        );
    }
}
