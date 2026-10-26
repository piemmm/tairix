//! Following one file through its directory's change watch.
//!
//! The file's directory is watched once it exists, and its parent until then;
//! while neither can be reached — absent, or on a volume no driver serves yet —
//! the mount table's notice stands in, so a service started before the volume
//! holding the file is mounted follows it once it is. [`Followed::drain`]
//! answers what a wake asks for: nothing, a re-read of the file, or a re-follow
//! because the watched directory came or went.

use tairix_abi::fs::{DirChangeBatch, DirWatchStatus};
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
use tairix_abi::{Errno, NoticeTopic};

use crate::{Dir, WaitSet};

/// The shortest time between two reports of a storm of changes to a followed
/// file's directory; an isolated change reports at once.
const STORM_LATENCY_NS: u64 = 1_000_000_000;

/// Change batches one drain reads before the file is read anyway, so a
/// directory changing without pause cannot hold its follower.
const DRAIN_BATCHES: usize = 16;

/// What a drained batch from a followed file's watch asks for, least first.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Verdict {
    /// Nothing the file lives in changed.
    Unconcerned,
    /// The file may have changed: read it again.
    Reread,
    /// The watched directory went away, or the file's own directory came or
    /// went: follow again, then read.
    Rewatch,
}

/// The verdict on change batch `bytes` from a watch on a directory holding
/// the entry `name`, and whether more changes wait.
///
/// `exact` is a watch on the file's own directory, where `name` is the file;
/// otherwise it is on that directory's parent, waiting for the directory
/// `name` to appear. A batch that cannot be decoded is taken as the worst
/// case.
#[must_use]
pub fn verdict(bytes: &[u8], exact: bool, name: &[u8]) -> (Verdict, bool) {
    let Ok(batch) = DirChangeBatch::decode(bytes) else {
        return (Verdict::Rewatch, false);
    };
    if batch.status == DirWatchStatus::Gone {
        return (Verdict::Rewatch, false);
    }
    if !batch.concerns(name) {
        return (Verdict::Unconcerned, batch.more);
    }
    let verdict = if exact {
        Verdict::Reread
    } else {
        Verdict::Rewatch
    };
    (verdict, batch.more)
}

/// Where a followed file is watched from.
#[derive(Copy, Clone, Debug)]
struct Places {
    /// The file's directory.
    dir: &'static str,
    /// The file's name in it.
    file: &'static str,
    /// That directory's parent.
    parent: &'static str,
    /// That directory's name in its parent.
    leaf: &'static str,
}

impl Places {
    fn of(path: &'static str) -> Option<Self> {
        let (dir, file) = path.rsplit_once('/')?;
        let (parent, leaf) = dir.rsplit_once('/')?;
        Some(Self {
            dir,
            file,
            parent,
            leaf,
        })
    }
}

/// What looking for a followed file found.
pub enum Follow {
    /// Neither its directory nor that directory's parent can be reached yet;
    /// the mount table's notice wakes the caller, under the follow's token, to
    /// look again.
    NotYet,
    /// It is followed.
    Watching(Followed),
    /// Its directory, or the parent while the directory is absent, cannot be
    /// opened or watched, or nothing could wake the caller to look again, for
    /// this reason.
    Unwatched(Errno),
}

/// Where a look for a followed file's directory ended.
enum Look {
    /// It, or the parent while it is absent, opened; `true` for itself.
    Found(Dir, bool),
    /// Neither can be reached yet.
    Unreachable,
    /// One is there but refused.
    Refused(Errno),
}

/// A file followed through a watch joined to a wait set.
pub struct Followed {
    dir: Dir,
    places: Places,
    exact: bool,
    token: u64,
}

impl Followed {
    /// Follow the file at `path`, its watch joined to `set` under `token`.
    ///
    /// Only a directory that cannot be reached yet is waited for, through the
    /// mount table's notice joined under `token` — joined before the look, so a
    /// mount between the two still wakes the caller. One there but refused, or
    /// not a directory, is answered [`Follow::Unwatched`] with why.
    #[must_use]
    pub fn follow(path: &'static str, set: &WaitSet, token: u64) -> Follow {
        let Some(places) = Places::of(path) else {
            return Follow::Unwatched(Errno::OutOfRange);
        };
        let mounts = u64::from(NoticeTopic::Mounts.as_u32());
        let waiting = match set.add(WaitSourceKind::SystemNotice, mounts, token) {
            Ok(()) | Err(Errno::AlreadyExists) => Ok(()),
            Err(refused) => Err(refused),
        };
        let open = |dir: &str| Dir::open(dir.as_bytes()).map_err(Errno::from_syscall);
        let look = match open(places.dir) {
            Ok(opened) => Look::Found(opened, true),
            // The parent's listing would not change when the volume arrives:
            // only the mount table's notice says so.
            Err(Errno::NotImplemented) => Look::Unreachable,
            Err(Errno::NotFound) => match open(places.parent) {
                Ok(opened) => Look::Found(opened, false),
                Err(Errno::NotFound | Errno::NotImplemented) => Look::Unreachable,
                Err(refused) => Look::Refused(refused),
            },
            Err(refused) => Look::Refused(refused),
        };
        let (opened, exact) = match look {
            Look::Found(opened, exact) => (opened, exact),
            Look::Unreachable => {
                return waiting.map_or_else(Follow::Unwatched, |()| Follow::NotYet)
            }
            Look::Refused(refused) => {
                leave_notice(set, mounts, token);
                return Follow::Unwatched(refused);
            }
        };
        leave_notice(set, mounts, token);
        let armed = opened
            .watch(STORM_LATENCY_NS)
            .map_err(Errno::from_syscall)
            .and_then(|()| set.add(WaitSourceKind::DirWatch, u64::from(opened.fd()), token));
        match armed {
            Ok(()) => Follow::Watching(Self {
                dir: opened,
                places,
                exact,
                token,
            }),
            Err(err) => Follow::Unwatched(err),
        }
    }

    /// Drain the watch, a bounded number of batches, answering the most they
    /// ask for; a directory still changing after them is read again.
    #[must_use]
    pub fn drain(&self) -> Verdict {
        let name = if self.exact {
            self.places.file
        } else {
            self.places.leaf
        };
        let mut scratch = [0u8; 2 * DirChangeBatch::MIN_BUFFER];
        let mut most = Verdict::Unconcerned;
        for _ in 0..DRAIN_BATCHES {
            let Ok(len) = self.dir.read_changes(&mut scratch) else {
                return Verdict::Rewatch;
            };
            let (said, more) = verdict(
                scratch.get(..len).unwrap_or_default(),
                self.exact,
                name.as_bytes(),
            );
            most = most.max(said);
            if !more {
                return most;
            }
        }
        most.max(Verdict::Reread)
    }

    /// Leave `set` and close the watch.
    pub fn leave(self, set: &WaitSet) {
        // A member the set no longer holds has nothing left to remove.
        let _ = crate::waitset_ctl(
            set.handle(),
            WaitSetOp::Del,
            WaitSourceKind::DirWatch,
            u64::from(self.dir.fd()),
            self.token,
        );
    }
}

/// Leave the notice `topic` joined to `set` under `token`; one the set does not
/// hold has nothing to leave.
fn leave_notice(set: &WaitSet, topic: u64, token: u64) {
    let _ = crate::waitset_ctl(
        set.handle(),
        WaitSetOp::Del,
        WaitSourceKind::SystemNotice,
        topic,
        token,
    );
}

#[cfg(test)]
mod tests {
    use super::{verdict, Verdict};
    use alloc::vec;
    use alloc::vec::Vec;
    use tairix_abi::fs::{DirChange, DirChangeBatch, DirWatchStatus};
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};

    fn batch(status: DirWatchStatus, more: bool, names: &[&[u8]]) -> Vec<u8> {
        let mut out = vec![0u8; 4096];
        let count = u32::try_from(names.len()).expect("few");
        let mut at = DirChangeBatch::encode_header(&mut out, status, more, count).expect("fits");
        for name in names {
            at += DirChange::Absent(name)
                .encode_into(&mut out[at..])
                .expect("fits");
        }
        out.truncate(at);
        out
    }

    #[test]
    fn the_watch_rereads_on_its_file_and_rewatches_on_its_directory() {
        let file = b"system.conf".as_slice();
        let dir = b"Configuration".as_slice();
        assert_eq!(
            verdict(&batch(DirWatchStatus::Changes, true, &[file]), true, file),
            (Verdict::Reread, true)
        );
        assert_eq!(
            verdict(
                &batch(DirWatchStatus::Changes, false, &[b"network.conf"]),
                true,
                file
            ),
            (Verdict::Unconcerned, false)
        );
        assert_eq!(
            verdict(&batch(DirWatchStatus::Rescan, false, &[]), true, file),
            (Verdict::Reread, false)
        );
        assert_eq!(
            verdict(&batch(DirWatchStatus::Gone, false, &[]), true, file),
            (Verdict::Rewatch, false)
        );
        // Watching the parent: the directory's own arrival means follow it.
        assert_eq!(
            verdict(&batch(DirWatchStatus::Changes, false, &[dir]), false, dir),
            (Verdict::Rewatch, false)
        );
        assert_eq!(
            verdict(
                &batch(DirWatchStatus::Changes, false, &[b"Time"]),
                false,
                dir
            ),
            (Verdict::Unconcerned, false)
        );
        assert_eq!(verdict(&[0xFF; 8], true, file), (Verdict::Rewatch, false));
    }

    const CONF: &str = "/Example/Settings/example.conf";

    /// The `(op, kind)` of every wait-set edit the follow made, in order.
    fn set_edits() -> Vec<(u64, u64)> {
        tairix_abi_trap::seam::calls()
            .into_iter()
            .filter(|(number, _)| *number == crate::NUM_WAITSET_CTL)
            .map(|(_, args)| (args[1], args[2]))
            .collect()
    }

    fn edit(op: tairix_abi::waitset::WaitSetOp, kind: WaitSourceKind) -> (u64, u64) {
        (u64::from(op.as_u32()), u64::from(kind.as_u32()))
    }

    /// Only a directory that cannot be reached yet is waited for: one refused,
    /// or not a directory, is answered with why, never awaited in silence.
    #[test]
    fn only_an_unreachable_directory_is_awaited() {
        use super::{Follow, Followed};
        use tairix_abi::Errno;
        use tairix_abi_trap::seam;
        let set = crate::WaitSet(0);
        for refused in [Errno::PermissionDenied, Errno::NotADirectory] {
            seam::arm_script(&[0], seam::refusal(refused));
            let followed = Followed::follow(CONF, &set, 0);
            assert!(
                matches!(followed, Follow::Unwatched(errno) if errno == refused),
                "{refused:?}"
            );
        }
        // Absent, its volume not served yet, or its parent's not served: a
        // mount is what could bring each.
        let unreachable: [&[Errno]; 3] = [
            &[Errno::NotFound, Errno::NotFound],
            &[Errno::NotImplemented],
            &[Errno::NotFound, Errno::NotImplemented],
        ];
        for answers in unreachable {
            let mut script = vec![0];
            script.extend(answers.iter().map(|errno| seam::refusal(*errno)));
            seam::arm_script(&script, seam::refusal(Errno::DeviceFault));
            assert!(
                matches!(Followed::follow(CONF, &set, 0), Follow::NotYet),
                "{answers:?}"
            );
            assert_eq!(
                set_edits(),
                [edit(WaitSetOp::Add, WaitSourceKind::SystemNotice)]
            );
        }
        // A notice already joined by an earlier look still wakes the caller.
        seam::arm_script(
            &[seam::refusal(Errno::AlreadyExists)],
            seam::refusal(Errno::NotImplemented),
        );
        assert!(matches!(Followed::follow(CONF, &set, 0), Follow::NotYet));
        // With nothing to wake it, an unreachable directory is never waited
        // for in silence.
        seam::arm_script(
            &[seam::refusal(Errno::LimitExceeded)],
            seam::refusal(Errno::NotImplemented),
        );
        assert!(matches!(
            Followed::follow(CONF, &set, 0),
            Follow::Unwatched(Errno::LimitExceeded)
        ));
    }

    /// The notice is joined before the directory is looked for, so a mount
    /// between the two is not missed, and left once the directory is watched.
    #[test]
    fn the_mount_notice_stands_in_only_until_the_directory_is_watched() {
        use super::{Follow, Followed};
        use tairix_abi_trap::seam;
        let set = crate::WaitSet(0);
        // Joined, the directory opened as descriptor 5, the notice left, the
        // watch armed and joined.
        seam::arm_script(&[0, 5, 0, 0, 0], 0);
        let followed = Followed::follow(CONF, &set, 0);
        assert!(matches!(followed, Follow::Watching(_)));
        assert_eq!(
            set_edits(),
            [
                edit(WaitSetOp::Add, WaitSourceKind::SystemNotice),
                edit(WaitSetOp::Del, WaitSourceKind::SystemNotice),
                edit(WaitSetOp::Add, WaitSourceKind::DirWatch),
            ]
        );
        assert_eq!(
            seam::calls().first().map(|(number, _)| *number),
            Some(crate::NUM_WAITSET_CTL),
            "the notice is joined before anything is looked for"
        );
    }
}
