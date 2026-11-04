//! The system tasks the desktop performs for its own components
//! ([`SystemTask`]): which component may ask for each, what each runs, how,
//! and what the system's authorisation prompt says about it.
//!
//! A component names a task and nothing else, so the program run and the
//! words the prompt states are both the desktop's. Every word a task carries
//! is checked against the form its program takes before the prompt comes up:
//! an account change names exactly one well-formed account, an eject one
//! well-formed volume, and that is what the prompt names.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::elevate::{ElevateWords, ELEVATE_MAX_OUTPUT};
use tairix_abi::volume::validate_volume_name;
use tairix_abi::window_ipc::{AuthorisedOutcome, SystemTask};
use tairix_abi::{Errno, DATETIME_RUN_PATH, FILES_RUN_PATH, SETTINGS_RUN_PATH};
use tairix_taskbar::system::SETTINGS_BUNDLE;
use tairix_window::AuthoriseFor;

use crate::config::{FILES_LABEL, SETTINGS_LABEL};
use crate::elevate::{Done, Elevation, Request};

/// The command that reads and changes the machine's configuration.
const CONFIGURE_RUN_PATH: &str = "/System/Commands/configure.app/Run";
/// The command that lists the accounts and groups.
const USERS_RUN_PATH: &str = "/System/Commands/users.app/Run";
/// The command that changes an account's fields.
const USERMOD_RUN_PATH: &str = "/System/Commands/usermod.app/Run";
/// The command that replaces an account's password.
const PASSWD_RUN_PATH: &str = "/System/Commands/passwd.app/Run";
/// The command that detaches a volume.
const UNMOUNT_RUN_PATH: &str = "/System/Commands/unmount.app/Run";

/// The file manager's signed identifier: the identity the desktop trusts it
/// by.
pub const FILES_BUNDLE: &str = "os.tairix.files";

/// The desktop component a task is performed for: the only one that may ask
/// for it, and the one the prompt names as asking.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Component {
    /// Settings: the machine's configuration, its accounts and its clock.
    Settings,
    /// The file manager: ejecting a volume.
    Files,
}

impl Component {
    /// The component `task` belongs to.
    #[must_use]
    pub const fn of(task: &SystemTask) -> Self {
        match task {
            SystemTask::Eject(_) => Self::Files,
            SystemTask::Configure(_)
            | SystemTask::ChangeAccount(_)
            | SystemTask::SetPassword(_)
            | SystemTask::ShowAddressing
            | SystemTask::ShowAccounts
            | SystemTask::SetClock => Self::Settings,
        }
    }

    /// The signed bundle identifier the component is known by.
    #[must_use]
    pub const fn bundle_id(self) -> &'static str {
        match self {
            Self::Settings => SETTINGS_BUNDLE,
            Self::Files => FILES_BUNDLE,
        }
    }

    /// The name the prompt gives the one asking.
    const fn label(self) -> &'static str {
        match self {
            Self::Settings => SETTINGS_LABEL,
            Self::Files => FILES_LABEL,
        }
    }

    /// The component's entry point, whose bundle's picture stands for it.
    const fn run_path(self) -> &'static str {
        match self {
            Self::Settings => SETTINGS_RUN_PATH,
            Self::Files => FILES_RUN_PATH,
        }
    }
}

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

/// The request the prompt asks about for `task`, which its component asked
/// for through `asker`.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for words not in the form the task's program
/// takes them.
pub fn request_for(task: &SystemTask, asker: AuthoriseFor) -> Result<Request, Errno> {
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
        // `unmount` is handed the durable root alone, so a volume put under
        // the catalog name meanwhile is not the one ejected; the name only
        // tells the prompt's reader which volume it is.
        SystemTask::Eject(words) => {
            let words = owned(words);
            let [root, name] = words.as_slice() else {
                return Err(Errno::OutOfRange);
            };
            if !is_volume_root(root) || validate_volume_name(name.as_bytes()).is_err() {
                return Err(Errno::OutOfRange);
            }
            let purpose = format!("wants to eject \u{201c}{name}\u{201d}.");
            let run = alloc::vec![String::from("--"), root.clone()];
            (UNMOUNT_RUN_PATH, Elevation::Run(run), purpose)
        }
    };
    let component = Component::of(task);
    Ok(Request {
        program: String::from(program),
        elevation,
        requester: String::from(component.label()),
        bundle: tairix_appstore::bundle_of_entry(component.run_path()).map(String::from),
        purpose,
        asker: Some(asker),
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

/// Whether `spelled` is a volume's root, nothing below it, in the one
/// spelling that root renders as.
fn is_volume_root(spelled: &str) -> bool {
    tairix_path::parse(spelled).is_ok_and(|path| {
        path.volume_id()
            .is_some_and(|id| tairix_path::Path::of_volume(*id) == path)
            && path.to_string() == spelled
    })
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

    const ASKER: AuthoriseFor = AuthoriseFor::Window(9);

    /// A volume's durable root, spelled canonically.
    const ROOT: &str = "id::07070707-0707-0707-0707-070707070707/";

    #[test]
    fn every_task_runs_the_program_that_owns_it_as_its_component() {
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
            (SystemTask::Eject(words(&[ROOT, "usb1"])), UNMOUNT_RUN_PATH),
        ];
        for (task, program) in cases {
            let request = request_for(&task, ASKER).expect("a well-formed task");
            let (label, entry) = match Component::of(&task) {
                Component::Settings => (SETTINGS_LABEL, SETTINGS_RUN_PATH),
                Component::Files => (FILES_LABEL, FILES_RUN_PATH),
            };
            assert_eq!(request.program, program);
            assert_eq!(request.requester, label);
            assert_eq!(request.asker, Some(ASKER));
            assert_eq!(
                request.bundle.as_deref(),
                tairix_appstore::bundle_of_entry(entry)
            );
        }
    }

    /// Only the file manager may ask to eject, and an eject from its icon-bar
    /// slot is answered there.
    #[test]
    fn an_eject_is_the_file_managers_and_names_its_volume() {
        let eject = SystemTask::Eject(words(&[ROOT, "usb1"]));
        assert_eq!(Component::of(&eject), Component::Files);
        assert_eq!(Component::Files.bundle_id(), FILES_BUNDLE);
        assert_eq!(
            Component::of(&SystemTask::SetClock).bundle_id(),
            SETTINGS_BUNDLE
        );
        let slot = AuthoriseFor::Slot(tairix_abi::ProcId::from_raw([3; 16]));
        let request = request_for(&eject, slot).expect("well formed");
        assert_eq!(request.asker, Some(slot));
        assert_eq!(request.purpose, "wants to eject \u{201c}usb1\u{201d}.");
        assert_eq!(
            request.elevation,
            Elevation::Run(alloc::vec![String::from("--"), String::from(ROOT)])
        );
    }

    /// The root the file manager names a volume by is one an eject takes.
    ///
    /// Regression: the file manager asked by durable root while an eject took
    /// only a catalog name, so every administrator's eject was refused before
    /// the prompt came up.
    #[test]
    fn the_file_managers_root_for_a_volume_is_an_ejects() {
        let root = tairix_browse::mount::volume_root([7; 16]);
        assert_eq!(root, ROOT);
        let eject = SystemTask::Eject(words(&[&root, "usb1"]));
        assert!(request_for(&eject, ASKER).is_ok());
    }

    #[test]
    fn a_task_runs_as_its_program_takes_it() {
        let run = |task| {
            request_for(&task, ASKER)
                .expect("a well-formed task")
                .elevation
        };
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
            ASKER,
        )
        .expect("well formed");
        assert_eq!(
            change.purpose,
            "wants to change the account \u{201c}ada\u{201d}."
        );
        let password = request_for(
            &SystemTask::SetPassword(words(&["--record", "r", "--", "grace"])),
            ASKER,
        )
        .expect("well formed");
        assert_eq!(
            password.purpose,
            "wants to set a new password for the account \u{201c}grace\u{201d}."
        );
    }

    #[test]
    fn a_task_not_in_its_programs_form_is_refused() {
        type Task = fn(ElevateWords) -> SystemTask;
        let (change, password, configure, eject): (Task, Task, Task, Task) = (
            SystemTask::ChangeAccount,
            SystemTask::SetPassword,
            SystemTask::Configure,
            SystemTask::Eject,
        );
        let refused: [(Task, &[&str]); 22] = [
            (change, &["--lock", "ada"]),
            (change, &["--lock", "--", "Ada"]),
            (change, &["--lock", "--", "ad\u{202e}a"]),
            (password, &["--record", "r", "ada"]),
            (password, &["--record", "r", "--", "ada", "grace"]),
            (password, &["--lock", "r", "--", "ada"]),
            (configure, &["cache.all"]),
            (eject, &["--", "usb1"]),
            (eject, &[ROOT]),
            (eject, &["usb1", ROOT]),
            (eject, &[ROOT, "Usb1"]),
            (eject, &[ROOT, "-f"]),
            (eject, &[ROOT, "usb1", "usb2"]),
            (eject, &["-f", ROOT, "usb1"]),
            (eject, &["id::07070707-0707-0707-0707-070707070707", "usb1"]),
            (
                eject,
                &["id::07070707-0707-0707-0707-070707070707/x", "usb1"],
            ),
            (
                eject,
                &["id::0707070A-0707-0707-0707-070707070707/", "usb1"],
            ),
            (eject, &["id::07070707070707070707070707070707/", "usb1"]),
            (eject, &["id::/", "usb1"]),
            (eject, &["id::", "usb1"]),
            (eject, &["/Storage/usb1", "usb1"]),
            (eject, &["usb1:/", "usb1"]),
        ];
        for (task, args) in refused {
            let task = task(words(args));
            assert_eq!(
                request_for(&task, ASKER).map(|_| ()),
                Err(Errno::OutOfRange),
                "{task:?}"
            );
        }
    }

    #[test]
    fn a_change_touching_only_the_network_says_so() {
        let purpose = |pairs: &[&str]| {
            request_for(&SystemTask::Configure(words(pairs)), ASKER)
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
