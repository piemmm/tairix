//! The reference harness's part for BASIC's language: each program in
//! [`PROGRAMS`] run by genuine RISC OS 5's BASIC V and BASIC VI, and what each
//! printed written to [`ANSWERS`], which `tairix-riscos-basic`'s replay holds
//! the interpreter to.
//!
//! A program opens `program NAME`; its numbered lines follow, then any `input
//! "TEXT"`, bytes its keyboard types, printable ASCII but for `\xHH`. Each
//! dialect runs it with `-quit`, what it prints redirected to a file and its
//! input read from another. The answers name each program, then give what
//! each dialect, `v` or `vi`, printed: a quoted line for each line, ending
//! with the LF CR that ended it, or `""` for nothing at all.
//!
//! A program prints its own text and numbers only. A report of an error it
//! did not trap carries the ROM's message, so it refuses the recording.

use std::fmt::Write as _;
use std::path::Path;

use tairix_riscos_basic::corpus::language::{self as corpus, Program};

use super::basic::AT_LINE;
use super::{find, quote};

/// The programs, from the workspace's root.
pub(super) const PROGRAMS: &str =
    "userland/apps/tairix-bridge/riscos/basic/tests/riscos5/programs.txt";
/// The answers the harness records.
pub(super) const ANSWERS: &str =
    "userland/apps/tairix-bridge/riscos/basic/tests/riscos5/printed.txt";

/// Each dialect: its command, its name in the answers, and the suffix of the
/// file it prints to.
const DIALECTS: [(&str, &str, &str); 2] = [("BASIC", "v", "V"), ("BASIC64", "vi", "VI")];

/// The programs `text` holds.
pub(super) fn programs(text: &str) -> Result<Vec<Program>, String> {
    corpus::programs(text).map_err(|e| format!("riscos: {PROGRAMS}: {e}"))
}

/// The HostFS name program `n` is staged under.
fn program_name(n: usize) -> String {
    format!("Lang{n}")
}

/// The HostFS name program `n`'s input is staged under.
fn input_name(n: usize) -> String {
    format!("LangKeys{n}")
}

/// The HostFS name program `n` prints to under the dialect of `suffix`.
fn printed_name(n: usize, suffix: &str) -> String {
    format!("Lang{suffix}{n}")
}

/// The files the harness stages: each program, and its input where it has
/// any.
pub(super) fn staged(programs: &[Program]) -> Vec<(String, Vec<u8>)> {
    let mut staged = Vec::with_capacity(2 * programs.len());
    for (n, program) in programs.iter().enumerate() {
        staged.push((program_name(n), program.text.clone()));
        if !program.input.is_empty() {
            staged.push((input_name(n), program.input.clone()));
        }
    }
    staged
}

/// The `!Boot` commands that run every program in each dialect.
pub(super) fn run(programs: &[Program]) -> Vec<String> {
    let mut commands = Vec::with_capacity(DIALECTS.len() * programs.len());
    for (command, _, suffix) in DIALECTS {
        for (n, program) in programs.iter().enumerate() {
            let input = if program.input.is_empty() {
                String::new()
            } else {
                format!("< HostFS:$.{} ", input_name(n))
            };
            commands.push(format!(
                "{command} -quit HostFS:$.{} {{ {input}> HostFS:$.{} }}",
                program_name(n),
                printed_name(n, suffix)
            ));
        }
    }
    commands
}

/// Each HostFS file a run leaves.
pub(super) fn left(programs: &[Program]) -> Vec<String> {
    let mut names = Vec::with_capacity(4 * programs.len());
    for (n, program) in programs.iter().enumerate() {
        names.push(program_name(n));
        if !program.input.is_empty() {
            names.push(input_name(n));
        }
        names.extend(
            DIALECTS
                .iter()
                .map(|(_, _, suffix)| printed_name(n, suffix)),
        );
    }
    names
}

/// The answers file: the recording's provenance, then what each dialect
/// printed for each program, from the files they left in `hostfs`.
pub(super) fn answers(
    provenance: &[String],
    programs: &[Program],
    hostfs: &Path,
) -> Result<String, String> {
    let mut out = tairix_riscos_kernel::corpus::answers_head(provenance);
    for (n, program) in programs.iter().enumerate() {
        let _ = writeln!(out, "program {}", program.name);
        for (_, dialect, suffix) in DIALECTS {
            let name = printed_name(n, suffix);
            let path =
                find(hostfs, &name).ok_or_else(|| format!("riscos: BASIC left no {name}"))?;
            let printed =
                std::fs::read(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
            if printed.windows(AT_LINE.len()).any(|w| w == AT_LINE) {
                return Err(format!(
                    "riscos: {} met an error it did not trap under {dialect}",
                    program.name
                ));
            }
            if printed.is_empty() {
                let _ = writeln!(out, "{dialect} \"\"");
            }
            for line in printed_lines(&printed) {
                let _ = writeln!(out, "{dialect} {}", quote(line));
            }
        }
    }
    Ok(out)
}

/// `printed` cut after each LF CR, BASIC's newline.
fn printed_lines(printed: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = printed;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .windows(2)
            .position(|w| w == b"\n\r")
            .map_or(rest.len(), |at| at + 2);
        let (line, after) = rest.split_at(end);
        rest = after;
        Some(line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_corpus_is_refused_naming_it() {
        let refused = programs("program a\n20 PRINT\n10 PRINT\n").expect_err("refused");
        assert!(
            refused.starts_with(&format!("riscos: {PROGRAMS}: line 3")),
            "{refused}"
        );
    }

    #[test]
    fn each_program_runs_in_both_dialects_with_its_redirection() {
        let read = programs("program a\n10 PRINT\nprogram b\n10 INPUT a\ninput \"1\\x0D\"\n")
            .expect("read");
        assert_eq!(
            run(&read),
            [
                "BASIC -quit HostFS:$.Lang0 { > HostFS:$.LangV0 }",
                "BASIC -quit HostFS:$.Lang1 { < HostFS:$.LangKeys1 > HostFS:$.LangV1 }",
                "BASIC64 -quit HostFS:$.Lang0 { > HostFS:$.LangVI0 }",
                "BASIC64 -quit HostFS:$.Lang1 { < HostFS:$.LangKeys1 > HostFS:$.LangVI1 }",
            ]
        );
        assert_eq!(
            left(&read),
            [
                "Lang0",
                "LangV0",
                "LangVI0",
                "Lang1",
                "LangKeys1",
                "LangV1",
                "LangVI1"
            ]
        );
        let staged = staged(&read);
        assert_eq!(staged.len(), 3);
        assert_eq!(staged[2], ("LangKeys1".to_string(), b"1\r".to_vec()));
    }

    #[test]
    fn printed_text_is_cut_after_each_newline() {
        let cut: Vec<&[u8]> = printed_lines(b"a\n\rb\n\r\n\rc").collect();
        assert_eq!(cut, [&b"a\n\r"[..], b"b\n\r", b"\n\r", b"c"]);
        assert_eq!(printed_lines(b"").count(), 0);
    }
}
