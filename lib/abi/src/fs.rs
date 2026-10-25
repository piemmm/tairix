//! Userland filesystem API wire types.
//!
//! These types describe the data a program exchanges with the kernel over
//! the `fs_*` syscalls. A path a call takes is resolved from the directory
//! descriptor beside it, or from the working directory under [`FD_CWD`]; the
//! syscalls resolve it, or act on an open-file handle, against the kernel's
//! secured VFS, which makes every
//! per-inode owner/mode/ACL/capability decision; this module only fixes the
//! *shapes* that cross the boundary, so the kernel and `lib/rt` cannot drift
//! (no duplication).
//!
//! Like the rest of the ABI surface the encodings are little-endian and
//! allocation-free, and every decoder treats its bytes as untrusted input:
//! it bounds-checks against the structure's `WIRE_LEN` and fails closed with
//! an [`Errno`] rather than indexing out of range.

use crate::driver::filesystem::NodeTimes;
use crate::le::{put_i64, put_u32, put_u64, read_i64, read_u32, read_u64};
use crate::time::Time64;
use crate::Errno;

/// The entries of the default root view `/`, and the machine aliases of the
/// same names (`System:`, …): every top-level directory the OS lays out.
pub const ROOT_VIEW: [&str; 3] = ["System", crate::home::USERS_DIR, STORAGE_CATALOG];

/// The root-view entry that is the catalog of attached volumes: it lists the
/// volumes attached beneath it and is stored on none of them.
pub const STORAGE_CATALOG: &str = "Storage";

/// The root-view entries a root volume stores: every one but the catalog.
pub fn stored_root_entries() -> impl Iterator<Item = &'static str> {
    ROOT_VIEW
        .into_iter()
        .filter(|&name| name != STORAGE_CATALOG)
}

/// Maximum length, in bytes, of a single path passed to a filesystem
/// syscall.
///
/// A fail-closed validation bound on untrusted input, not a capacity that
/// scales with hardware: it caps the kernel staging buffer a path is copied
/// into. It is far longer than any real path, and a path exceeding it is
/// refused before any resolution begins.
pub const FS_PATH_MAX: usize = 4096;

/// The `dirfd` naming the caller's working directory: a relative path given
/// with it resolves from there. No descriptor is ever numbered so.
pub const FD_CWD: u32 = u32::MAX;

/// Maximum length, in bytes, of a symbolic link's target.
///
/// A link's target *is* a path, so this is [`FS_PATH_MAX`] rather than an
/// independent number: a smaller bound would make legal paths unnameable as
/// a target, and a larger one could never resolve. Derived, never restated,
/// so the two cannot drift apart. A target exceeding it is refused before
/// any node is written.
pub const FS_SYMLINK_MAX: usize = FS_PATH_MAX;

/// Maximum number of bytes a single `fs_read` / `fs_write` transfers.
///
/// A fail-closed bound on the per-call kernel staging buffer (the same role
/// [`crate::RANDOM_REQUEST_MAX_BYTES`] plays for `random_get`): a larger
/// transfer is split by the `lib/rt` wrapper into successive calls, so this
/// never caps total file size, only one syscall's copy.
pub const FS_IO_MAX: usize = 1 << 20;

/// Most bytes of [`DirEntry`] records one `fs_readdir` call fills.
///
/// The batch is staged in kernel memory until the mount's lock is released,
/// so this bounds what one listing call holds; a longer listing is more
/// calls, never a larger one. It holds hundreds of records, so a batch still
/// amortises its call.
pub const READDIR_BATCH_MAX: usize = 1 << 16;

/// The permission bits a mode word may carry: the one
/// [`fs_set_mode`](crate::SyscallNumber::FS_SET_MODE) sets, and the one a
/// create asks for through [`fs_open`](crate::SyscallNumber::FS_OPEN) and
/// [`fs_mkdir`](crate::SyscallNumber::FS_MKDIR). The owner/group/other `rwx`
/// triads plus the setuid/setgid/sticky bits (`0o7777`).
///
/// A fixed validation bound on untrusted input: the dispatcher refuses a
/// mode word carrying any higher bit with [`Errno::OutOfRange`] rather than
/// masking it (never silently apply a different mode than the one asked
/// for). The file-type bits [`FileStat::mode`] reports above this mask are
/// the filesystem's own and are never settable through the call.
pub const FS_MODE_MASK: u32 = 0o7777;

/// The set-user-ID permission bit (`0o4000`), which an ownership change takes
/// from anything but a directory ([`mode_after_owner_change`]).
pub const FS_SETUID_BIT: u32 = 0o4000;

/// The set-group-ID permission bit (`0o2000`): on a directory, the group its
/// entries are born in; on anything else group-executable, a privilege an
/// ownership change takes away ([`mode_after_owner_change`]).
pub const FS_SETGID_BIT: u32 = 0o2000;

/// The group-execute permission bit (`0o0010`).
pub const FS_GROUP_EXEC_BIT: u32 = 0o0010;

/// The mode bits a node with `mode` keeps once its owner or group changes.
///
/// A directory keeps them all, so a shared set-group-ID directory goes on
/// giving its entries its group. Anything else loses [`FS_SETUID_BIT`], and
/// [`FS_SETGID_BIT`] where [`FS_GROUP_EXEC_BIT`] makes it a privilege, so a
/// reassigned file never carries one granted for its old owner.
#[must_use]
pub const fn mode_after_owner_change(mode: u32, directory: bool) -> u32 {
    if directory {
        return mode;
    }
    let mode = mode & !FS_SETUID_BIT;
    if mode & FS_GROUP_EXEC_BIT == 0 {
        mode
    } else {
        mode & !FS_SETGID_BIT
    }
}

/// The sticky bit (`0o1000`): in a directory carrying it, an entry is removed
/// or renamed only by its own owner or the directory's.
pub const FS_STICKY_BIT: u32 = 0o1000;

/// The owner/group/other `rwx` triads (`0o777`).
pub const FS_PERMISSION_BITS: u32 = 0o777;

/// The mode a file is created with when its creator asks for no narrower one
/// (`0o666`); the creator's umask narrows it.
pub const FS_NEW_FILE_MODE: u32 = 0o666;

/// The mode a directory is created with when its creator asks for no narrower
/// one (`0o777`); the creator's umask narrows it.
pub const FS_NEW_DIR_MODE: u32 = 0o777;

/// The mode every symbolic link is born with (`0o777`), which no umask
/// narrows: a link's own mode never gates access.
pub const FS_LINK_MODE: u32 = 0o777;

/// The file mode creation mask PID 1 starts with (`0o022`), and so every
/// process whose ancestors kept the mask they were given.
pub const FS_INITIAL_UMASK: u32 = 0o022;

/// The bits a file mode creation mask may carry: the permission triads. A mask
/// never withholds the setuid, setgid or sticky bit.
///
/// [`fs_umask`](crate::SyscallNumber::FS_UMASK) refuses a mask carrying any
/// other bit with [`Errno::OutOfRange`].
pub const FS_UMASK_BITS: u32 = FS_PERMISSION_BITS;

/// The [`fs_umask`](crate::SyscallNumber::FS_UMASK) word that reads the
/// caller's mask without changing it. It carries bits no mask may, so it is
/// never taken for one.
pub const FS_UMASK_QUERY: u32 = u32::MAX;

/// The [`fs_set_owner`](crate::SyscallNumber::FS_SET_OWNER) `uid`/`gid`
/// sentinel meaning "leave this field unchanged" (the `(uid_t)-1` /
/// `(gid_t)-1` convention of `chown(2)`).
///
/// A call may pass this for either field to change only the other; passing it
/// for both is a well-formed no-op. `0xFFFF_FFFF` is not an assignable id, so
/// the sentinel can never collide with a real owner id.
pub const FS_OWNER_UNCHANGED: u32 = u32::MAX;

/// Longest extended-attribute key, in bytes, a `fs_attr_*` syscall accepts.
///
/// A fixed *security* validation bound on untrusted input, never a growable
/// capacity: the dispatcher refuses a longer (or empty) key with
/// [`Errno::LengthOutOfRange`] before any user memory beyond it is read.
/// `lib/fsmeta`'s `KEY_MAX` aliases this value so the wire bound and the
/// stored-set bound can never diverge; the key *grammar*
/// (`namespace.rest`, closed namespace set) is validated by the secured
/// VFS through the one `lib/fsmeta` definition.
pub const FS_ATTR_KEY_MAX: usize = 255;

/// Largest extended-attribute value, in bytes, a `fs_attr_*` syscall
/// accepts or returns.
///
/// A fixed *security* validation bound (the `lib/fsmeta` `VALUE_MAX`
/// aliases it): it caps the kernel staging buffer for `fs_attr_set` and
/// `fs_attr_get`, and is chosen so a full attribute set encodes into one
/// copy-on-write metadata block on a 4 KiB-block volume. A payload larger
/// than this is a named stream, not an attribute.
pub const FS_ATTR_VALUE_MAX: usize = 3072;

/// What an inode is, as reported by [`FileStat`] and each [`DirEntry`].
///
/// Deliberately closed, and an unknown discriminant on decode fails closed
/// rather than being guessed. A [`Symlink`](Self::Symlink) is only ever
/// reported for the link *itself* — a resolved path yields the target's
/// kind — so a caller sees this variant exactly when it asked not to follow.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FileKind {
    /// A regular file: readable/writable byte content.
    Regular = 0,
    /// A directory: listable with `fs_readdir`.
    Directory = 1,
    /// A symbolic link: its content is a path, read with `fs_readlink`.
    Symlink = 2,
}

impl FileKind {
    /// Raw on-wire discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a [`FileKind`] from its wire discriminant, or
    /// [`Errno::OutOfRange`] for an unknown value (fail closed — never
    /// guess an unrecognised kind).
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `raw` is not a defined discriminant.
    pub const fn from_u8(raw: u8) -> Result<Self, Errno> {
        match raw {
            0 => Ok(Self::Regular),
            1 => Ok(Self::Directory),
            2 => Ok(Self::Symlink),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Whether this is a directory.
    #[must_use]
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// Whether this is a symbolic link.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// The ten-byte POSIX long-format mode string for a node, e.g. `drwxr-xr-x`.
///
/// The first byte is the kind indicator — `d` for a directory, `l` for a
/// symbolic link, `-` for a regular file — and the next nine are the
/// owner/group/other `rwx` triads
/// taken from the low nine bits of `mode`: a set bit renders as its
/// `r`/`w`/`x` letter, a clear bit as `-`. Every byte is ASCII, so the array
/// is valid UTF-8 by construction and a caller may render it as a string
/// without a fallible decode.
///
/// This is the single definition of the mapping so the `ls` long format and
/// the file manager's properties view can never disagree on what a mode
/// means. Only the nine permission bits are rendered; the higher mode bits
/// (setuid/setgid/sticky) are not part of this spelling.
#[must_use]
pub const fn mode_string(kind: FileKind, mode: u32) -> [u8; 10] {
    const PERMISSIONS: [(u32, u8); 9] = [
        (0o400, b'r'),
        (0o200, b'w'),
        (0o100, b'x'),
        (0o040, b'r'),
        (0o020, b'w'),
        (0o010, b'x'),
        (0o004, b'r'),
        (0o002, b'w'),
        (0o001, b'x'),
    ];
    let mut out = [b'-'; 10];
    out[0] = match kind {
        FileKind::Directory => b'd',
        FileKind::Symlink => b'l',
        FileKind::Regular => b'-',
    };
    let mut i = 0;
    while i < PERMISSIONS.len() {
        let (bit, ch) = PERMISSIONS[i];
        if mode & bit != 0 {
            out[i + 1] = ch;
        }
        i += 1;
    }
    out
}

/// Flags accepted by [`fs_open`](crate::SyscallNumber::FS_OPEN).
///
/// A `#[repr(transparent)]` newtype over the `u32` flags register, mirroring
/// [`crate::MapFlags`]: only the bits named here are defined and
/// [`OpenFlags::from_bits`] rejects any reserved bit, so a future flag is
/// never silently ignored by an older kernel (validate every input, fail
/// closed).
///
/// An open with neither [`READ`](Self::READ) nor [`WRITE`](Self::WRITE) is a
/// *resolve-only* handle: it validates the path and search permission and
/// can be `fs_stat`'d or `fs_readdir`'d, but `fs_read`/`fs_write` against it
/// fail closed. This is the handle a caller opens purely to stat a node it
/// may traverse to but not read.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct OpenFlags(u32);

impl OpenFlags {
    /// Request read access. `fs_read` requires it.
    pub const READ: Self = Self(1 << 0);
    /// Request write access. `fs_write`/`fs_truncate` require it.
    pub const WRITE: Self = Self(1 << 1);
    /// Create the file if it does not exist, born with the mode the open
    /// names less the caller's umask. Only an open carrying this flag may
    /// name a mode. With [`DIRECTORY`](Self::DIRECTORY) and
    /// [`EXCLUSIVE`](Self::EXCLUSIVE), and neither access bit, it makes a
    /// directory and answers it open, so no name can be swapped in between
    /// the make and the open.
    pub const CREATE: Self = Self(1 << 2);
    /// Truncate the file to zero length on open. Requires
    /// [`WRITE`](Self::WRITE).
    pub const TRUNCATE: Self = Self(1 << 3);
    /// Every `fs_write` appends at the current end of file, ignoring the
    /// supplied offset (the journal-append posture). Requires
    /// [`WRITE`](Self::WRITE).
    pub const APPEND: Self = Self(1 << 4);
    /// The target must be a directory; opening a regular file fails closed.
    pub const DIRECTORY: Self = Self(1 << 5);
    /// With [`CREATE`](Self::CREATE), fail closed if the name already
    /// exists (exclusive create); a symbolic link there, dangling or not, is
    /// never followed.
    pub const EXCLUSIVE: Self = Self(1 << 6);
    /// Do not follow a symbolic link in the **final** path component: the
    /// handle names the link itself. Intermediate components are still
    /// followed, exactly as `O_NOFOLLOW` specifies — a link anywhere but the
    /// end is a path, not the target of the open.
    ///
    /// The link's bytes are not byte-readable, so this with
    /// [`READ`](Self::READ) or [`WRITE`](Self::WRITE) fails closed with
    /// [`Errno::LinkLoop`] *when the final component really is a link*; the
    /// flag combination itself is legal, because the final component
    /// usually is not one. A resolve-only handle (neither access bit) is
    /// therefore the `lstat` posture: it stats the link rather than its
    /// target.
    pub const NO_FOLLOW: Self = Self(1 << 7);

    /// The set of all defined flag bits.
    const DEFINED_BITS: u32 = Self::READ.0
        | Self::WRITE.0
        | Self::CREATE.0
        | Self::TRUNCATE.0
        | Self::APPEND.0
        | Self::DIRECTORY.0
        | Self::EXCLUSIVE.0
        | Self::NO_FOLLOW.0;

    /// An empty flag set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw flag bits, as carried on the ABI.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The union of `self` and `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Build a flag set from raw bits, rejecting any reserved bit and any
    /// combination the contract forbids.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `bits` sets a reserved bit, if
    /// [`TRUNCATE`](Self::TRUNCATE) / [`APPEND`](Self::APPEND) /
    /// [`EXCLUSIVE`](Self::EXCLUSIVE) appears without the access it requires,
    /// or if [`CREATE`](Self::CREATE) with [`DIRECTORY`](Self::DIRECTORY)
    /// lacks [`EXCLUSIVE`](Self::EXCLUSIVE) or asks an access (so an illegal
    /// request is rejected at the boundary, never half-applied).
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        if bits & !Self::DEFINED_BITS != 0 {
            return Err(Errno::OutOfRange);
        }
        let flags = Self(bits);
        let writes = flags.contains(Self::WRITE);
        if flags.contains(Self::TRUNCATE) && !writes {
            return Err(Errno::OutOfRange);
        }
        if flags.contains(Self::APPEND) && !writes {
            return Err(Errno::OutOfRange);
        }
        if flags.contains(Self::EXCLUSIVE) && !flags.contains(Self::CREATE) {
            return Err(Errno::OutOfRange);
        }
        if flags.contains(Self::DIRECTORY) && writes {
            // A directory is never opened for byte writes.
            return Err(Errno::OutOfRange);
        }
        // A directory made by its open is answered with no access at all, so
        // asking it to read is a request that cannot be honoured.
        if flags.contains(Self::CREATE.union(Self::DIRECTORY))
            && (!flags.contains(Self::EXCLUSIVE) || flags.is_read())
        {
            return Err(Errno::OutOfRange);
        }
        Ok(flags)
    }

    /// Whether every bit set in `other` is also set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether read access was requested.
    #[must_use]
    pub const fn is_read(self) -> bool {
        self.contains(Self::READ)
    }

    /// Whether write access was requested.
    #[must_use]
    pub const fn is_write(self) -> bool {
        self.contains(Self::WRITE)
    }

    /// Whether the final component's symbolic link must not be followed.
    #[must_use]
    pub const fn is_no_follow(self) -> bool {
        self.contains(Self::NO_FOLLOW)
    }

    /// Whether no flag at all is set — the resolve-only posture, which
    /// conveys neither reading nor writing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The read/write access alone, with every open-time flag dropped.
    ///
    /// The access is the lasting property of an open description; `CREATE`,
    /// `TRUNCATE`, `EXCLUSIVE`, `APPEND`, `DIRECTORY`, and `NO_FOLLOW` all
    /// describe how the *open* resolved and mean nothing to an already-open
    /// file. Delegating a descriptor (`fd_grant`) carries exactly this, so a
    /// delegation conveys what the grantor opened and no more.
    #[must_use]
    pub const fn access(self) -> Self {
        Self(self.0 & (Self::READ.0 | Self::WRITE.0))
    }
}

/// Whether a symbolic link in a path's **final** position is followed — the
/// `stat` versus `lstat` reading.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FinalLink {
    /// Resolve through it to whatever it names (the default posture).
    Follow,
    /// Stop at the link itself — the `lstat` / `readlink` / `unlink`
    /// posture, and the only one under which a dangling link is a node.
    Keep,
}

impl FinalLink {
    /// The posture an open descriptor's flags fix, so an open and a later
    /// stat through the same handle cannot disagree. An exclusive create keeps
    /// a final link, as POSIX's `O_CREAT | O_EXCL` does, so a link planted at
    /// the name, dangling or not, is an existing name rather than a path to
    /// create through.
    #[must_use]
    pub const fn for_open(flags: OpenFlags) -> Self {
        if flags.is_no_follow() || flags.contains(OpenFlags::CREATE.union(OpenFlags::EXCLUSIVE)) {
            Self::Keep
        } else {
            Self::Follow
        }
    }

    /// The resolve-only open that takes this posture: no access asked for,
    /// and [`OpenFlags::NO_FOLLOW`] when the link is kept.
    #[must_use]
    pub const fn resolve_only(self) -> OpenFlags {
        match self {
            Self::Follow => OpenFlags::empty(),
            Self::Keep => OpenFlags::NO_FOLLOW,
        }
    }
}

/// Flags accepted by [`fs_unlink`](crate::SyscallNumber::FS_UNLINK).
///
/// A `#[repr(transparent)]` newtype over the `u32` flags register, mirroring
/// [`OpenFlags`]: only the bits named here are defined and
/// [`UnlinkFlags::from_bits`] rejects any reserved bit, so a future flag is
/// never silently ignored by an older kernel (validate every input, fail
/// closed).
///
/// An empty flag set is the historical `fs_unlink`: it removes the named
/// file or (empty) directory. [`DIRECTORY`](Self::DIRECTORY) is the
/// `rmdir`/`unlinkat(AT_REMOVEDIR)` posture: the removal succeeds only when
/// the name is an (empty) **directory**, decided atomically by the
/// filesystem under its own lock — never by a caller-side `fs_stat` that a
/// concurrent rename could invalidate between the check and the removal.
///
/// [`EXPECT_NODE`](Self::EXPECT_NODE) and
/// [`EXPECT_UNCHANGED`](Self::EXPECT_UNCHANGED) make the removal conditional
/// on the [`NodeExpect`] the call carries, checked under the volume's lock: a
/// name that no longer names that node, or one changed since, answers
/// [`Errno::Stale`] and nothing is removed.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct UnlinkFlags(u32);

impl UnlinkFlags {
    /// Remove the name only if it is an (empty) directory; a non-directory
    /// fails closed with [`Errno::NotADirectory`].
    pub const DIRECTORY: Self = Self(1 << 0);
    /// Remove the name only if it names the node the expectation names; the
    /// expectation's other fields must be zero.
    pub const EXPECT_NODE: Self = Self(1 << 1);
    /// Remove the name only if it names that node with every field of the
    /// expectation unchanged.
    pub const EXPECT_UNCHANGED: Self = Self(1 << 2);

    /// The set of all defined flag bits.
    const DEFINED_BITS: u32 = Self::DIRECTORY.0 | Self::EXPECT_NODE.0 | Self::EXPECT_UNCHANGED.0;

    /// An empty flag set: remove the named file or (empty) directory.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw flag bits, as carried on the ABI.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Every flag `self` or `other` sets. Both expectation bits together are
    /// still refused where the kernel decodes the word.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Build a flag set from raw bits, rejecting any reserved bit.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `bits` sets a reserved bit (an unknown
    /// request is rejected at the boundary, never silently ignored), or both
    /// expectation bits, which ask two different things of one node.
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        if bits & !Self::DEFINED_BITS != 0 {
            return Err(Errno::OutOfRange);
        }
        let expect_both = Self::EXPECT_NODE.0 | Self::EXPECT_UNCHANGED.0;
        if bits & expect_both == expect_both {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(bits))
    }

    /// Whether every bit set in `other` is also set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the removal is restricted to an (empty) directory.
    #[must_use]
    pub const fn is_directory_only(self) -> bool {
        self.0 & Self::DIRECTORY.0 != 0
    }

    /// Whether the removal carries a [`NodeExpect`].
    #[must_use]
    pub const fn expects(self) -> bool {
        self.0 & (Self::EXPECT_NODE.0 | Self::EXPECT_UNCHANGED.0) != 0
    }
}

/// Flags selecting how [`SyscallNumber::FS_LINK`](crate::SyscallNumber::FS_LINK)
/// treats a final symbolic link on the name it is giving a second name to.
///
/// [`LinkFlags::from_bits`] rejects any reserved bit, so a future flag is
/// never silently ignored by an older kernel (validate every input, fail
/// closed).
///
/// An empty flag set is POSIX `link()`: **neither** operand's final
/// component is followed, so the inode that gains a name is the one the
/// caller spelled — a symbolic link planted on the way cannot redirect the
/// new name onto an object the caller never asked for.
/// [`FOLLOW`](Self::FOLLOW) is the `linkat(AT_SYMLINK_FOLLOW)` posture,
/// which `ln -L` asks for: the existing name resolves through its final link
/// and the new name is given to what that link names. The *new* name is
/// never followed under either flag — it is a name being created, and a
/// create never replaces an existing name.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct LinkFlags(u32);

impl LinkFlags {
    /// Resolve the existing name's final symbolic link and give the second
    /// name to what it names, rather than to the link itself.
    pub const FOLLOW: Self = Self(1 << 0);

    /// The set of all defined flag bits.
    const DEFINED_BITS: u32 = Self::FOLLOW.0;

    /// An empty flag set: POSIX `link()`, following neither final component.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw flag bits, as carried on the ABI.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Build a flag set from raw bits, rejecting any reserved bit.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `bits` sets a reserved bit (an unknown
    /// request is rejected at the boundary, never silently ignored).
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        if bits & !Self::DEFINED_BITS != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(bits))
    }

    /// Whether the existing name's final symbolic link is resolved.
    #[must_use]
    pub const fn follows(self) -> bool {
        self.0 & Self::FOLLOW.0 != 0
    }
}

/// Flags accepted by
/// [`SyscallNumber::FS_RENAME`](crate::SyscallNumber::FS_RENAME).
///
/// [`RenameFlags::from_bits`] rejects any reserved bit, so a future flag is
/// never silently ignored by an older kernel (validate every input, fail
/// closed).
///
/// An empty flag set is POSIX `rename()`: an existing destination is
/// replaced, subject to kind compatibility. [`NO_REPLACE`](Self::NO_REPLACE)
/// is `renameat2(RENAME_NOREPLACE)`: the move fails with
/// [`Errno::AlreadyExists`] when the destination names any entry but the
/// source's own, decided under the volume's lock — so a name another program
/// creates after the caller looked cannot be destroyed by the move. The
/// destination is matched by the volume's own rule, so on a volume that
/// ignores letter case a re-spelling of the source (`Foo` to `foo`) is the
/// source's own entry and is renamed, while a sibling spelled differently in
/// case only is a clash.
///
/// [`EXPECT_NODE`](Self::EXPECT_NODE) makes the move conditional on the
/// source still naming the node the call's [`NodeExpect`] names, checked under
/// the volume's lock; any other node answers [`Errno::Stale`] and nothing
/// moves.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct RenameFlags(u32);

impl RenameFlags {
    /// Refuse the move, rather than replace, when the destination exists.
    pub const NO_REPLACE: Self = Self(1 << 0);
    /// Move the source only if it names the node the expectation names; the
    /// expectation's other fields must be zero.
    pub const EXPECT_NODE: Self = Self(1 << 1);

    /// The set of all defined flag bits.
    const DEFINED_BITS: u32 = Self::NO_REPLACE.0 | Self::EXPECT_NODE.0;

    /// An empty flag set: POSIX `rename()`, replacing an existing destination.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw flag bits, as carried on the ABI.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Every flag `self` or `other` sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Build a flag set from raw bits, rejecting any reserved bit.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `bits` sets a reserved bit (an unknown
    /// request is rejected at the boundary, never silently ignored).
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        if bits & !Self::DEFINED_BITS != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(bits))
    }

    /// Whether an existing destination refuses the move.
    #[must_use]
    pub const fn refuses_replace(self) -> bool {
        self.0 & Self::NO_REPLACE.0 != 0
    }

    /// Whether the move carries a [`NodeExpect`].
    #[must_use]
    pub const fn expects(self) -> bool {
        self.0 & Self::EXPECT_NODE.0 != 0
    }
}

/// Flags of the descriptor-relative calls that act on a node in place:
/// [`fs_chdir`](crate::SyscallNumber::FS_CHDIR),
/// [`fs_set_mode`](crate::SyscallNumber::FS_SET_MODE),
/// [`fs_set_owner`](crate::SyscallNumber::FS_SET_OWNER) and
/// [`fs_access`](crate::SyscallNumber::FS_ACCESS).
///
/// [`AtFlags::from_bits`] rejects any reserved bit. Without
/// [`EMPTY_PATH`](Self::EMPTY_PATH) an empty path is refused, as it is
/// everywhere a path is taken.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct AtFlags(u32);

impl AtFlags {
    /// Act on a final symbolic link itself rather than on what it names.
    pub const NO_FOLLOW: Self = Self(1 << 0);
    /// Act on the node `dirfd` holds, which an empty path then names: the
    /// `fchmod` / `fchown` / `fchdir` spelling.
    pub const EMPTY_PATH: Self = Self(1 << 1);

    /// The set of all defined flag bits.
    const DEFINED_BITS: u32 = Self::NO_FOLLOW.0 | Self::EMPTY_PATH.0;

    /// An empty flag set: follow a final link, and take a non-empty path.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw flag bits, as carried on the ABI.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Build a flag set from raw bits, rejecting any reserved bit.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `bits` sets a reserved bit (an unknown
    /// request is rejected at the boundary, never silently ignored).
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        if bits & !Self::DEFINED_BITS != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(bits))
    }

    /// Whether every bit set in `other` is also set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The final-link posture these flags ask for.
    #[must_use]
    pub const fn final_link(self) -> FinalLink {
        if self.contains(Self::NO_FOLLOW) {
            FinalLink::Keep
        } else {
            FinalLink::Follow
        }
    }
}

/// What [`fs_access`](crate::SyscallNumber::FS_ACCESS) asks of a node: POSIX
/// `access()`'s bits, with their values. No bit at all asks only whether the
/// node exists.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct Access(u32);

impl Access {
    /// Read the file, or list the directory.
    pub const READ: Self = Self(4);
    /// Write the file, or add and remove the directory's names.
    pub const WRITE: Self = Self(2);
    /// Run the file, or search the directory.
    pub const EXEC: Self = Self(1);

    /// The set of all defined bits.
    const DEFINED_BITS: u32 = Self::READ.0 | Self::WRITE.0 | Self::EXEC.0;

    /// No access asked: only whether the node can be reached.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Every access `self` or `other` asks.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Raw bits, as carried on the ABI.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Build a request from raw bits, rejecting any other bit.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a bit outside [`READ`](Self::READ),
    /// [`WRITE`](Self::WRITE) and [`EXEC`](Self::EXEC).
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        if bits & !Self::DEFINED_BITS != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(bits))
    }

    /// Whether every bit set in `other` is also set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// A path and the directory it is relative to, as
/// [`fs_rename`](crate::SyscallNumber::FS_RENAME) and
/// [`fs_link`](crate::SyscallNumber::FS_LINK) take each of their two names:
/// [`WIRE_LEN`](Self::WIRE_LEN) bytes, little-endian, laid out as the C view
/// `tairix_path_at_t`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PathAt {
    /// The directory a relative path resolves from, or [`FD_CWD`]; an
    /// absolute path ignores it.
    pub dirfd: u32,
    /// The path's length in bytes, `1..=FS_PATH_MAX`.
    pub len: u32,
    /// The caller's address of the path's bytes; never zero.
    pub path: u64,
}

impl PathAt {
    /// Byte length on the wire.
    pub const WIRE_LEN: usize = 16;

    /// The little-endian wire encoding.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, self.dirfd);
        put_u32(&mut out, 4, self.len);
        put_u64(&mut out, 8, self.path);
        out
    }

    /// Decode the wire encoding, refusing what no path call accepts.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a length outside `1..=FS_PATH_MAX`, and
    /// [`Errno::BadAlignment`] for a zero address, as the dispatcher answers a
    /// null pointer register.
    pub fn from_le_bytes(raw: &[u8; Self::WIRE_LEN]) -> Result<Self, Errno> {
        let at = Self {
            dirfd: read_u32(raw, 0),
            len: read_u32(raw, 4),
            path: read_u64(raw, 8),
        };
        if !usize::try_from(at.len).is_ok_and(|len| (1..=FS_PATH_MAX).contains(&len)) {
            return Err(Errno::LengthOutOfRange);
        }
        if at.path == 0 {
            return Err(Errno::BadAlignment);
        }
        Ok(at)
    }
}

/// What a removal or a move expects of the node it is about to act on, so the
/// call fails rather than act on a node swapped in or changed since the caller
/// looked: [`WIRE_LEN`](Self::WIRE_LEN) bytes, little-endian, laid out as the
/// C view `tairix_node_expect_t`.
///
/// Every field is as [`FileStat`] reports it, so a volume swapped in under the
/// same name is never taken for the one looked at. The wire keeps each field
/// at its natural alignment, so both stamps' seconds precede both stamps'
/// nanoseconds.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeExpect {
    /// The node's identity, [`FileStat::id`]: its volume and its number there.
    pub id: FileId,
    /// Its content generation.
    pub content_gen: u64,
    /// Its length in bytes.
    pub size: u64,
    /// Its contents-modification stamp.
    pub modified: Time64,
    /// Its metadata-change stamp.
    pub changed: Time64,
}

impl NodeExpect {
    /// Byte length on the wire.
    pub const WIRE_LEN: usize = 64;

    /// The expectation that the node `stat` describes is still there exactly
    /// as described.
    #[must_use]
    pub const fn of(stat: &FileStat) -> Self {
        Self {
            id: stat.id,
            content_gen: stat.content_gen,
            size: stat.size,
            modified: stat.times.modified,
            changed: stat.times.changed,
        }
    }

    /// The expectation that the node `id` is still the one named, whatever has
    /// changed in it: what `EXPECT_NODE` carries.
    #[must_use]
    pub const fn of_node(id: FileId) -> Self {
        Self {
            id,
            content_gen: 0,
            size: 0,
            modified: Time64::UNIX_EPOCH,
            changed: Time64::UNIX_EPOCH,
        }
    }

    /// Whether only the node is named, every other field zero, as an
    /// expectation of the node alone must be.
    #[must_use]
    pub fn names_only_the_node(&self) -> bool {
        self.content_gen == 0
            && self.size == 0
            && self.modified == Time64::UNIX_EPOCH
            && self.changed == Time64::UNIX_EPOCH
    }

    /// The little-endian wire encoding.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        out[..16].copy_from_slice(&self.id.volume);
        put_u64(&mut out, 16, self.id.node);
        put_u64(&mut out, 24, self.content_gen);
        put_u64(&mut out, 32, self.size);
        put_i64(&mut out, 40, self.modified.secs());
        put_i64(&mut out, 48, self.changed.secs());
        put_u32(&mut out, 56, self.modified.subsec_nanos());
        put_u32(&mut out, 60, self.changed.subsec_nanos());
        out
    }

    /// Decode the wire encoding.
    ///
    /// # Errors
    ///
    /// [`Errno::TimestampOutOfRange`] for a stamp whose nanoseconds are not
    /// canonical.
    pub fn from_le_bytes(raw: &[u8; Self::WIRE_LEN]) -> Result<Self, Errno> {
        let mut volume = [0u8; 16];
        volume.copy_from_slice(&raw[..16]);
        Ok(Self {
            id: FileId {
                volume,
                node: read_u64(raw, 16),
            },
            content_gen: read_u64(raw, 24),
            size: read_u64(raw, 32),
            modified: Time64::new(read_i64(raw, 40), read_u32(raw, 56))?,
            changed: Time64::new(read_i64(raw, 48), read_u32(raw, 60))?,
        })
    }
}

/// How much of a path
/// [`SyscallNumber::FS_REALPATH`](crate::SyscallNumber::FS_REALPATH)
/// requires to exist.
///
/// The three readings are **alternatives**, not modifiers, so they are one
/// value rather than independent bits: a caller asks for exactly one, and
/// two of them cannot be combined into a request with no meaning. They are
/// GNU's three canonicalisation switches, one apiece — `realpath -e` /
/// `readlink -e` is [`Existing`](Self::Existing), `readlink -f` is
/// [`Final`](Self::Final), and `readlink -m` is
/// [`Missing`](Self::Missing).
///
/// Every reading canonicalises the same way: each component is resolved in
/// turn under the caller's attested identity, every symbolic link is
/// followed (including one in the final position), and `..` names the
/// directory the walk actually came through. They differ only in whether a
/// component that does not exist ends the call or is carried through to the
/// answer — never in how much authority is checked, because a component
/// that does not exist has nothing to authorise.
#[repr(u32)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum RealpathMode {
    /// Every component must exist. The strictest reading, and the zero
    /// value, so a caller that passes an uninitialised word gets it.
    #[default]
    Existing = 0,
    /// Every component but the **last** must exist; a vacant final name is
    /// carried into the answer. This is what names a file a caller is about
    /// to create.
    Final = 1,
    /// No component need exist: resolution runs as far as the volume goes
    /// and the components it could not resolve are appended unchanged.
    Missing = 2,
}

impl RealpathMode {
    /// The reading `raw` selects, rejecting any other value.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `raw` is not one of the three defined
    /// readings (an unknown request is rejected at the boundary, never
    /// silently treated as one of them).
    pub const fn from_raw(raw: u32) -> Result<Self, Errno> {
        match raw {
            0 => Ok(Self::Existing),
            1 => Ok(Self::Final),
            2 => Ok(Self::Missing),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Raw value, as carried on the ABI.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Whether a **vacant final name** is carried into the answer rather
    /// than refused.
    #[must_use]
    pub const fn tolerates_vacant_final(self) -> bool {
        matches!(self, Self::Final | Self::Missing)
    }

    /// Whether a **missing intermediate** component is carried into the
    /// answer rather than refused.
    #[must_use]
    pub const fn tolerates_missing_intermediate(self) -> bool {
        matches!(self, Self::Missing)
    }
}

/// Which kind of region of a file a hole-aware seek looks for: the
/// `SEEK_DATA` / `SEEK_HOLE` pair.
///
/// A file is a sequence of *data* — bytes its volume stores — and *holes*,
/// ranges that read as zeros and occupy no storage. The end of a file always
/// begins a hole, so every byte before it lies in exactly one region and a
/// file with no stored bytes is one hole from offset zero.
#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SeekRegion {
    /// The first byte at or after the offset that the volume stores.
    Data = 0,
    /// The first byte at or after the offset that lies in a hole.
    Hole = 1,
}

impl SeekRegion {
    /// The region kind `raw` selects, rejecting any other value.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `raw` names neither kind.
    pub const fn from_raw(raw: u32) -> Result<Self, Errno> {
        match raw {
            0 => Ok(Self::Data),
            1 => Ok(Self::Hole),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Raw value, as carried on the ABI.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Where the next region of this kind begins at or after `offset` in a
    /// file of `size` bytes that is data throughout — the answer of every
    /// format that keeps no holes. `None` at or past the end, where there is
    /// nothing left to seek.
    #[must_use]
    pub const fn in_solid_file(self, offset: u64, size: u64) -> Option<u64> {
        if offset >= size {
            return None;
        }
        match self {
            Self::Data => Some(offset),
            Self::Hole => Some(size),
        }
    }
}

/// The stable, system-wide identity of a filesystem node.
///
/// A node is identified by the pair `(volume, node)`: `volume` is the
/// 16-byte identifier of the mounted volume the node lives on (the value the
/// mount was registered with), and `node` is the number the volume's format
/// gives the node
/// ([`NodeInfo::ino`](crate::driver::filesystem::NodeInfo::ino)). Two paths
/// that resolve to the same `FileId` name the same file, and a path whose
/// `FileId` changed between two stats has been replaced by a different file
/// at that name (log rotation). A format that numbers its nodes keeps the
/// number across a rename; a FAT file's number is where its directory entry
/// lies, which a rename moves.
///
/// `tail -f`/`-F` uses it to distinguish "the file I am following grew" from
/// "a different file now sits at this name", and the kernel keys its
/// file-change notification (the [`crate::WaitSourceKind::File`] wait source)
/// on it so a write wakes only the watchers of *that* node, never every
/// watcher on every write.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FileId {
    /// The 16-byte identifier of the mounted volume the node lives on.
    pub volume: [u8; 16],
    /// The number the volume's format gives the node.
    pub node: u64,
}

impl FileId {
    /// The all-zero identity: no volume, node `0`. Reported for a node whose
    /// backing has no distinct identity to offer (the fail-closed default);
    /// never equal to a real mounted node, whose volume id is non-zero.
    pub const NONE: Self = Self {
        volume: [0u8; 16],
        node: 0,
    };

    /// Whether this is the [`FileId::NONE`] placeholder rather than a real
    /// node identity.
    #[must_use]
    pub const fn is_none(self) -> bool {
        // A `const fn` cannot iterate a slice, so fold the 16 bytes by hand.
        let mut i = 0;
        while i < self.volume.len() {
            if self.volume[i] != 0 {
                return false;
            }
            i += 1;
        }
        self.node == 0
    }
}

/// The structural metadata `fs_stat` reports for an inode.
///
/// Carries only what the userland contract exposes: the node kind, its byte
/// size, its allocated on-disk bytes, the POSIX mode bits, the owning
/// uid/gid, its stable system-wide [`FileId`], and its four
/// timestamps. The kernel fills it
/// from the VFS's authorised view of the node; a program never reads it from
/// a `/proc`-style file (there is none).
///
/// The `times` field carries the same four [`Time64`] stamps a filesystem
/// driver reports through [`NodeTimes`] — the one definition of a node's
/// timestamps, translated straight through the VFS rather than restated. A
/// backing that keeps no stamp of a given kind reports
/// [`Time64::UNIX_EPOCH`] for it (ARXFS, for instance, tracks no access
/// time, so its `times.accessed` is always the epoch), never a fabricated
/// wall time.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FileStat {
    /// Whether the node is a regular file or a directory.
    pub kind: FileKind,
    /// How many directory entries name this node — POSIX `st_nlink`, what
    /// `ls -l`'s second column prints.
    ///
    /// Reported by the filesystem driver from the format's own record and
    /// carried through unchanged; a format that keeps no count answers `1`,
    /// the one name the caller's own path just walked. A directory's count
    /// includes its `.` and each child's `..`, so an empty directory is `2`.
    /// Never derived from anything else — a fabricated count would be a lie
    /// a caller cannot tell from a read one.
    pub nlink: u32,
    /// File length in bytes; `0` for a directory.
    pub size: u64,
    /// Bytes of on-disk storage the node's data occupies — the real
    /// allocation the mounted format tracks, reported by the filesystem
    /// driver (never derived from `size` when the format knows better).
    /// `0` for a node whose data occupies no dedicated blocks.
    pub allocated: u64,
    /// POSIX mode bits (the low 12 bits are meaningful).
    pub mode: u32,
    /// Owning user id.
    pub uid: u32,
    /// Owning group id.
    pub gid: u32,
    /// The node's stable system-wide identity, for distinguishing "this file
    /// grew" from "a different file now sits at this name" (log rotation),
    /// and the key the kernel's file-change notification uses.
    pub id: FileId,
    /// The node's four timestamps (creation, contents-modification (mtime),
    /// access (atime), metadata-change (ctime)), each 64-bit-native. A stamp
    /// the backing format does not keep is [`Time64::UNIX_EPOCH`].
    pub times: NodeTimes,
    /// The node's content generation
    /// ([`NodeInfo::content_gen`](crate::driver::filesystem::NodeInfo::content_gen)):
    /// the volume never hands one out twice, so with [`id`](Self::id) it names
    /// one version of the node's data. `0` where the volume keeps none.
    pub content_gen: u64,
}

impl FileStat {
    /// Encoded size of a [`FileStat`] on the wire.
    ///
    /// `kind(1)` + `pad(3)` + `nlink(4)` + `size(8)` + `allocated(8)` +
    /// `mode(4)` + `uid(4)` + `gid(4)` + `pad(4)` + `id.volume(16)` +
    /// `id.node(8)` + `created(12)` + `modified(12)` + `accessed(12)` +
    /// `changed(12)` + `content_gen(8)`.
    pub const WIRE_LEN: usize = 120;

    /// Encode `self` into the first [`FileStat::WIRE_LEN`] bytes of `out`.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` is shorter than
    /// [`FileStat::WIRE_LEN`].
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        if out.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        out[..Self::WIRE_LEN].fill(0);
        out[0] = self.kind.as_u8();
        put_u32(out, 4, self.nlink);
        put_u64(out, 8, self.size);
        put_u64(out, 16, self.allocated);
        put_u32(out, 24, self.mode);
        put_u32(out, 28, self.uid);
        put_u32(out, 32, self.gid);
        out[40..56].copy_from_slice(&self.id.volume);
        put_u64(out, 56, self.id.node);
        out[64..76].copy_from_slice(&self.times.created.to_le_bytes());
        out[76..88].copy_from_slice(&self.times.modified.to_le_bytes());
        out[88..100].copy_from_slice(&self.times.accessed.to_le_bytes());
        out[100..112].copy_from_slice(&self.times.changed.to_le_bytes());
        put_u64(out, 112, self.content_gen);
        Ok(Self::WIRE_LEN)
    }

    /// Decode a [`FileStat`] from the first [`FileStat::WIRE_LEN`] bytes of
    /// `bytes`.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] if `bytes` is shorter than
    ///   [`FileStat::WIRE_LEN`].
    /// * [`Errno::OutOfRange`] if the `kind` byte is not a defined
    ///   [`FileKind`].
    /// * [`Errno::TimestampOutOfRange`] if any timestamp is not a canonical
    ///   [`Time64`] encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        let mut volume = [0u8; 16];
        volume.copy_from_slice(&bytes[40..56]);
        Ok(Self {
            kind: FileKind::from_u8(bytes[0])?,
            nlink: read_u32(bytes, 4),
            size: read_u64(bytes, 8),
            allocated: read_u64(bytes, 16),
            mode: read_u32(bytes, 24),
            uid: read_u32(bytes, 28),
            gid: read_u32(bytes, 32),
            id: FileId {
                volume,
                node: read_u64(bytes, 56),
            },
            times: NodeTimes {
                created: Time64::from_bytes(&bytes[64..76])?,
                modified: Time64::from_bytes(&bytes[76..88])?,
                accessed: Time64::from_bytes(&bytes[88..100])?,
                changed: Time64::from_bytes(&bytes[100..112])?,
            },
            content_gen: read_u64(bytes, 112),
        })
    }
}

/// Maximum length, in bytes, of a single directory-entry name in the
/// `fs_readdir` stream.
///
/// A fail-closed bound, and the VFS's own component-length limit; a driver
/// reporting a longer name is a structural fault, not a capacity.
pub const FS_NAME_MAX: usize = 255;

/// One entry in the packed `fs_readdir` stream.
///
/// `fs_readdir` fills the caller's buffer with consecutive records, each a
/// fixed [`DirEntry::HEADER_LEN`]-byte header (kind, name length, size,
/// allocated bytes, modification stamp, node identity, name count)
/// followed by exactly `name_len`
/// UTF-8 name bytes (no NUL). The reader walks the buffer with
/// [`DirEntry::decode`]; the kernel
/// writes it with [`DirEntry::encode_into`]. The packing lives here, once,
/// so producer and consumer cannot disagree (no duplication).
///
/// The record carries each entry's `size`, `allocated`, `modified`, `id`,
/// and `nlink` because the
/// listing filesystem already holds the child's metadata while producing
/// the entry; a consumer that needs per-entry sizes or stamps (`du`,
/// `ls -l`, a file manager's listing) reads
/// them from the one listing instead of opening and statting every child —
/// on an uncached, authenticated volume each such stat is a full
/// re-resolution of the child's path (and a format such as FAT stores the
/// stamp only in the parent's directory record, so the listing is the one
/// place it is reportable at all).
///
/// `id` and `nlink` are what let a tree walk tell a second *name* for one
/// node from a second *node*: `du` sums a hard-linked file once by keying a
/// seen-set on `id`, and remembers only the entries whose `nlink` exceeds
/// one, since a single-named node can never be reached twice.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DirEntry<'a> {
    /// Whether the entry names a regular file or a directory.
    pub kind: FileKind,
    /// Apparent length in bytes; `0` for a directory.
    pub size: u64,
    /// Bytes of on-disk storage the entry's data occupies, as the mounted
    /// format's own allocation tracking reports it (the [`FileStat`]
    /// `allocated` field of the same node).
    pub allocated: u64,
    /// The entry's last contents-modification instant (mtime), as the
    /// mounted format stores it, widened to [`Time64`] without truncation.
    /// A backing that stores no per-node stamp reports
    /// [`Time64::UNIX_EPOCH`], never a fabricated wall time.
    pub modified: Time64,
    /// The node this name resolves to — the same stable identity
    /// [`FileStat::id`] reports, so two names for one node carry one `id`.
    /// [`FileId::NONE`] for an entry whose backing offers no identity (a
    /// covered mount point listed in its parent), which is therefore never
    /// a key a consumer may compare for equality.
    pub id: FileId,
    /// How many directory entries name this node — the [`FileStat::nlink`]
    /// of the same node, read from the format and never derived.
    pub nlink: u32,
    /// The node's content generation — the [`FileStat::content_gen`] of the
    /// same node, so a listing names which version of each file's data it
    /// saw. `0` where the volume keeps none.
    pub content_gen: u64,
    /// The entry's name (UTF-8, no terminator, never empty, never `.`/`..`).
    pub name: &'a [u8],
}

impl<'a> DirEntry<'a> {
    /// Size of the fixed per-entry header: `kind(1)` + `pad(1)` +
    /// `name_len(2)` + `size(8)` + `allocated(8)` + `modified(12)` +
    /// `id.volume(16)` + `id.node(8)` + `nlink(4)` + `content_gen(8)`.
    pub const HEADER_LEN: usize = 68;

    /// The longest record: the header and a name of [`FS_NAME_MAX`] bytes.
    pub const MAX_LEN: usize = Self::HEADER_LEN + FS_NAME_MAX;

    /// The total encoded length of this entry (header plus name).
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.name.len()
    }

    /// Encode this entry into the front of `out`, returning the number of
    /// bytes written.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] if the name is empty or longer than
    ///   [`FS_NAME_MAX`].
    /// * [`Errno::BufferTooSmall`] if `out` cannot hold the whole record.
    pub fn encode_into(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let name_len = self.name.len();
        if name_len == 0 || name_len > FS_NAME_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let total = self.encoded_len();
        if out.len() < total {
            return Err(Errno::BufferTooSmall);
        }
        out[0] = self.kind.as_u8();
        out[1] = 0;
        // `name_len <= FS_NAME_MAX` (255) fits a u16 with room to spare; the
        // checked conversion makes the bound explicit rather than truncating.
        let name_len_u16 = u16::try_from(name_len).map_err(|_| Errno::LengthOutOfRange)?;
        let [lo, hi] = name_len_u16.to_le_bytes();
        out[2] = lo;
        out[3] = hi;
        out[4..12].copy_from_slice(&self.size.to_le_bytes());
        out[12..20].copy_from_slice(&self.allocated.to_le_bytes());
        out[20..32].copy_from_slice(&self.modified.to_le_bytes());
        out[32..48].copy_from_slice(&self.id.volume);
        put_u64(out, 48, self.id.node);
        put_u32(out, 56, self.nlink);
        put_u64(out, 60, self.content_gen);
        out[Self::HEADER_LEN..total].copy_from_slice(self.name);
        Ok(total)
    }

    /// Decode the first entry from `bytes`, returning it and the number of
    /// bytes it consumed (so the caller can advance to the next record).
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] if `bytes` is shorter than the header or
    ///   than the declared name.
    /// * [`Errno::OutOfRange`] if the `kind` byte is not a defined
    ///   [`FileKind`], or the pad byte is not zero — every record has exactly
    ///   one encoding.
    /// * [`Errno::LengthOutOfRange`] if the declared name length is zero or
    ///   exceeds [`FS_NAME_MAX`].
    /// * [`Errno::TimestampOutOfRange`] if the modification stamp is not a
    ///   canonical [`Time64`] encoding.
    pub fn decode(bytes: &'a [u8]) -> Result<(Self, usize), Errno> {
        if bytes.len() < Self::HEADER_LEN {
            return Err(Errno::BufferTooSmall);
        }
        let kind = FileKind::from_u8(bytes[0])?;
        if bytes[1] != 0 {
            return Err(Errno::OutOfRange);
        }
        let name_len = usize::from(bytes[2]) | (usize::from(bytes[3]) << 8);
        if name_len == 0 || name_len > FS_NAME_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let total = Self::HEADER_LEN + name_len;
        if bytes.len() < total {
            return Err(Errno::BufferTooSmall);
        }
        let mut volume = [0u8; 16];
        volume.copy_from_slice(&bytes[32..48]);
        Ok((
            Self {
                kind,
                size: read_u64(bytes, 4),
                allocated: read_u64(bytes, 12),
                modified: Time64::from_bytes(&bytes[20..32])?,
                id: FileId {
                    volume,
                    node: read_u64(bytes, 48),
                },
                nlink: read_u32(bytes, 56),
                content_gen: read_u64(bytes, 60),
                name: &bytes[Self::HEADER_LEN..total],
            },
            total,
        ))
    }
}

/// Iterator over an `fs_readdir` byte stream — one batch, or a listing's
/// batches laid end to end — and the one walker every consumer uses, so the
/// advance-by-`consumed` bookkeeping is never re-derived per tool.
///
/// Yields each decoded [`DirEntry`] in stream order. The first malformed
/// record surfaces as one terminal `Err` and ends the iteration (the
/// iterator is fused): a caller refuses the whole listing rather than
/// showing a partial or guessed one — fail closed, never a truncated view
/// presented as complete.
///
/// Forward progress is structural: a successful decode consumes at least
/// [`DirEntry::HEADER_LEN`] bytes, so the walk always terminates.
#[derive(Clone, Debug)]
pub struct DirEntries<'a> {
    /// The undecoded remainder of the stream; emptied on error so the
    /// iterator fuses.
    rest: &'a [u8],
}

impl<'a> DirEntries<'a> {
    /// Walk `stream`, the bytes one or more `fs_readdir` batches produced.
    #[must_use]
    pub const fn new(stream: &'a [u8]) -> Self {
        Self { rest: stream }
    }
}

impl<'a> Iterator for DirEntries<'a> {
    type Item = Result<DirEntry<'a>, Errno>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        match DirEntry::decode(self.rest) {
            Ok((entry, consumed)) => {
                self.rest = &self.rest[consumed..];
                Some(Ok(entry))
            }
            Err(err) => {
                self.rest = &[];
                Some(Err(err))
            }
        }
    }
}

impl core::iter::FusedIterator for DirEntries<'_> {}

/// Where an [`fs_readdir`](crate::SyscallNumber::FS_READDIR) batch starts.
///
/// The listing position belongs to the open file description, so `Next`
/// continues where the description's last batch stopped and `Start` restarts
/// the listing in the same call.
#[repr(u32)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ReaddirFrom {
    /// After the last entry this description returned; the first batch of a
    /// fresh description starts at the beginning.
    #[default]
    Next = 0,
    /// At the directory's first entry, whatever the description read before.
    Start = 1,
}

impl ReaddirFrom {
    /// The start `raw` selects, rejecting any other value.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `raw` is neither defined value.
    pub const fn from_raw(raw: u32) -> Result<Self, Errno> {
        match raw {
            0 => Ok(Self::Next),
            1 => Ok(Self::Start),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Raw value, as carried on the ABI.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// The longest latency [`SyscallNumber::FS_WATCH`](crate::SyscallNumber::FS_WATCH)
/// accepts: a watcher that wants changes less often than this wants a
/// periodic re-read, not a watch.
pub const DIR_WATCH_LATENCY_MAX_NS: u64 = 60 * 1_000_000_000;

/// What one [`DirChangeBatch`] says about its watch.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirWatchStatus {
    /// Records follow, each naming one entry that changed and stating it now.
    Changes = 0,
    /// More changed than the watch could record by name, or something a name
    /// cannot express did: re-read the whole directory with `fs_readdir` on
    /// the same descriptor. The watch stays armed and reports from here.
    Rescan = 1,
    /// The descriptor's path no longer reaches the watched directory — it
    /// was removed, renamed or moved away, or its volume left. The watch is
    /// spent and records nothing more.
    Gone = 2,
}

impl DirWatchStatus {
    const fn from_u8(raw: u8) -> Result<Self, Errno> {
        match raw {
            0 => Ok(Self::Changes),
            1 => Ok(Self::Rescan),
            2 => Ok(Self::Gone),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// One entry a directory watch reports as changed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirChange<'a> {
    /// The name exists: exactly the record `fs_readdir` reports for it now.
    Present(DirEntry<'a>),
    /// The name no longer exists.
    Absent(&'a [u8]),
}

impl<'a> DirChange<'a> {
    /// The tag byte of a [`DirChange::Present`] record, followed by one
    /// [`DirEntry`].
    pub const PRESENT: u8 = 0;
    /// The tag byte of a [`DirChange::Absent`] record, followed by a
    /// little-endian `u16` name length and the name.
    pub const ABSENT: u8 = 1;
    const ABSENT_HEADER_LEN: usize = 3;

    /// The longest record: a present entry with the longest name.
    pub const MAX_LEN: usize = 1 + DirEntry::MAX_LEN;

    /// The encoded length of a record for `name`, present or absent.
    #[must_use]
    pub const fn len_for(present: bool, name_len: usize) -> usize {
        if present {
            1 + DirEntry::HEADER_LEN + name_len
        } else {
            Self::ABSENT_HEADER_LEN + name_len
        }
    }

    /// The changed entry's name.
    #[must_use]
    pub const fn name(&self) -> &'a [u8] {
        match self {
            Self::Present(entry) => entry.name,
            Self::Absent(name) => name,
        }
    }

    /// Encode this record into the front of `out`, returning its length.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for an empty name or one longer than
    /// [`FS_NAME_MAX`]; [`Errno::BufferTooSmall`] if `out` is shorter than the
    /// record.
    pub fn encode_into(&self, out: &mut [u8]) -> Result<usize, Errno> {
        match self {
            Self::Present(entry) => {
                let (tag, rest) = out.split_first_mut().ok_or(Errno::BufferTooSmall)?;
                let written = entry.encode_into(rest)?;
                *tag = Self::PRESENT;
                Ok(1 + written)
            }
            Self::Absent(name) => {
                let len = u16::try_from(name.len())
                    .ok()
                    .filter(|&len| len != 0 && usize::from(len) <= FS_NAME_MAX)
                    .ok_or(Errno::LengthOutOfRange)?;
                let total = Self::ABSENT_HEADER_LEN + name.len();
                let record = out.get_mut(..total).ok_or(Errno::BufferTooSmall)?;
                record[0] = Self::ABSENT;
                record[1..3].copy_from_slice(&len.to_le_bytes());
                record[3..].copy_from_slice(name);
                Ok(total)
            }
        }
    }

    /// Decode the first record of `bytes`, returning it and its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a truncated record, [`Errno::OutOfRange`]
    /// for an unknown tag, and whatever [`DirEntry::decode`] refuses for a
    /// present one.
    pub fn decode(bytes: &'a [u8]) -> Result<(Self, usize), Errno> {
        let (&tag, rest) = bytes.split_first().ok_or(Errno::BufferTooSmall)?;
        match tag {
            Self::PRESENT => {
                let (entry, used) = DirEntry::decode(rest)?;
                Ok((Self::Present(entry), 1 + used))
            }
            Self::ABSENT => {
                let header = bytes
                    .get(..Self::ABSENT_HEADER_LEN)
                    .ok_or(Errno::BufferTooSmall)?;
                let len = usize::from(u16::from_le_bytes([header[1], header[2]]));
                if len == 0 || len > FS_NAME_MAX {
                    return Err(Errno::LengthOutOfRange);
                }
                let total = Self::ABSENT_HEADER_LEN + len;
                let name = bytes
                    .get(Self::ABSENT_HEADER_LEN..total)
                    .ok_or(Errno::BufferTooSmall)?;
                Ok((Self::Absent(name), total))
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// One [`SyscallNumber::FS_WATCH_READ`](crate::SyscallNumber::FS_WATCH_READ)
/// answer: an 8-byte header — status, flags, two reserved zero bytes, a
/// little-endian record count — then that many [`DirChange`] records.
///
/// [`decode`](Self::decode) validates every record before handing out any, so
/// a consumer never applies part of a malformed batch.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DirChangeBatch<'a> {
    /// What the batch says about the watch.
    pub status: DirWatchStatus,
    /// More changes are recorded than this batch held: read again.
    pub more: bool,
    records: &'a [u8],
}

impl<'a> DirChangeBatch<'a> {
    /// Length of the batch header.
    pub const HEADER_LEN: usize = 8;

    /// The smallest buffer a drain accepts: one header and the longest
    /// record, so every drain makes progress.
    pub const MIN_BUFFER: usize = Self::HEADER_LEN + DirChange::MAX_LEN;

    /// The flag bit saying more changes are recorded than the batch held.
    pub const MORE: u8 = 1;

    /// Write a batch header into the front of `out`.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` is shorter than
    /// [`HEADER_LEN`](Self::HEADER_LEN).
    pub fn encode_header(
        out: &mut [u8],
        status: DirWatchStatus,
        more: bool,
        count: u32,
    ) -> Result<usize, Errno> {
        let header = out
            .get_mut(..Self::HEADER_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        header[0] = status as u8;
        header[1] = if more { Self::MORE } else { 0 };
        header[2] = 0;
        header[3] = 0;
        header[4..8].copy_from_slice(&count.to_le_bytes());
        Ok(Self::HEADER_LEN)
    }

    /// Decode and validate a whole batch.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a truncated header or record,
    /// [`Errno::OutOfRange`] for an unknown status or flag, set reserved
    /// bytes, records on a batch whose status carries none, a record count
    /// that disagrees with the records, or trailing bytes; and whatever
    /// [`DirChange::decode`] refuses.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        let header = bytes.get(..Self::HEADER_LEN).ok_or(Errno::BufferTooSmall)?;
        let status = DirWatchStatus::from_u8(header[0])?;
        if header[1] & !Self::MORE != 0 || header[2] != 0 || header[3] != 0 {
            return Err(Errno::OutOfRange);
        }
        let more = header[1] & Self::MORE != 0;
        let count = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let records = &bytes[Self::HEADER_LEN..];
        if status != DirWatchStatus::Changes && (count != 0 || more || !records.is_empty()) {
            return Err(Errno::OutOfRange);
        }
        let mut rest = records;
        let mut seen: u32 = 0;
        while !rest.is_empty() {
            let (_, used) = DirChange::decode(rest)?;
            rest = &rest[used..];
            seen = seen.checked_add(1).ok_or(Errno::OutOfRange)?;
        }
        if seen != count {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            status,
            more,
            records,
        })
    }

    /// The batch's records, in the order the kernel wrote them.
    #[must_use]
    pub const fn changes(&self) -> DirChanges<'a> {
        DirChanges { rest: self.records }
    }

    /// Whether the entry `name` may have changed: the batch names it, or it
    /// itemises nothing because it is a rescan or the watch is spent.
    #[must_use]
    pub fn concerns(&self, name: &[u8]) -> bool {
        self.status != DirWatchStatus::Changes || self.changes().any(|change| change.name() == name)
    }
}

/// The records of a validated [`DirChangeBatch`].
#[derive(Clone, Debug)]
pub struct DirChanges<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for DirChanges<'a> {
    type Item = DirChange<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        // The batch validated every record, so a failure here is impossible;
        // stopping rather than guessing keeps it so if that ever changes.
        if let Ok((change, used)) = DirChange::decode(self.rest) {
            self.rest = &self.rest[used..];
            Some(change)
        } else {
            self.rest = &[];
            None
        }
    }
}

impl core::iter::FusedIterator for DirChanges<'_> {}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::{
        mode_after_owner_change, mode_string, Access, AtFlags, DirChange, DirChangeBatch,
        DirEntries, DirEntry, DirWatchStatus, FileId, FileKind, FileStat, FinalLink, NodeExpect,
        NodeTimes, OpenFlags, PathAt, RealpathMode, RenameFlags, UnlinkFlags, FD_CWD, FS_MODE_MASK,
        FS_NAME_MAX, FS_PATH_MAX, FS_SYMLINK_MAX, FS_UMASK_BITS, FS_UMASK_QUERY,
    };
    use crate::time::Time64;
    use crate::Errno;
    use alloc::vec;

    #[test]
    fn mode_string_renders_kind_and_permission_triads() {
        // A directory renders its kind indicator and full rwx set.
        assert_eq!(&mode_string(FileKind::Directory, 0o755), b"drwxr-xr-x");
        // A regular file leads with `-`.
        assert_eq!(&mode_string(FileKind::Regular, 0o644), b"-rw-r--r--");
        // No permission bits: only the kind indicator, all triads cleared.
        assert_eq!(&mode_string(FileKind::Regular, 0), b"----------");
        // Every permission bit set.
        assert_eq!(&mode_string(FileKind::Regular, 0o777), b"-rwxrwxrwx");
        // A private file (owner read/write only).
        assert_eq!(&mode_string(FileKind::Regular, 0o600), b"-rw-------");
        // Higher (setuid/setgid/sticky) bits do not affect the nine triads.
        assert_eq!(&mode_string(FileKind::Regular, 0o7644), b"-rw-r--r--");
    }

    #[test]
    fn file_kind_round_trips_and_rejects_unknown() {
        for k in [FileKind::Regular, FileKind::Directory, FileKind::Symlink] {
            assert_eq!(FileKind::from_u8(k.as_u8()), Ok(k));
        }
        assert_eq!(FileKind::from_u8(3), Err(Errno::OutOfRange));
        assert_eq!(FileKind::from_u8(0xFF), Err(Errno::OutOfRange));
        assert!(FileKind::Directory.is_dir());
        assert!(!FileKind::Regular.is_dir());
        // A link is neither a directory nor byte content: exactly one
        // predicate answers for each kind, so no caller can treat a link as
        // a directory it may descend.
        assert!(FileKind::Symlink.is_symlink());
        assert!(!FileKind::Symlink.is_dir());
        assert!(!FileKind::Directory.is_symlink());
        assert!(!FileKind::Regular.is_symlink());
    }

    #[test]
    fn mode_string_marks_a_symlink() {
        // The kind indicator is the only difference; the triads are shared.
        assert_eq!(&mode_string(FileKind::Symlink, 0o777), b"lrwxrwxrwx");
        assert_eq!(&mode_string(FileKind::Directory, 0o777), b"drwxrwxrwx");
        assert_eq!(&mode_string(FileKind::Regular, 0o777), b"-rwxrwxrwx");
    }

    #[test]
    fn the_umask_query_word_is_no_mask_and_a_mask_withholds_only_mode_bits() {
        assert_ne!(FS_UMASK_QUERY & !FS_UMASK_BITS, 0);
        assert_eq!(FS_UMASK_BITS & !FS_MODE_MASK, 0);
        assert_eq!(FS_UMASK_BITS, 0o777);
    }

    #[test]
    fn open_flags_reject_reserved_bits() {
        assert_eq!(OpenFlags::from_bits(1 << 8), Err(Errno::OutOfRange));
        assert_eq!(OpenFlags::from_bits(u32::MAX), Err(Errno::OutOfRange));
        assert_eq!(OpenFlags::from_bits(0).map(OpenFlags::bits), Ok(0));
    }

    #[test]
    fn no_follow_is_defined_and_composes_with_access() {
        // The flag combination is legal: the final component is usually not
        // a link, and it is resolution — not `from_bits` — that refuses byte
        // access to one that is.
        let bare = OpenFlags::from_bits(OpenFlags::NO_FOLLOW.bits()).expect("defined");
        assert!(bare.is_no_follow());
        assert!(!bare.is_read() && !bare.is_write());
        let reading = OpenFlags::from_bits(OpenFlags::NO_FOLLOW.union(OpenFlags::READ).bits())
            .expect("defined");
        assert!(reading.is_no_follow() && reading.is_read());
        assert!(!OpenFlags::READ.is_no_follow());
    }

    #[test]
    fn symlink_target_bound_is_the_path_bound() {
        // Derived, never restated: a target is a path.
        assert_eq!(FS_SYMLINK_MAX, FS_PATH_MAX);
    }

    #[test]
    fn unlink_flags_reject_reserved_bits_and_decode_directory() {
        // Bits 0..=2 are defined; anything else fails closed.
        assert_eq!(UnlinkFlags::from_bits(1 << 3), Err(Errno::OutOfRange));
        assert_eq!(UnlinkFlags::from_bits(u32::MAX), Err(Errno::OutOfRange));
        let plain = UnlinkFlags::from_bits(0).unwrap();
        assert!(!plain.is_directory_only());
        assert!(!plain.expects());
        assert_eq!(plain, UnlinkFlags::empty());
        let dir_only = UnlinkFlags::from_bits(UnlinkFlags::DIRECTORY.bits()).unwrap();
        assert!(dir_only.is_directory_only());
        assert_eq!(dir_only, UnlinkFlags::DIRECTORY);
    }

    #[test]
    fn unlink_expectations_are_exclusive_and_compose_with_directory() {
        let node = UnlinkFlags::from_bits(UnlinkFlags::EXPECT_NODE.bits()).unwrap();
        assert!(node.expects() && node.contains(UnlinkFlags::EXPECT_NODE));
        let unchanged = UnlinkFlags::from_bits(UnlinkFlags::EXPECT_UNCHANGED.bits()).unwrap();
        assert!(unchanged.expects() && !unchanged.contains(UnlinkFlags::EXPECT_NODE));
        assert_eq!(
            UnlinkFlags::from_bits(
                UnlinkFlags::EXPECT_NODE.bits() | UnlinkFlags::EXPECT_UNCHANGED.bits()
            ),
            Err(Errno::OutOfRange),
            "one node cannot be expected two ways"
        );
        let rmdir =
            UnlinkFlags::from_bits(UnlinkFlags::DIRECTORY.bits() | UnlinkFlags::EXPECT_NODE.bits())
                .unwrap();
        assert!(rmdir.is_directory_only() && rmdir.expects());
    }

    #[test]
    fn rename_flags_reject_reserved_bits_and_decode_no_replace() {
        assert_eq!(RenameFlags::from_bits(1 << 2), Err(Errno::OutOfRange));
        assert_eq!(RenameFlags::from_bits(u32::MAX), Err(Errno::OutOfRange));
        let plain = RenameFlags::from_bits(0).unwrap();
        assert!(!plain.refuses_replace());
        assert!(!plain.expects());
        assert_eq!(plain, RenameFlags::empty());
        let exclusive = RenameFlags::from_bits(RenameFlags::NO_REPLACE.bits()).unwrap();
        assert!(exclusive.refuses_replace());
        let expecting = RenameFlags::from_bits(
            RenameFlags::NO_REPLACE.bits() | RenameFlags::EXPECT_NODE.bits(),
        )
        .unwrap();
        assert!(expecting.refuses_replace() && expecting.expects());
    }

    #[test]
    fn the_working_directory_word_is_no_descriptor_number() {
        assert_eq!(FD_CWD, u32::MAX);
    }

    #[test]
    fn at_flags_reject_reserved_bits_and_fix_the_final_link() {
        assert_eq!(AtFlags::from_bits(1 << 2), Err(Errno::OutOfRange));
        assert_eq!(AtFlags::from_bits(u32::MAX), Err(Errno::OutOfRange));
        assert_eq!(AtFlags::empty().final_link(), FinalLink::Follow);
        let both = AtFlags::from_bits(3).unwrap();
        assert!(both.contains(AtFlags::NO_FOLLOW) && both.contains(AtFlags::EMPTY_PATH));
        assert_eq!(both.final_link(), FinalLink::Keep);
        assert_eq!(AtFlags::EMPTY_PATH.final_link(), FinalLink::Follow);
    }

    #[test]
    fn access_takes_posix_values_and_refuses_any_other_bit() {
        assert_eq!(
            (
                Access::READ.bits(),
                Access::WRITE.bits(),
                Access::EXEC.bits()
            ),
            (4, 2, 1)
        );
        for bits in 0..=7 {
            assert_eq!(Access::from_bits(bits).map(Access::bits), Ok(bits));
        }
        assert_eq!(Access::from_bits(8), Err(Errno::OutOfRange));
        assert_eq!(Access::from_bits(u32::MAX), Err(Errno::OutOfRange));
        let all = Access::READ.union(Access::WRITE).union(Access::EXEC);
        assert_eq!(Access::from_bits(7), Ok(all));
        assert!(all.contains(Access::WRITE) && !Access::empty().contains(Access::EXEC));
        assert_eq!(Access::empty().bits(), 0);
    }

    #[test]
    fn a_path_at_round_trips_and_refuses_what_no_path_call_takes() {
        let at = PathAt {
            dirfd: FD_CWD,
            len: 7,
            path: 0x7fff_0000_1000,
        };
        let raw = at.to_le_bytes();
        assert_eq!(raw.len(), PathAt::WIRE_LEN);
        assert_eq!(&raw[0..4], &u32::MAX.to_le_bytes());
        assert_eq!(&raw[4..8], &7u32.to_le_bytes());
        assert_eq!(PathAt::from_le_bytes(&raw), Ok(at));
        let longest = PathAt {
            len: u32::try_from(FS_PATH_MAX).unwrap(),
            ..at
        };
        assert_eq!(PathAt::from_le_bytes(&longest.to_le_bytes()), Ok(longest));
        for len in [0, u32::try_from(FS_PATH_MAX + 1).unwrap(), u32::MAX] {
            let bad = PathAt { len, ..at };
            assert_eq!(
                PathAt::from_le_bytes(&bad.to_le_bytes()),
                Err(Errno::LengthOutOfRange)
            );
        }
        let null = PathAt { path: 0, ..at };
        assert_eq!(
            PathAt::from_le_bytes(&null.to_le_bytes()),
            Err(Errno::BadAlignment)
        );
    }

    #[test]
    fn a_node_expectation_round_trips_at_natural_alignment() {
        let expect = NodeExpect {
            id: FileId {
                volume: [0xA5; 16],
                node: 0x0102_0304_0506_0708,
            },
            content_gen: 9,
            size: 4096,
            modified: Time64::new(-86_400, 999_999_999).unwrap(),
            changed: Time64::new(4_102_444_800, 1).unwrap(),
        };
        let raw = expect.to_le_bytes();
        assert_eq!(raw.len(), NodeExpect::WIRE_LEN);
        assert_eq!(&raw[..16], &[0xA5; 16]);
        assert_eq!(&raw[16..24], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&raw[40..48], &(-86_400i64).to_le_bytes());
        assert_eq!(&raw[48..56], &4_102_444_800i64.to_le_bytes());
        assert_eq!(&raw[56..60], &999_999_999u32.to_le_bytes());
        assert_eq!(&raw[60..64], &1u32.to_le_bytes());
        assert_eq!(NodeExpect::from_le_bytes(&raw), Ok(expect));
        let mut torn = raw;
        torn[56..60].copy_from_slice(&1_000_000_000u32.to_le_bytes());
        assert_eq!(
            NodeExpect::from_le_bytes(&torn),
            Err(Errno::TimestampOutOfRange)
        );
    }

    #[test]
    fn a_node_expectation_is_what_stat_reports() {
        let stat = FileStat {
            kind: FileKind::Regular,
            nlink: 1,
            size: 12,
            allocated: 4096,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            id: FileId {
                volume: [7; 16],
                node: 42,
            },
            times: NodeTimes {
                created: Time64::from_secs(1),
                modified: Time64::from_secs(2),
                accessed: Time64::from_secs(3),
                changed: Time64::from_secs(4),
            },
            content_gen: 5,
        };
        let expect = NodeExpect::of(&stat);
        assert_eq!(
            (expect.id, expect.content_gen, expect.size),
            (stat.id, 5, 12)
        );
        assert_eq!(
            (expect.modified, expect.changed),
            (Time64::from_secs(2), Time64::from_secs(4))
        );
        assert!(!expect.names_only_the_node());
        let node_only = NodeExpect::of_node(stat.id);
        assert_eq!(
            node_only,
            NodeExpect {
                id: stat.id,
                ..NodeExpect::default()
            }
        );
        assert!(node_only.names_only_the_node());
    }

    #[test]
    fn an_owner_change_takes_a_files_set_id_privileges_and_keeps_a_directorys_bits() {
        assert_eq!(mode_after_owner_change(0o6755, false), 0o0755);
        // Without group-execute the set-group-ID bit grants nothing.
        assert_eq!(mode_after_owner_change(0o6745, false), 0o2745);
        assert_eq!(mode_after_owner_change(0o2775, true), 0o2775);
        assert_eq!(mode_after_owner_change(0o7777, true), 0o7777);
    }

    #[test]
    fn flag_words_combine_what_each_sets() {
        assert_eq!(
            UnlinkFlags::DIRECTORY
                .union(UnlinkFlags::EXPECT_NODE)
                .bits(),
            UnlinkFlags::DIRECTORY.bits() | UnlinkFlags::EXPECT_NODE.bits()
        );
        let both = UnlinkFlags::EXPECT_NODE.union(UnlinkFlags::EXPECT_UNCHANGED);
        assert_eq!(UnlinkFlags::from_bits(both.bits()), Err(Errno::OutOfRange));
        assert_eq!(
            RenameFlags::NO_REPLACE
                .union(RenameFlags::EXPECT_NODE)
                .bits(),
            RenameFlags::NO_REPLACE.bits() | RenameFlags::EXPECT_NODE.bits()
        );
    }

    #[test]
    fn a_final_link_posture_is_the_resolve_only_open_that_takes_it() {
        for link in [FinalLink::Follow, FinalLink::Keep] {
            let open = link.resolve_only();
            assert!(open.access().is_empty(), "{link:?} asks for no access");
            assert_eq!(FinalLink::for_open(open), link);
        }
        assert_eq!(FinalLink::Keep.resolve_only(), OpenFlags::NO_FOLLOW);
        assert_eq!(
            FinalLink::for_open(OpenFlags::READ.union(OpenFlags::NO_FOLLOW)),
            FinalLink::Keep
        );
    }

    #[test]
    fn an_exclusive_create_keeps_a_final_link() {
        let exclusive = OpenFlags::WRITE
            .union(OpenFlags::CREATE)
            .union(OpenFlags::EXCLUSIVE);
        assert_eq!(FinalLink::for_open(exclusive), FinalLink::Keep);
        let made_dir = OpenFlags::CREATE
            .union(OpenFlags::DIRECTORY)
            .union(OpenFlags::EXCLUSIVE);
        assert_eq!(FinalLink::for_open(made_dir), FinalLink::Keep);
        // A create that may open what is there follows a link to it.
        assert_eq!(
            FinalLink::for_open(OpenFlags::WRITE.union(OpenFlags::CREATE)),
            FinalLink::Follow
        );
    }

    #[test]
    fn realpath_modes_round_trip_and_reject_an_undefined_value() {
        for mode in [
            RealpathMode::Existing,
            RealpathMode::Final,
            RealpathMode::Missing,
        ] {
            assert_eq!(RealpathMode::from_raw(mode.as_u32()), Ok(mode));
        }
        // The strictest reading is the zero value, so an uninitialised word
        // asks for the one that refuses the most.
        assert_eq!(RealpathMode::default(), RealpathMode::Existing);
        assert_eq!(RealpathMode::Existing.as_u32(), 0);
        // An undefined value is refused, never rounded to a neighbour.
        assert_eq!(RealpathMode::from_raw(3), Err(Errno::OutOfRange));
        assert_eq!(RealpathMode::from_raw(u32::MAX), Err(Errno::OutOfRange));
    }

    #[test]
    fn realpath_modes_differ_only_in_what_may_be_absent() {
        // `-e`: everything must exist. `-f`: the final name need not.
        // `-m`: nothing need.
        assert!(!RealpathMode::Existing.tolerates_vacant_final());
        assert!(!RealpathMode::Existing.tolerates_missing_intermediate());
        assert!(RealpathMode::Final.tolerates_vacant_final());
        assert!(!RealpathMode::Final.tolerates_missing_intermediate());
        assert!(RealpathMode::Missing.tolerates_vacant_final());
        assert!(RealpathMode::Missing.tolerates_missing_intermediate());
    }

    #[test]
    fn open_flags_enforce_dependent_combinations() {
        // TRUNCATE/APPEND require WRITE; EXCLUSIVE requires CREATE.
        assert_eq!(
            OpenFlags::from_bits(OpenFlags::TRUNCATE.bits()),
            Err(Errno::OutOfRange)
        );
        assert_eq!(
            OpenFlags::from_bits(OpenFlags::APPEND.bits()),
            Err(Errno::OutOfRange)
        );
        assert_eq!(
            OpenFlags::from_bits(OpenFlags::EXCLUSIVE.bits()),
            Err(Errno::OutOfRange)
        );
        // A directory open never carries WRITE.
        assert_eq!(
            OpenFlags::from_bits(OpenFlags::DIRECTORY.union(OpenFlags::WRITE).bits()),
            Err(Errno::OutOfRange)
        );
        // Valid combinations decode.
        let rw = OpenFlags::READ
            .union(OpenFlags::WRITE)
            .union(OpenFlags::CREATE)
            .union(OpenFlags::TRUNCATE);
        let decoded = OpenFlags::from_bits(rw.bits()).expect("valid");
        assert!(decoded.is_read() && decoded.is_write());
        assert!(decoded.contains(OpenFlags::CREATE));
        let excl = OpenFlags::from_bits(
            OpenFlags::WRITE
                .union(OpenFlags::CREATE)
                .union(OpenFlags::EXCLUSIVE)
                .bits(),
        )
        .expect("create+excl valid");
        assert!(excl.contains(OpenFlags::EXCLUSIVE));
    }

    #[test]
    fn a_directory_made_by_its_open_is_exclusive_and_asks_no_access() {
        let make = OpenFlags::CREATE
            .union(OpenFlags::DIRECTORY)
            .union(OpenFlags::EXCLUSIVE);
        assert_eq!(OpenFlags::from_bits(make.bits()), Ok(make));
        assert_eq!(
            OpenFlags::from_bits(make.union(OpenFlags::NO_FOLLOW).bits()),
            Ok(make.union(OpenFlags::NO_FOLLOW))
        );
        assert_eq!(
            OpenFlags::from_bits(OpenFlags::CREATE.union(OpenFlags::DIRECTORY).bits()),
            Err(Errno::OutOfRange),
            "an existing directory is never opened as if made"
        );
        assert_eq!(
            OpenFlags::from_bits(make.union(OpenFlags::READ).bits()),
            Err(Errno::OutOfRange),
            "the new directory is answered with no access"
        );
    }

    #[test]
    fn file_stat_round_trips() {
        let stat = FileStat {
            kind: FileKind::Regular,
            nlink: 3,
            size: 0x0123_4567_89AB_CDEF,
            allocated: 0x0FED_CBA9_8765_4321,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            id: FileId {
                volume: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
                node: 0xDEAD_BEEF_0BAD_F00D,
            },
            // Each stamp distinct and spanning the pre-1970 / post-2038
            // range, so the full signed round-trip is exercised.
            times: NodeTimes {
                created: Time64::from_secs(-2_000_000_000),
                modified: Time64::new(4_000_000_000, 999_999_999).expect("canonical"),
                accessed: Time64::UNIX_EPOCH,
                changed: Time64::from_secs(1_700_000_000),
            },
            content_gen: 0x1122_3344_5566_7788,
        };
        let mut buf = [0u8; FileStat::WIRE_LEN];
        assert_eq!(stat.encode(&mut buf), Ok(FileStat::WIRE_LEN));
        assert_eq!(FileStat::decode(&buf), Ok(stat));
        assert_eq!(
            buf[112..120],
            0x1122_3344_5566_7788u64.to_le_bytes(),
            "the generation closes the record"
        );
    }

    #[test]
    fn file_stat_rejects_short_buffers() {
        let stat = FileStat {
            kind: FileKind::Directory,
            nlink: 2,
            size: 0,
            allocated: 0,
            mode: 0o755,
            uid: 0,
            gid: 0,
            id: FileId::NONE,
            times: NodeTimes::default(),
            content_gen: 0,
        };
        let mut tiny = [0u8; FileStat::WIRE_LEN - 1];
        assert_eq!(stat.encode(&mut tiny), Err(Errno::BufferTooSmall));
        assert_eq!(
            FileStat::decode(&[0u8; FileStat::WIRE_LEN - 1]),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn file_stat_decode_rejects_unknown_kind() {
        let mut buf = [0u8; FileStat::WIRE_LEN];
        buf[0] = 9;
        assert_eq!(FileStat::decode(&buf), Err(Errno::OutOfRange));
    }

    #[test]
    fn dir_entry_stream_round_trips() {
        let entries = [
            DirEntry {
                kind: FileKind::Directory,
                size: 0,
                allocated: 4096,
                // Pre-1970: the full signed range must round-trip.
                modified: Time64::from_secs(-2_000_000_000),
                id: FileId {
                    volume: [7u8; 16],
                    node: 42,
                },
                nlink: 2,
                content_gen: 0,
                name: b"Logs",
            },
            DirEntry {
                kind: FileKind::Regular,
                size: u64::MAX,
                allocated: 0x0102_0304_0506_0708,
                // Post-2038: never a 32-bit seconds wrap.
                modified: Time64::new(4_000_000_000, 999_999_999).expect("canonical"),
                id: FileId {
                    volume: [0xab; 16],
                    node: u64::MAX,
                },
                nlink: u32::MAX,
                content_gen: u64::MAX,
                name: b"motd.txt",
            },
        ];
        let mut buf = vec![0u8; 512];
        let mut off = 0;
        for e in &entries {
            off += e.encode_into(&mut buf[off..]).expect("fits");
        }
        let total = off;

        let mut cursor = 0;
        let mut decoded = vec![];
        while cursor < total {
            let (entry, used) = DirEntry::decode(&buf[cursor..total]).expect("valid");
            decoded.push((
                entry.kind,
                entry.size,
                entry.allocated,
                entry.modified,
                entry.id,
                entry.nlink,
                entry.content_gen,
                entry.name.to_vec(),
            ));
            cursor += used;
        }
        assert_eq!(cursor, total);
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded[0],
            (
                FileKind::Directory,
                0,
                4096,
                Time64::from_secs(-2_000_000_000),
                FileId {
                    volume: [7u8; 16],
                    node: 42
                },
                2,
                0,
                b"Logs".to_vec()
            )
        );
        assert_eq!(
            decoded[1],
            (
                FileKind::Regular,
                u64::MAX,
                0x0102_0304_0506_0708,
                Time64::new(4_000_000_000, 999_999_999).expect("canonical"),
                FileId {
                    volume: [0xab; 16],
                    node: u64::MAX
                },
                u32::MAX,
                u64::MAX,
                b"motd.txt".to_vec()
            )
        );
        assert_eq!(
            buf[DirEntry::HEADER_LEN + 4 + 60..DirEntry::HEADER_LEN + 4 + 68],
            u64::MAX.to_le_bytes(),
            "the generation closes the second record's header"
        );
    }

    #[test]
    fn two_names_for_one_node_carry_one_identity() {
        // The property `du`'s deduplication rests on: a second *name* is
        // told from a second *node* by the identity, not by the name.
        let id = FileId {
            volume: [3u8; 16],
            node: 9,
        };
        let stream = encoded_stream(&[
            DirEntry {
                kind: FileKind::Regular,
                size: 12,
                allocated: 4096,
                modified: Time64::UNIX_EPOCH,
                id,
                nlink: 2,
                name: b"first",
                content_gen: 0,
            },
            DirEntry {
                kind: FileKind::Regular,
                size: 12,
                allocated: 4096,
                modified: Time64::UNIX_EPOCH,
                id,
                nlink: 2,
                name: b"second",
                content_gen: 0,
            },
        ]);
        let decoded: alloc::vec::Vec<DirEntry<'_>> = DirEntries::new(&stream)
            .collect::<Result<_, _>>()
            .expect("valid");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].id, decoded[1].id);
        assert_eq!(decoded[0].nlink, 2);
        assert_ne!(decoded[0].name, decoded[1].name);
    }

    #[test]
    fn dir_entry_decode_rejects_non_canonical_stamp() {
        let entry = DirEntry {
            kind: FileKind::Regular,
            size: 1,
            allocated: 1,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: b"f",
            content_gen: 0,
        };
        let mut buf = [0u8; DirEntry::HEADER_LEN + 1];
        entry.encode_into(&mut buf).expect("fits");
        // Corrupt the stamp's nanosecond field past its canonical bound.
        buf[28..32].copy_from_slice(&1_000_000_000u32.to_le_bytes());
        assert_eq!(DirEntry::decode(&buf), Err(Errno::TimestampOutOfRange));
    }

    #[test]
    fn dir_entry_rejects_empty_and_oversize_names() {
        let mut buf = [0u8; 8];
        let empty = DirEntry {
            kind: FileKind::Regular,
            size: 0,
            allocated: 0,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: b"",
            content_gen: 0,
        };
        assert_eq!(empty.encode_into(&mut buf), Err(Errno::LengthOutOfRange));
        let big = vec![b'a'; FS_NAME_MAX + 1];
        let oversize = DirEntry {
            kind: FileKind::Regular,
            size: 0,
            allocated: 0,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: &big,
            content_gen: 0,
        };
        let mut wide = vec![0u8; FS_NAME_MAX + 8];
        assert_eq!(
            oversize.encode_into(&mut wide),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn dir_entry_encode_into_rejects_short_buffer() {
        let e = DirEntry {
            kind: FileKind::Regular,
            size: 0,
            allocated: 0,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: b"abcd",
            content_gen: 0,
        };
        let mut buf = [0u8; DirEntry::HEADER_LEN + 2];
        assert_eq!(e.encode_into(&mut buf), Err(Errno::BufferTooSmall));
    }

    #[test]
    fn dir_entry_decode_rejects_truncated_record() {
        // A whole header claiming a 4-byte name, but only 2 follow.
        let mut buf = [0u8; DirEntry::HEADER_LEN + 2];
        buf[0] = FileKind::Regular.as_u8();
        buf[2] = 4;
        buf[DirEntry::HEADER_LEN] = b'a';
        buf[DirEntry::HEADER_LEN + 1] = b'b';
        assert_eq!(DirEntry::decode(&buf), Err(Errno::BufferTooSmall));
        // A header cut short before its last field, likewise.
        assert_eq!(
            DirEntry::decode(&buf[..DirEntry::HEADER_LEN - 1]),
            Err(Errno::BufferTooSmall)
        );
        assert_eq!(DirEntry::decode(&[0u8; 2]), Err(Errno::BufferTooSmall));
    }

    /// Encode `entries` back to back into one stream, as the kernel's
    /// `fs_readdir` handler does.
    fn encoded_stream(entries: &[DirEntry<'_>]) -> alloc::vec::Vec<u8> {
        let mut buf = vec![0u8; 1024];
        let mut off = 0;
        for e in entries {
            off += e.encode_into(&mut buf[off..]).expect("fits");
        }
        buf.truncate(off);
        buf
    }

    #[test]
    fn dir_entries_walks_the_whole_stream_in_order() {
        let stream = encoded_stream(&[
            DirEntry {
                kind: FileKind::Directory,
                size: 0,
                allocated: 4096,
                modified: Time64::UNIX_EPOCH,
                id: FileId::NONE,
                nlink: 1,
                name: b"Logs",
                content_gen: 0,
            },
            DirEntry {
                kind: FileKind::Regular,
                size: 7,
                allocated: 4096,
                modified: Time64::from_secs(4_000_000_000),
                id: FileId::NONE,
                nlink: 1,
                name: b"motd.txt",
                content_gen: 0,
            },
        ]);
        let mut it = DirEntries::new(&stream);
        let first = it.next().expect("first entry").expect("valid");
        assert_eq!(
            (first.kind, first.name),
            (FileKind::Directory, &b"Logs"[..])
        );
        let second = it.next().expect("second entry").expect("valid");
        assert_eq!(
            (second.kind, second.name),
            (FileKind::Regular, &b"motd.txt"[..])
        );
        assert!(it.next().is_none());
    }

    #[test]
    fn a_dir_entry_with_a_set_pad_byte_is_refused() {
        let mut stream = encoded_stream(&[DirEntry {
            kind: FileKind::Regular,
            size: 1,
            allocated: 1,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: b"x",
            content_gen: 0,
        }]);
        assert!(DirEntry::decode(&stream).is_ok());
        stream[1] = 1;
        assert_eq!(DirEntry::decode(&stream), Err(Errno::OutOfRange));
    }

    #[test]
    fn dir_entries_over_an_empty_stream_yields_nothing() {
        assert!(DirEntries::new(&[]).next().is_none());
    }

    #[test]
    fn dir_entries_surfaces_the_first_bad_record_and_fuses() {
        let mut stream = encoded_stream(&[DirEntry {
            kind: FileKind::Regular,
            size: 1,
            allocated: 1,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: b"ok",
            content_gen: 0,
        }]);
        // A second record with an undefined kind byte: the walk must yield
        // the good entry, then exactly one error, then fuse.
        let mut bad = vec![0u8; DirEntry::HEADER_LEN + 1];
        DirEntry {
            kind: FileKind::Regular,
            size: 0,
            allocated: 0,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: b"x",
            content_gen: 0,
        }
        .encode_into(&mut bad)
        .expect("fits");
        bad[0] = 9;
        stream.extend_from_slice(&bad);

        let mut it = DirEntries::new(&stream);
        assert!(it.next().expect("good entry").is_ok());
        assert_eq!(
            it.next().expect("the bad record surfaces"),
            Err(Errno::OutOfRange)
        );
        assert!(it.next().is_none());
        assert!(it.next().is_none());
    }

    #[test]
    fn dir_entries_truncated_tail_fails_closed() {
        let mut stream = encoded_stream(&[DirEntry {
            kind: FileKind::Directory,
            size: 0,
            allocated: 0,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            nlink: 1,
            name: b"Users",
            content_gen: 0,
        }]);
        // A dangling half header can never be a listing the caller shows.
        stream.extend_from_slice(&[0u8; 3]);
        let mut it = DirEntries::new(&stream);
        assert!(it.next().expect("good entry").is_ok());
        assert_eq!(
            it.next().expect("the truncation surfaces"),
            Err(Errno::BufferTooSmall)
        );
        assert!(it.next().is_none());
    }

    fn present(name: &[u8]) -> DirEntry<'_> {
        DirEntry {
            kind: FileKind::Regular,
            size: 5,
            allocated: 4096,
            modified: Time64::from_secs(-86_400),
            id: FileId {
                volume: [9; 16],
                node: 77,
            },
            nlink: 2,
            name,
            content_gen: 0,
        }
    }

    /// A batch as the kernel's drain writes it: the header, then each record.
    fn batch(status: DirWatchStatus, more: bool, changes: &[DirChange<'_>]) -> alloc::vec::Vec<u8> {
        let mut buf = vec![0u8; 4096];
        let count = u32::try_from(changes.len()).expect("small");
        let mut off = DirChangeBatch::encode_header(&mut buf, status, more, count).expect("fits");
        for change in changes {
            let len = change.encode_into(&mut buf[off..]).expect("fits");
            assert_eq!(
                len,
                DirChange::len_for(matches!(change, DirChange::Present(_)), change.name().len())
            );
            off += len;
        }
        buf.truncate(off);
        buf
    }

    #[test]
    fn a_change_batch_round_trips_in_order() {
        let changes = [
            DirChange::Present(present(b"report.txt")),
            DirChange::Absent(b"old.log"),
            DirChange::Present(present(&[b'n'; FS_NAME_MAX])),
        ];
        let bytes = batch(DirWatchStatus::Changes, true, &changes);
        let decoded = DirChangeBatch::decode(&bytes).expect("valid batch");
        assert_eq!(decoded.status, DirWatchStatus::Changes);
        assert!(decoded.more);
        assert!(decoded.changes().eq(changes.iter().copied()));
    }

    #[test]
    fn the_minimum_buffer_holds_the_longest_record() {
        let longest = DirChange::Present(present(&[b'x'; FS_NAME_MAX]));
        let mut buf = vec![0u8; DirChangeBatch::MIN_BUFFER];
        let off = DirChangeBatch::encode_header(&mut buf, DirWatchStatus::Changes, false, 1)
            .expect("header fits");
        assert_eq!(longest.encode_into(&mut buf[off..]), Ok(DirChange::MAX_LEN));
        assert_eq!(off + DirChange::MAX_LEN, DirChangeBatch::MIN_BUFFER);
    }

    #[test]
    fn a_batch_concerns_the_names_it_lists_and_every_name_when_it_lists_none() {
        let listed = batch(
            DirWatchStatus::Changes,
            false,
            &[
                DirChange::Present(present(b"system.conf")),
                DirChange::Absent(b"old.log"),
            ],
        );
        let listed = DirChangeBatch::decode(&listed).expect("valid batch");
        assert!(listed.concerns(b"system.conf"));
        assert!(listed.concerns(b"old.log"));
        assert!(!listed.concerns(b"network.conf"));
        for status in [DirWatchStatus::Rescan, DirWatchStatus::Gone] {
            let bare = batch(status, false, &[]);
            let bare = DirChangeBatch::decode(&bare).expect("bare status");
            assert!(bare.concerns(b"system.conf"), "{status:?}");
        }
    }

    #[test]
    fn rescan_and_gone_carry_no_records() {
        for status in [DirWatchStatus::Rescan, DirWatchStatus::Gone] {
            let bytes = batch(status, false, &[]);
            let decoded = DirChangeBatch::decode(&bytes).expect("bare status");
            assert_eq!(decoded.status, status);
            assert!(decoded.changes().next().is_none());
            let padded = batch(status, false, &[DirChange::Absent(b"x")]);
            assert_eq!(DirChangeBatch::decode(&padded), Err(Errno::OutOfRange));
            let more = batch(status, true, &[]);
            assert_eq!(DirChangeBatch::decode(&more), Err(Errno::OutOfRange));
        }
    }

    #[test]
    fn a_malformed_batch_is_refused_whole() {
        let good = batch(
            DirWatchStatus::Changes,
            false,
            &[DirChange::Absent(b"a"), DirChange::Absent(b"b")],
        );
        assert!(DirChangeBatch::decode(&good).is_ok());
        // A record count that disagrees with the records.
        let mut miscounted = good.clone();
        miscounted[4] = 3;
        assert_eq!(DirChangeBatch::decode(&miscounted), Err(Errno::OutOfRange));
        // An unknown status, an undefined flag, a set reserved byte.
        for (at, value) in [(0, 9), (1, 2), (2, 1), (3, 1)] {
            let mut bad = good.clone();
            bad[at] = value;
            assert_eq!(DirChangeBatch::decode(&bad), Err(Errno::OutOfRange));
        }
        // A truncated last record, an unknown tag, an empty absent name.
        assert_eq!(
            DirChangeBatch::decode(&good[..good.len() - 1]),
            Err(Errno::BufferTooSmall)
        );
        let mut tagged = good.clone();
        tagged[DirChangeBatch::HEADER_LEN] = 7;
        assert_eq!(DirChangeBatch::decode(&tagged), Err(Errno::OutOfRange));
        let mut empty = good;
        empty[DirChangeBatch::HEADER_LEN + 1] = 0;
        assert_eq!(DirChangeBatch::decode(&empty), Err(Errno::LengthOutOfRange));
        assert_eq!(
            DirChangeBatch::decode(&[0u8; 7]),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn an_absent_record_refuses_an_unencodable_name() {
        let mut buf = [0u8; 600];
        assert_eq!(
            DirChange::Absent(b"").encode_into(&mut buf),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            DirChange::Absent(&[b'a'; FS_NAME_MAX + 1]).encode_into(&mut buf),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            DirChange::Absent(b"abc").encode_into(&mut buf[..5]),
            Err(Errno::BufferTooSmall)
        );
    }
}
