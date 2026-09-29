//! `cargo xtask cfg-check` implementation.
//!
//! the charter forbids target-conditional compilation —
//! `#[cfg(target_arch = "…")]`, `#[cfg(target_pointer_width = …)]`, and
//! equivalents — everywhere except the architecture ports
//! (`kernel/arch/<target>/`) and the build glue (`.cargo/`,
//! `tools/mkimage/`, `tools/xtask/`). Conditioning behaviour on the
//! target anywhere else means the modularity boundary (the Arch HAL) has
//! leaked, so it is a defect.
//!
//! This checker walks every tracked `.rs` source file in the workspace
//! and fails if a `cfg`/`cfg_attr` predicate names `target_arch` or
//! `target_pointer_width` outside the allow-list. A small, explicit
//! [`GRANDFATHERED`] list pins the directories that violate the rule
//! *today*; each is a tracked defect to be burned down (see `PLAN.md`),
//! and the set may only shrink — a new file under a grandfathered tree
//! is still rejected unless the tree itself is listed.
//!
//! Inside a freestanding port the allow-list stops applying and a second
//! rule takes over: a `cfg` naming `target_arch` must also name
//! `target_os`. Gating on the architecture alone selects the bare-metal
//! body in a *host* build of the port too — where the instruction is
//! privileged, and where the UB oracle cannot interpret it at all — so
//! the omission only shows up on the machine whose architecture the port
//! names, and passes everywhere else.
//!
//! A third rule holds everywhere but the build tooling: no attribute `cfg`
//! or `cfg_attr` may name `miri`. One that does excludes or alters code
//! under the UB oracle where its stage cannot report it; every such
//! exclusion lives, with its reason, in the miri registry. The `cfg!(miri)`
//! expression that scales a test's budget is not an attribute and stays.

use std::path::Path;

/// Directory prefixes (workspace-relative, `/`-separated) where
/// target-conditional compilation is permitted by.
const ALLOWED: &[&str] = &["kernel/arch/", ".cargo/", "tools/mkimage/", "tools/xtask/"];

/// Directory prefixes that violate *today* and are tolerated until
/// the burn-down lands (`PLAN.md`). This list is append-never: it may
/// only shrink. Each entry is a tracked defect, not a sanctioned pattern.
///
/// Empty: every directory that named the target instruction set inline has
/// been migrated. `kernel/tairix-kernel` was the last entry; it now gates
/// its freestanding body on the build-script-emitted `freestanding` cfg
/// (`kernel/tairix-kernel/build.rs`) instead of `cfg(target_arch = …)`.
const GRANDFATHERED: &[&str] = &[];

/// The cfg predicates the charter forbids outside the allow-list.
const FORBIDDEN_KEYS: &[&str] = &["target_arch", "target_pointer_width"];

/// The ports whose target is freestanding, where an architecture gate
/// must also name `target_os`.
///
/// `kernel/arch/wasm32` is absent deliberately: its target reports
/// `target_os = "unknown"`, so pairing the gate there would disable the
/// real body rather than the host one.
const FREESTANDING_PORTS: &[&str] = &[
    "kernel/arch/x86_64/",
    "kernel/arch/aarch64/",
    "kernel/arch/riscv64/",
];

/// The build tooling, exempt from the interpreter-gate rule: it holds the
/// miri registry and this checker's own spellings of the gates it catches.
const INTERPRETER_GATE_EXEMPT: &str = "tools/xtask/";

/// Which rule an occurrence breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Target-conditional compilation outside the ports and build glue.
    TargetConditional,
    /// A freestanding port's architecture gate that omits `target_os`,
    /// so it also selects the bare-metal body in a host build.
    ArchGateWithoutOs,
    /// An attribute `cfg` naming `miri`: an exclusion from the UB oracle
    /// the miri registry does not record.
    InterpreterGate,
}

/// A single offending occurrence: a workspace-relative path and the
/// 1-based line number that names a forbidden predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub rule: Rule,
}

/// Scan the workspace rooted at `root` and return every violation
/// outside the allow-list and grandfather list.
pub fn scan(root: &Path) -> Result<Vec<Violation>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("cfg-check: cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cfg-check: dir entry: {e}"))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("cfg-check: file type {}: {e}", path.display()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_dir() {
                if name == "target" || name == ".git" {
                    continue;
                }
                dirs.push(path);
            } else if file_type.is_file() && name.ends_with(".rs") {
                let rel = relative(root, &path);
                if rel.starts_with(INTERPRETER_GATE_EXEMPT) {
                    continue;
                }
                let src = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cfg-check: cannot read {}: {e}", path.display()))?;
                if is_freestanding_port(&rel) {
                    scan_lines(
                        &src,
                        &rel,
                        (Rule::ArchGateWithoutOs, arch_gate_lacks_os),
                        &mut out,
                    );
                } else if !is_allowed(&rel) {
                    scan_lines(
                        &src,
                        &rel,
                        (Rule::TargetConditional, line_offends),
                        &mut out,
                    );
                }
                let lines: Vec<&str> = src.lines().collect();
                out.extend(interpreter_gates(&src).into_iter().map(|line| {
                    Violation {
                        path: rel.clone(),
                        line,
                        text: lines
                            .get(line - 1)
                            .map_or("", |text| text.trim())
                            .to_string(),
                        rule: Rule::InterpreterGate,
                    }
                }));
            }
        }
    }
    out.retain(|v| !is_grandfathered(&v.path));
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    Ok(out)
}

/// Record every line of `src` that `offends` under `rule`.
fn scan_lines(
    src: &str,
    rel: &str,
    (rule, offends): (Rule, fn(&str) -> bool),
    out: &mut Vec<Violation>,
) {
    for (idx, line) in src.lines().enumerate() {
        if offends(line) {
            out.push(Violation {
                path: rel.to_string(),
                line: idx + 1,
                text: line.trim().to_string(),
                rule,
            });
        }
    }
}

/// The 1-based lines of every attribute `cfg`/`cfg_attr` in `src` whose
/// predicate names `miri`.
///
/// Read with whitespace squeezed out, because such an attribute carrying a
/// reason is usually split across lines; comment lines are dropped first.
/// `cfg!(miri)` never matches: its `!` stands between the name and the
/// parenthesis.
fn interpreter_gates(src: &str) -> Vec<usize> {
    let mut squeezed = String::new();
    let mut line_of = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for c in line.chars().filter(|c| !c.is_whitespace()) {
            squeezed.push(c);
            line_of.extend(core::iter::repeat_n(idx + 1, c.len_utf8()));
        }
    }
    let mut lines = Vec::new();
    for keyword in ["cfg(", "cfg_attr("] {
        for (at, _) in squeezed.match_indices(keyword) {
            if squeezed[..at].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            if names_miri(first_argument(&squeezed[at + keyword.len()..])) {
                lines.push(line_of[at]);
            }
        }
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// The first argument of a call whose opening parenthesis `rest` follows:
/// everything up to its first comma or closing parenthesis at depth zero.
fn first_argument(rest: &str) -> &str {
    let mut depth = 0usize;
    for (at, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' | ',' if depth == 0 => return &rest[..at],
            ')' => depth -= 1,
            _ => {}
        }
    }
    rest
}

/// Whether `predicate` names the `miri` cfg as a whole identifier outside
/// any quoted value: `feature = "miri-probe"` names a feature, not the cfg.
fn names_miri(predicate: &str) -> bool {
    predicate.split('"').step_by(2).any(|bare| {
        bare.match_indices("miri").any(|(at, word)| {
            !bare[..at].chars().next_back().is_some_and(is_ident)
                && !bare[at + word.len()..].chars().next().is_some_and(is_ident)
        })
    })
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A line offends when it mentions `cfg` and a forbidden predicate key.
/// Pairing the two keeps plain prose (a doc comment that merely names an
/// architecture) from tripping the check while still catching every
/// `cfg`/`cfg_attr`/`cfg!` form.
fn line_offends(line: &str) -> bool {
    line.contains("cfg") && FORBIDDEN_KEYS.iter().any(|k| line.contains(k))
}

/// Inside a freestanding port: a `cfg` gating on `target_arch` alone.
///
/// Comments are skipped — one gates no compilation, and a wrapped
/// sentence quoting a predicate would otherwise read as an offence.
/// Line-based like [`line_offends`], so a predicate split across lines
/// reads as unpaired; every gate in the ports fits on one line today.
fn arch_gate_lacks_os(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return false;
    }
    line.contains("cfg") && line.contains("target_arch") && !line.contains("target_os")
}

fn is_allowed(rel: &str) -> bool {
    ALLOWED.iter().any(|p| rel.starts_with(p))
}

fn is_freestanding_port(rel: &str) -> bool {
    FREESTANDING_PORTS.iter().any(|p| rel.starts_with(p))
}

fn is_grandfathered(rel: &str) -> bool {
    GRANDFATHERED.iter().any(|p| rel.starts_with(p))
}

fn relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Run the check, printing a report and returning an error if any
/// non-grandfathered violation remains.
pub fn run(root: &Path) -> Result<(), String> {
    use std::fmt::Write as _;
    let violations = scan(root)?;
    if violations.is_empty() {
        return Ok(());
    }
    let mut msg = String::new();
    for (rule, heading) in [
        (
            Rule::TargetConditional,
            "cfg-check: target-conditional compilation is forbidden outside \
             the architecture ports and build glue (AGENTS.md §17.2):",
        ),
        (
            Rule::ArchGateWithoutOs,
            "cfg-check: a freestanding port's `target_arch` gate must also name \
             `target_os` (AGENTS.md §17.2) — gating on the architecture alone \
             selects the bare-metal body in a host build of the port too:",
        ),
        (
            Rule::InterpreterGate,
            "cfg-check: an attribute `cfg` naming `miri` excludes code from the UB \
             oracle where its stage cannot report it; record the exclusion and its \
             reason in the miri registry (`tools/xtask/src/commands/miri.rs`) \
             instead (AGENTS.md §19.11):",
        ),
    ] {
        let mut hit = violations.iter().filter(|v| v.rule == rule).peekable();
        if hit.peek().is_none() {
            continue;
        }
        let _ = writeln!(msg, "{heading}");
        for v in hit {
            let _ = writeln!(msg, "  {}:{}: {}", v.path, v.line, v.text);
        }
    }
    Err(msg)
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
    fn workspace_is_clean_modulo_grandfathered() {
        let root = workspace_root();
        let violations = scan(&root).expect("scan");
        assert!(
            violations.is_empty(),
            "unexpected cfg-check violations: {violations:#?}"
        );
    }

    /// The shape that kept two sweeps out of the UB oracle while its
    /// registry reported their crate whole: a reasoned `cfg_attr` split
    /// across lines.
    #[test]
    fn an_interpreter_gate_is_caught_however_it_is_spelled() {
        let src = "\
fn body() {}

#[cfg_attr(
    miri,
    ignore = \"too slow\"
)]
#[test]
fn slow() {}

#[cfg(not(miri))]
mod host_only {}
#[cfg(all(test, not(miri)))]
fn one() {}
#![cfg_attr(miri, allow(dead_code))]
";
        assert_eq!(interpreter_gates(src), [3, 10, 12, 14]);
    }

    /// A budget scaled for the interpreter still runs under it, and a
    /// comment, a feature's name or an unrelated call gates nothing.
    #[test]
    fn a_scaled_budget_or_a_mention_is_no_interpreter_gate() {
        let src = "\
const STEPS: u32 = if cfg!(miri) { 24 } else { 600 };
// #[cfg_attr(miri, ignore)] belongs in the registry.
/// Runs under miri at a smaller budget.
#[cfg(feature = \"miri-probe\")]
#[cfg(test)]
fn my_cfg(miri: u32) -> u32 { miri }
";
        assert!(interpreter_gates(src).is_empty());
    }

    #[test]
    fn arch_ports_are_allowed() {
        assert!(is_allowed("kernel/arch/x86_64/src/preempt.rs"));
        assert!(is_allowed("tools/xtask/src/commands/cfg_check.rs"));
        assert!(!is_allowed("kernel/mem/src/lib.rs"));
    }

    #[test]
    fn detects_cfg_target_arch_only_with_cfg() {
        assert!(line_offends("#[cfg(target_arch = \"x86_64\")]"));
        assert!(line_offends(
            "#![cfg_attr(target_pointer_width = \"64\", x)]"
        ));
        assert!(!line_offends("// runs on the x86_64 target_arch in prose"));
        assert!(!line_offends("#[cfg(target_os = \"none\")]"));
    }

    #[test]
    fn freestanding_ports_take_the_arch_gate_rule() {
        assert!(is_freestanding_port(
            "kernel/arch/riscv64/src/kernel_arch.rs"
        ));
        // Its target is `unknown`, not `none`, so the pairing does not apply.
        assert!(!is_freestanding_port("kernel/arch/wasm32/src/lib.rs"));
        // Arch-neutral, and not a port.
        assert!(!is_freestanding_port("kernel/arch/api/src/lib.rs"));
    }

    /// The exact shape that let a host build execute `rdtsc` and took the
    /// UB oracle's whole run down on an x86_64 runner while passing on
    /// every other host.
    #[test]
    fn an_arch_gate_without_target_os_is_caught() {
        assert!(arch_gate_lacks_os(
            "        #[cfg(target_arch = \"x86_64\")]"
        ));
        assert!(arch_gate_lacks_os("#[cfg(not(target_arch = \"riscv64\"))]"));
        assert!(!arch_gate_lacks_os(
            "#[cfg(all(target_arch = \"x86_64\", target_os = \"none\"))]"
        ));
        assert!(!arch_gate_lacks_os(
            "#[cfg(not(all(target_arch = \"aarch64\", target_os = \"none\")))]"
        ));
        // A gate on the OS alone is already host-safe.
        assert!(!arch_gate_lacks_os(
            "#[cfg(any(target_os = \"none\", doc))]"
        ));
    }

    /// A comment gates no compilation, and a wrapped sentence quoting a
    /// predicate must not read as an offence.
    #[test]
    fn a_comment_quoting_a_gate_is_not_an_offence() {
        assert!(!arch_gate_lacks_os("// the surrounding `cfg(target_arch ="));
        assert!(!arch_gate_lacks_os(
            "//! modules are gated on `cfg(target_arch = \"aarch64\")`"
        ));
        assert!(!arch_gate_lacks_os(
            "    /// Reads `0` unless `cfg(target_arch = \"riscv64\")`."
        ));
    }
}
