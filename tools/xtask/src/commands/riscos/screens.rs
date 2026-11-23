//! The reference harness's part for the VDU drivers and the Draw modules:
//! each case of each corpus in [`CORPORA`] run by genuine RISC OS 5's BASIC V
//! on the screen, framed as `tairix_riscos_vdu::corpus::program` frames it
//! and numbered on from the corpus before it, and what each recorded and
//! the screen it drew written to its corpus's answers, which that corpus's
//! replay holds the modules to.

use std::path::Path;

use tairix_riscos_vdu::corpus::{self, Case, Recorded};

use super::find;

/// A corpus of screen cases: its cases and the answers the harness records,
/// from the workspace's root.
pub(super) struct Corpus {
    cases: &'static str,
    answers: &'static str,
}

/// The screen corpora, in the order their cases are numbered.
pub(super) const CORPORA: [Corpus; 2] = [
    Corpus {
        cases: "userland/apps/tairix-bridge/riscos/vdu/tests/riscos5/screens.txt",
        answers: "userland/apps/tairix-bridge/riscos/vdu/tests/riscos5/drawn.txt",
    },
    Corpus {
        cases: "userland/apps/tairix-bridge/riscos/draw/tests/riscos5/screens.txt",
        answers: "userland/apps/tairix-bridge/riscos/draw/tests/riscos5/drawn.txt",
    },
];

/// The screen corpora as read: each one's text and its cases, in
/// [`CORPORA`]'s order.
pub(super) struct Screens {
    texts: Vec<String>,
    cases: Vec<Vec<Case>>,
}

impl Screens {
    /// Each corpus, read from the workspace at `root`.
    pub(super) fn read(root: &Path) -> Result<Self, String> {
        let mut texts = Vec::new();
        let mut cases = Vec::new();
        for corpus in &CORPORA {
            let text = std::fs::read_to_string(root.join(corpus.cases))
                .map_err(|e| format!("riscos: {}: {e}", corpus.cases))?;
            cases.push(parsed(corpus, &text)?);
            texts.push(text);
        }
        Ok(Self { texts, cases })
    }

    /// Each corpus's text.
    pub(super) fn texts(&self) -> impl Iterator<Item = &str> {
        self.texts.iter().map(String::as_str)
    }

    /// How many cases the corpora hold between them.
    pub(super) fn len(&self) -> usize {
        self.cases.iter().map(Vec::len).sum()
    }

    /// Every case, numbered across the corpora.
    fn all(&self) -> impl Iterator<Item = (usize, &Case)> {
        self.cases.iter().flatten().enumerate()
    }
}

/// The cases `text` holds, refused naming `corpus`.
fn parsed(corpus: &Corpus, text: &str) -> Result<Vec<Case>, String> {
    corpus::cases(text).map_err(|e| format!("riscos: {}: {e}", corpus.cases))
}

/// The program run after the cases, which leaves the screen as the machine
/// booted with it — the configured mode, and the screen area no larger than
/// that needs — since what the harness records next reads the screen.
const RESTORE_NAME: &str = "ScrEnd";
const RESTORE: &str =
    "10 SYS \"OS_ReadSysInfo\",1 TO m%:MODE m%:SYS \"XOS_ChangeDynamicArea\",2,-&10000000\n";

/// Each case's program, to stage, then the program that restores the screen.
pub(super) fn staged(screens: &Screens) -> Vec<(String, Vec<u8>)> {
    screens
        .all()
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
pub(super) fn run(screens: &Screens) -> Vec<String> {
    (0..screens.len())
        .map(corpus::program_name)
        .chain([RESTORE_NAME.to_string()])
        .map(|name| corpus::run_command(&name))
        .collect()
}

/// Each HostFS file a run leaves.
pub(super) fn left(screens: &Screens) -> Vec<String> {
    (0..screens.len())
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

/// What case `n` recorded, from the files the run left in `hostfs`.
fn recorded(n: usize, case: &Case, hostfs: &Path) -> Result<Recorded, String> {
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
}

/// Each corpus's answers file, and where it goes.
type Answers = Vec<(&'static str, String)>;

/// Each corpus's answers file and where it goes, and every reading alone
/// for the search for runs of ROM bytes. A screen's lines are left out of
/// that: they are the colours the case chose, written in hexadecimal digits
/// that ROM data reads as by chance, and the frame refuses a screen drawn in
/// any font but the first-party one.
pub(super) fn answers(
    provenance: &[String],
    screens: &Screens,
    hostfs: &Path,
) -> Result<(Answers, String), String> {
    let mut files = Vec::new();
    let mut readings = String::new();
    let mut n = 0;
    for (corpus, cases) in CORPORA.iter().zip(&screens.cases) {
        let mut recordings = Vec::new();
        for case in cases {
            let recording = recorded(n, case, hostfs)?;
            for line in &recording.lines {
                readings.push_str(line);
                readings.push('\n');
            }
            recordings.push(recording);
            n += 1;
        }
        files.push((
            corpus.answers,
            corpus::answers(provenance, cases, &recordings),
        ));
    }
    Ok((files, readings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screens(texts: [&str; 2]) -> Screens {
        let cases = CORPORA
            .iter()
            .zip(texts)
            .map(|(corpus, text)| parsed(corpus, text).expect("read"))
            .collect();
        Screens {
            texts: texts.map(String::from).to_vec(),
            cases,
        }
    }

    #[test]
    fn a_malformed_corpus_is_refused_naming_it() {
        let refused = parsed(&CORPORA[1], "case a\n9500 PRINT\n").expect_err("refused");
        assert!(
            refused.starts_with(&format!("riscos: {}: line 2", CORPORA[1].cases)),
            "{refused}"
        );
    }

    #[test]
    fn each_case_runs_on_the_screen_numbered_across_the_corpora() {
        let read = screens([
            "case a\n10 MODE 1\ncase b\n10 MODE 12\n",
            "case c\n10 MODE 28\n",
        ]);
        assert_eq!(read.len(), 3);
        assert_eq!(
            run(&read),
            [
                "BASIC -quit HostFS:$.Scr0",
                "BASIC -quit HostFS:$.Scr1",
                "BASIC -quit HostFS:$.Scr2",
                "BASIC -quit HostFS:$.ScrEnd"
            ]
        );
        assert_eq!(
            left(&read),
            [
                "Scr0", "ScrRec0", "ScrDump0", "Scr1", "ScrRec1", "ScrDump1", "Scr2", "ScrRec2",
                "ScrDump2", "ScrEnd"
            ]
        );
        let staged = staged(&read);
        let text = String::from_utf8(staged[2].1.clone()).expect("text");
        assert!(text.contains("\n10 MODE 28\n"), "{text}");
        assert!(text.contains("HostFS:$.ScrRec2"), "{text}");
    }
}
