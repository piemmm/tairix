//! The flags a stage adds to a build, carried to cargo after the caller's own.
//!
//! Cargo — cargo-miri too — reads `CARGO_ENCODED_RUSTFLAGS` over `RUSTFLAGS`
//! whenever it is set, so a stage setting only `RUSTFLAGS` would lose its
//! flags to a caller's encoded ones without a word.

use std::process::Command;

/// The unit separator `CARGO_ENCODED_RUSTFLAGS` joins its flags with.
const SEPARATOR: char = '\u{1f}';

/// The flags cargo takes from a caller's environment — `encoded`
/// (`CARGO_ENCODED_RUSTFLAGS`) where set, an empty value naming none, else
/// `plain` (`RUSTFLAGS`) split on whitespace — then `tokens`, encoded for
/// `CARGO_ENCODED_RUSTFLAGS`.
pub fn encoded_with(encoded: Option<&str>, plain: Option<&str>, tokens: &[&str]) -> String {
    let caller: Vec<&str> = match encoded {
        Some("") => Vec::new(),
        Some(encoded) => encoded.split(SEPARATOR).collect(),
        None => plain.map_or_else(Vec::new, |plain| plain.split_whitespace().collect()),
    };
    let mut flags = caller;
    flags.extend_from_slice(tokens);
    flags.join(&SEPARATOR.to_string())
}

/// Have `cmd` build with the caller's flags and `tokens` after them.
pub fn add(cmd: &mut Command, tokens: &[&str]) {
    let encoded = std::env::var("CARGO_ENCODED_RUSTFLAGS").ok();
    let plain = std::env::var("RUSTFLAGS").ok();
    cmd.env(
        "CARGO_ENCODED_RUSTFLAGS",
        encoded_with(encoded.as_deref(), plain.as_deref(), tokens),
    );
    cmd.env_remove("RUSTFLAGS");
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{add, encoded_with};
    use std::process::Command;

    /// What `cmd` sets `CARGO_ENCODED_RUSTFLAGS` to, and whether it removes
    /// `RUSTFLAGS`.
    pub(crate) fn flags_of(cmd: &Command) -> (Option<String>, bool) {
        let mut encoded = None;
        let mut removes_plain = false;
        for (key, value) in cmd.get_envs() {
            match (key.to_str(), value) {
                (Some("CARGO_ENCODED_RUSTFLAGS"), Some(value)) => {
                    encoded = Some(value.to_string_lossy().into_owned());
                }
                (Some("RUSTFLAGS"), None) => removes_plain = true,
                _ => {}
            }
        }
        (encoded, removes_plain)
    }

    #[test]
    fn a_stage_s_tokens_follow_the_flags_cargo_would_read() {
        let tokens = ["--cfg", "loom"];
        assert_eq!(
            encoded_with(
                Some("-Cdebuginfo=0\u{1f}-Dwarnings"),
                Some("--cfg ignored"),
                &tokens
            ),
            "-Cdebuginfo=0\u{1f}-Dwarnings\u{1f}--cfg\u{1f}loom",
            "the encoded flags win, as cargo reads them"
        );
        assert_eq!(
            encoded_with(Some(""), Some("--cfg ignored"), &tokens),
            "--cfg\u{1f}loom",
            "an empty encoded value is no flags, and still wins"
        );
        assert_eq!(
            encoded_with(None, Some("  -Cdebuginfo=0   -Dwarnings "), &tokens),
            "-Cdebuginfo=0\u{1f}-Dwarnings\u{1f}--cfg\u{1f}loom"
        );
        assert_eq!(encoded_with(None, None, &tokens), "--cfg\u{1f}loom");
    }

    #[test]
    fn a_command_carries_its_tokens_encoded_and_no_plain_flags() {
        let mut cmd = Command::new("true");
        add(&mut cmd, &["--cfg", "sha2_backend=\"soft\""]);
        let (encoded, removes_plain) = flags_of(&cmd);
        assert!(
            encoded.is_some_and(|flags| flags.ends_with("--cfg\u{1f}sha2_backend=\"soft\"")),
            "the tokens close the encoded flags"
        );
        assert!(removes_plain, "no plain flags beside them");
    }
}
