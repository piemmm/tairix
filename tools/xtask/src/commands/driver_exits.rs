//! `cargo xtask driver-exits` implementation.
//!
//! Every driver ends through `tairix_drvrt::exit`, so a status means one
//! thing in every driver and every ending states its reason. This stage fails
//! when a crate built on the driver runtime declares an exit code or an exit
//! vocabulary of its own: an `EXIT_*` integer constant, or a module named
//! `exit`.

use std::path::Path;

use super::event_ids::{
    blank_comments_and_strings, collect, is_test_file, line_of, relative, without_tests,
};

/// The trees whose crates may be built on the driver runtime.
const SCANNED: &[&str] = &["drivers", "lib"];

/// The runtime itself, through which every driver ends.
const RUNTIME: &str = "lib/drvrt";

/// Run the check over the workspace at `root`.
///
/// # Errors
///
/// Every violation found, one per line, or a failure to read the tree.
pub fn run(root: &Path) -> Result<(), String> {
    let mut violations = Vec::new();
    for tree in SCANNED {
        let mut manifests = Vec::new();
        collect_manifests(&root.join(tree), &mut manifests)?;
        for manifest in manifests {
            let dir = manifest.parent().unwrap_or(root);
            let crate_path = relative(root, dir);
            let text = std::fs::read_to_string(&manifest)
                .map_err(|e| format!("{}: {e}", manifest.display()))?;
            if crate_path == RUNTIME || !text.contains("tairix-drvrt") {
                continue;
            }
            let mut files = Vec::new();
            collect(&dir.join("src"), &mut files)?;
            for file in files {
                let rel = relative(root, &file);
                if is_test_file(&rel) {
                    continue;
                }
                let source = std::fs::read_to_string(&file).map_err(|e| format!("{rel}: {e}"))?;
                for (line, what) in own_vocabulary(&source) {
                    violations.push(format!("{rel}:{line}: {what}"));
                }
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} driver exit violation(s); a driver ends through `tairix_drvrt::exit`:\n{}",
        violations.len(),
        violations.join("\n")
    ))
}

fn collect_manifests(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|name| name != "target" && name != "src")
            {
                collect_manifests(&path, out)?;
            }
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            out.push(path);
        }
    }
    Ok(())
}

/// Every exit code or exit vocabulary `source` declares, with its line.
fn own_vocabulary(source: &str) -> Vec<(usize, String)> {
    let text = without_tests(&blank_comments_and_strings(source));
    let mut found = Vec::new();
    for (at, _) in text.match_indices("const EXIT_") {
        let rest = &text[at + "const ".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if rest[name.len()..].trim_start().starts_with(": i32") {
            found.push((
                line_of(&text, at),
                format!("declares its own exit code `{name}`"),
            ));
        }
    }
    for (at, _) in text.match_indices("mod exit") {
        let after = text[at + "mod exit".len()..].chars().next();
        let before = text[..at].chars().next_back();
        if after.is_some_and(|c| c == ' ' || c == '{' || c == ';')
            && before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
        {
            found.push((line_of(&text, at), "declares its own `exit` module".into()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::own_vocabulary;

    #[test]
    fn an_own_code_or_exit_module_is_found_and_a_test_one_is_not() {
        let source = "const EXIT_NO_HOST: i32 = 80;\n\
                      pub mod exit {\n}\n\
                      // const EXIT_IN_A_COMMENT: i32 = 1;\n\
                      const EXIT_LIMIT: u32 = 3;\n\
                      mod exits;\n\
                      #[cfg(test)]\nmod tests {\n    const EXIT_FIXTURE: i32 = 9;\n}\n";
        let found = own_vocabulary(source);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, 1);
        assert!(found[0].1.contains("EXIT_NO_HOST"));
        assert_eq!(found[1].0, 2);
    }
}
