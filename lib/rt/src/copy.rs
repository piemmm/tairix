//! The shared tree seam (`tairix_fscopy::tree`) and the copy engine's data
//! seams over the running system: a [`File`] is a held node, read and
//! written through its descriptor; a [`Dir`] is a held directory whose
//! children are reached by name from it, through a link only by
//! [`tree::Dir::enter`]; and [`WorkingDirectory`] opens a tool's operands.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::ControlFlow;

use tairix_abi::fs::{DirEntries, SeekRegion, FD_CWD};
use tairix_abi::{
    Access, AtFlags, DirEntry, Errno, FileStat, LinkFlags, NodeExpect, OpenFlags, RenameFlags,
    UnlinkFlags, READDIR_BATCH_MAX,
};
use tairix_fscopy::tree::{self, entry_name, Entry, Kind, Roots};
use tairix_fscopy::{Source, Target};

use crate::{Dir, File};

impl tree::Node for File {
    fn stat(&self) -> Result<FileStat, Errno> {
        File::stat(self).map_err(Errno::from_syscall)
    }

    fn set_mode(&self, mode: u32) -> Result<(), Errno> {
        File::set_mode(self, mode).map_err(Errno::from_syscall)
    }

    fn set_owner(&self, uid: u32, gid: u32) -> Result<(), Errno> {
        File::set_owner(self, uid, gid).map_err(Errno::from_syscall)
    }

    fn attr_keys(&self) -> Result<Vec<String>, Errno> {
        File::attr_keys(self).map_err(Errno::from_syscall)
    }

    fn attr_value(&self, key: &str) -> Result<Vec<u8>, Errno> {
        File::attr_value(self, key.as_bytes()).map_err(Errno::from_syscall)
    }

    fn attr_set(&self, key: &str, value: &[u8]) -> Result<(), Errno> {
        File::attr_set(self, key.as_bytes(), value).map_err(Errno::from_syscall)
    }

    fn attr_remove(&self, key: &str) -> Result<(), Errno> {
        File::attr_remove(self, key.as_bytes()).map_err(Errno::from_syscall)
    }

    fn sync(&self) -> Result<(), Errno> {
        File::sync(self).map_err(Errno::from_syscall)
    }
}

impl tree::Node for Dir {
    fn stat(&self) -> Result<FileStat, Errno> {
        tree::Node::stat(self.as_file())
    }

    fn set_mode(&self, mode: u32) -> Result<(), Errno> {
        tree::Node::set_mode(self.as_file(), mode)
    }

    fn set_owner(&self, uid: u32, gid: u32) -> Result<(), Errno> {
        tree::Node::set_owner(self.as_file(), uid, gid)
    }

    fn attr_keys(&self) -> Result<Vec<String>, Errno> {
        tree::Node::attr_keys(self.as_file())
    }

    fn attr_value(&self, key: &str) -> Result<Vec<u8>, Errno> {
        tree::Node::attr_value(self.as_file(), key)
    }

    fn attr_set(&self, key: &str, value: &[u8]) -> Result<(), Errno> {
        tree::Node::attr_set(self.as_file(), key, value)
    }

    fn attr_remove(&self, key: &str) -> Result<(), Errno> {
        tree::Node::attr_remove(self.as_file(), key)
    }

    fn sync(&self) -> Result<(), Errno> {
        tree::Node::sync(self.as_file())
    }
}

impl Source for File {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        File::read_at(self, offset, buf).map_err(Errno::from_syscall)
    }

    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
        self.seek_region(offset, region)
            .map_err(Errno::from_syscall)
    }

    fn size(&mut self) -> Result<u64, Errno> {
        File::stat(self)
            .map(|stat| stat.size)
            .map_err(Errno::from_syscall)
    }
}

impl Target for File {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let written = File::write_at(self, offset, bytes).map_err(Errno::from_syscall)?;
        // A write that stopped short made no progress, and would never finish.
        if written == bytes.len() {
            Ok(())
        } else {
            Err(Errno::LengthOutOfRange)
        }
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        self.truncate(len).map_err(Errno::from_syscall)
    }
}

impl tree::Dir for Dir {
    type File = File;

    fn entries(&self) -> Result<Vec<Entry>, Errno> {
        let mut batch = Vec::new();
        batch
            .try_reserve_exact(READDIR_BATCH_MAX)
            .map_err(|_| Errno::OutOfMemory)?;
        batch.resize(READDIR_BATCH_MAX, 0);
        let mut entries = Vec::new();
        let mut refused = None;
        self.for_each_batch(&mut batch, |records| {
            for record in DirEntries::new(records) {
                if let Err(errno) = record.and_then(|record| decoded(&record, &mut entries)) {
                    refused = Some(errno);
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        })
        .map_err(Errno::from_syscall)?;
        refused.map_or(Ok(entries), Err)
    }

    fn open_dir(&self, name: &str) -> Result<Self, Errno> {
        Dir::open_dir(self, name.as_bytes()).map_err(Errno::from_syscall)
    }

    fn enter(&self, name: &str) -> Result<Self, Errno> {
        Dir::enter(self, name.as_bytes()).map_err(Errno::from_syscall)
    }

    fn open_file(&self, name: &str, write: bool) -> Result<File, Errno> {
        let access = if write {
            OpenFlags::WRITE
        } else {
            OpenFlags::READ
        };
        Dir::open_file(self, name.as_bytes(), access.union(OpenFlags::NO_FOLLOW))
            .map_err(Errno::from_syscall)
    }

    fn follow(&self, name: &str, read: bool) -> Result<File, Errno> {
        let access = if read {
            OpenFlags::READ
        } else {
            OpenFlags::empty()
        };
        Dir::open_file(self, name.as_bytes(), access).map_err(Errno::from_syscall)
    }

    fn open_node(&self, name: &str) -> Result<File, Errno> {
        Dir::open_file(self, name.as_bytes(), OpenFlags::NO_FOLLOW).map_err(Errno::from_syscall)
    }

    fn create_file(&self, name: &str, mode: u32) -> Result<File, Errno> {
        let flags = OpenFlags::WRITE
            .union(OpenFlags::EXCLUSIVE)
            .union(OpenFlags::NO_FOLLOW);
        Dir::create_file(self, name.as_bytes(), flags, mode).map_err(Errno::from_syscall)
    }

    fn make_dir(&self, name: &str, mode: u32) -> Result<Self, Errno> {
        Dir::make_dir(self, name.as_bytes(), mode).map_err(Errno::from_syscall)
    }

    fn symlink(&self, target: &[u8], name: &str) -> Result<(), Errno> {
        Dir::symlink(self, target, name.as_bytes()).map_err(Errno::from_syscall)
    }

    fn readlink(&self, name: &str) -> Result<String, Errno> {
        Dir::readlink(self, name.as_bytes())
    }

    fn unlink(
        &self,
        name: &str,
        flags: UnlinkFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        Dir::unlink(self, name.as_bytes(), flags, expect).map_err(Errno::from_syscall)
    }

    fn rename(
        &self,
        from: &str,
        to_dir: &Self,
        to: &str,
        flags: RenameFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        Dir::rename(self, from.as_bytes(), to_dir, to.as_bytes(), flags, expect)
            .map_err(Errno::from_syscall)
    }

    fn link(&self, existing: &str, to_dir: &Self, new: &str, follow: bool) -> Result<(), Errno> {
        let flags = if follow {
            LinkFlags::FOLLOW
        } else {
            LinkFlags::empty()
        };
        Dir::link(self, existing.as_bytes(), to_dir, new.as_bytes(), flags)
            .map_err(Errno::from_syscall)
    }

    fn access(&self, name: &str, want: Access) -> Result<(), Errno> {
        Dir::access(self, name.as_bytes(), want, AtFlags::NO_FOLLOW).map_err(Errno::from_syscall)
    }
}

/// The working directory, which a tool's operands are opened from: a
/// relative path resolves from it, an absolute one as written.
#[derive(Copy, Clone, Debug, Default)]
pub struct WorkingDirectory;

impl Roots for WorkingDirectory {
    type Dir = Dir;

    fn open_dir(&self, path: &str, follow: bool) -> Result<Dir, Errno> {
        let flags = if follow {
            OpenFlags::DIRECTORY
        } else {
            OpenFlags::DIRECTORY.union(OpenFlags::NO_FOLLOW)
        };
        Dir::from_open_result(crate::fs_open_at(FD_CWD, path.as_bytes(), flags, 0))
            .map_err(Errno::from_syscall)
    }

    fn parent(&self, path: &str) -> Result<(Dir, String), Errno> {
        let name = entry_name(path).ok_or(Errno::OutOfRange)?;
        let dir = Dir::open(tairix_path::dir_name(path).as_bytes()).map_err(Errno::from_syscall)?;
        Ok((dir, owned(name)?))
    }

    fn open_node(&self, path: &str, follow: bool) -> Result<File, Errno> {
        let flags = if follow {
            OpenFlags::empty()
        } else {
            OpenFlags::NO_FOLLOW
        };
        File::open(path.as_bytes(), flags).map_err(Errno::from_syscall)
    }
}

/// `name` as an owned string the heap may refuse.
/// `record` added to `entries`.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for a name that is not UTF-8 — the ABI's contract,
/// so a listing breaking it is corrupt or hostile and refused whole — or
/// [`Errno::OutOfMemory`].
fn decoded(record: &DirEntry<'_>, entries: &mut Vec<Entry>) -> Result<(), Errno> {
    let name = core::str::from_utf8(record.name).map_err(|_| Errno::OutOfRange)?;
    entries.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
    entries.push(Entry {
        name: owned(name)?,
        kind: Kind::of(record.kind),
        id: record.id,
        nlink: record.nlink,
    });
    Ok(())
}

fn owned(name: &str) -> Result<String, Errno> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(name.len())
        .map_err(|_| Errno::OutOfMemory)?;
    owned.push_str(name);
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use tairix_abi::fs::FD_CWD;
    use tairix_abi::{Access, AtFlags, Errno, OpenFlags};
    use tairix_abi_trap::seam::{capture, no_trap};
    use tairix_fscopy::tree::{self, Roots};

    use super::WorkingDirectory;
    use crate::{Dir, File, NUM_FS_ACCESS, NUM_FS_OPEN};

    fn flags(args: &[u64; 6]) -> OpenFlags {
        OpenFlags::from_bits(u32::try_from(args[3]).expect("flag bits fit u32"))
            .expect("a legal flag combination")
    }

    #[test]
    fn a_held_directorys_children_are_opened_by_name_never_through_a_link() {
        let dir = Dir {
            file: File { fd: 7 },
        };
        let opened = |write: bool| {
            capture(8, || {
                core::mem::forget(tree::Dir::open_file(&dir, "f", write).expect("opens"));
            })
        };
        let (number, args) = opened(false);
        assert_eq!((number, args[0]), (NUM_FS_OPEN, 7));
        assert_eq!(flags(&args), OpenFlags::READ.union(OpenFlags::NO_FOLLOW));
        let (_, args) = opened(true);
        assert_eq!(
            flags(&args),
            OpenFlags::WRITE.union(OpenFlags::NO_FOLLOW),
            "for writing alone"
        );
        let followed = |read: bool| {
            capture(8, || {
                core::mem::forget(tree::Dir::follow(&dir, "l", read).expect("opens"));
            })
        };
        assert_eq!(
            flags(&followed(true).1),
            OpenFlags::READ,
            "through a final link"
        );
        assert_eq!(flags(&followed(false).1), OpenFlags::empty());
        let (_, args) = capture(8, || {
            core::mem::forget(tree::Dir::open_node(&dir, "f").expect("opens"));
        });
        assert_eq!(
            flags(&args),
            OpenFlags::NO_FOLLOW,
            "no read or write access"
        );
        let (_, args) = capture(8, || {
            core::mem::forget(tree::Dir::open_dir(&dir, "d").expect("opens"));
        });
        assert_eq!(
            flags(&args),
            OpenFlags::DIRECTORY.union(OpenFlags::NO_FOLLOW)
        );
        let (_, args) = capture(8, || {
            core::mem::forget(tree::Dir::create_file(&dir, "new", 0o600).expect("made"));
        });
        assert_eq!(
            flags(&args),
            OpenFlags::WRITE
                .union(OpenFlags::CREATE)
                .union(OpenFlags::EXCLUSIVE)
                .union(OpenFlags::NO_FOLLOW)
        );
        assert_eq!(args[4], 0o600);
        let (number, args) = capture(0, || {
            assert_eq!(tree::Dir::access(&dir, "f", Access::WRITE), Ok(()));
        });
        assert_eq!(number, NUM_FS_ACCESS);
        assert_eq!(
            args[4],
            u64::from(AtFlags::NO_FOLLOW.bits()),
            "a link itself"
        );
        core::mem::forget(dir);
    }

    #[test]
    fn an_operand_is_opened_through_its_final_link_only_when_asked() {
        let roots = WorkingDirectory;
        for (follow, wanted) in [
            (true, OpenFlags::DIRECTORY),
            (false, OpenFlags::DIRECTORY.union(OpenFlags::NO_FOLLOW)),
        ] {
            let (_, args) = capture(4, || {
                core::mem::forget(roots.open_dir("a/b", follow).expect("opens"));
            });
            assert_eq!(args[0], u64::from(FD_CWD));
            assert_eq!(flags(&args), wanted);
        }
        let (_, args) = capture(4, || {
            core::mem::forget(roots.open_node("a/b", false).expect("opens"));
        });
        assert_eq!(flags(&args), OpenFlags::NO_FOLLOW);
        let (_, args) = capture(4, || {
            let (dir, name) = roots.parent("/a/b").expect("opens");
            assert_eq!(name, "b");
            core::mem::forget(dir);
        });
        assert_eq!(args[2], 2, "the directory holding the name, as written");
        no_trap(|| {
            for nameless in ["/", "Home:/", ".", "a/..", ""] {
                assert_eq!(
                    roots.parent(nameless).map(drop).err(),
                    Some(Errno::OutOfRange),
                    "{nameless}"
                );
            }
        });
    }
}
