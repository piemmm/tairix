//! The desktop's *About This System* window: which system this is, and the
//! processors and memory it runs on, stated as one of the desktop's own alerts.
//!
//! None of these facts changes while the system runs, so they are read once
//! before the serve loop starts and opening the window reads nothing.

use alloc::string::String;
use core::fmt::{Display, Write as _};

use tairix_abi::sysinfo::{OsVersion, SYSTEM_NAME};
use tairix_abi::window_ipc::{AlertSeverity, AlertSpec};
use tairix_procinfo::UNMEASURED;
use tairix_util::size::{format_binary, SIZE_TEXT_MAX};

/// The window's title.
pub const ABOUT_TITLE: &str = "About This System";

/// What the window states, each fact `None` where its reading was refused.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemFacts {
    /// The version of the system running.
    pub version: Option<OsVersion>,
    /// The processor parts the machine reported, as
    /// [`processor_summary`](tairix_procinfo::processor_summary) words them.
    pub processor: Option<String>,
    /// The machine's usable physical memory, in bytes.
    pub memory_bytes: Option<u64>,
}

impl SystemFacts {
    /// The alert stating these facts: the system's name, then its version,
    /// processors and memory a line each, acknowledged by one *OK*. `None`
    /// only if the *OK* label were refused, which a well-formed one never is.
    #[must_use]
    pub fn spec(&self) -> Option<AlertSpec> {
        let mut memory = [0u8; SIZE_TEXT_MAX];
        let memory = self
            .memory_bytes
            .map(|bytes| format_binary(bytes, &mut memory));
        let mut detail = String::new();
        fact(&mut detail, "Version", self.version.as_ref());
        fact(&mut detail, "Processor", self.processor.as_ref());
        fact(&mut detail, "Memory", memory.as_ref());
        AlertSpec::acknowledged(AlertSeverity::Information, SYSTEM_NAME, &detail).ok()
    }
}

/// Append the line stating `label`'s `value`, or that it was not measured.
fn fact(detail: &mut String, label: &str, value: Option<&impl Display>) {
    if !detail.is_empty() {
        detail.push('\n');
    }
    // Writing into a `String` never fails.
    let _ = match value {
        Some(value) => write!(detail, "{label}: {value}"),
        None => write!(detail, "{label}: {UNMEASURED}"),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_names_the_system_and_states_each_fact_a_line() {
        let facts = SystemFacts {
            version: Some(OsVersion {
                major: 0,
                minor: 4,
                patch: 12,
            }),
            processor: Some(String::from("4 \u{d7} Cortex-A72")),
            memory_bytes: Some(4 * 1024 * 1024 * 1024),
        };
        let spec = facts.spec().expect("an OK label is well formed");
        assert_eq!(spec.severity(), AlertSeverity::Information);
        assert_eq!(spec.headline(), SYSTEM_NAME);
        assert_eq!(
            spec.detail(),
            "Version: 0.4.12\nProcessor: 4 \u{d7} Cortex-A72\nMemory: 4.0 GiB"
        );
        assert_eq!(spec.button_count(), 1);
    }

    #[test]
    fn a_fact_that_was_not_read_is_said_to_be_unmeasured_never_guessed() {
        let spec = SystemFacts {
            memory_bytes: Some(3 * 1024 * 1024 * 1024 / 2),
            ..SystemFacts::default()
        }
        .spec()
        .expect("an OK label is well formed");
        assert_eq!(
            spec.detail(),
            "Version: not measured\nProcessor: not measured\nMemory: 1.5 GiB"
        );
    }
}
