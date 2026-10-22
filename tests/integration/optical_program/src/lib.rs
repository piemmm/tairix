//! The `optical` fixture of the USB optical QEMU vertical
//! (`plans/DEVICES.md`). Run from the scripted root shell, it waits for the
//! RISC OS disc ([`tairix_test_iso9660_image::riscos_disc`]) to be mounted,
//! copies it onto ARXFS with the real `cp`, and checks that every file of the
//! disc reads back with its bytes and its RISC OS filetype on the disc and in
//! the copy. It then prints [`SWAP_MARKER`], on which the vertical changes the
//! disc in the drive ([`tairix_test_iso9660_image::second_disc`]), and waits
//! for the first disc's volume to go and the second's to come, reading the
//! second back the same way.
//!
//! A pass prints [`PASS_MARKER`] and exits `0`. A failure prints its reason
//! and parks rather than exiting: the vertical arms its end on this program's
//! audited exit, so a failed check times the run out with the reason in the
//! transcript.
//!
//! The check is pure and host-tested; `src/run.rs` hands it the running
//! system's files.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_test_iso9660_image::{RiscOsFile, RISCOS_FILES, SECOND_FILES};

/// The command word: the bundle's name, the word the vertical's script
/// types, and the `comm` the kernel attests on the fixture's audited calls.
pub const COMMAND: &str = "optical";

/// The line the fixture prints once every check passed.
pub const PASS_MARKER: &str = "OPTICAL PASS";

/// The line the fixture prints once the first disc is checked: the
/// vertical's cue to change the disc in the drive.
pub const SWAP_MARKER: &str = "OPTICAL DISC CHECKED";

/// Where the disc's volume appears: its volume identifier as the volume
/// manager names a volume, in kebab case.
pub const SOURCE: &str = "/Storage/riscos";

/// Where the fixture has `cp` copy the disc: into the session user's home.
pub const COPY: &str = "/Users/root/disc";

/// Where the disc that replaces the first appears.
pub const SECOND_SOURCE: &str = "/Storage/second-disc";

/// The `cp` the fixture runs.
pub const CP: &[u8] = b"/System/Commands/cp.app/Run";

/// How long the fixture waits for the disc's volume before it gives up: past
/// the controller's, the drive's and the volume manager's whole bring-up on
/// an emulated machine.
pub const MOUNT_DEADLINE_NS: u64 = 120_000_000_000;

/// The attribute a RISC OS filetype is kept in.
pub const FILETYPE_KEY: &str = "acorn.filetype";

/// Largest file the check reads: past every file of the disc.
pub const READ_CAP: usize = 4096;

/// The files the check reads.
pub trait Files {
    /// The whole of the file at `path`.
    ///
    /// # Errors
    ///
    /// The open's or the read's refusal.
    fn contents(&mut self, path: &str) -> Result<Vec<u8>, Errno>;

    /// The value of the file at `path`'s attribute `key`.
    ///
    /// # Errors
    ///
    /// The read's refusal: [`Errno::NoData`] for a key the file lacks.
    fn attr(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno>;
}

/// Check that every one of `expected` reads back under `dir` with its bytes
/// and its filetype.
///
/// # Errors
///
/// The first file that does not, and why.
pub fn check(files: &mut dyn Files, dir: &str, expected: &[RiscOsFile]) -> Result<(), String> {
    for file in expected {
        let path = format!("{dir}/{}", file.name);
        let bytes = files
            .contents(&path)
            .map_err(|err| format!("optical: {path}: unreadable: {err:?}"))?;
        if bytes != file.content {
            return Err(format!(
                "optical: {path}: holds {} bytes that are not the disc's {}",
                bytes.len(),
                file.content.len()
            ));
        }
        let want = format!("{:03x}", file.filetype);
        let kept = files
            .attr(&path, FILETYPE_KEY)
            .map_err(|err| format!("optical: {path}: no {FILETYPE_KEY}: {err:?}"))?;
        if kept != want.as_bytes() {
            return Err(format!(
                "optical: {path}: {FILETYPE_KEY} is {:?}, not {want}",
                String::from_utf8_lossy(&kept)
            ));
        }
    }
    Ok(())
}

/// The line printed once the first disc is checked.
#[must_use]
pub fn swap_line() -> String {
    format!("{SWAP_MARKER}: {SOURCE} and {COPY} read back; change the disc\n")
}

/// The line a pass prints.
#[must_use]
pub fn pass_line() -> String {
    format!(
        "{PASS_MARKER}: {} files kept their bytes and filetypes in {SOURCE} and {COPY}, and the \
         disc that replaced it read back its {} in {SECOND_SOURCE}\n",
        RISCOS_FILES.len(),
        SECOND_FILES.len()
    )
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::string::ToString;

    use super::*;

    #[derive(Default)]
    struct Disc {
        files: BTreeMap<String, Vec<u8>>,
        attrs: BTreeMap<(String, String), Vec<u8>>,
    }

    impl Disc {
        fn copied(dir: &str) -> Self {
            let mut disc = Self::default();
            for file in &RISCOS_FILES {
                let path = format!("{dir}/{}", file.name);
                disc.files.insert(path.clone(), file.content.to_vec());
                let filetype = format!("{:03x}", file.filetype).into_bytes();
                disc.attrs
                    .insert((path, FILETYPE_KEY.to_string()), filetype);
            }
            disc
        }
    }

    impl Files for Disc {
        fn contents(&mut self, path: &str) -> Result<Vec<u8>, Errno> {
            self.files.get(path).cloned().ok_or(Errno::NotFound)
        }

        fn attr(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
            self.attrs
                .get(&(path.to_string(), key.to_string()))
                .cloned()
                .ok_or(Errno::NoData)
        }
    }

    #[test]
    fn a_faithful_copy_passes() {
        assert_eq!(check(&mut Disc::copied(COPY), COPY, &RISCOS_FILES), Ok(()));
        let line = pass_line();
        assert!(
            line.starts_with(PASS_MARKER) && line.ends_with('\n'),
            "{line}"
        );
        let swap = swap_line();
        assert!(
            swap.starts_with(SWAP_MARKER) && !swap.contains(PASS_MARKER),
            "{swap}"
        );
    }

    #[test]
    fn the_first_discs_files_are_not_the_seconds() {
        let why = check(
            &mut Disc::copied(SECOND_SOURCE),
            SECOND_SOURCE,
            &SECOND_FILES,
        )
        .expect_err("the first disc's files where the second's belong");
        assert!(why.contains("SPRITES"), "{why}");
    }

    #[test]
    fn a_lost_filetype_a_wrong_one_and_wrong_bytes_each_fail_with_why() {
        let boot = format!("{COPY}/!BOOT");
        let mut lost = Disc::copied(COPY);
        lost.attrs.remove(&(boot.clone(), FILETYPE_KEY.to_string()));
        let why = check(&mut lost, COPY, &RISCOS_FILES).expect_err("lost");
        assert!(why.contains("!BOOT") && why.contains("NoData"), "{why}");

        let mut retyped = Disc::copied(COPY);
        retyped
            .attrs
            .insert((boot.clone(), FILETYPE_KEY.to_string()), b"fff".to_vec());
        let why = check(&mut retyped, COPY, &RISCOS_FILES).expect_err("retyped");
        assert!(why.contains("not feb"), "{why}");

        let mut truncated = Disc::copied(COPY);
        truncated.files.insert(boot, b"| An".to_vec());
        let why = check(&mut truncated, COPY, &RISCOS_FILES).expect_err("truncated");
        assert!(why.contains("4 bytes"), "{why}");
        assert!(!why.contains(PASS_MARKER));
    }

    /// [`COMMAND`] is the manifest source's `name`, so the script, the PASS
    /// keying and the composed bundle cannot spell it differently.
    #[test]
    fn command_matches_the_manifest_source_name() {
        let manifest = include_str!("../AppInfo.toml");
        let named = manifest.lines().any(|line| {
            line.split_once('=').is_some_and(|(key, value)| {
                key.trim() == "name" && value.trim() == format!("{COMMAND:?}")
            })
        });
        assert!(named, "AppInfo.toml `name` must be {COMMAND:?}");
    }
}
