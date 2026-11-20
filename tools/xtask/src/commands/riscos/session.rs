//! The reference harness's part for what is typed at BASIC's prompt: the
//! calibration, then each session in [`SESSIONS`], typed into genuine RISC OS
//! 5's BASIC V and BASIC VI, and what each printed written to [`ANSWERS`],
//! which `tairix-riscos-basic`'s replay holds the interpreter to. The format
//! and the calibration that tells messages by kind are the crate's own
//! (`tairix_riscos_basic::corpus::session`).

use std::path::Path;

use tairix_riscos_basic::corpus::session::{self, Session};

use super::find;

/// The sessions, from the workspace's root.
pub(super) const SESSIONS: &str =
    "userland/apps/tairix-bridge/riscos/basic/tests/riscos5/sessions.txt";
/// The answers the harness records.
pub(super) const ANSWERS: &str = "userland/apps/tairix-bridge/riscos/basic/tests/riscos5/typed.txt";

/// Each dialect: its command and the suffix of the files it prints to.
const DIALECTS: [(&str, &str); 2] = [("BASIC", "V"), ("BASIC64", "VI")];

/// The HostFS name of session `n`'s keys, the calibration's for none.
fn keys_name(n: Option<usize>) -> String {
    n.map_or_else(|| "SessCal".to_string(), |n| format!("SessKeys{n}"))
}

/// The HostFS name session `n`, or the calibration, prints to under the
/// dialect of `suffix`.
fn printed_name(n: Option<usize>, suffix: &str) -> String {
    n.map_or_else(
        || format!("SessCal{suffix}"),
        |n| format!("Sess{suffix}{n}"),
    )
}

/// Every run: the calibration, then each session.
fn runs(sessions: &[Session]) -> impl Iterator<Item = Option<usize>> {
    std::iter::once(None).chain((0..sessions.len()).map(Some))
}

/// The sessions `text` holds.
pub(super) fn sessions(text: &str) -> Result<Vec<Session>, String> {
    session::sessions(text).map_err(|e| format!("riscos: {SESSIONS}: {e}"))
}

/// The keys the harness stages: the calibration's, then each session's.
pub(super) fn staged(sessions: &[Session]) -> Vec<(String, Vec<u8>)> {
    runs(sessions)
        .map(|n| {
            let keys = match n {
                None => session::calibration_keys(),
                Some(n) => session::keys(sessions[n].typed.iter().map(Vec::as_slice)),
            };
            (keys_name(n), keys)
        })
        .collect()
}

/// The `!Boot` commands that type every run into each dialect.
pub(super) fn run(sessions: &[Session]) -> Vec<String> {
    DIALECTS
        .iter()
        .flat_map(|&(command, suffix)| {
            runs(sessions).map(move |n| {
                format!(
                    "{command} {{ < HostFS:$.{} > HostFS:$.{} }}",
                    keys_name(n),
                    printed_name(n, suffix)
                )
            })
        })
        .collect()
}

/// Each HostFS file a run leaves.
pub(super) fn left(sessions: &[Session]) -> Vec<String> {
    runs(sessions)
        .flat_map(|n| {
            std::iter::once(keys_name(n)).chain(
                DIALECTS
                    .iter()
                    .map(move |(_, suffix)| printed_name(n, suffix)),
            )
        })
        .collect()
}

/// The answers file: the recording's provenance, then what each dialect
/// printed in each session, its messages told by the calibration's.
pub(super) fn answers(
    provenance: &[String],
    sessions: &[Session],
    hostfs: &Path,
) -> Result<String, String> {
    let read = |name: &str| {
        let path = find(hostfs, name).ok_or_else(|| format!("riscos: BASIC left no {name}"))?;
        std::fs::read(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))
    };
    let mut told: [Vec<Vec<String>>; 2] = [Vec::new(), Vec::new()];
    for ((_, suffix), told) in DIALECTS.iter().zip(&mut told) {
        let calibration = printed_name(None, suffix);
        let messages = session::messages(&read(&calibration)?)
            .map_err(|e| format!("riscos: {calibration}: {e}"))?;
        for n in 0..sessions.len() {
            told.push(session::told(
                &read(&printed_name(Some(n), suffix))?,
                &messages,
            ));
        }
    }
    let [v, vi] = told;
    Ok(session::answers(provenance, sessions, &v, &vi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calibration_runs_first_in_each_dialect_then_every_session() {
        let read =
            sessions("session a\ntype \"PRINT 1\"\nsession b\ntype \"LIST\"\n").expect("sessions");
        assert_eq!(
            run(&read),
            [
                "BASIC { < HostFS:$.SessCal > HostFS:$.SessCalV }",
                "BASIC { < HostFS:$.SessKeys0 > HostFS:$.SessV0 }",
                "BASIC { < HostFS:$.SessKeys1 > HostFS:$.SessV1 }",
                "BASIC64 { < HostFS:$.SessCal > HostFS:$.SessCalVI }",
                "BASIC64 { < HostFS:$.SessKeys0 > HostFS:$.SessVI0 }",
                "BASIC64 { < HostFS:$.SessKeys1 > HostFS:$.SessVI1 }",
            ]
        );
        let staged = staged(&read);
        assert_eq!(
            staged[1],
            ("SessKeys0".to_string(), b"PRINT 1\nQUIT\n".to_vec())
        );
        assert_eq!(staged[0].0, "SessCal");
        assert_eq!(left(&read)[..3], ["SessCal", "SessCalV", "SessCalVI"]);
    }

    #[test]
    fn a_malformed_corpus_is_refused_naming_it() {
        let refused = sessions("type \"x\"\n").expect_err("refused");
        assert!(
            refused.starts_with(&format!("riscos: {SESSIONS}: line 1")),
            "{refused}"
        );
    }
}
