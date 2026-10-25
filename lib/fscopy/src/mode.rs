//! The modes a copy gives what it makes, as GNU's `cp` gives them where it
//! does not preserve modes: a file its source's permission bits, a directory
//! its source's permission and sticky bits, each less the umask at the make;
//! and the mode and owner a move carries onto what it made of its source.

use tairix_abi::{
    Errno, FileKind, FileStat, FS_MODE_MASK, FS_OWNER_UNCHANGED, FS_PERMISSION_BITS, FS_SETGID_BIT,
    FS_SETUID_BIT, FS_STICKY_BIT,
};

use crate::tree::{Dir, Node};

/// The owner's read, write and search bits: what a directory a copy makes
/// holds while its entries are made in it, and what one staged out of
/// anyone else's sight is born with.
pub const OWNER_RWX: u32 = 0o700;

/// The owner's read and write bits: what a file staged out of anyone else's
/// sight is born with, until it is whole.
pub const OWNER_RW: u32 = 0o600;

/// The mode a copy makes a file in for a source of mode `source`: its
/// permission bits, the set-ID and sticky bits dropped.
#[must_use]
pub const fn file_mode(source: u32) -> u32 {
    source & FS_PERMISSION_BITS
}

/// The modes a directory a copy makes passes through: made with its owner's
/// read, write and search so its entries can be made, given those outright
/// when the umask withheld any, and left once full with the bits it should
/// end with less the umask, and any setgid bit the directory it was made in
/// passed down.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirModes {
    bits: u32,
}

impl DirModes {
    /// For a directory that should end with `mode` less the umask: a source
    /// directory's mode, or the one a new directory takes when nothing names
    /// one. Only its permission and sticky bits count.
    #[must_use]
    pub const fn new(mode: u32) -> Self {
        Self {
            bits: mode & (FS_PERMISSION_BITS | FS_STICKY_BIT),
        }
    }

    /// The mode to make the directory in.
    #[must_use]
    pub const fn asked(self) -> u32 {
        self.bits | OWNER_RWX
    }

    /// Ready the directory `made`, just made in [`asked`](Self::asked), to be
    /// filled: given its owner's read, write and search outright where the
    /// umask withheld any. Answers what it was born as, and how it settles.
    ///
    /// # Errors
    ///
    /// The stat's or the mode change's refusal.
    pub fn ready<D: Dir>(self, made: &D) -> Result<(FileStat, Settle), Errno> {
        let born = made.stat()?;
        let settle = self.settle(born.mode);
        if let Some(fill) = settle.to_fill() {
            made.set_mode(fill)?;
        }
        Ok((born, settle))
    }

    /// The modes the directory made in [`asked`](Self::asked), and born
    /// with the mode `born` its stat reports, is given next.
    #[must_use]
    pub const fn settle(self, born: u32) -> Settle {
        let born = born & FS_MODE_MASK;
        let filling = born | OWNER_RWX;
        let full = (born & self.bits) | (born & FS_SETGID_BIT);
        Settle {
            fill: changed(born, filling),
            full: changed(filling, full),
        }
    }
}

/// What a move carries of a node, and what it reads of the node it carries
/// that onto.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ownership {
    /// The node's kind: a link keeps no bits of its own.
    pub kind: FileKind,
    /// Its mode.
    pub mode: u32,
    /// Its owner.
    pub uid: u32,
    /// Its group.
    pub gid: u32,
}

impl Ownership {
    /// What `stat` says of the node.
    #[must_use]
    pub const fn of(stat: &FileStat) -> Self {
        Self {
            kind: stat.kind,
            mode: stat.mode,
            uid: stat.uid,
            gid: stat.gid,
        }
    }
}

/// Give the held `at`, read as `current`, the owner and permission bits
/// `wanted` describes, as far as its volume keeps them and the mover may give
/// them. The bits go first, while the node is the mover's: only a node's owner
/// sets them. One the mover may not give away stays the mover's, in the
/// source's group where the mover may keep it, and without the set-ID bit of
/// an owner or a group it did not keep, so a move never grants the mover a
/// privilege the source held for someone else. A link keeps no bits of its
/// own.
///
/// # Errors
///
/// A refusal other than the volume's or the system's to keep a mode or an
/// owner.
pub fn carry_owner<T: Node + ?Sized>(
    wanted: Ownership,
    current: Ownership,
    at: &T,
) -> Result<(), Errno> {
    let bits = current.kind != FileKind::Symlink;
    let mode = wanted.mode & FS_MODE_MASK;
    let set_mode = |mode: u32| match at.set_mode(mode) {
        Ok(()) | Err(Errno::NotSupported) => Ok(()),
        Err(errno) => Err(errno),
    };
    if bits {
        set_mode(mode)?;
    }
    if (current.uid, current.gid) == (wanted.uid, wanted.gid) {
        return Ok(());
    }
    let refused = |errno: Errno| matches!(errno, Errno::PermissionDenied | Errno::NotSupported);
    match at.set_owner(wanted.uid, wanted.gid) {
        // The system clears set-ID bits as a group changes; a node still the
        // mover's takes them back.
        Ok(()) if bits && wanted.uid == current.uid => return set_mode(mode),
        Ok(()) => return Ok(()),
        Err(errno) if refused(errno) => {}
        Err(errno) => return Err(errno),
    }
    let grouped = current.gid == wanted.gid
        || match at.set_owner(FS_OWNER_UNCHANGED, wanted.gid) {
            Ok(()) => true,
            Err(errno) if refused(errno) => false,
            Err(errno) => return Err(errno),
        };
    if !bits {
        return Ok(());
    }
    let unkept = if grouped {
        FS_SETUID_BIT
    } else {
        FS_SETUID_BIT | FS_SETGID_BIT
    };
    set_mode(mode & !unkept)
}

/// `to`, when it is not `from`.
const fn changed(from: u32, to: u32) -> Option<u32> {
    if from == to {
        None
    } else {
        Some(to)
    }
}

/// The modes a directory a copy made is given, each only when it differs
/// from the one before.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settle {
    fill: Option<u32>,
    full: Option<u32>,
}

impl Settle {
    /// The mode to give it before its entries are made, when its birth left
    /// it without its owner's read, write or search.
    #[must_use]
    pub const fn to_fill(self) -> Option<u32> {
        self.fill
    }

    /// The mode to give it once it is full, when that is not the one it was
    /// filled under.
    #[must_use]
    pub const fn once_full(self) -> Option<u32> {
        self.full
    }

    /// Give the full directory `made` the mode it is left with.
    ///
    /// # Errors
    ///
    /// The mode change's refusal.
    pub fn finish<N: Node + ?Sized>(self, made: &N) -> Result<(), Errno> {
        self.once_full().map_or(Ok(()), |full| made.set_mode(full))
    }
}

#[cfg(test)]
mod tests {
    use super::{file_mode, DirModes};

    /// What a directory asked for in `mode` is given when the umask is
    /// `umask` and its parent passes `inherited` down: before it is filled,
    /// and once it is full.
    fn settled(mode: u32, umask: u32, inherited: u32) -> (Option<u32>, Option<u32>) {
        let modes = DirModes::new(mode);
        let settle = modes.settle((modes.asked() & !umask) | inherited);
        (settle.to_fill(), settle.once_full())
    }

    #[test]
    fn a_file_keeps_its_permission_bits_and_drops_the_rest() {
        assert_eq!(file_mode(0o100_644), 0o644);
        assert_eq!(file_mode(0o7755), 0o755);
    }

    #[test]
    fn a_directory_is_filled_by_its_owner_and_ends_as_its_source_less_the_umask() {
        assert_eq!(DirModes::new(0o500).asked(), 0o700);
        assert_eq!(settled(0o500, 0o022, 0), (None, Some(0o500)));
        assert_eq!(settled(0o755, 0o277, 0), (Some(0o700), Some(0o500)));
        assert_eq!(settled(0o750, 0o027, 0), (None, None));
    }

    #[test]
    fn a_directory_keeps_its_sources_sticky_bit_and_an_inherited_setgid_but_no_setuid() {
        assert_eq!(DirModes::new(0o5750).asked(), 0o1750);
        assert_eq!(settled(0o5750, 0o022, 0o2000), (None, None));
        assert_eq!(settled(0o5550, 0o022, 0o2000), (None, Some(0o3550)));
    }

    #[test]
    fn the_type_bits_a_stat_reports_beside_the_mode_are_not_a_change() {
        let modes = DirModes::new(0o755);
        assert_eq!(modes.settle(0o040_755).to_fill(), None);
    }
}
