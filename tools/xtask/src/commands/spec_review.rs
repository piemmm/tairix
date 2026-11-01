//! `cargo xtask spec-review` implementation.
//!
//! lets AI assistance *draft* specifications, proofs, models, and fuzz
//! harnesses, but the verifier — not the model — is the only oracle. A draft
//! that has not yet been reviewed by a human under the senior-developer
//! bar must carry a marker, and this checker "fails CI if any such marker
//! reaches `main`". It is the gate that keeps an unreviewed, AI-drafted
//! load-bearing artefact out of the trunk.
//!
//! The checker walks every tracked `.rs` source file and fails if the draft
//! marker appears anywhere. Its own source is skipped (it necessarily names
//! the marker to search for it); documentation is `.md`, not scanned, so the
//! charter prose that defines the marker does not trip the gate.

use std::path::Path;

use super::source_tree::{self, Finding};
use crate::children::Children;

/// The distinctive token of the draft marker (`// SPEC-DRAFT:`). Matching the
/// bare token rather than the exact comment punctuation catches every spelling
/// (`//!`, `///`, trailing text) without a brittle regex.
const MARKER: &str = "SPEC-DRAFT";

/// The checker's own file, skipped because it necessarily contains [`MARKER`].
const SELF_FILE: &str = "spec_review.rs";

/// Scan the workspace rooted at `root` and return every draft marker found.
pub fn scan(root: &Path, children: &Children) -> Result<Vec<Finding>, String> {
    let mut out = Vec::new();
    source_tree::for_each_file(root, children, "spec-review", |path, rel, name| {
        if source_tree::is_rust(name) && name != SELF_FILE {
            scan_file(path, rel, &mut out)?;
        }
        Ok(())
    })?;
    source_tree::sort(&mut out);
    Ok(out)
}

fn scan_file(path: &Path, rel: &str, out: &mut Vec<Finding>) -> Result<(), String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("spec-review: cannot read {}: {e}", path.display()))?;
    for (idx, line) in src.lines().enumerate() {
        if line.contains(MARKER) {
            out.push(Finding::at(rel, idx, line));
        }
    }
    Ok(())
}

/// Run the check, printing a report and returning an error if any draft
/// marker remains in the tree.
pub fn run(root: &Path, children: &Children) -> Result<(), String> {
    source_tree::refuse(
        "spec-review: unreviewed draft markers must not reach `main` \
         (AGENTS.md §19.7); review the artefact under the §2.6 bar and \
         remove the marker:",
        &scan(root, children)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_root() -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop();
        p.pop();
        p
    }

    #[test]
    fn workspace_carries_no_draft_markers() {
        let root = workspace_root();
        let violations = scan(&root, &Children::default()).expect("scan");
        assert!(
            violations.is_empty(),
            "unexpected §19.7 draft markers: {violations:#?}"
        );
    }

    #[test]
    fn marker_is_detected_in_every_comment_spelling() {
        let sample = format!("    // {MARKER}: drafted by an assistant, awaiting review");
        let mut out = Vec::new();
        let dir = std::env::temp_dir().join(format!("tairix-spec-review-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("draft.rs");
        std::fs::write(&file, format!("fn f() {{}}\n{sample}\n")).expect("write");
        scan_file(&file, "draft.rs", &mut out).expect("scan file");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line, 2);
    }

    #[test]
    fn clean_source_is_accepted() {
        let mut out = Vec::new();
        let dir = std::env::temp_dir().join(format!("tairix-spec-clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("clean.rs");
        std::fs::write(&file, "fn ok() {}\n// a normal comment\n").expect("write");
        scan_file(&file, "clean.rs", &mut out).expect("scan file");
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.is_empty());
    }
}
