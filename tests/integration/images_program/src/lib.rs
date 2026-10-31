//! The `images` fixture of the disk-image QEMU vertical
//! (`plans/DISK-IMAGES.md` DI7). Run from the scripted root shell, it copies
//! the images the vertical planted ([`PLANTED`], the system's) into the
//! user's home, mounts each copy with the real `mount`, reads what it holds,
//! writes where the volume can be written, ejects it with the real `unmount`,
//! and mounts the floppies again to read back what it wrote. It also mounts
//! a planted original, which the user may only read, to see it mounted
//! read-only.
//!
//! A pass prints [`PASS_MARKER`] and exits `0`. A failure prints its reason
//! and parks rather than exiting: the vertical arms its end on this program's
//! audited exit, so a failed check times the run out with the reason in the
//! transcript.
//!
//! The steps are pure and host-tested; `src/run.rs` hands them the running
//! system.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_test_iso9660_image::RISCOS_FILES;

/// The command word: the bundle's name, the word the vertical's script
/// types, and the `comm` the kernel attests on the fixture's audited calls.
pub const COMMAND: &str = "images";

/// The line the fixture prints once every step passed.
pub const PASS_MARKER: &str = "IMAGES PASS";

/// Where the vertical plants the images, owned by the system.
pub const PLANTED: &str = "/Users/root/Home/Disks";

/// Where the fixture copies them, as the session user.
pub const HOME: &str = "/Users/root/Home";

/// The `mount` the fixture runs.
pub const MOUNT: &str = "/System/Commands/mount.app/Run";

/// The `unmount` the fixture runs.
pub const UNMOUNT: &str = "/System/Commands/unmount.app/Run";

/// The Acorn ADFS floppy's file name.
pub const ACORN: &str = "acorn.adf";

/// The AmigaDOS floppy's file name.
pub const AMIGA: &str = "amiga.adf";

/// The ISO 9660 disc's file name: the RISC OS disc
/// ([`tairix_test_iso9660_image::riscos_disc`]).
pub const DISC: &str = "riscos.iso";

/// The file each floppy holds.
pub const README: &str = "ReadMe";

/// What the Acorn floppy's [`README`] holds.
pub const ACORN_TEXT: &[u8] = b"FileCore holds this, on an E-format floppy.";

/// What the Amiga floppy's [`README`] holds.
pub const AMIGA_TEXT: &[u8] = b"AmigaDOS holds this, on a Fast File System floppy.";

/// The Amiga floppy's volume label, which names its mount.
pub const AMIGA_LABEL: &[u8] = b"Workbench";

/// The catalog name [`AMIGA_LABEL`] gives the Amiga floppy's volume.
pub const AMIGA_NAME: &str = "workbench";

/// The file the fixture writes on each writable volume.
pub const WRITTEN: &str = "Written";

/// What it writes there.
pub const WRITTEN_TEXT: &[u8] = b"Written through a mounted disk image.";

/// The exit status `mount` and `unmount` give a success.
const SUCCESS: i32 = 0;

/// The longest file the fixture reads whole: past every image it copies.
pub const READ_CAP: usize = 2 * 1024 * 1024;

/// `bytes`, a file read whole with a cap of [`READ_CAP`], which answers one
/// byte past the cap for a longer file.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for a file longer than [`READ_CAP`].
pub fn read_whole(bytes: Vec<u8>) -> Result<Vec<u8>, Errno> {
    if bytes.len() > READ_CAP {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(bytes)
}

/// What the fixture does to the running system.
pub trait System {
    /// Run the program at `path` with `args`, its name first, and answer its
    /// exit status.
    ///
    /// # Errors
    ///
    /// Why it could not be run or waited for.
    fn run(&mut self, path: &str, args: &[&str]) -> Result<i32, String>;

    /// The whole of the file at `path`.
    ///
    /// # Errors
    ///
    /// The open's or the read's refusal; [`Errno::LengthOutOfRange`] for a
    /// file longer than the fixture reads whole.
    fn read(&mut self, path: &str) -> Result<Vec<u8>, Errno>;

    /// Make the file at `path` hold `bytes`, creating it.
    ///
    /// # Errors
    ///
    /// The create's or the write's refusal.
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno>;

    /// Whether anything is at `path`.
    fn exists(&mut self, path: &str) -> bool;
}

/// Every step, in turn.
///
/// # Errors
///
/// The first step that did not hold, and why.
pub fn run_steps(system: &mut dyn System) -> Result<(), String> {
    for image in [ACORN, AMIGA, DISC] {
        let planted = format!("{PLANTED}/{image}");
        let bytes = system
            .read(&planted)
            .map_err(|err| format!("images: {planted}: unreadable: {err:?}"))?;
        let copy = format!("{HOME}/{image}");
        system
            .write(&copy, &bytes)
            .map_err(|err| format!("images: {copy}: not written: {err:?}"))?;
    }
    let acorn = format!("{HOME}/{ACORN}");
    let amiga = format!("{HOME}/{AMIGA}");
    let disc = format!("{HOME}/{DISC}");
    mount(system, &["mount", "-v", &acorn, "/Storage/acorn"])?;
    mount(system, &["mount", "-v", "-t", "affs", &amiga])?;
    mount(system, &["mount", "-r", &disc, "/Storage/disc"])?;
    let original = format!("{PLANTED}/{AMIGA}");
    mount(system, &["mount", &original, "/Storage/original"])?;

    let workbench = format!("/Storage/{AMIGA_NAME}");
    expect_file(system, "/Storage/acorn", README, ACORN_TEXT)?;
    expect_file(system, &workbench, README, AMIGA_TEXT)?;
    expect_file(system, "/Storage/original", README, AMIGA_TEXT)?;
    let disc_file = &RISCOS_FILES[0];
    expect_file(system, "/Storage/disc", disc_file.name, disc_file.content)?;

    for writable in ["/Storage/acorn", workbench.as_str()] {
        write_file(system, writable)?;
        expect_file(system, writable, WRITTEN, WRITTEN_TEXT)?;
    }
    for read_only in ["/Storage/disc", "/Storage/original"] {
        let path = format!("{read_only}/{WRITTEN}");
        if system.write(&path, WRITTEN_TEXT).is_ok() {
            return Err(format!("images: {path}: written on a read-only volume"));
        }
    }
    let status = system.run(MOUNT, &["mount", &amiga])?;
    if status == SUCCESS {
        return Err(format!("images: {amiga} mounted a second time"));
    }

    for name in ["acorn", AMIGA_NAME, "disc", "original"] {
        unmount(system, name)?;
    }

    mount(system, &["mount", &acorn, "/Storage/acorn"])?;
    mount(system, &["mount", &amiga])?;
    for remounted in ["/Storage/acorn", workbench.as_str()] {
        expect_file(system, remounted, WRITTEN, WRITTEN_TEXT)?;
    }
    for name in ["acorn", AMIGA_NAME] {
        unmount(system, name)?;
    }
    Ok(())
}

/// Run `mount` with `args`, and see it succeed.
fn mount(system: &mut dyn System, args: &[&str]) -> Result<(), String> {
    match system.run(MOUNT, args)? {
        SUCCESS => Ok(()),
        status => Err(format!("images: `{}` exited {status}", args.join(" "))),
    }
}

/// Eject the volume `name` with `unmount`, and see it gone.
fn unmount(system: &mut dyn System, name: &str) -> Result<(), String> {
    match system.run(UNMOUNT, &["unmount", name])? {
        SUCCESS => {}
        status => return Err(format!("images: `unmount {name}` exited {status}")),
    }
    let at = format!("/Storage/{name}");
    if system.exists(&at) {
        return Err(format!("images: {at} is still there once ejected"));
    }
    Ok(())
}

/// See `dir`'s `file` hold `expected`.
fn expect_file(
    system: &mut dyn System,
    dir: &str,
    file: &str,
    expected: &[u8],
) -> Result<(), String> {
    let path = format!("{dir}/{file}");
    let bytes = system
        .read(&path)
        .map_err(|err| format!("images: {path}: unreadable: {err:?}"))?;
    if bytes != expected {
        return Err(format!(
            "images: {path}: holds {} bytes that are not the {} expected",
            bytes.len(),
            expected.len()
        ));
    }
    Ok(())
}

/// Write [`WRITTEN_TEXT`] to `dir`'s [`WRITTEN`].
fn write_file(system: &mut dyn System, dir: &str) -> Result<(), String> {
    let path = format!("{dir}/{WRITTEN}");
    system
        .write(&path, WRITTEN_TEXT)
        .map_err(|err| format!("images: {path}: not written: {err:?}"))
}

/// The line a pass prints.
#[must_use]
pub fn pass_line() -> String {
    format!(
        "{PASS_MARKER}: {ACORN}, {AMIGA} and {DISC} mounted from {HOME}, read, written where \
         writable, ejected, and read back once mounted again\n"
    )
}

#[cfg(test)]
mod tests {
    use alloc::collections::{BTreeMap, BTreeSet};
    use alloc::string::ToString;
    use alloc::vec;

    use super::*;

    /// A running system as far as the steps see it: the files, and a mount
    /// table that `mount` and `unmount` change as the real ones would.
    #[derive(Default)]
    struct Fake {
        files: BTreeMap<String, Vec<u8>>,
        /// Each attached image: its file, its catalog name, whether
        /// writable, and what it holds.
        mounted: Vec<(String, String, bool)>,
        /// What each image file holds, by path, as a volume.
        volumes: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
        ran: Vec<String>,
        /// Paths the system user owns, which the session user may only read.
        system_owned: BTreeSet<String>,
    }

    impl Fake {
        fn planted() -> Self {
            let mut fake = Self::default();
            for (image, file, text) in [
                (ACORN, README, ACORN_TEXT),
                (AMIGA, README, AMIGA_TEXT),
                (DISC, RISCOS_FILES[0].name, RISCOS_FILES[0].content),
            ] {
                let planted = format!("{PLANTED}/{image}");
                fake.files
                    .insert(planted.clone(), image.as_bytes().to_vec());
                fake.system_owned.insert(planted);
                fake.volumes.insert(
                    image.to_string(),
                    BTreeMap::from([(file.to_string(), text.to_vec())]),
                );
            }
            fake
        }

        fn mount(&mut self, args: &[&str]) -> i32 {
            let operands: Vec<&str> = args[1..]
                .iter()
                .copied()
                .filter(|arg| !arg.starts_with('-') && *arg != "affs")
                .collect();
            let read_only = args.contains(&"-r");
            let image = operands[0];
            if self.mounted.iter().any(|(file, ..)| file == image) {
                return 32;
            }
            let leaf = image.rsplit('/').next().unwrap_or(image);
            let name = match operands.get(1) {
                Some(target) => target.trim_start_matches("/Storage/").to_string(),
                None if leaf == AMIGA => AMIGA_NAME.to_string(),
                None => leaf.to_string(),
            };
            let writable = !read_only && leaf != DISC && !self.system_owned.contains(image);
            self.mounted.push((image.to_string(), name, writable));
            0
        }

        fn unmount(&mut self, name: &str) -> i32 {
            let before = self.mounted.len();
            self.mounted.retain(|(_, mounted, _)| mounted != name);
            i32::from(self.mounted.len() == before)
        }

        /// The volume and file `path` names, if a mounted volume holds it.
        fn on_volume(&self, path: &str) -> Option<(String, bool, String)> {
            let rest = path.strip_prefix("/Storage/")?;
            let (name, file) = rest.split_once('/')?;
            let (image, _, writable) = self.mounted.iter().find(|(_, n, _)| n == name)?;
            let leaf = image.rsplit('/').next().unwrap_or(image);
            Some((leaf.to_string(), *writable, file.to_string()))
        }
    }

    impl System for Fake {
        fn run(&mut self, path: &str, args: &[&str]) -> Result<i32, String> {
            self.ran.push(args.join(" "));
            Ok(match path {
                MOUNT => self.mount(args),
                UNMOUNT => self.unmount(args[1]),
                _ => return Err(format!("no program {path}")),
            })
        }

        fn read(&mut self, path: &str) -> Result<Vec<u8>, Errno> {
            if let Some((image, _, file)) = self.on_volume(path) {
                return self.volumes[&image]
                    .get(&file)
                    .cloned()
                    .ok_or(Errno::NotFound);
            }
            self.files.get(path).cloned().ok_or(Errno::NotFound)
        }

        fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
            if let Some((image, writable, file)) = self.on_volume(path) {
                if !writable {
                    return Err(Errno::ReadOnly);
                }
                self.volumes
                    .entry(image)
                    .or_default()
                    .insert(file, bytes.to_vec());
                return Ok(());
            }
            self.files.insert(path.to_string(), bytes.to_vec());
            Ok(())
        }

        fn exists(&mut self, path: &str) -> bool {
            let name = path.trim_start_matches("/Storage/");
            self.mounted.iter().any(|(_, mounted, _)| mounted == name)
        }
    }

    #[test]
    fn every_step_holds_on_a_system_that_behaves() {
        let mut fake = Fake::planted();
        assert_eq!(run_steps(&mut fake), Ok(()));
        assert!(fake.mounted.is_empty(), "everything is ejected at the end");
        assert_eq!(
            fake.ran.first().map(String::as_str),
            Some("mount -v /Users/root/Home/acorn.adf /Storage/acorn")
        );
        let line = pass_line();
        assert!(
            line.starts_with(PASS_MARKER) && line.ends_with('\n'),
            "{line}"
        );
    }

    #[test]
    fn a_file_past_the_cap_is_refused_rather_than_read_as_its_prefix() {
        assert_eq!(
            read_whole(vec![7; READ_CAP]).map(|bytes| bytes.len()),
            Ok(READ_CAP)
        );
        assert_eq!(
            read_whole(vec![7; READ_CAP + 1]),
            Err(Errno::LengthOutOfRange),
            "the capped read's one byte past the cap names a longer file"
        );
    }

    #[test]
    fn a_write_that_did_not_reach_the_image_is_caught_on_the_remount() {
        struct Forgetful(Fake);

        impl System for Forgetful {
            fn run(&mut self, path: &str, args: &[&str]) -> Result<i32, String> {
                if path == UNMOUNT {
                    for volume in self.0.volumes.values_mut() {
                        volume.remove(WRITTEN);
                    }
                }
                self.0.run(path, args)
            }
            fn read(&mut self, path: &str) -> Result<Vec<u8>, Errno> {
                self.0.read(path)
            }
            fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
                self.0.write(path, bytes)
            }
            fn exists(&mut self, path: &str) -> bool {
                self.0.exists(path)
            }
        }

        let why = run_steps(&mut Forgetful(Fake::planted())).expect_err("lost");
        assert!(
            why.contains("/Storage/acorn/Written") && why.contains("NotFound"),
            "{why}"
        );
    }

    #[test]
    fn a_read_only_volume_that_takes_a_write_fails() {
        let mut fake = Fake::planted();
        fake.system_owned.clear();
        let why = run_steps(&mut fake).expect_err("the original took a write");
        assert!(why.contains("/Storage/original/Written"), "{why}");
    }

    #[test]
    fn a_failing_mount_names_the_command() {
        struct Refusing;

        impl System for Refusing {
            fn run(&mut self, _path: &str, _args: &[&str]) -> Result<i32, String> {
                Ok(32)
            }
            fn read(&mut self, _path: &str) -> Result<Vec<u8>, Errno> {
                Ok(vec![1])
            }
            fn write(&mut self, _path: &str, _bytes: &[u8]) -> Result<(), Errno> {
                Ok(())
            }
            fn exists(&mut self, _path: &str) -> bool {
                false
            }
        }

        let why = run_steps(&mut Refusing).expect_err("refused");
        assert_eq!(
            why,
            "images: `mount -v /Users/root/Home/acorn.adf /Storage/acorn` exited 32"
        );
    }

    /// [`COMMAND`] is the manifest source's `name`, so the script, the end's
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
