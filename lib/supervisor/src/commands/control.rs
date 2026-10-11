//! Control commands: the ones that change boot state.
//!
//! Each audits its intent through [`crate::SupervisorHost::audit`] before
//! acting and fails closed. `mount` performs the *real* root mount, asking
//! for the passphrase an encrypted root needs — there is no command that
//! bypasses the passphrase or reveals key material.

use tairix_abi::boot::MAX_ROOT_PASSPHRASE_LEN;
use tairix_abi::BootSession;
use tairix_util::secret::Wiped;

use crate::commands::arg_str;
use crate::dispatch::{Flow, Session};
use crate::repl::{read_line, LineOutcome};
use crate::{MountOutcome, RootLock, SupervisorEvent, SupervisorExit};

/// The one-line spelling of `continue` shown when an operand is refused.
const CONTINUE_USAGE: &str = "usage: continue [text|gui]";

/// `continue` / `boot` — leave the Supervisor and resume the normal boot,
/// optionally choosing which login session this one boot starts.
///
/// A bare `continue` makes no choice ([`BootSession::Unset`]) and the stored
/// default decides. `text` (or `console`) and `gui` (or `graphical` /
/// `desktop`) override it for this boot only, matched case-insensitively.
///
/// An unrecognised operand, or more than one, is refused with the usage line
/// and the REPL stays open: an ambiguous instruction must never resume the
/// boot into a session the operator did not ask for.
pub fn cmd_continue(args: &[&[u8]], session: &mut Session<'_>) -> Flow {
    let choice = match args.get(1..).unwrap_or(&[]) {
        [] => BootSession::Unset,
        [word] => {
            let Some(choice) = parse_session(word) else {
                session.out.write_str("continue: unknown session: ");
                session.out.write_bytes(word);
                session.out.newline();
                session.out.line(CONTINUE_USAGE);
                return Flow::Stay;
            };
            choice
        }
        _ => {
            session.out.line("continue: one session at most.");
            session.out.line(CONTINUE_USAGE);
            return Flow::Stay;
        }
    };
    session.host.audit(SupervisorEvent::ContinueBoot);
    Flow::Exit(SupervisorExit::ContinueBoot(choice))
}

/// Map a `continue` operand to the session it names, or [`None`] if it names
/// no session. Compared ASCII case-insensitively, like the command words.
fn parse_session(word: &[u8]) -> Option<BootSession> {
    let word = arg_str(word)?;
    if word.eq_ignore_ascii_case("text") || word.eq_ignore_ascii_case("console") {
        Some(BootSession::Text)
    } else if word.eq_ignore_ascii_case("gui")
        || word.eq_ignore_ascii_case("graphical")
        || word.eq_ignore_ascii_case("desktop")
    {
        Some(BootSession::Graphical)
    } else {
        None
    }
}

/// `mount` — perform the real root mount now, prompting for the ARXFS
/// passphrase only when the root is encrypted. On success the boot continues
/// without a second prompt.
pub fn cmd_mount(_args: &[&[u8]], session: &mut Session<'_>) -> Flow {
    session.host.audit(SupervisorEvent::MountAttempt);
    let outcome = if session.host.root_lock() == Some(RootLock::Passphrase) {
        let Some(outcome) = mount_with_passphrase(session) else {
            session.host.audit(SupervisorEvent::MountFailed);
            return Flow::Stay;
        };
        outcome
    } else {
        session.host.mount(None, session.out)
    };

    match outcome {
        MountOutcome::Mounted => {
            session.out.line("Root mounted; continuing boot.");
            session.host.audit(SupervisorEvent::MountOk);
            Flow::Exit(SupervisorExit::Mounted)
        }
        MountOutcome::WrongPassphrase => {
            session.out.line("Incorrect passphrase.");
            session.host.audit(SupervisorEvent::MountFailed);
            Flow::Stay
        }
        MountOutcome::Failed => {
            session
                .out
                .line("Mount failed (the root volume could not be read).");
            session.host.audit(SupervisorEvent::MountFailed);
            Flow::Stay
        }
    }
}

/// Ask for the ARXFS passphrase and mount the encrypted root under it, or
/// answer `None` — having said why — when no passphrase was read.
fn mount_with_passphrase(session: &mut Session<'_>) -> Option<MountOutcome> {
    session.out.write_str("ARXFS passphrase: ");
    // Un-echoed, on the stack, and wiped on every return path.
    let mut passphrase = Wiped::<MAX_ROOT_PASSPHRASE_LEN>::new();
    let read = read_line(session.input, &mut passphrase[..], false, session.out);
    session.out.newline();
    match read {
        LineOutcome::Line(len) => Some(session.host.mount(Some(&passphrase[..len]), session.out)),
        LineOutcome::TooLong => {
            session.out.line("Passphrase too long; not attempted.");
            None
        }
        LineOutcome::Eof => {
            session.out.line("No passphrase entered; mount cancelled.");
            None
        }
    }
}

/// `reboot` — restart the machine. If the platform cannot reboot the call
/// returns and the failure is reported; otherwise it never returns.
pub fn cmd_reboot(_args: &[&[u8]], session: &mut Session<'_>) -> Flow {
    session.host.audit(SupervisorEvent::Reboot);
    session.out.line("Rebooting...");
    session.host.reboot();
    // Reaching here means the platform reset primitive returned: report it
    // fail-safe and stay in the Supervisor rather than pretend it worked.
    session.out.line("reboot: the platform did not restart.");
    Flow::Stay
}

/// `poweroff` / `halt` — power the machine off. Reports and stays if the
/// platform has no power-off; otherwise never returns.
pub fn cmd_poweroff(_args: &[&[u8]], session: &mut Session<'_>) -> Flow {
    session.host.audit(SupervisorEvent::Poweroff);
    session.out.line("Powering off...");
    session.host.poweroff();
    session
        .out
        .line("poweroff: not supported on this platform.");
    Flow::Stay
}

#[cfg(test)]
mod tests {
    use tairix_abi::BootSession;

    use crate::commands::test_support::MockSession;
    use crate::dispatch::{dispatch, Flow};
    use crate::{MountOutcome, SupervisorEvent, SupervisorExit};

    /// Run one `continue` line and return the flow it produced.
    fn continue_flow(line: &[u8]) -> (Flow, MockSession) {
        let mut s = MockSession::new(&[]);
        let flow = dispatch(line, &mut s.session());
        (flow, s)
    }

    #[test]
    fn a_bare_continue_exits_with_no_choice_and_is_audited() {
        let (flow, s) = continue_flow(b"continue");
        assert_eq!(
            flow,
            Flow::Exit(SupervisorExit::ContinueBoot(BootSession::Unset))
        );
        assert!(s.audited(SupervisorEvent::ContinueBoot));
    }

    #[test]
    fn boot_is_an_alias_for_continue() {
        let (flow, _) = continue_flow(b"boot");
        assert_eq!(
            flow,
            Flow::Exit(SupervisorExit::ContinueBoot(BootSession::Unset))
        );
    }

    #[test]
    fn continue_text_and_its_alias_choose_the_text_session() {
        for line in [b"continue text".as_slice(), b"continue console".as_slice()] {
            let (flow, _) = continue_flow(line);
            assert_eq!(
                flow,
                Flow::Exit(SupervisorExit::ContinueBoot(BootSession::Text)),
                "line: {}",
                core::str::from_utf8(line).unwrap_or("?")
            );
        }
    }

    #[test]
    fn the_session_operand_is_matched_case_insensitively() {
        let (flow, _) = continue_flow(b"continue TEXT");
        assert_eq!(
            flow,
            Flow::Exit(SupervisorExit::ContinueBoot(BootSession::Text))
        );
        let (flow, _) = continue_flow(b"continue Gui");
        assert_eq!(
            flow,
            Flow::Exit(SupervisorExit::ContinueBoot(BootSession::Graphical))
        );
    }

    #[test]
    fn continue_gui_and_its_aliases_choose_the_graphical_session() {
        for line in [
            b"continue gui".as_slice(),
            b"continue graphical".as_slice(),
            b"continue desktop".as_slice(),
        ] {
            let (flow, _) = continue_flow(line);
            assert_eq!(
                flow,
                Flow::Exit(SupervisorExit::ContinueBoot(BootSession::Graphical)),
                "line: {}",
                core::str::from_utf8(line).unwrap_or("?")
            );
        }
    }

    #[test]
    fn an_unknown_session_is_refused_and_the_boot_does_not_resume() {
        let (flow, s) = continue_flow(b"continue bogus");
        assert_eq!(flow, Flow::Stay);
        assert!(s.output_contains("unknown session: bogus"));
        assert!(s.output_contains("usage: continue [text|gui]"));
        // Nothing was decided, so nothing is audited as decided.
        assert!(!s.audited(SupervisorEvent::ContinueBoot));
    }

    #[test]
    fn two_operands_are_refused_rather_than_guessed_between() {
        let (flow, s) = continue_flow(b"continue text gui");
        assert_eq!(flow, Flow::Stay);
        assert!(s.output_contains("one session at most"));
        assert!(s.output_contains("usage: continue [text|gui]"));
        assert!(!s.audited(SupervisorEvent::ContinueBoot));
    }

    #[test]
    fn reboot_calls_the_host_and_is_audited() {
        let mut s = MockSession::new(&[]);
        // The mock reboot returns (does not diverge), so the handler stays.
        assert_eq!(dispatch(b"reboot", &mut s.session()), Flow::Stay);
        assert!(s.rebooted());
        assert!(s.audited(SupervisorEvent::Reboot));
    }

    #[test]
    fn poweroff_and_halt_call_the_host() {
        let mut s = MockSession::new(&[]);
        dispatch(b"halt", &mut s.session());
        assert!(s.powered_off());
        assert!(s.audited(SupervisorEvent::Poweroff));
    }

    #[test]
    fn mount_with_the_correct_passphrase_mounts_and_exits() {
        // Scripted keyboard bytes: the passphrase then Enter.
        let mut s = MockSession::new(b"opensesame\n");
        s.set_mount_result(MountOutcome::Mounted);
        let flow = dispatch(b"mount", &mut s.session());
        assert_eq!(flow, Flow::Exit(SupervisorExit::Mounted));
        assert!(s.audited(SupervisorEvent::MountAttempt));
        assert!(s.audited(SupervisorEvent::MountOk));
        // The passphrase must never appear in the rendered output.
        assert!(!s.output_contains("opensesame"));
    }

    #[test]
    fn mount_with_a_wrong_passphrase_stays_and_reports() {
        let mut s = MockSession::new(b"wrong\n");
        s.set_mount_result(MountOutcome::WrongPassphrase);
        let flow = dispatch(b"mount", &mut s.session());
        assert_eq!(flow, Flow::Stay);
        assert!(s.output_contains("Incorrect passphrase"));
        assert!(s.audited(SupervisorEvent::MountFailed));
    }

    #[test]
    fn mount_never_reveals_the_typed_passphrase() {
        let mut s = MockSession::new(b"topsecret\n");
        s.set_mount_result(MountOutcome::Failed);
        dispatch(b"mount", &mut s.session());
        assert!(!s.output_contains("topsecret"));
    }

    #[test]
    fn a_plaintext_root_mounts_with_no_prompt_and_no_passphrase() {
        let mut s = MockSession::new(b"");
        s.set_root_lock(Some(crate::RootLock::Plaintext));
        s.set_mount_result(MountOutcome::Mounted);
        let flow = dispatch(b"mount", &mut s.session());
        assert_eq!(flow, Flow::Exit(SupervisorExit::Mounted));
        assert!(!s.output_contains("passphrase"));
        assert_eq!(s.mounted_with_passphrase(), Some(false));
        assert!(s.audited(SupervisorEvent::MountOk));
    }

    #[test]
    fn an_encrypted_root_is_mounted_with_the_typed_passphrase() {
        let mut s = MockSession::new(b"opensesame\n");
        s.set_mount_result(MountOutcome::Mounted);
        dispatch(b"mount", &mut s.session());
        assert!(s.output_contains("ARXFS passphrase: "));
        assert_eq!(s.mounted_with_passphrase(), Some(true));
    }

    #[test]
    fn a_disk_that_cannot_say_how_its_root_is_locked_asks_the_host_without_a_prompt() {
        let mut s = MockSession::new(b"");
        s.set_root_lock(None);
        let flow = dispatch(b"mount", &mut s.session());
        assert_eq!(flow, Flow::Stay);
        assert!(!s.output_contains("passphrase"));
        assert_eq!(s.mounted_with_passphrase(), Some(false));
        assert!(s.output_contains("Mount failed"));
    }
}
