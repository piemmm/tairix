//! Replacing a whole file so that a crash or power loss at any point leaves
//! either the old contents or the new, never an empty or torn file, and
//! replacing a name with a link so that it is never missing.
//!
//! The new bytes go to a sibling temporary of their own, made owner-only in
//! the file's directory, held open, and kept open itself: the bytes, the
//! original's extended attributes, owner, group and mode, and the sync all
//! reach the temporary through its descriptor, whatever its name has come to
//! hold, and the rename puts it in place only while that name still holds it.
//! A second sync then puts the rename itself on the medium. Each replace draws
//! its own temporary name, so two writers of one file never write through each
//! other's temporary, and the rename stays within the file's own directory and
//! volume. A link replacing a name is made under such a temporary and renamed
//! over the name the same way.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::{FD_CWD, FS_NEW_DIR_MODE, FS_SETGID_BIT, FS_SETUID_BIT};
use tairix_abi::{
    Errno, FileStat, LinkFlags, NodeExpect, OpenFlags, RenameFlags, UnlinkFlags, FS_MODE_MASK,
};
use tairix_fscopy::tree::{self, Roots};
use tairix_fscopy::{copy_attributes, AttrError, Attributes, OWNER_RW};

use crate::copy::WorkingDirectory;
use crate::{Dir, File};

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
/// The replacement keeps the extended attributes, owner, group and mode of
/// what `path` leads to, a final link followed; the name itself is replaced.
/// A file that did not exist is created with `mode` less the process's
/// umask. A file's ACL and capability gate are not visible to a program, so
/// neither would be kept: this is not the way to replace a file carrying
/// either.
///
/// # Errors
///
/// The first refusal: opening the file's directory, reading the original's
/// security, drawing the temporary's name, creating the temporary, writing
/// it, giving it the original's attributes or security, syncing it, or the
/// rename, which answers [`Errno::Stale`] when the temporary's name no longer
/// holds it. Each leaves the original in place and removes the temporary
/// while its name still holds it. A refusal of the final sync comes after the
/// rename: the file holds the new bytes, which may not yet be on the medium.
pub fn replace_file(path: &str, bytes: &[u8], parent: ParentDir, mode: u32) -> Result<(), Errno> {
    let mut tag = [0u8; TAG_LEN];
    crate::random_fill(&mut tag)?;
    replace(&mut Syscalls, path, bytes, parent, mode, tag)
}

/// The link a name is replaced with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NewLink<'a> {
    /// A symbolic link storing this target verbatim.
    Symbolic(&'a str),
    /// A second name for the node `existing` names, its final link followed
    /// when `follow` (`linkat`'s `AT_SYMLINK_FOLLOW`).
    Hard {
        /// The name of the node that gains a name.
        existing: &'a str,
        /// Whether a final link of `existing` is followed.
        follow: bool,
    },
}

/// Make `link` the link `new` describes in place of whatever the name holds,
/// in one rename: the link is made under a temporary beside `link` and renamed
/// over it, so the name is never missing, and nothing it held is removed
/// unless the new link exists. A hard link's missing or refused source is
/// therefore refused before the name is touched.
///
/// # Errors
///
/// The link's directory could not be opened or the temporary's name drawn;
/// the link could not be made (the link call's own refusal); or the rename
/// was refused, the temporary then removed while its name still holds it.
/// Each leaves `link` as it was.
pub fn replace_with_link(link: &str, new: NewLink<'_>) -> Result<(), Errno> {
    let mut tag = [0u8; TAG_LEN];
    crate::random_fill(&mut tag)?;
    replace_link(&mut Syscalls, link, new, tag)
}

fn replace_link<F: ReplaceFs>(
    fs: &mut F,
    link: &str,
    new: NewLink<'_>,
    tag: [u8; TAG_LEN],
) -> Result<(), Errno> {
    let (dir, name) = fs.parent(link)?;
    let staged = staged_name(&name, tag)?;
    fs.make_link(new, &dir, &staged)?;
    let made = fs
        .node(&dir, &staged)
        .and_then(|made| fs.stat(&made))
        .inspect_err(|_| fs.forsake(&dir, &staged))?;
    let made = NodeExpect::of_node(made.id);
    fs.rename(&dir, &staged, &name, &made)
        .inspect_err(|_| fs.unlink(&dir, &staged, &made))
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
    type Dir;
    type File;
    /// The directory holding `path`'s last component, opened through the path
    /// as written, and that component.
    fn parent(&mut self, path: &str) -> Result<(Self::Dir, String), Errno>;
    /// Make the directory `path`.
    fn mkdir(&mut self, path: &str, mode: u32) -> Result<(), Errno>;
    /// What `name` leads to in `dir`, a final link followed, held with no
    /// access; `None` when it names nothing.
    fn original(&mut self, dir: &Self::Dir, name: &str) -> Result<Option<Self::File>, Errno>;
    /// What `name` names in `dir`, a link itself, held with no access.
    fn node(&mut self, dir: &Self::Dir, name: &str) -> Result<Self::File, Errno>;
    /// A new file `name` in `dir`, which must be vacant, born with `mode`
    /// less the umask.
    fn create(&mut self, dir: &Self::Dir, name: &str, mode: u32) -> Result<Self::File, Errno>;
    fn stat(&mut self, file: &Self::File) -> Result<FileStat, Errno>;
    /// Give `to` every extended attribute `from` carries.
    fn carry_attributes(&mut self, from: &Self::File, to: &Self::File) -> Result<(), Errno>;
    fn set_owner(&mut self, file: &Self::File, uid: u32, gid: u32) -> Result<(), Errno>;
    fn set_mode(&mut self, file: &Self::File, mode: u32) -> Result<(), Errno>;
    fn write_all(&mut self, file: &Self::File, bytes: &[u8]) -> Result<(), Errno>;
    fn sync(&mut self, file: &Self::File) -> Result<(), Errno>;
    /// Rename `from` over `to` in `dir`, only while `from` names the node
    /// `expect` names.
    fn rename(
        &mut self,
        dir: &Self::Dir,
        from: &str,
        to: &str,
        expect: &NodeExpect,
    ) -> Result<(), Errno>;
    /// Remove `name` from `dir`, only while it names the node `expect` names.
    fn unlink(&mut self, dir: &Self::Dir, name: &str, expect: &NodeExpect);
    /// Remove the temporary `name` this replace just made in `dir`, before it
    /// could be read: by the name alone, as far as it will go.
    fn forsake(&mut self, dir: &Self::Dir, name: &str);
    /// The process's file mode creation mask.
    fn umask(&mut self) -> u32;
    /// Make the link `new` describes at `at` in `dir`, which must be vacant.
    fn make_link(&mut self, new: NewLink<'_>, dir: &Self::Dir, at: &str) -> Result<(), Errno>;
}

fn replace<F: ReplaceFs>(
    fs: &mut F,
    path: &str,
    bytes: &[u8],
    parent: ParentDir,
    mode: u32,
    tag: [u8; TAG_LEN],
) -> Result<(), Errno> {
    let (dir, name) = match fs.parent(path) {
        Err(Errno::NotFound) if parent == ParentDir::CreatedIfMissing => {
            match fs.mkdir(tairix_path::dir_name(path), FS_NEW_DIR_MODE) {
                Ok(()) | Err(Errno::AlreadyExists) => {}
                Err(err) => return Err(err),
            }
            fs.parent(path)?
        }
        held => held?,
    };
    let original = fs.original(&dir, &name)?;
    let kept = match &original {
        Some(original) => Some(Security::of(&fs.stat(original)?)),
        None => None,
    };
    let staged = staged_name(&name, tag)?;
    let file = fs.create(&dir, &staged, OWNER_RW)?;
    let born = fs.stat(&file).inspect_err(|_| fs.forsake(&dir, &staged))?;
    let expect = NodeExpect::of_node(born.id);
    let wanted = kept.unwrap_or_else(|| Security {
        mode: mode & FS_MODE_MASK & !fs.umask(),
        ..Security::of(&born)
    });
    let published = stage(
        fs,
        &file,
        original.as_ref(),
        Security::of(&born),
        wanted,
        bytes,
    )
    .and_then(|()| fs.rename(&dir, &staged, &name, &expect));
    if let Err(err) = published {
        fs.unlink(&dir, &staged, &expect);
        return Err(err);
    }
    fs.sync(&file)
}

/// Give the temporary `file`, born `made`, the new bytes, the `original`'s
/// attributes and the security it is `wanted` with, and put them on the
/// medium before the rename can make them the file.
fn stage<F: ReplaceFs>(
    fs: &mut F,
    file: &F::File,
    original: Option<&F::File>,
    made: Security,
    wanted: Security,
    bytes: &[u8],
) -> Result<(), Errno> {
    fs.write_all(file, bytes)?;
    if let Some(original) = original {
        // While the writer still owns the temporary, so the owner and mode it
        // is about to be given cannot refuse it the attributes.
        fs.carry_attributes(original, file)?;
    }
    // Only its owner sets a node's mode, so the mode goes while the writer
    // still owns it. The system clears set-ID bits as the owner or group
    // changes: one the writer still owns takes them back.
    if made.mode != wanted.mode {
        fs.set_mode(file, wanted.mode)?;
    }
    if (made.uid, made.gid) != (wanted.uid, wanted.gid) {
        fs.set_owner(file, wanted.uid, wanted.gid)?;
        if made.uid == wanted.uid && wanted.mode & (FS_SETUID_BIT | FS_SETGID_BIT) != 0 {
            fs.set_mode(file, wanted.mode)?;
        }
    }
    fs.sync(file)
}

/// A fresh name beside `path`, in its own directory, for building `path`'s
/// replacement under and renaming it from: the rule [`replace_file`] stages
/// by, so every staged replacement on the system is spelled alike. `path`
/// may be a bare name, which answers a bare name.
///
/// # Errors
///
/// The random draw's refusal, or [`Errno::OutOfMemory`].
pub fn staging_name(path: &str) -> Result<String, Errno> {
    let mut tag = [0u8; TAG_LEN];
    crate::random_fill(&mut tag)?;
    staged_name(path, tag)
}

/// [`staged_path`] of `name`, as text.
fn staged_name(name: &str, tag: [u8; TAG_LEN]) -> Result<String, Errno> {
    String::from_utf8(staged_path(name.as_bytes(), tag)?).map_err(|_| Errno::OutOfRange)
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
    type Dir = Dir;
    type File = File;

    fn parent(&mut self, path: &str) -> Result<(Dir, String), Errno> {
        WorkingDirectory.parent(path)
    }

    fn mkdir(&mut self, path: &str, mode: u32) -> Result<(), Errno> {
        refusal(crate::fs_mkdir(path.as_bytes(), mode))
    }

    fn original(&mut self, dir: &Dir, name: &str) -> Result<Option<File>, Errno> {
        match Dir::open_file(dir, name.as_bytes(), OpenFlags::empty()) {
            Ok(file) => Ok(Some(file)),
            Err(ret) if Errno::from_syscall(ret) == Errno::NotFound => Ok(None),
            Err(ret) => Err(Errno::from_syscall(ret)),
        }
    }

    fn node(&mut self, dir: &Dir, name: &str) -> Result<File, Errno> {
        tree::Dir::open_node(dir, name)
    }

    fn create(&mut self, dir: &Dir, name: &str, mode: u32) -> Result<File, Errno> {
        tree::Dir::create_file(dir, name, mode)
    }

    fn stat(&mut self, file: &File) -> Result<FileStat, Errno> {
        file.stat().map_err(Errno::from_syscall)
    }

    fn carry_attributes(&mut self, from: &File, to: &File) -> Result<(), Errno> {
        // Every one, or the replace: a rewrite that dropped a file's
        // attributes would be a different file under the same name.
        copy_attributes(from, to, Attributes::Require)
            .map(drop)
            .map_err(|err| match err {
                AttrError::Read(errno) | AttrError::Write { error: errno, .. } => errno,
            })
    }

    fn set_owner(&mut self, file: &File, uid: u32, gid: u32) -> Result<(), Errno> {
        file.set_owner(uid, gid).map_err(Errno::from_syscall)
    }

    fn set_mode(&mut self, file: &File, mode: u32) -> Result<(), Errno> {
        file.set_mode(mode).map_err(Errno::from_syscall)
    }

    fn write_all(&mut self, file: &File, bytes: &[u8]) -> Result<(), Errno> {
        crate::fs_write_all(file.fd(), 0, bytes)
    }

    fn sync(&mut self, file: &File) -> Result<(), Errno> {
        file.sync().map_err(Errno::from_syscall)
    }

    fn rename(
        &mut self,
        dir: &Dir,
        from: &str,
        to: &str,
        expect: &NodeExpect,
    ) -> Result<(), Errno> {
        dir.rename(
            from.as_bytes(),
            dir,
            to.as_bytes(),
            RenameFlags::EXPECT_NODE,
            Some(expect),
        )
        .map_err(Errno::from_syscall)
    }

    fn unlink(&mut self, dir: &Dir, name: &str, expect: &NodeExpect) {
        // Best effort: a temporary that cannot be removed is never read.
        let _ = dir.unlink(name.as_bytes(), UnlinkFlags::EXPECT_NODE, Some(expect));
    }

    fn forsake(&mut self, dir: &Dir, name: &str) {
        let _ = dir.unlink(name.as_bytes(), UnlinkFlags::empty(), None);
    }

    fn umask(&mut self) -> u32 {
        crate::umask_get()
    }

    fn make_link(&mut self, new: NewLink<'_>, dir: &Dir, at: &str) -> Result<(), Errno> {
        match new {
            NewLink::Symbolic(target) => dir
                .symlink(target.as_bytes(), at.as_bytes())
                .map_err(Errno::from_syscall),
            NewLink::Hard { existing, follow } => {
                let flags = if follow {
                    LinkFlags::FOLLOW
                } else {
                    LinkFlags::empty()
                };
                refusal(crate::fs_link_at(
                    FD_CWD,
                    existing.as_bytes(),
                    dir.fd(),
                    at.as_bytes(),
                    flags,
                ))
            }
        }
    }
}

#[cfg(test)]
#[path = "replace_tests.rs"]
mod tests;
