//! `cargo xtask shaders` implementation.
//!
//! The pinned shader set (`lib/shaders`) is each module's Rust source and the
//! SPIR-V it builds, committed under `lib/shaders/spv/` for the desktop to
//! load. This command builds every module of the set from its source —
//! through `lib/spirv`'s builder, which answers a module only once the
//! validator admits it — and compares the bytes with the committed file.
//!
//! - `cargo xtask shaders` (part of `cargo xtask ci`) fails closed where a
//!   committed module has drifted from its source or is missing, and where a
//!   file under `spv/` names no module of the set.
//! - `cargo xtask shaders --write` commits what the sources build, and takes
//!   away a file no module names.
//!
//! One source builds one sequence of bytes on every host, so the guard is
//! meaningful. A new module's file is committed empty first, since the set
//! compiles its committed bytes in; `--write` then fills it.

use std::collections::BTreeSet;
use std::path::Path;

use crate::Context;

/// Where the set is committed.
const SPV: &str = "lib/shaders/spv";

/// The extension every committed module's file carries.
const EXTENSION: &str = "spv";

pub fn run(ctx: &Context, args: &[std::ffi::OsString]) -> Result<(), String> {
    let write = match args {
        [] => false,
        [flag] if flag == "--write" => true,
        _ => return Err("shaders: usage: cargo xtask shaders [--write]".to_string()),
    };
    let dir = ctx.workspace_root.join(SPV);
    let mut named = BTreeSet::new();
    let mut drifted = Vec::new();
    for pinned in tairix_shaders::SET {
        let built = (pinned.build)()
            .map_err(|error| format!("shaders: {} does not build: {error}", pinned.name))?;
        let file = format!("{}.{EXTENSION}", pinned.name);
        let path = dir.join(&file);
        let bytes = built.module().bytes();
        if write {
            std::fs::write(&path, bytes)
                .map_err(|e| format!("shaders: cannot write {}: {e}", path.display()))?;
        } else if std::fs::read(&path).ok().as_deref() != Some(bytes) {
            drifted.push(file.clone());
        }
        named.insert(file);
    }
    let strays = strays(&dir, &named)?;
    if write {
        noteln!("xtask: [shaders --write] {SPV}");
        for stray in &strays {
            let path = dir.join(stray);
            std::fs::remove_file(&path)
                .map_err(|e| format!("shaders: cannot remove {}: {e}", path.display()))?;
        }
        return Ok(());
    }
    noteln!("xtask: [shaders] {SPV}");
    if drifted.is_empty() && strays.is_empty() {
        return Ok(());
    }
    Err(format!(
        "shaders: under `{SPV}`, drifted from their sources: [{}]; naming no module: [{}]; \
         run `cargo xtask shaders --write` and commit the result.",
        drifted.join(", "),
        strays.join(", ")
    ))
}

/// The files under `dir` that are not among `named`.
fn strays(dir: &Path, named: &BTreeSet<String>) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("shaders: cannot read {}: {e}", dir.display()))?;
    let mut strays = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("shaders: cannot read {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !named.contains(&name) {
            strays.push(name);
        }
    }
    strays.sort();
    Ok(strays)
}
