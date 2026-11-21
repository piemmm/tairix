//! The reference harness's part for the VDU drivers: each case in [`CASES`]
//! run by genuine RISC OS 5's BASIC V on the screen, framed as
//! `tairix_riscos_vdu::corpus::program` frames it, and what each recorded
//! and the screen it drew written to [`ANSWERS`], which
//! `tairix-riscos-vdu`'s replay holds the drivers to.

use std::path::Path;

use tairix_riscos_vdu::corpus::{self, Case, Recorded};

use super::find;

/// The cases, from the workspace's root.
pub(super) const CASES: &str = "userland/apps/tairix-bridge/riscos/vdu/tests/riscos5/screens.txt";
/// The answers the harness records.
pub(super) const ANSWERS: &str = "userland/apps/tairix-bridge/riscos/vdu/tests/riscos5/drawn.txt";

/// The cases `text` holds.
pub(super) fn cases(text: &str) -> Result<Vec<Case>, String> {
    corpus::cases(text).map_err(|e| format!("riscos: {CASES}: {e}"))
}

/// The program run after the cases, which leaves the screen as the machine
/// booted with it — the configured mode, and the screen area no larger than
/// that needs — since what the harness records next reads the screen.
const RESTORE_NAME: &str = "ScrEnd";
const RESTORE: &str =
    "10 SYS \"OS_ReadSysInfo\",1 TO m%:MODE m%:SYS \"XOS_ChangeDynamicArea\",2,-&10000000\n";

/// Each case's program, to stage, then the program that restores the screen.
pub(super) fn staged(cases: &[Case]) -> Vec<(String, Vec<u8>)> {
    cases
        .iter()
        .enumerate()
        .map(|(n, case)| {
            (
                corpus::program_name(n),
                corpus::program(case, n).into_bytes(),
            )
        })
        .chain([(RESTORE_NAME.to_string(), RESTORE.as_bytes().to_vec())])
        .collect()
}

/// The `!Boot` commands that run every case on the screen, then restore it.
pub(super) fn run(cases: &[Case]) -> Vec<String> {
    (0..cases.len())
        .map(corpus::program_name)
        .chain([RESTORE_NAME.to_string()])
        .map(|name| corpus::run_command(&name))
        .collect()
}

/// Each HostFS file a run leaves.
pub(super) fn left(cases: &[Case]) -> Vec<String> {
    (0..cases.len())
        .flat_map(|n| {
            [
                corpus::program_name(n),
                corpus::record_name(n),
                corpus::dump_name(n),
            ]
        })
        .chain([RESTORE_NAME.to_string()])
        .collect()
}

/// What each case recorded, from the files the run left in `hostfs`.
pub(super) fn recorded(cases: &[Case], hostfs: &Path) -> Result<Vec<Recorded>, String> {
    cases
        .iter()
        .enumerate()
        .map(|(n, case)| {
            let read = |name: &str| {
                find(hostfs, name).map(|path| {
                    std::fs::read(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))
                })
            };
            let record = read(&corpus::record_name(n))
                .ok_or_else(|| format!("riscos: case {} left no record", case.name))??;
            let dump = read(&corpus::dump_name(n)).transpose()?;
            corpus::recorded(&record, dump.as_deref())
                .map_err(|e| format!("riscos: case {}: {e}", case.name))
        })
        .collect()
}

/// The answers file, and its readings alone for the search for runs of ROM
/// bytes. A screen's lines are left out of that: they are the colours the
/// case chose, written in hexadecimal digits that ROM data reads as by
/// chance, and the frame refuses a screen drawn in any font but the
/// first-party one.
pub(super) fn answers(
    provenance: &[String],
    cases: &[Case],
    hostfs: &Path,
) -> Result<(String, String), String> {
    let recorded = recorded(cases, hostfs)?;
    let readings = recorded
        .iter()
        .flat_map(|r| &r.lines)
        .fold(String::new(), |mut all, line| {
            all.push_str(line);
            all.push('\n');
            all
        });
    Ok((corpus::answers(provenance, cases, &recorded), readings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_corpus_is_refused_naming_it() {
        let refused = cases("case a\n9500 PRINT\n").expect_err("refused");
        assert!(
            refused.starts_with(&format!("riscos: {CASES}: line 2")),
            "{refused}"
        );
    }

    #[test]
    fn each_case_runs_on_the_screen_and_leaves_its_record_and_screen() {
        let read = cases("case a\n10 MODE 1\ncase b\n10 MODE 12\n").expect("read");
        assert_eq!(
            run(&read),
            [
                "BASIC -quit HostFS:$.Scr0",
                "BASIC -quit HostFS:$.Scr1",
                "BASIC -quit HostFS:$.ScrEnd"
            ]
        );
        assert_eq!(
            left(&read),
            ["Scr0", "ScrRec0", "ScrDump0", "Scr1", "ScrRec1", "ScrDump1", "ScrEnd"]
        );
        let staged = staged(&read);
        let text = String::from_utf8(staged[1].1.clone()).expect("text");
        assert!(text.contains("\n10 MODE 12\n"), "{text}");
        assert!(text.contains("HostFS:$.ScrRec1"), "{text}");
    }
}
