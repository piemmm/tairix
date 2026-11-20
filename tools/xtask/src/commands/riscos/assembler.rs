//! The reference harness's part for BASIC's assembler: each line in [`LINES`]
//! assembled alone by genuine RISC OS 5's BASIC V and BASIC VI, and what each
//! laid down written to [`ANSWERS`], which `tairix-riscos-basic`'s replay holds
//! the assembler to.
//!
//! The lines run in programs of [`CHUNK`] lines, each built by the corpus's
//! own probe program and writing what its lines laid down to a file of its
//! own.

use std::path::Path;

use tairix_riscos_basic::corpus::assembler::{self, Line};

use super::find;

/// The lines, from the workspace's root.
pub(super) const LINES: &str =
    "userland/apps/tairix-bridge/riscos/basic/tests/riscos5/assembler.txt";
/// The answers the harness records.
pub(super) const ANSWERS: &str =
    "userland/apps/tairix-bridge/riscos/basic/tests/riscos5/assembled.txt";

/// The most lines one program assembles: a quarter of the largest program a
/// probe has had BASIC V load.
const CHUNK: usize = 1000;

/// Each dialect: its command and the suffix of its files.
const DIALECTS: [(&str, &str); 2] = [("BASIC", "V"), ("BASIC64", "VI")];

/// The HostFS name of chunk `n`'s program under the dialect of `suffix`.
fn program_name(suffix: &str, n: usize) -> String {
    format!("Asm{suffix}{n}")
}

/// The HostFS name chunk `n` writes what it laid down to.
fn laid_name(suffix: &str, n: usize) -> String {
    format!("Laid{suffix}{n}")
}

/// The lines `text` holds.
pub(super) fn lines(text: &str) -> Result<Vec<Line>, String> {
    assembler::lines(text).map_err(|e| format!("riscos: {LINES}: {e}"))
}

/// The programs the harness stages: each dialect's, chunk by chunk.
pub(super) fn staged(lines: &[Line]) -> Vec<(String, Vec<u8>)> {
    DIALECTS
        .iter()
        .flat_map(|&(_, suffix)| {
            lines.chunks(CHUNK).enumerate().map(move |(n, chunk)| {
                let output = format!("HostFS:$.{}", laid_name(suffix, n));
                (
                    program_name(suffix, n),
                    assembler::program(chunk, &output).into_bytes(),
                )
            })
        })
        .collect()
}

/// The `!Boot` commands that run every chunk in each dialect.
pub(super) fn run(lines: &[Line]) -> Vec<String> {
    let chunks = lines.len().div_ceil(CHUNK);
    DIALECTS
        .iter()
        .flat_map(|&(command, suffix)| {
            (0..chunks)
                .map(move |n| format!("{command} -quit HostFS:$.{}", program_name(suffix, n)))
        })
        .collect()
}

/// Each HostFS file a run leaves.
pub(super) fn left(lines: &[Line]) -> Vec<String> {
    let chunks = lines.len().div_ceil(CHUNK);
    DIALECTS
        .iter()
        .flat_map(|&(_, suffix)| {
            (0..chunks).flat_map(move |n| [program_name(suffix, n), laid_name(suffix, n)])
        })
        .collect()
}

/// The answers file: the recording's provenance, then each line and what
/// each dialect laid down, from the files they left in `hostfs`.
pub(super) fn answers(
    provenance: &[String],
    lines: &[Line],
    hostfs: &Path,
) -> Result<String, String> {
    let mut laid: [Vec<String>; 2] = [
        Vec::with_capacity(lines.len()),
        Vec::with_capacity(lines.len()),
    ];
    for ((_, suffix), laid) in DIALECTS.iter().zip(&mut laid) {
        for (n, chunk) in lines.chunks(CHUNK).enumerate() {
            let name = laid_name(suffix, n);
            let path =
                find(hostfs, &name).ok_or_else(|| format!("riscos: BASIC left no {name}"))?;
            let written =
                std::fs::read(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
            laid.extend(
                assembler::results(&written, chunk.len())
                    .map_err(|e| format!("riscos: {name}: {e}"))?,
            );
        }
    }
    let [v, vi] = laid;
    Ok(assembler::answers(provenance, lines, &v, &vi))
}

/// The bytes each answer of `answers` says a line laid down, which the
/// recording's check for a ROM's bytes reads as bytes.
pub(super) fn laid(answers: &str) -> Vec<Vec<u8>> {
    answers
        .lines()
        .filter_map(|line| {
            line.strip_prefix("= ")
                .or_else(|| line.strip_prefix("v "))
                .or_else(|| line.strip_prefix("vi "))
        })
        .map(|answer| {
            answer
                .split(' ')
                .filter_map(|hex| u8::from_str_radix(hex, 16).ok())
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn what_each_line_laid_is_read_back_as_bytes() {
        assert_eq!(
            laid("at MOV\n= 01 00 A0 E3\nat X\nv E1\nvi 41 42\n"),
            [vec![1, 0, 0xA0, 0xE3], vec![0xE1], vec![0x41, 0x42]]
        );
    }

    fn corpus(count: usize) -> Vec<Line> {
        let text = (0..count).fold(String::new(), |mut text, n| {
            let _ = writeln!(text, "at MOV R0,#{n}");
            text
        });
        lines(&text).expect("lines")
    }

    #[test]
    fn each_dialect_runs_every_chunk_and_leaves_its_program_and_answers() {
        let lines = corpus(CHUNK + 1);
        let staged = staged(&lines);
        assert_eq!(staged.len(), 4);
        assert_eq!(staged[1].0, "AsmV1");
        assert!(String::from_utf8_lossy(&staged[1].1)
            .starts_with("10 DIM C% 300:F%=OPENOUT\"HostFS:$.LaidV1\""));
        assert_eq!(
            run(&lines),
            [
                "BASIC -quit HostFS:$.AsmV0",
                "BASIC -quit HostFS:$.AsmV1",
                "BASIC64 -quit HostFS:$.AsmVI0",
                "BASIC64 -quit HostFS:$.AsmVI1"
            ]
        );
        assert_eq!(
            left(&lines),
            ["AsmV0", "LaidV0", "AsmV1", "LaidV1", "AsmVI0", "LaidVI0", "AsmVI1", "LaidVI1"]
        );
    }

    #[test]
    fn the_answers_join_each_dialect_s_chunks_in_order() {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "riscos-assembler");
        let lines = corpus(CHUNK + 1);
        let first = (0..CHUNK).fold(String::new(), |mut first, i| {
            let _ = writeln!(first, "{} E1", assembler::line_number(i));
            first
        });
        for suffix in ["V", "VI"] {
            std::fs::write(scratch.path().join(format!("Laid{suffix}0,ffd")), &first)
                .expect("write");
        }
        std::fs::write(scratch.path().join("LaidV1,ffd"), "30 01 00 A0 E3\n").expect("write");
        std::fs::write(scratch.path().join("LaidVI1,ffd"), "30 E2\n").expect("write");
        let text = answers(&["recorded".into()], &lines, scratch.path()).expect("answers");
        let back = assembler::recorded(&text).expect("recorded");
        assert_eq!(back.len(), CHUNK + 1);
        assert_eq!(back[0].v, "E1");
        assert_eq!(
            (back[CHUNK].v.as_str(), back[CHUNK].vi.as_str()),
            ("01 00 A0 E3", "E2")
        );
        std::fs::remove_file(scratch.path().join("LaidVI1,ffd")).expect("remove");
        assert!(
            answers(&[], &lines, scratch.path()).is_err(),
            "a chunk left nothing"
        );
    }
}
