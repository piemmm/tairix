//! Replacing a whole file so that a crash or power loss at any point leaves
//! either the old contents or the new, never an empty or torn file.
//!
//! The new bytes are written to a sibling temporary of their own, which takes
//! the original's extended attributes, owner, group and mode, is synced, and
//! is renamed over the original; a second sync then puts the rename itself on
//! the medium. Each
//! replace draws its own temporary name, so two writers of one file never
//! write through each other's temporary, and the rename stays within the
//! file's own directory and volume.

use alloc::vec::Vec;

use tairix_abi::{Errno, FileStat, OpenFlags, RenameFlags, UnlinkFlags, FS_MODE_MASK};
use tairix_fscopy::{copy_attributes, AttrError, Attributes};

use crate::copy::RtAttributes;
use crate::File;

/// Whether a replace may create the directory its file lives in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ParentDir {
    /// The directory must already exist.
    Existing,
    /// The directory is created when, and only when, the replace finds it
    /// missing, so a writer that may not create it is not refused (and
    /// audited) on every replace for trying.
    CreatedIfMissing,
}

/// Replace the file at the absolute `path` with `bytes`, so that a crash or
/// power loss at any point leaves the old contents or the new.
///
/// The replacement keeps the original's extended attributes, owner, group and
/// mode. A file's ACL and capability gate are not visible to a program, so
/// neither would be kept: this is not the way to replace a file carrying
/// either.
///
/// # Errors
///
/// The first refusal: reading the original's security, drawing the
/// temporary's name, creating the temporary, giving it the original's
/// attributes or security, writing or syncing it, or the rename. Each leaves
/// the original in place and removes the temporary.
/// A refusal of the final sync comes after the rename: the file holds the new
/// bytes, which may not yet be on the medium.
pub fn replace_file(path: &[u8], bytes: &[u8], parent: ParentDir) -> Result<(), Errno> {
    let mut tag = [0u8; TAG_LEN];
    crate::random_fill(&mut tag)?;
    replace(&mut Syscalls, path, bytes, parent, tag)
}

/// Random bytes in a temporary's name.
const TAG_LEN: usize = 8;

/// What a replacement keeps of the file it replaces.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Security {
    uid: u32,
    gid: u32,
    mode: u32,
}

impl Security {
    fn of(stat: &FileStat) -> Self {
        Self {
            uid: stat.uid,
            gid: stat.gid,
            mode: stat.mode & FS_MODE_MASK,
        }
    }
}

/// The filesystem calls a replace makes, so the ordering can be tested
/// against a medium that loses what was never synced.
trait ReplaceFs {
    type File;
    fn security_of_path(&mut self, path: &[u8]) -> Result<Option<Security>, Errno>;
    fn create_new(&mut self, path: &[u8]) -> Result<Self::File, Errno>;
    fn security_of(&mut self, file: &Self::File) -> Result<Security, Errno>;
    /// Give `to` every extended attribute `from` carries.
    fn carry_attributes(&mut self, from: &[u8], to: &[u8]) -> Result<(), Errno>;
    fn set_owner(&mut self, path: &[u8], uid: u32, gid: u32) -> Result<(), Errno>;
    fn set_mode(&mut self, path: &[u8], mode: u32) -> Result<(), Errno>;
    fn write_all(&mut self, file: &Self::File, bytes: &[u8]) -> Result<(), Errno>;
    fn sync(&mut self, file: &Self::File) -> Result<(), Errno>;
    fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), Errno>;
    fn unlink(&mut self, path: &[u8]);
    fn mkdir(&mut self, path: &[u8]) -> Result<(), Errno>;
}

fn replace<F: ReplaceFs>(
    fs: &mut F,
    path: &[u8],
    bytes: &[u8],
    parent: ParentDir,
    tag: [u8; TAG_LEN],
) -> Result<(), Errno> {
    let kept = fs.security_of_path(path)?;
    let staged = staged_path(path, tag)?;
    let file = match fs.create_new(&staged) {
        Err(Errno::NotFound) if parent == ParentDir::CreatedIfMissing => {
            match fs.mkdir(parent_of(path).ok_or(Errno::NotFound)?) {
                Ok(()) | Err(Errno::AlreadyExists) => {}
                Err(err) => return Err(err),
            }
            fs.create_new(&staged)?
        }
        created => created?,
    };
    let published =
        stage(fs, &file, (path, &staged), kept, bytes).and_then(|()| fs.rename(&staged, path));
    if let Err(err) = published {
        fs.unlink(&staged);
        return Err(err);
    }
    fs.sync(&file)
}

/// Give the temporary `staged` the `original`'s attributes and security and
/// the new bytes, and put them on the medium before the rename can make them
/// the file.
fn stage<F: ReplaceFs>(
    fs: &mut F,
    file: &F::File,
    (original, staged): (&[u8], &[u8]),
    kept: Option<Security>,
    bytes: &[u8],
) -> Result<(), Errno> {
    if let Some(kept) = kept {
        // While the writer still owns the temporary, so the owner and mode it
        // is about to be given cannot refuse it the attributes.
        fs.carry_attributes(original, staged)?;
        let made = fs.security_of(file)?;
        // An ownership change clears the setuid bit, so the mode follows it.
        if (made.uid, made.gid) != (kept.uid, kept.gid) {
            fs.set_owner(staged, kept.uid, kept.gid)?;
            fs.set_mode(staged, kept.mode)?;
        } else if made.mode != kept.mode {
            fs.set_mode(staged, kept.mode)?;
        }
    }
    fs.write_all(file, bytes)?;
    fs.sync(file)
}

/// A fresh name beside `path`, in its own directory, for building `path`'s
/// replacement under and renaming it from: the rule [`replace_file`] stages
/// by, so every staged replacement on the system is spelled alike.
///
/// # Errors
///
/// The random draw's refusal, or [`Errno::OutOfMemory`].
pub fn staging_name(path: &str) -> Result<alloc::string::String, Errno> {
    let mut tag = [0u8; TAG_LEN];
    crate::random_fill(&mut tag)?;
    let staged = staged_path(path.as_bytes(), tag)?;
    alloc::string::String::from_utf8(staged).map_err(|_| Errno::OutOfRange)
}

/// `<path>.<tag>.new`, the tag in lowercase hex; a name too long to carry the
/// tag is staged as `.<tag>.new` in the same directory instead, so a name of
/// any length can be replaced.
fn staged_path(path: &[u8], tag: [u8; TAG_LEN]) -> Result<Vec<u8>, Errno> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    const SUFFIX: &[u8] = b".new";
    let tagged = 1 + 2 * TAG_LEN + SUFFIX.len();
    let leaf = path.len()
        - path
            .iter()
            .rposition(|&b| b == b'/')
            .map_or(0, |slash| slash + 1);
    let stem = if leaf + tagged <= tairix_abi::fs::FS_NAME_MAX {
        path
    } else {
        &path[..path.len() - leaf]
    };
    let mut staged = Vec::new();
    staged
        .try_reserve_exact(stem.len() + tagged)
        .map_err(|_| Errno::OutOfMemory)?;
    staged.extend_from_slice(stem);
    staged.push(b'.');
    for byte in tag {
        staged.push(HEX[usize::from(byte >> 4)]);
        staged.push(HEX[usize::from(byte & 0x0f)]);
    }
    staged.extend_from_slice(SUFFIX);
    Ok(staged)
}

/// The directory an absolute `path` names a file in.
fn parent_of(path: &[u8]) -> Option<&[u8]> {
    match path.iter().rposition(|&b| b == b'/')? {
        0 => Some(b"/"),
        slash => Some(&path[..slash]),
    }
}

/// The calls as the kernel serves them.
struct Syscalls;

/// A negative syscall result as its error.
fn refusal(ret: i64) -> Result<(), Errno> {
    if ret == 0 {
        Ok(())
    } else {
        Err(Errno::from_syscall(ret))
    }
}

impl ReplaceFs for Syscalls {
    type File = File;

    fn security_of_path(&mut self, path: &[u8]) -> Result<Option<Security>, Errno> {
        // A resolve-only handle: its stat needs no read access to the file.
        match File::open(path, OpenFlags::empty()) {
            Ok(file) => file
                .stat()
                .map(|stat| Some(Security::of(&stat)))
                .map_err(Errno::from_syscall),
            Err(raw) if Errno::from_syscall(raw) == Errno::NotFound => Ok(None),
            Err(raw) => Err(Errno::from_syscall(raw)),
        }
    }

    fn create_new(&mut self, path: &[u8]) -> Result<File, Errno> {
        crate::create_new(path).map_err(Errno::from_syscall)
    }

    fn security_of(&mut self, file: &File) -> Result<Security, Errno> {
        file.stat()
            .map(|stat| Security::of(&stat))
            .map_err(Errno::from_syscall)
    }

    fn carry_attributes(&mut self, from: &[u8], to: &[u8]) -> Result<(), Errno> {
        let spelled = |path| core::str::from_utf8(path).map_err(|_| Errno::OutOfRange);
        // Every one, or the replace: a rewrite that dropped a file's
        // attributes would be a different file under the same name.
        copy_attributes(
            &RtAttributes,
            spelled(from)?,
            spelled(to)?,
            Attributes::Require,
        )
        .map(drop)
        .map_err(|err| match err {
            AttrError::Read(errno) | AttrError::Write { error: errno, .. } => errno,
        })
    }

    fn set_owner(&mut self, path: &[u8], uid: u32, gid: u32) -> Result<(), Errno> {
        refusal(crate::fs_set_owner(path, uid, gid))
    }

    fn set_mode(&mut self, path: &[u8], mode: u32) -> Result<(), Errno> {
        refusal(crate::fs_set_mode(path, mode))
    }

    fn write_all(&mut self, file: &File, bytes: &[u8]) -> Result<(), Errno> {
        crate::fs_write_all(file.fd(), 0, bytes)
    }

    fn sync(&mut self, file: &File) -> Result<(), Errno> {
        file.sync().map_err(Errno::from_syscall)
    }

    fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), Errno> {
        refusal(crate::fs_rename(from, to, RenameFlags::empty()))
    }

    fn unlink(&mut self, path: &[u8]) {
        // Best effort: a temporary that cannot be removed is never read.
        let _ = crate::fs_unlink(path, UnlinkFlags::empty());
    }

    fn mkdir(&mut self, path: &[u8]) -> Result<(), Errno> {
        refusal(crate::fs_mkdir(path))
    }
}

#[cfg(test)]
#[path = "replace_tests.rs"]
mod tests;
