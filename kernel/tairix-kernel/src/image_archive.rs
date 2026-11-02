//! The archive attachment (`plans/ARCHIVES.md` §2): an archive file read as
//! its exact bytes, never written in place, and for a writable one its two
//! work files and the commit that makes the next archive the archive.
//!
//! The work files live beside the archive, as the attacher's identity frozen
//! at the attach, so they are charged to the attacher and every check the
//! filesystem makes holds for them as for the attacher's own writes. The
//! staging file loses its name the moment it is made; the next archive keeps
//! its name until the commit renames it over the archive's, decided under the
//! volume's lock only while that name still holds the archive attached.

use alloc::string::String;
use alloc::sync::Arc;

use tairix_abi::fs::{FileStat, FinalLink, NodeExpect, OpenFlags, RenameFlags, UnlinkFlags};
use tairix_abi::volume::{
    ArchiveName, ARCHIVE_NEXT, ARCHIVE_REGION, ARCHIVE_STAGING, ARCHIVE_WORK_FILES,
};
use tairix_abi::{Errno, FileId};
use tairix_caps::CapabilitySet;
use tairix_kernel_core::fs::{
    Acting, At, Authority, CreateMode, FrozenAuthority, ImageDirectory, NodeHandle, Opened, Start,
};
use tairix_kernel_core::{FilesystemService, SleepLock};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};

use super::{FileHold, ImageBlock, Shape, IMAGE_BLOCK};
use crate::served_volume::Window;
use crate::system_mount::FS_SERVICE;

/// The bits a work file is born with: its attacher's alone.
const WORK_FILE_MODE: u32 = 0o600;

/// A commit left work undone: the next archive kept under its own name, the
/// archive's record not carried to it, or its directory not yet durable.
const IMAGE_ARCHIVE_COMMIT: EventId = EventId(4326);

/// Whom an attachment acts as: its attacher, frozen at the attach.
#[derive(Copy, Clone)]
struct Attacher {
    uid: u32,
    caps: CapabilitySet,
}

impl Attacher {
    fn acting(&self) -> Acting<'_> {
        Acting {
            uid: self.uid,
            caps: &self.caps,
            granted: false,
        }
    }
}

/// One work file: open as the attacher, and its length, which the kernel
/// alone sets.
struct WorkFile {
    file: NodeHandle,
    id: FileId,
    len: u64,
}

/// The next archive while it is being written: a work file still named in
/// the archive's directory, for the commit to rename.
struct NextArchive {
    work: WorkFile,
    name: String,
}

/// A writable archive attachment's directory, its work files, and the lock
/// that follows the archive from file to file.
pub(super) struct WorkFiles {
    dir: NodeHandle,
    name: String,
    name_key: [u8; 32],
    /// How many work-file names have been drawn.
    drawn: u64,
    /// The attachment's lock on its archive, which a commit moves to the
    /// next archive.
    hold: Arc<SleepLock<FileHold>>,
    task: u64,
    staging: Option<WorkFile>,
    next: Option<NextArchive>,
    /// Whether the directory holds a commit's rename not yet durable, which
    /// the next flush makes so.
    unsynced: bool,
    audit: &'static (dyn Sink + Sync),
}

/// What a replace that answered an error left, by what the archive's name
/// names now.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Settled {
    /// The next archive is the archive after all.
    Committed,
    /// The archive is as it was.
    Unchanged,
    /// The archive's name names neither, so the next archive is kept beside
    /// it under its own name rather than lost with it.
    Kept,
}

/// What a replace left, given the node the archive's name now names: the
/// next archive `next`, the archive `was`, nothing — a format replacing by
/// taking the old name away before writing the new may stop between — in
/// which case `finish` moves the next archive into the name, or another.
fn settled(
    named: Result<FileId, Errno>,
    next: FileId,
    was: FileId,
    finish: impl FnOnce() -> Result<(), Errno>,
) -> Settled {
    match named {
        Ok(id) if id == next => Settled::Committed,
        Ok(id) if id == was => Settled::Unchanged,
        Err(Errno::NotFound) if finish().is_ok() => Settled::Committed,
        _ => Settled::Kept,
    }
}

/// Whether the archive's record — its security and its attributes, which a
/// change stamps — differs between `before` and `after`.
fn record_changed(before: &FileStat, after: &FileStat) -> bool {
    (before.mode, before.uid, before.gid, before.times.changed)
        != (after.mode, after.uid, after.gid, after.times.changed)
}

impl WorkFiles {
    /// The work files of an archive held under `name` in `dir`, both as the
    /// caller handed them over, whose attachment locks it as `hold` for
    /// `task`.
    pub(super) fn new(
        dir: &ImageDirectory,
        hold: Arc<SleepLock<FileHold>>,
        task: u64,
        audit: &'static (dyn Sink + Sync),
    ) -> Result<Self, Errno> {
        let mut name = String::new();
        name.try_reserve_exact(dir.name.len())
            .map_err(|_| Errno::OutOfMemory)?;
        name.push_str(&dir.name);
        Ok(Self {
            dir: dir.dir.clone(),
            name,
            name_key: dir.name_key,
            drawn: 0,
            hold,
            task,
            staging: None,
            next: None,
            unsynced: false,
            audit,
        })
    }

    /// Record that the commit left `what` undone.
    fn undone(&self, what: &str) {
        log(
            self.audit,
            &Event {
                level: Level::Warn,
                id: IMAGE_ARCHIVE_COMMIT,
                message: "image-volume: an archive commit left work undone",
                fields: &[
                    Field {
                        key: "archive",
                        value: FieldValue::Str(&self.name),
                    },
                    Field {
                        key: "cause",
                        value: FieldValue::Str(what),
                    },
                ],
            },
        );
    }

    /// What a replace that answered an error left of the archive `was` and
    /// the next archive, as `who`: what its name names looked up again, and a
    /// replace stopped between taking the name away and putting the next
    /// archive in it finished.
    fn settle(&self, next: &NextArchive, was: FileId, who: &Attacher) -> Settled {
        let named = FS_SERVICE
            .stat(
                who.uid,
                &who.caps,
                At {
                    start: Start::Dir(&self.dir),
                    path: &self.name,
                },
                FinalLink::Keep,
            )
            .map(|stat| stat.id);
        settled(named, next.work.id, was, || {
            FS_SERVICE.rename(
                who.uid,
                &who.caps,
                At {
                    start: Start::Dir(&self.dir),
                    path: &next.name,
                },
                At {
                    start: Start::Dir(&self.dir),
                    path: &self.name,
                },
                RenameFlags::NO_REPLACE.union(RenameFlags::EXPECT_NODE),
                Some(&NodeExpect::of_node(next.work.id)),
            )
        })
    }

    /// Let the work files go uncommitted: the staging file, and the next
    /// archive taken back out of the directory while its name still holds it.
    /// Parks.
    pub(super) fn discard(&mut self) {
        self.staging = None;
        if let Some(next) = self.next.take() {
            if let Authority::Frozen(frozen) = &next.work.file.authority {
                let who = Attacher {
                    uid: frozen.uid,
                    caps: frozen.caps,
                };
                if self.unlink(&next.name, next.work.id, &who).is_err() {
                    self.undone("next_left");
                }
            }
        }
    }

    /// A fresh name for a work file beside the archive, by the rule every
    /// staged replacement on the system is spelled by, its tag a keyed
    /// digest of how many names came before it.
    fn draw_name(&mut self) -> Result<String, Errno> {
        let tag = tairix_crypto::mac::hmac_sha256_parts(
            &self.name_key,
            &[b"tairix/archive-work", &self.drawn.to_le_bytes()],
        );
        self.drawn += 1;
        let mut short = [0u8; tairix_path::STAGING_TAG_LEN];
        short.copy_from_slice(&tag[..tairix_path::STAGING_TAG_LEN]);
        let staged = tairix_path::staging_name(self.name.as_bytes(), short)
            .map_err(|_| Errno::OutOfMemory)?;
        String::from_utf8(staged).map_err(|_| Errno::OutOfRange)
    }

    /// Make a work file under a fresh name beside the archive, as `who`.
    fn create(&mut self, who: &Attacher) -> Result<(WorkFile, String), Errno> {
        let name = self.draw_name()?;
        let opened = FS_SERVICE.open(
            who.uid,
            &who.caps,
            At {
                start: Start::Dir(&self.dir),
                path: &name,
            },
            OpenFlags::READ
                .union(OpenFlags::WRITE)
                .union(OpenFlags::CREATE)
                .union(OpenFlags::EXCLUSIVE)
                .union(OpenFlags::NO_FOLLOW),
            CreateMode::new(WORK_FILE_MODE, 0)?,
        )?;
        let Opened::Node {
            node,
            projection,
            opened_as,
            at_floor,
            ..
        } = opened
        else {
            return Err(Errno::IsADirectory);
        };
        let file = NodeHandle {
            node,
            projection,
            authority: Authority::Frozen(FrozenAuthority {
                uid: who.uid,
                caps: who.caps,
                grant: None,
            }),
            opened_as,
            at_floor,
        };
        let id = match FS_SERVICE.stat_node(&file, &who.acting()) {
            Ok(stat) => stat.id,
            Err(errno) => {
                self.forsake(&name, who);
                return Err(errno);
            }
        };
        Ok((WorkFile { file, id, len: 0 }, name))
    }

    /// Remove `name`, a work file this attachment just made and has not yet
    /// identified, as far as that will go.
    fn forsake(&self, name: &str, who: &Attacher) {
        let _ = FS_SERVICE.unlink(
            who.uid,
            &who.caps,
            At {
                start: Start::Dir(&self.dir),
                path: name,
            },
            UnlinkFlags::empty(),
            None,
        );
    }

    /// Remove `name` from the archive's directory while it names `id`.
    fn unlink(&self, name: &str, id: FileId, who: &Attacher) -> Result<(), Errno> {
        FS_SERVICE.unlink(
            who.uid,
            &who.caps,
            At {
                start: Start::Dir(&self.dir),
                path: name,
            },
            UnlinkFlags::EXPECT_NODE,
            Some(&NodeExpect::of_node(id)),
        )
    }

    /// The work file `region`, made first where `make` asks and it has none
    /// yet; `None` for one not made.
    fn work(
        &mut self,
        region: u8,
        make: bool,
        who: &Attacher,
    ) -> Result<Option<&mut WorkFile>, Errno> {
        match region {
            ARCHIVE_STAGING => {
                if self.staging.is_none() && make {
                    let (work, name) = self.create(who)?;
                    // It needs no name: what it holds goes with the
                    // attachment.
                    self.unlink(&name, work.id, who)?;
                    self.staging = Some(work);
                }
                Ok(self.staging.as_mut())
            }
            ARCHIVE_NEXT => {
                if self.next.is_none() && make {
                    let (work, name) = self.create(who)?;
                    self.next = Some(NextArchive { work, name });
                }
                Ok(self.next.as_mut().map(|next| &mut next.work))
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

impl Drop for WorkFiles {
    /// Does no filesystem work, as the attachment may end where nothing may
    /// park: a next archive is taken back out by [`WorkFiles::discard`].
    fn drop(&mut self) {
        tairix_util::secret::wipe(&mut self.name_key);
    }
}

impl ImageBlock {
    /// The work files of a writable archive attachment.
    fn work_files_mut(&mut self) -> Result<&mut WorkFiles, Errno> {
        match &mut self.shape {
            Shape::Archive(Some(files)) => Ok(files),
            Shape::Archive(None) => Err(Errno::OutOfRange),
            Shape::Disk => Err(Errno::NotSupported),
        }
    }

    /// Read from the archive itself: its bytes up to its length, a short read
    /// before that being a fault, never zeros.
    fn read_archive(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        read_file(&self.file, &self.acting(), self.len, offset, buf)
    }

    /// Make the next archive, cut to `len` bytes, the archive: put on the
    /// medium, given the archive's attributes and security record, locked as
    /// the archive is, and renamed over the archive's name while that name
    /// still holds the archive attached. The attachment then reads it as the
    /// archive. Anything refused before the rename leaves the archive as it
    /// was; a replace that stops partway is settled, the next archive never
    /// lost with the archive it was to replace.
    fn commit_next(&mut self, len: u64) -> Result<(), Errno> {
        let attacher = Attacher {
            uid: self.uid,
            caps: self.caps,
        };
        let who = attacher.acting();
        let archive = self.file.clone();
        let files = self.work_files_mut()?;
        let next = files.next.as_mut().ok_or(Errno::OutOfRange)?;
        if len > next.work.len {
            return Err(Errno::LengthOutOfRange);
        }
        FS_SERVICE.truncate_node(&next.work.file, &who, len)?;
        next.work.len = len;
        FS_SERVICE.sync_node(&next.work.file, &who)?;
        // The record is read and given last, so a change made to the
        // archive while the next one was put on the medium is kept.
        let was = FS_SERVICE.stat_node(&archive, &who)?;
        give_record(&archive, &next.work.file, &who)?;
        // Taken before the rename, so no writer comes between.
        let hold = FileHold::take(&next.work.file, true, files.task)?;
        let replaced = FS_SERVICE.rename_replacing(
            who.uid,
            who.caps,
            At {
                start: Start::Dir(&files.dir),
                path: &next.name,
            },
            At {
                start: Start::Dir(&files.dir),
                path: &files.name,
            },
            &NodeExpect::of_node(next.work.id),
            &NodeExpect::of_node(was.id),
        );
        if let Err(errno) = replaced {
            let next = files.next.as_ref().ok_or(Errno::OutOfRange)?;
            match files.settle(next, was.id, &attacher) {
                Settled::Committed => {}
                Settled::Unchanged => return Err(errno),
                Settled::Kept => {
                    files.next = None;
                    files.undone("next_kept");
                    return Err(errno);
                }
            }
        }
        // From here the archive is the new file, whatever else answers.
        let next = files.next.take().ok_or(Errno::OutOfRange)?;
        let changed = FS_SERVICE
            .stat_node(&archive, &who)
            .is_ok_and(|now| record_changed(&was, &now));
        if changed && give_record(&archive, &next.work.file, &who).is_err() {
            files.undone("record_stale");
        }
        // The old archive is let go once the new one's name is durable; a
        // directory that cannot be made so now is the next flush's answer.
        files.unsynced = FS_SERVICE.sync_node(&files.dir, &who).is_err();
        if files.unsynced {
            files.undone("not_durable");
        }
        *files.hold.lock() = hold;
        self.file = next.work.file;
        self.len = len;
        self.blocks = len.div_ceil(u64::from(IMAGE_BLOCK));
        Ok(())
    }

    /// Make the directory durable where a commit's rename is not yet.
    pub(super) fn sync_directory(&mut self) -> Result<(), Errno> {
        let attacher = Attacher {
            uid: self.uid,
            caps: self.caps,
        };
        let who = attacher.acting();
        let Shape::Archive(Some(files)) = &mut self.shape else {
            return Ok(());
        };
        if files.unsynced {
            FS_SERVICE.sync_node(&files.dir, &who)?;
            files.unsynced = false;
        }
        Ok(())
    }
}

/// Give `to` the record `from` has: every extended attribute it carries,
/// then its security.
fn give_record(from: &NodeHandle, to: &NodeHandle, who: &Acting<'_>) -> Result<(), Errno> {
    carry_attributes(from, to, who)?;
    FS_SERVICE.adopt_security_node(to, from, who)
}

/// Read up to `buf.len()` bytes at `offset` of `file`, which holds `len`:
/// fewer only where it ends, and a short read before then a fault.
fn read_file(
    file: &NodeHandle,
    who: &Acting<'_>,
    len: u64,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, Errno> {
    let want =
        usize::try_from(len.saturating_sub(offset)).map_or(buf.len(), |left| left.min(buf.len()));
    let mut filled = 0;
    while filled < want {
        match FS_SERVICE.read_node(file, who, offset + filled as u64, &mut buf[filled..want])? {
            // The file shrank under the attachment.
            0 => return Err(Errno::DeviceFault),
            read => filled += read,
        }
    }
    Ok(filled)
}

/// Give `to` every extended attribute `from` carries.
fn carry_attributes(from: &NodeHandle, to: &NodeHandle, who: &Acting<'_>) -> Result<(), Errno> {
    let (Some(mut key), Some(mut value)) = (
        tairix_util::fallible::filled(tairix_abi::fs::FS_ATTR_KEY_MAX, 0u8),
        tairix_util::fallible::filled(tairix_abi::fs::FS_ATTR_VALUE_MAX, 0u8),
    ) else {
        return Err(Errno::OutOfMemory);
    };
    let mut index = 0;
    loop {
        let key_len = match FS_SERVICE.attr_list_node(from, who, index, &mut key) {
            Ok(Some(len)) => len,
            Ok(None) => return Ok(()),
            // A format that stores none has none to carry.
            Err(Errno::NotSupported) if index == 0 => return Ok(()),
            Err(errno) => return Err(errno),
        };
        let key = key.get(..key_len).ok_or(Errno::OutOfRange)?;
        let value_len = FS_SERVICE.attr_get_node(from, who, key, &mut value)?;
        let value = value.get(..value_len).ok_or(Errno::OutOfRange)?;
        FS_SERVICE.attr_set_node(to, who, key, value)?;
        index += 1;
    }
}

impl Window for ImageBlock {
    fn byte_len(&self) -> Option<u64> {
        match self.shape {
            Shape::Archive(_) => Some(self.len),
            Shape::Disk => None,
        }
    }

    fn work_files(&self) -> u8 {
        match self.shape {
            Shape::Archive(Some(_)) => ARCHIVE_WORK_FILES,
            Shape::Archive(None) | Shape::Disk => 0,
        }
    }

    fn archive_name(&self) -> Option<ArchiveName> {
        match self.shape {
            Shape::Archive(_) => self.archive_name,
            Shape::Disk => None,
        }
    }

    fn region_read(&mut self, region: u8, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if region == ARCHIVE_REGION {
            return match self.shape {
                Shape::Archive(_) => self.read_archive(offset, buf),
                Shape::Disk => Err(Errno::NotSupported),
            };
        }
        let attacher = Attacher {
            uid: self.uid,
            caps: self.caps,
        };
        let who = attacher.acting();
        let files = self.work_files_mut()?;
        match files.work(region, false, &attacher)? {
            Some(work) => read_file(&work.file, &who, work.len, offset, buf),
            None => Ok(0),
        }
    }

    fn region_write(&mut self, region: u8, offset: u64, data: &[u8]) -> Result<(), Errno> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(Errno::LengthOutOfRange)?;
        let attacher = Attacher {
            uid: self.uid,
            caps: self.caps,
        };
        let who = attacher.acting();
        let files = self.work_files_mut()?;
        let work = files
            .work(region, true, &attacher)?
            .ok_or(Errno::OutOfRange)?;
        // Written from no further than its end: a hole would have the volume
        // fill it with zeros first, as long as it asks.
        if offset > work.len {
            return Err(Errno::LengthOutOfRange);
        }
        let mut written = 0;
        while written < data.len() {
            match FS_SERVICE.write_node(
                &work.file,
                &who,
                offset + written as u64,
                false,
                &data[written..],
            )? {
                0 => return Err(Errno::DeviceFault),
                wrote => written += wrote,
            }
        }
        work.len = work.len.max(end);
        Ok(())
    }

    fn region_truncate(&mut self, region: u8, len: u64) -> Result<(), Errno> {
        let attacher = Attacher {
            uid: self.uid,
            caps: self.caps,
        };
        let who = attacher.acting();
        let files = self.work_files_mut()?;
        // Only ever cut: lengthening is writing, which fills no hole.
        let Some(work) = files.work(region, false, &attacher)? else {
            return if len == 0 {
                Ok(())
            } else {
                Err(Errno::LengthOutOfRange)
            };
        };
        if len > work.len {
            return Err(Errno::LengthOutOfRange);
        }
        FS_SERVICE.truncate_node(&work.file, &who, len)?;
        work.len = len;
        Ok(())
    }

    fn region_len(&self, region: u8) -> Result<u64, Errno> {
        match (&self.shape, region) {
            (Shape::Archive(_), ARCHIVE_REGION) => Ok(self.len),
            (Shape::Archive(Some(files)), ARCHIVE_STAGING) => {
                Ok(files.staging.as_ref().map_or(0, |work| work.len))
            }
            (Shape::Archive(Some(files)), ARCHIVE_NEXT) => {
                Ok(files.next.as_ref().map_or(0, |next| next.work.len))
            }
            (Shape::Disk, _) => Err(Errno::NotSupported),
            _ => Err(Errno::OutOfRange),
        }
    }

    fn commit(&mut self, len: u64) -> Result<(), Errno> {
        self.commit_next(len)
    }

    fn discard_work(&mut self) {
        if let Shape::Archive(Some(files)) = &mut self.shape {
            files.discard();
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use tairix_abi::driver::filesystem::NodeTimes;
    use tairix_abi::fs::FileKind;
    use tairix_abi::time::Time64;

    use super::*;

    fn file(node: u64) -> FileId {
        FileId {
            volume: [7; 16],
            node,
        }
    }

    #[test]
    fn a_replace_that_answered_an_error_is_settled_by_what_the_name_names() {
        let (next, was) = (file(2), file(1));
        let nothing_to_finish = || -> Result<(), Errno> { std::panic!("finished nothing") };
        assert_eq!(
            settled(Ok(next), next, was, nothing_to_finish),
            Settled::Committed
        );
        assert_eq!(
            settled(Ok(was), next, was, nothing_to_finish),
            Settled::Unchanged
        );
        assert_eq!(
            settled(Ok(file(3)), next, was, nothing_to_finish),
            Settled::Kept
        );
        assert_eq!(
            settled(Err(Errno::DeviceFault), next, was, nothing_to_finish),
            Settled::Kept
        );
        // The old name taken away and the new not put in it: finished now,
        // or the next archive kept rather than lost with the old.
        assert_eq!(
            settled(Err(Errno::NotFound), next, was, || Ok(())),
            Settled::Committed
        );
        assert_eq!(
            settled(Err(Errno::NotFound), next, was, || Err(Errno::NoSpace)),
            Settled::Kept
        );
    }

    fn stat(mode: u32, changed: i64) -> FileStat {
        FileStat {
            kind: FileKind::Regular,
            nlink: 1,
            size: 0,
            allocated: 0,
            mode,
            uid: 1000,
            gid: 100,
            id: file(1),
            times: NodeTimes {
                created: Time64::UNIX_EPOCH,
                modified: Time64::UNIX_EPOCH,
                accessed: Time64::UNIX_EPOCH,
                changed: Time64::from_secs(changed),
            },
            content_gen: 0,
        }
    }

    #[test]
    fn a_record_changes_with_its_security_or_its_change_stamp() {
        assert!(!record_changed(&stat(0o644, 5), &stat(0o644, 5)));
        assert!(record_changed(&stat(0o644, 5), &stat(0o600, 5)));
        // A format that keeps no change stamp still shows a new mode.
        assert!(record_changed(&stat(0o644, 0), &stat(0o640, 0)));
        assert!(record_changed(&stat(0o644, 5), &stat(0o644, 6)));
    }
}
