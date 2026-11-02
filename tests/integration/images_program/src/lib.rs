//! The `images` fixture of the disk-image QEMU vertical
//! (`plans/DISK-IMAGES.md` DI7, `plans/ARCHIVES.md` AR11). Run from the
//! scripted root shell, it copies the images and archives the vertical
//! planted ([`PLANTED`], the system's) into the user's home, mounts each copy
//! with the real `mount`, reads what it holds and its RISC OS filetypes,
//! writes and types a file where the volume can be written, ejects it with
//! the real `unmount`, and mounts the floppies and the written archives again
//! to read back what it wrote. It also mounts a planted original, which the
//! user may only read, to see it mounted read-only, and a RAR archive, which
//! no one writes.
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

/// An archive the vertical plants.
#[derive(Copy, Clone, Debug)]
pub struct Archive {
    /// Its file name, which its format's ending is part of.
    pub file: &'static str,
    /// The catalog name the fixture mounts it as.
    pub volume: &'static str,
}

/// The archives the vertical plants, one of each format the archive driver
/// writes, each holding [`README`] typed [`TEXT_TYPE`].
pub const WRITTEN_ARCHIVES: [Archive; 6] = [
    Archive {
        file: "archive.zip",
        volume: "zip",
    },
    Archive {
        file: "archive.tar",
        volume: "tar",
    },
    Archive {
        file: "archive.tgz",
        volume: "tgz",
    },
    Archive {
        file: "archive.tbz2",
        volume: "tbz2",
    },
    Archive {
        file: "archive.spk",
        volume: "spark",
    },
    Archive {
        file: "archive.arc",
        volume: "arcfs",
    },
];

/// What each written archive's [`README`] holds.
pub const ARCHIVE_TEXT: &[u8] = b"An archive holds this, typed as RISC OS text.";

/// The RAR archive the vertical plants, which the driver only reads: the RAR
/// encoder's, holding [`RAR_MEMBERS`].
pub const RAR: Archive = Archive {
    file: "archive.rar",
    volume: "rar",
};

/// The RAR archive's members: the same text, one stored and one compressed.
pub const RAR_MEMBERS: [&str; 2] = ["stest1.txt", "stest2.txt"];

/// How long each of [`RAR_MEMBERS`] is.
pub const RAR_MEMBER_LEN: usize = 2048;

/// The attribute a RISC OS filetype is carried as.
pub const FILETYPE: &str = "acorn.filetype";

/// The filetype each written archive's [`README`] is planted with: Text.
pub const TEXT_TYPE: &[u8] = b"fff";

/// The filetype the fixture gives [`WRITTEN`] in each written archive: Data.
pub const DATA_TYPE: &[u8] = b"ffd";

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

    /// The value of the attribute `key` of the node at `path`.
    ///
    /// # Errors
    ///
    /// The open's or the read's refusal: [`Errno::NoData`] where the node
    /// carries no such attribute.
    fn attribute(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno>;

    /// Give the node at `path` the attribute `key` holding `value`.
    ///
    /// # Errors
    ///
    /// The open's or the write's refusal.
    fn set_attribute(&mut self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno>;
}

/// Every step, in turn.
///
/// # Errors
///
/// The first step that did not hold, and why.
pub fn run_steps(system: &mut dyn System) -> Result<(), String> {
    for image in [ACORN, AMIGA, DISC] {
        copy_home(system, image)?;
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
    archive_steps(system)
}

/// The archives' steps: each copied into the home and mounted; what it holds
/// read, with its filetype; a file written and typed where the driver writes
/// the format, and refused on the RAR; each ejected, and every written one
/// mounted again to read both files and their types back.
fn archive_steps(system: &mut dyn System) -> Result<(), String> {
    for archive in WRITTEN_ARCHIVES.iter().chain([&RAR]) {
        copy_home(system, archive.file)?;
        let copy = format!("{HOME}/{}", archive.file);
        mount(
            system,
            &["mount", &copy, &format!("/Storage/{}", archive.volume)],
        )?;
    }
    for archive in &WRITTEN_ARCHIVES {
        let volume = format!("/Storage/{}", archive.volume);
        expect_file(system, &volume, README, ARCHIVE_TEXT)?;
        expect_filetype(system, &volume, README, TEXT_TYPE)?;
        write_file(system, &volume)?;
        let written = format!("{volume}/{WRITTEN}");
        system
            .set_attribute(&written, FILETYPE, DATA_TYPE)
            .map_err(|err| format!("images: {written}: not typed: {err:?}"))?;
    }
    expect_rar(system, &format!("/Storage/{}", RAR.volume))?;
    for archive in WRITTEN_ARCHIVES.iter().chain([&RAR]) {
        unmount(system, archive.volume)?;
    }
    for archive in &WRITTEN_ARCHIVES {
        let copy = format!("{HOME}/{}", archive.file);
        let volume = format!("/Storage/{}", archive.volume);
        mount(system, &["mount", &copy, &volume])?;
        expect_file(system, &volume, README, ARCHIVE_TEXT)?;
        expect_filetype(system, &volume, README, TEXT_TYPE)?;
        expect_file(system, &volume, WRITTEN, WRITTEN_TEXT)?;
        expect_filetype(system, &volume, WRITTEN, DATA_TYPE)?;
        unmount(system, archive.volume)?;
    }
    Ok(())
}

/// Copy the planted `name` into the home, as the session user.
fn copy_home(system: &mut dyn System, name: &str) -> Result<(), String> {
    let planted = format!("{PLANTED}/{name}");
    let bytes = system
        .read(&planted)
        .map_err(|err| format!("images: {planted}: unreadable: {err:?}"))?;
    let copy = format!("{HOME}/{name}");
    system
        .write(&copy, &bytes)
        .map_err(|err| format!("images: {copy}: not written: {err:?}"))
}

/// See the RAR archive at `volume` hold its members whole, as the decoder
/// checked them, and refuse a write.
fn expect_rar(system: &mut dyn System, volume: &str) -> Result<(), String> {
    let mut members = Vec::new();
    for member in RAR_MEMBERS {
        let path = format!("{volume}/{member}");
        let bytes = system
            .read(&path)
            .map_err(|err| format!("images: {path}: unreadable: {err:?}"))?;
        if bytes.len() != RAR_MEMBER_LEN {
            return Err(format!(
                "images: {path}: {} bytes, not {RAR_MEMBER_LEN}",
                bytes.len()
            ));
        }
        members.push(bytes);
    }
    if members.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(format!("images: {volume}: its members differ"));
    }
    let path = format!("{volume}/{WRITTEN}");
    if system.write(&path, WRITTEN_TEXT).is_ok() {
        return Err(format!("images: {path}: written on a RAR archive"));
    }
    Ok(())
}

/// See `dir`'s `file` typed `filetype`.
fn expect_filetype(
    system: &mut dyn System,
    dir: &str,
    file: &str,
    filetype: &[u8],
) -> Result<(), String> {
    let path = format!("{dir}/{file}");
    let value = system
        .attribute(&path, FILETYPE)
        .map_err(|err| format!("images: {path}: no {FILETYPE}: {err:?}"))?;
    if value != filetype {
        return Err(format!("images: {path}: typed {value:?}, not {filetype:?}"));
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
        "{PASS_MARKER}: {ACORN}, {AMIGA}, {DISC} and {} archives mounted from {HOME}, read, \
         written where writable, ejected, and read back with their RISC OS types once mounted \
         again\n",
        WRITTEN_ARCHIVES.len() + 1
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
        /// Each volume's files' attributes, by image file, file and key.
        attributes: BTreeMap<(String, String, String), Vec<u8>>,
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
                fake.plant(image, &[(file, text)]);
            }
            for archive in WRITTEN_ARCHIVES {
                fake.plant(archive.file, &[(README, ARCHIVE_TEXT)]);
                fake.attributes.insert(
                    (
                        archive.file.to_string(),
                        README.to_string(),
                        FILETYPE.to_string(),
                    ),
                    TEXT_TYPE.to_vec(),
                );
            }
            let member = vec![7u8; RAR_MEMBER_LEN];
            fake.plant(RAR.file, &RAR_MEMBERS.map(|name| (name, member.as_slice())));
            fake
        }

        /// Plant the system's `image`, a volume of `files`.
        fn plant(&mut self, image: &str, files: &[(&str, &[u8])]) {
            let planted = format!("{PLANTED}/{image}");
            self.files
                .insert(planted.clone(), image.as_bytes().to_vec());
            self.system_owned.insert(planted);
            self.volumes.insert(
                image.to_string(),
                files
                    .iter()
                    .map(|(name, bytes)| (name.to_string(), bytes.to_vec()))
                    .collect(),
            );
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
            let writable = !read_only
                && leaf != DISC
                && leaf != RAR.file
                && !self.system_owned.contains(image);
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

        fn attribute(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
            let (image, _, file) = self.on_volume(path).ok_or(Errno::NotFound)?;
            self.attributes
                .get(&(image, file, key.to_string()))
                .cloned()
                .ok_or(Errno::NoData)
        }

        fn set_attribute(&mut self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
            let (image, writable, file) = self.on_volume(path).ok_or(Errno::NotFound)?;
            if !writable {
                return Err(Errno::ReadOnly);
            }
            self.attributes
                .insert((image, file, key.to_string()), value.to_vec());
            Ok(())
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
            fn attribute(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
                self.0.attribute(path, key)
            }
            fn set_attribute(&mut self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
                self.0.set_attribute(path, key, value)
            }
        }

        let why = run_steps(&mut Forgetful(Fake::planted())).expect_err("lost");
        assert!(
            why.contains("/Storage/acorn/Written") && why.contains("NotFound"),
            "{why}"
        );
    }

    /// A volume that loses its files' types on the way to its archive is
    /// caught when the archive is mounted again.
    #[test]
    fn a_filetype_that_did_not_reach_the_archive_is_caught_on_the_remount() {
        struct Untyping(Fake);

        impl System for Untyping {
            fn run(&mut self, path: &str, args: &[&str]) -> Result<i32, String> {
                if path == UNMOUNT {
                    self.0.attributes.clear();
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
            fn attribute(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
                self.0.attribute(path, key)
            }
            fn set_attribute(&mut self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
                self.0.set_attribute(path, key, value)
            }
        }

        let why = run_steps(&mut Untyping(Fake::planted())).expect_err("untyped");
        assert!(
            why.contains("/Storage/zip/ReadMe") && why.contains("NoData"),
            "{why}"
        );
    }

    #[test]
    fn a_rar_archive_that_takes_a_write_fails() {
        struct Writable(Fake);
        impl System for Writable {
            fn run(&mut self, path: &str, args: &[&str]) -> Result<i32, String> {
                let status = self.0.run(path, args)?;
                for (image, _, writable) in &mut self.0.mounted {
                    *writable |= image.ends_with(RAR.file);
                }
                Ok(status)
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
            fn attribute(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
                self.0.attribute(path, key)
            }
            fn set_attribute(&mut self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
                self.0.set_attribute(path, key, value)
            }
        }
        let why = run_steps(&mut Writable(Fake::planted())).expect_err("a RAR took a write");
        assert!(why.contains("written on a RAR archive"), "{why}");
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
            fn attribute(&mut self, _path: &str, _key: &str) -> Result<Vec<u8>, Errno> {
                Err(Errno::NoData)
            }
            fn set_attribute(
                &mut self,
                _path: &str,
                _key: &str,
                _value: &[u8],
            ) -> Result<(), Errno> {
                Ok(())
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
