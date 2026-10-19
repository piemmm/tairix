//! The system tasks the desktop performs for its own Settings application
//! ([`SystemTask`]): what each runs, how, and what the system's
//! authorisation prompt says about it.
//!
//! Settings names a task and nothing else, so the program run and the words
//! the prompt states are both the desktop's. Every word a task carries is
//! checked against the form its program takes before the prompt comes up: an
//! account change names exactly one well-formed account, and that account is
//! what the prompt names.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::elevate::{ElevateWords, ELEVATE_MAX_OUTPUT};
use tairix_abi::window_ipc::{AuthorisedOutcome, SystemTask};
use tairix_abi::{Errno, DATETIME_RUN_PATH, SETTINGS_RUN_PATH};

use crate::config::SETTINGS_LABEL;
use crate::elevate::{Done, Elevation, Request};

/// The command that reads and changes the machine's configuration.
const CONFIGURE_RUN_PATH: &str = "/System/Commands/configure.app/Run";
/// The command that lists the accounts and groups.
const USERS_RUN_PATH: &str = "/System/Commands/users.app/Run";
/// The command that changes an account's fields.
const USERMOD_RUN_PATH: &str = "/System/Commands/usermod.app/Run";
/// The command that replaces an account's password.
const PASSWD_RUN_PATH: &str = "/System/Commands/passwd.app/Run";

/// What changing the date, time and time zone says, after the asker's name.
pub const SET_TIME_PURPOSE: &str = "wants to change the date, time and time zone of this computer.";

/// What a configuration change says.
const CONFIGURE_PURPOSE: &str = "wants to change the settings of this computer.";

/// What a configuration change touching only the network says.
const CONFIGURE_NETWORK_PURPOSE: &str = "wants to change the network settings of this computer.";

/// What reading the configuration says: its network addressing names the
/// machine's hardware and its addresses, which is why it is not open.
const SHOW_ADDRESSING_PURPOSE: &str = "wants to read the network addressing of this computer.";

/// What reading the accounts says.
const SHOW_ACCOUNTS_PURPOSE: &str = "wants to read the accounts and groups on this computer.";

/// The request the prompt asks about for `task`, which Settings asked for
/// its window `window_id`.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for words not in the form the task's program
/// takes them.
pub fn request_for(task: &SystemTask, window_id: u64) -> Result<Request, Errno> {
    let (program, elevation, purpose) = match task {
        SystemTask::Configure(words) => {
            let words = owned(words);
            if !words.len().is_multiple_of(2) {
                return Err(Errno::OutOfRange);
            }
            let purpose = if changes_only_network(&words) {
                CONFIGURE_NETWORK_PURPOSE
            } else {
                CONFIGURE_PURPOSE
            };
            (
                CONFIGURE_RUN_PATH,
                Elevation::Run(words),
                String::from(purpose),
            )
        }
        SystemTask::ChangeAccount(words) => {
            let words = owned(words);
            let account = account_of(&words)?;
            let purpose = format!("wants to change the account \u{201c}{account}\u{201d}.");
            (USERMOD_RUN_PATH, Elevation::Run(words), purpose)
        }
        SystemTask::SetPassword(words) => {
            let words = owned(words);
            let [record, _, end, account] = words.as_slice() else {
                return Err(Errno::OutOfRange);
            };
            if record != "--record" || end != "--" || !tairix_users::valid_username(account) {
                return Err(Errno::OutOfRange);
            }
            let purpose =
                format!("wants to set a new password for the account \u{201c}{account}\u{201d}.");
            (PASSWD_RUN_PATH, Elevation::Run(words), purpose)
        }
        SystemTask::ShowAddressing => (
            CONFIGURE_RUN_PATH,
            Elevation::Capture(Vec::new()),
            String::from(SHOW_ADDRESSING_PURPOSE),
        ),
        // The listing, not the interactive session: a captured run's input is
        // closed.
        SystemTask::ShowAccounts => (
            USERS_RUN_PATH,
            Elevation::Capture(alloc::vec![String::from("-l")]),
            String::from(SHOW_ACCOUNTS_PURPOSE),
        ),
        SystemTask::SetClock => (
            DATETIME_RUN_PATH,
            Elevation::Launch,
            String::from(SET_TIME_PURPOSE),
        ),
    };
    Ok(Request {
        program: String::from(program),
        elevation,
        requester: String::from(SETTINGS_LABEL),
        bundle: tairix_appstore::bundle_of_entry(SETTINGS_RUN_PATH).map(String::from),
        purpose,
        window: Some(window_id),
    })
}

/// What a request's end tells the window that asked, and what it holds for
/// it to take: `None` is a prompt that ran nothing.
#[must_use]
pub fn outcome_of(done: Option<Done>) -> (AuthorisedOutcome, Vec<u8>) {
    match done {
        None => (AuthorisedOutcome::Declined, Vec::new()),
        Some(Done::Ran { exit_code }) => (AuthorisedOutcome::Ran { exit_code }, Vec::new()),
        Some(Done::Launched { .. }) => (AuthorisedOutcome::Launched, Vec::new()),
        Some(Done::Captured { exit_code, output }) => match u32::try_from(output.len()) {
            Ok(len) if output.len() <= ELEVATE_MAX_OUTPUT => {
                (AuthorisedOutcome::Captured { exit_code, len }, output)
            }
            _ => (AuthorisedOutcome::Overran { exit_code }, Vec::new()),
        },
        Some(Done::Overran { exit_code }) => (AuthorisedOutcome::Overran { exit_code }, Vec::new()),
    }
}

/// The words, owned, as the request hands them on.
fn owned(words: &ElevateWords) -> Vec<String> {
    words.argv().iter().map(String::from).collect()
}

/// The one account `usermod`'s words end with, after the `--` that ends its
/// options.
///
/// The tool takes exactly one operand, so a word before that `--` that is
/// not an option makes it refuse rather than act on another account.
fn account_of(words: &[String]) -> Result<&str, Errno> {
    match words {
        [.., end, account] if end == "--" && tairix_users::valid_username(account) => Ok(account),
        _ => Err(Errno::OutOfRange),
    }
}

/// Whether every key of `pairs` is one of the network's: a machine key of
/// the stack-wide family, or a key the machine registry does not hold, which
/// `configure` reads as an interface's.
fn changes_only_network(pairs: &[String]) -> bool {
    pairs.iter().step_by(2).all(|key| {
        tairix_sysconfig::Key::from_name(key).is_none_or(tairix_sysconfig::Key::is_network)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> ElevateWords {
        ElevateWords::new(args).expect("within bounds")
    }

    #[test]
    fn every_task_runs_the_program_that_owns_it_as_settings() {
        let cases = [
            (
                SystemTask::Configure(words(&["os.loginType", "text"])),
                CONFIGURE_RUN_PATH,
            ),
            (
                SystemTask::ChangeAccount(words(&["--lock", "--", "ada"])),
                USERMOD_RUN_PATH,
            ),
            (
                SystemTask::SetPassword(words(&["--record", "r", "--", "ada"])),
                PASSWD_RUN_PATH,
            ),
            (SystemTask::ShowAddressing, CONFIGURE_RUN_PATH),
            (SystemTask::ShowAccounts, USERS_RUN_PATH),
            (SystemTask::SetClock, DATETIME_RUN_PATH),
        ];
        for (task, program) in cases {
            let request = request_for(&task, 9).expect("a well-formed task");
            assert_eq!(request.program, program);
            assert_eq!(request.requester, SETTINGS_LABEL);
            assert_eq!(request.window, Some(9));
            assert_eq!(
                request.bundle.as_deref(),
                tairix_appstore::bundle_of_entry(SETTINGS_RUN_PATH)
            );
        }
    }

    #[test]
    fn a_task_runs_as_its_program_takes_it() {
        let run = |task| request_for(&task, 9).expect("a well-formed task").elevation;
        assert_eq!(
            run(SystemTask::Configure(words(&["cache.all", "off"]))),
            Elevation::Run(alloc::vec![String::from("cache.all"), String::from("off")])
        );
        assert_eq!(
            run(SystemTask::ShowAddressing),
            Elevation::Capture(Vec::new())
        );
        assert_eq!(
            run(SystemTask::ShowAccounts),
            Elevation::Capture(alloc::vec![String::from("-l")])
        );
        assert_eq!(run(SystemTask::SetClock), Elevation::Launch);
    }

    #[test]
    fn the_prompt_names_the_account_a_change_acts_on() {
        let change = request_for(
            &SystemTask::ChangeAccount(words(&["--comment", "Ada L", "--", "ada"])),
            9,
        )
        .expect("well formed");
        assert_eq!(
            change.purpose,
            "wants to change the account \u{201c}ada\u{201d}."
        );
        let password = request_for(
            &SystemTask::SetPassword(words(&["--record", "r", "--", "grace"])),
            9,
        )
        .expect("well formed");
        assert_eq!(
            password.purpose,
            "wants to set a new password for the account \u{201c}grace\u{201d}."
        );
    }

    #[test]
    fn an_account_task_not_in_its_programs_form_is_refused() {
        for task in [
            SystemTask::ChangeAccount(words(&["--lock", "ada"])),
            SystemTask::ChangeAccount(words(&["--lock", "--", "Ada"])),
            SystemTask::ChangeAccount(words(&["--lock", "--", "ad\u{202e}a"])),
            SystemTask::SetPassword(words(&["--record", "r", "ada"])),
            SystemTask::SetPassword(words(&["--record", "r", "--", "ada", "grace"])),
            SystemTask::SetPassword(words(&["--lock", "r", "--", "ada"])),
            SystemTask::Configure(words(&["cache.all"])),
        ] {
            assert_eq!(
                request_for(&task, 9).map(|_| ()),
                Err(Errno::OutOfRange),
                "{task:?}"
            );
        }
    }

    #[test]
    fn a_change_touching_only_the_network_says_so() {
        let purpose = |pairs: &[&str]| {
            request_for(&SystemTask::Configure(words(pairs)), 9)
                .expect("well formed")
                .purpose
        };
        assert_eq!(
            purpose(&["wan.ipv4.method", "dhcp", "net.tcp.ecn", "true"]),
            CONFIGURE_NETWORK_PURPOSE
        );
        assert_eq!(
            purpose(&["wan.ipv4.method", "dhcp", "os.loginType", "text"]),
            CONFIGURE_PURPOSE
        );
        assert_eq!(purpose(&["time.servers", "none"]), CONFIGURE_PURPOSE);
    }

    #[test]
    fn what_was_done_is_told_to_the_window_with_only_a_capture_held() {
        assert_eq!(outcome_of(None), (AuthorisedOutcome::Declined, Vec::new()));
        assert_eq!(
            outcome_of(Some(Done::Ran { exit_code: 2 })),
            (AuthorisedOutcome::Ran { exit_code: 2 }, Vec::new())
        );
        assert_eq!(
            outcome_of(Some(Done::Launched { pid: 40 })),
            (AuthorisedOutcome::Launched, Vec::new())
        );
        assert_eq!(
            outcome_of(Some(Done::Captured {
                exit_code: 0,
                output: b"ada 1000".to_vec()
            })),
            (
                AuthorisedOutcome::Captured {
                    exit_code: 0,
                    len: 8
                },
                b"ada 1000".to_vec()
            )
        );
        assert_eq!(
            outcome_of(Some(Done::Captured {
                exit_code: 0,
                output: alloc::vec![b'x'; ELEVATE_MAX_OUTPUT + 1]
            })),
            (AuthorisedOutcome::Overran { exit_code: 0 }, Vec::new()),
            "an answer is never longer than the window could be handed"
        );
        assert_eq!(
            outcome_of(Some(Done::Overran { exit_code: 1 })),
            (AuthorisedOutcome::Overran { exit_code: 1 }, Vec::new())
        );
    }
}
