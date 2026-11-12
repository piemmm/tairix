//! `cargo xtask static-mut-check`: no Rust source declares a `static mut`.
//!
//! A `static mut` is state anything may write with no owner and no
//! synchronisation, which the charter forbids outright. Storage a binary
//! hands out by address is a plain `static` over interior mutability
//! (`tairix_kalloc::Arena` for page-aligned bytes); other state is owned and
//! passed.

use std::path::Path;

use super::source_tree::{self, Finding};
use crate::children::Children;

/// Every `static mut` declared in the workspace rooted at `root`.
pub fn scan(root: &Path, children: &Children) -> Result<Vec<Finding>, String> {
    let mut out = Vec::new();
    source_tree::for_each_file(root, children, "static-mut-check", |path, rel, name| {
        if source_tree::is_rust(name) {
            let src = std::fs::read_to_string(path)
                .map_err(|e| format!("static-mut-check: cannot read {}: {e}", path.display()))?;
            scan_source(rel, &src, &mut out);
        }
        Ok(())
    })?;
    source_tree::sort(&mut out);
    Ok(out)
}

fn scan_source(rel: &str, src: &str, out: &mut Vec<Finding>) {
    for (index, line) in src.lines().enumerate() {
        if declares_static_mut(line) {
            out.push(Finding::at(rel, index, line));
        }
    }
}

/// Whether `line` opens a `static mut` item: after any visibility and an
/// extern block's `safe` or `unsafe`, the two keywords. A comment, a string,
/// and the `&'static mut` reference type open none.
fn declares_static_mut(line: &str) -> bool {
    let mut rest = line.trim_start();
    if let Some(after) = keyword(rest, "pub") {
        rest = after;
    } else if let Some(scoped) = rest.strip_prefix("pub(") {
        let Some(close) = scoped.find(')') else {
            return false;
        };
        rest = scoped[close + 1..].trim_start();
    }
    rest = keyword(rest, "safe")
        .or_else(|| keyword(rest, "unsafe"))
        .unwrap_or(rest);
    keyword(rest, "static").is_some_and(|after| keyword(after, "mut").is_some())
}

/// What follows the keyword `word` that `text` opens with, its whitespace
/// skipped; `None` where `text` opens with another word.
fn keyword<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let after = text.strip_prefix(word)?;
    let gap = after.len() - after.trim_start().len();
    (gap > 0).then(|| after.trim_start())
}

/// Run the check, failing with every `static mut` found.
pub fn run(root: &Path, children: &Children) -> Result<(), String> {
    source_tree::refuse(
        "static-mut-check: a `static mut` is state with no owner and no \
         synchronisation; back storage reached by address with a plain `static` \
         over interior mutability (`tairix_kalloc::Arena` for page-aligned bytes), \
         and own and pass any other state:",
        &scan(root, children)?,
    )
}

#[cfg(test)]
mod tests {
    use super::{scan, scan_source};
    use crate::children::Children;

    /// The regression check of `plans/OPEN-DEFECTS.md` D127 and D242.
    #[test]
    fn the_workspace_declares_no_static_mut() {
        let mut root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        root.pop();
        root.pop();
        let found = scan(&root, &Children::default()).expect("scan");
        assert!(found.is_empty(), "static mut: {found:#?}");
    }

    #[test]
    fn every_spelling_of_a_static_mut_is_found() {
        for source in [
            "static mut X: u8 = 0;",
            "    static mut FRAME_POOL: FramePool = FramePool([0; 4]);",
            "pub static mut X: u8 = 0;",
            "pub(crate) static mut X: u8 = 0;",
            "pub(in crate::a) static mut X: u8 = 0;",
            "    pub unsafe static mut Y: u8;",
            "    safe static mut Y: u8;",
            "static  mut X: u8 = 0;",
            "\tstatic\tmut X: u8 = 0;",
        ] {
            let mut out = Vec::new();
            scan_source("a.rs", source, &mut out);
            assert_eq!(out.len(), 1, "{source}");
        }
    }

    #[test]
    fn what_declares_no_static_mut_is_not_found() {
        for source in [
            "static X: u8 = 0;",
            "pub static HEAP: Arena<HEAP_BYTES> = Arena::ZERO;",
            "fn f(x: &'static mut u8) {}",
            "let region: &'static mut [u8] = buf;",
            "static mutable: u8 = 0;",
            "static MUTEX: Lock = Lock::new();",
            "// static mut X: u8 = 0;",
            "/// nothing needs a `static mut` to write them",
            "let s = \"static mut\";",
            "publish static mut",
            "pub(crate static mut X: u8 = 0;",
        ] {
            let mut out = Vec::new();
            scan_source("a.rs", source, &mut out);
            assert!(out.is_empty(), "{source}");
        }
    }

    #[test]
    fn each_finding_names_its_line() {
        let mut out = Vec::new();
        scan_source("a.rs", "fn f() {}\nstatic mut X: u8 = 0;\n", &mut out);
        let found: Vec<_> = out.iter().map(|f| (f.path.as_str(), f.line)).collect();
        assert_eq!(found, [("a.rs", 2)]);
    }
}
