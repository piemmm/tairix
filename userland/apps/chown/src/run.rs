//! The `Run` binary of the `chown` tool.
//!
//! `main` runs the parsed command against the production seams:
//! `tairix_rt::copy::WorkingDirectory`, through whose descriptors every
//! operand is opened and every tree walked, the kernel deciding each change;
//! `RtAccounts`, the ungated account and group directories; the bundle's own
//! `Help/` tree; and the inherited standard streams. On the host it is an
//! inert stub, so the workspace build, clippy and fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::string::String;
    use tairix_abi::sysinfo::{GroupDirectoryRecord, UserDirectoryRecord};
    use tairix_abi::Errno;
    use tairix_chown::{parse, run, Account, Accounts};
    use tairix_help::BundleHelp;
    use tairix_procinfo::{
        for_each_group, for_each_user, CallError, IpcTransport, ListError, WalkStep,
    };
    use tairix_rt::copy::WorkingDirectory;
    use tairix_rt::io::{write_stderr_line, Stderr, Stdout};

    /// The production [`Accounts`]: the ungated `USER_DIRECTORY` and
    /// `GROUP_DIRECTORY` queries `stat` and `whoami` read. A name listed twice
    /// answers with its first entry, so the answer does not hang on the
    /// service's order.
    struct RtAccounts;

    impl Accounts for RtAccounts {
        fn user(&self, name: &str) -> Result<Option<Account>, Errno> {
            user_where(|record| {
                (record.name_bytes() == name.as_bytes()).then_some(Account {
                    uid: record.uid,
                    login_group: record.primary_gid,
                })
            })
        }

        fn group(&self, name: &str) -> Result<Option<u32>, Errno> {
            group_where(|record| (record.name_bytes() == name.as_bytes()).then_some(record.gid))
        }

        fn user_name(&self, uid: u32) -> Result<Option<String>, Errno> {
            user_where(|record| (record.uid == uid).then(|| name_of(record.name_bytes())))
                .map(Option::flatten)
        }

        fn group_name(&self, gid: u32) -> Result<Option<String>, Errno> {
            group_where(|record| (record.gid == gid).then(|| name_of(record.name_bytes())))
                .map(Option::flatten)
        }
    }

    /// What `pick` takes from the first account record it takes anything from.
    fn user_where<T>(
        mut pick: impl FnMut(&UserDirectoryRecord) -> Option<T>,
    ) -> Result<Option<T>, Errno> {
        let mut found = None;
        for_each_user(&IpcTransport, |record| {
            found = pick(record);
            Ok(if found.is_some() {
                WalkStep::Stop
            } else {
                WalkStep::Continue
            })
        })
        .map_err(walk_errno)?;
        Ok(found)
    }

    /// What `pick` takes from the first group record it takes anything from.
    fn group_where<T>(
        mut pick: impl FnMut(&GroupDirectoryRecord) -> Option<T>,
    ) -> Result<Option<T>, Errno> {
        let mut found = None;
        for_each_group(&IpcTransport, |record| {
            found = pick(record);
            Ok(if found.is_some() {
                WalkStep::Stop
            } else {
                WalkStep::Continue
            })
        })
        .map_err(walk_errno)?;
        Ok(found)
    }

    /// A listed name as text; one that is not UTF-8 is reported by number.
    fn name_of(bytes: &[u8]) -> Option<String> {
        core::str::from_utf8(bytes).ok().map(String::from)
    }

    /// Why a System Information query failed, as the seam's [`Errno`].
    fn call_errno(failure: CallError) -> Errno {
        match failure {
            CallError::PermissionDenied => Errno::PermissionDenied,
            CallError::Service(errno) => errno,
        }
    }

    /// Why a directory walk failed, as the seam's [`Errno`].
    fn walk_errno(failure: ListError) -> Errno {
        match failure {
            ListError::Call(failure) => call_errno(failure),
            ListError::Sink(errno) => errno,
        }
    }

    /// Program entry point, which `tairix-rt`'s `_start` calls and whose
    /// value it exits with: `0` when every change succeeded, `1` otherwise or
    /// for a command line not understood, as GNU's `chown` ends.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments("chown") else {
            return tairix_help::MISUSE;
        };
        let command = match parse(&arguments) {
            Ok(command) => command,
            Err(err) => {
                tairix_help::report_misuse("chown", &err);
                return tairix_help::MISUSE;
            }
        };
        match run(
            command,
            tairix_help::user_locale(),
            &WorkingDirectory,
            &RtAccounts,
            &BundleHelp::new("chown"),
            &Stdout,
            &Stderr,
        ) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                write_stderr_line(format_args!("chown: {err}"));
                1
            }
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
