//! Per-inode permission model: POSIX mode bits **plus** ACLs **plus** a
//! capability gate.
//!
//! [`Metadata::authorize`] is the single decision point every VFS
//! operation routes through. It fails closed and never branches on
//! `uid == 0`: authority comes from capability grants, ACL entries, and
//! mode bits, never from a magic user id.
//!
//! The three layers compose in a fixed, documented order:
//!
//! 1. **Capability gate.** If the inode declares a required capability and
//!    the caller does not hold it, access is denied *regardless* of mode
//!    bits — a file marked `CAP_AUDIT_READ` is unreadable at mode `0644`
//!    by a caller without that capability.
//! 2. **ACL.** An explicit deny for the requested access wins; otherwise an
//!    explicit allow grants it. With no matching entry, the decision falls
//!    through to the mode bits.
//! 3. **Mode bits.** The owner / owning-group / other triad is selected by
//!    the caller's identity and the requested `rwx` bit is checked.
//!
//! A POSIX access ACL ([`PosixAcl`]) replaces the second and third layers
//! with POSIX.1e's own order.

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{AclModel, NodeSecurity, SecuritySubject, MAX_ACL_ENTRIES};
use tairix_abi::fs::Access as Wanted;
use tairix_abi::CapabilityId;
use tairix_abi::CapabilityQuery;
use tairix_abi::{Errno, FS_LINK_MODE, FS_MODE_MASK, FS_UMASK_BITS};
use tairix_kernel_sec::{GroupId, UserId};

use super::VfsError;

/// How an access through a descriptor is judged.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Judged {
    /// As a byte operation of that access through the descriptor is: the
    /// owner of a descriptor of its own passes the mode and the ACL.
    AsOperation,
    /// On the gate, mode and ACL alone, as a path is asked about, so a
    /// descriptor and a path answer alike for one identity.
    AsQuery,
}

/// A single access right requested against an inode.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Access {
    /// Read the file's contents or list a directory.
    Read,
    /// Modify the file's contents or the directory's entries.
    Write,
    /// Execute the file or search (traverse) the directory.
    Execute,
}

impl Access {
    /// The POSIX `rwx` bit for this access within a single mode triad.
    const fn bit(self) -> u16 {
        match self {
            Self::Read => 0b100,
            Self::Write => 0b010,
            Self::Execute => 0b001,
        }
    }

    /// Each access `want` asks for, in the order `fs_access` judges them:
    /// read, write, then execute.
    pub fn asked_by(want: Wanted) -> impl Iterator<Item = Self> {
        [
            (Wanted::READ, Self::Read),
            (Wanted::WRITE, Self::Write),
            (Wanted::EXEC, Self::Execute),
        ]
        .into_iter()
        .filter_map(move |(bit, access)| want.contains(bit).then_some(access))
    }
}

/// POSIX permission bits (the low 12 bits: `setuid`, `setgid`, sticky, and
/// the owner/group/other `rwx` triads).
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct Mode(u16);

impl Mode {
    /// `setuid` bit (`0o4000`).
    pub const SETUID: u16 = 0o4000;
    /// `setgid` bit (`0o2000`).
    pub const SETGID: u16 = 0o2000;
    /// Sticky bit (`0o1000`).
    pub const STICKY: u16 = 0o1000;

    /// Construct a [`Mode`] from its raw permission bits, masking off
    /// anything above the low 12.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & 0o7777)
    }

    /// The raw permission bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// `true` if any of `flags` (e.g. [`Mode::SETUID`]) are set.
    #[must_use]
    pub const fn has(self, flags: u16) -> bool {
        self.0 & flags == flags
    }

    /// The owner triad's `rwx` bits (`0..=7`).
    const fn owner_rwx(self) -> u16 {
        (self.0 >> 6) & 0b111
    }

    /// The owning-group triad's `rwx` bits (`0..=7`).
    const fn group_rwx(self) -> u16 {
        (self.0 >> 3) & 0b111
    }

    /// The other triad's `rwx` bits (`0..=7`).
    const fn other_rwx(self) -> u16 {
        self.0 & 0b111
    }
}

/// Which principal an [`AclEntry`] applies to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AclWho {
    /// A specific user.
    User(UserId),
    /// A specific group (matched against the caller's primary and
    /// supplementary groups).
    Group(GroupId),
}

/// A single access-control list entry: an explicit allow or deny of one
/// [`Access`] for one [`AclWho`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct AclEntry {
    /// The principal the entry applies to.
    pub who: AclWho,
    /// The access right the entry governs.
    pub access: Access,
    /// `true` to allow, `false` to deny.
    pub allow: bool,
}

/// The caller's security identity, as seen by the VFS.
///
/// Borrows the capability query rather than owning it: the live set lives
/// in the task's [`tairix_kernel_sec::TaskCapabilities`], which the VFS
/// never copies.
#[derive(Copy, Clone)]
pub struct Credentials<'a> {
    /// Numeric user id.
    pub uid: UserId,
    /// Primary group id.
    pub gid: GroupId,
    /// Supplementary group ids.
    pub supplementary_gids: &'a [GroupId],
    /// The caller's granted capabilities.
    pub caps: &'a dyn CapabilityQuery,
}

impl Credentials<'_> {
    /// `true` if the caller is a member of `gid` through its primary or
    /// supplementary groups.
    #[must_use]
    pub fn is_in_group(&self, gid: GroupId) -> bool {
        self.gid == gid || self.supplementary_gids.contains(&gid)
    }
}

/// What a create asks the new node's permission bits to be: the mode its
/// caller named, and the file mode creation mask of the process creating it.
///
/// Built only from a mode within [`FS_MODE_MASK`] and a mask within
/// [`FS_UMASK_BITS`], so a node is never born with a bit the permission
/// model does not know.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CreateMode {
    mode: u32,
    umask: u32,
}

impl CreateMode {
    /// What an open that creates nothing carries: no mode and no mask.
    pub const NONE: Self = Self { mode: 0, umask: 0 };

    /// What a symbolic link is born with: every permission bit, unmasked,
    /// since a link's own mode never gates access.
    pub const LINK: Self = Self {
        mode: FS_LINK_MODE,
        umask: 0,
    };

    /// `mode` asked for by a process whose mask is `umask`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a mode past [`FS_MODE_MASK`] or a mask past
    /// [`FS_UMASK_BITS`].
    pub fn new(mode: u32, umask: u32) -> Result<Self, Errno> {
        if mode & !FS_MODE_MASK != 0 || umask & !FS_UMASK_BITS != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self { mode, umask })
    }

    /// The permission bits the mask lets through: the setuid, setgid and
    /// sticky bits asked for always, the `rwx` triads less the mask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.mode & !self.umask
    }
}

/// Everything the VFS stores about an inode for the purpose of access
/// control. Mirrors the on-disk inode header describes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    /// Owning user.
    pub owner: UserId,
    /// Owning group.
    pub group: GroupId,
    /// POSIX mode bits.
    pub mode: Mode,
    /// An optional capability the caller must hold to access the inode at
    /// all, on top of the mode/ACL checks.
    pub required_cap: Option<CapabilityId>,
    /// Explicit allow/deny entries, consulted before the mode bits.
    pub acl: Vec<AclEntry>,
    /// The node's POSIX access ACL, which, when present, decides alone below
    /// the capability gate.
    pub posix: Option<PosixAcl>,
}

/// A POSIX access ACL: the owning group's own entry, the named users' and
/// groups' entries, and the mask every named entry and the group class are
/// held under, each an `rwx` triad.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PosixAcl {
    /// The owning group's entry.
    pub group_obj: u16,
    /// The mask.
    pub mask: u16,
    /// The named entries, each kept whole: one granting nothing still decides
    /// its principal's access. Sized as the driver's record is, so every
    /// entry it holds has a slot.
    pub named: [Option<PosixEntry>; MAX_ACL_ENTRIES],
}

/// One named entry of a [`PosixAcl`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PosixEntry {
    /// The user or group it names.
    pub who: AclWho,
    /// What it grants, before the mask.
    pub rwx: u16,
}

impl Metadata {
    /// Construct metadata with the given owner, group, and mode and no
    /// capability gate or ACL entries.
    #[must_use]
    pub fn new(owner: UserId, group: GroupId, mode: Mode) -> Self {
        Self {
            owner,
            group,
            mode,
            required_cap: None,
            acl: Vec::new(),
            posix: None,
        }
    }

    /// Translate a filesystem driver's stored security record
    /// ([`NodeSecurity`]) into the VFS policy [`Metadata`].
    ///
    /// This is the bridge that lets a driver such as `arxfs`, which stores
    /// full per-inode ownership, mode bits, an ACL, and an optional
    /// capability gate, drive the decision instead of a uniform
    /// mount-point template. Each additive driver ACL entry expands into one
    /// *allow* [`AclEntry`] per `rwx` bit it carries; the driver surface
    /// carries no explicit deny. A POSIX record becomes a [`PosixAcl`].
    #[must_use]
    pub fn from_node_security(sec: &NodeSecurity) -> Self {
        let mode = Mode::from_bits(u16::try_from(sec.mode & 0o7777).unwrap_or(0));
        let mut meta = Self::new(UserId(sec.uid), GroupId(sec.gid), mode);
        meta.required_cap = sec.required_cap;
        let who = |subject| match subject {
            SecuritySubject::User(id) => AclWho::User(UserId(id)),
            SecuritySubject::Group(id) => AclWho::Group(GroupId(id)),
        };
        match sec.acl_model {
            AclModel::Additive => {
                for entry in sec.acl() {
                    for access in [Access::Read, Access::Write, Access::Execute] {
                        if u16::from(entry.perms) & access.bit() != 0 {
                            meta.acl.push(AclEntry {
                                who: who(entry.subject),
                                access,
                                allow: true,
                            });
                        }
                    }
                }
            }
            AclModel::Posix { group_obj, mask } => {
                let mut named = [None; MAX_ACL_ENTRIES];
                for (slot, entry) in named.iter_mut().zip(sec.acl()) {
                    *slot = Some(PosixEntry {
                        who: who(entry.subject),
                        rwx: u16::from(entry.perms) & 0b111,
                    });
                }
                meta.posix = Some(PosixAcl {
                    group_obj: u16::from(group_obj) & 0b111,
                    mask: u16::from(mask) & 0b111,
                    named,
                });
            }
        }
        meta
    }

    /// Decide whether `cred` may perform `access` on this inode.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::PermissionDenied`] if the capability gate, an
    /// ACL deny, or the mode bits forbid the access.
    pub fn authorize(&self, cred: &Credentials<'_>, access: Access) -> Result<(), VfsError> {
        if let Some(cap) = self.required_cap {
            if !cred.caps.holds(cap) {
                return Err(VfsError::PermissionDenied);
            }
        }

        if let Some(acl) = self.posix {
            return if self.posix_rwx(&acl, cred) & access.bit() != 0 {
                Ok(())
            } else {
                Err(VfsError::PermissionDenied)
            };
        }

        match self.acl_decision(cred, access) {
            Some(true) => return Ok(()),
            Some(false) => return Err(VfsError::PermissionDenied),
            None => {}
        }

        if self.mode_triad(cred) & access.bit() != 0 {
            Ok(())
        } else {
            Err(VfsError::PermissionDenied)
        }
    }

    /// Decide `access` for a byte operation through a descriptor: as
    /// [`Self::authorize`], except that when `owner_exempt` — the descriptor is
    /// its holder's own or was conferred, never granted — the node's owner
    /// passes the mode and the ACL, which they could restore at will. The
    /// capability gate binds everyone.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::PermissionDenied`] as [`Self::authorize`] does.
    pub fn authorize_held(
        &self,
        cred: &Credentials<'_>,
        access: Access,
        owner_exempt: bool,
    ) -> Result<(), VfsError> {
        if !(owner_exempt && cred.uid == self.owner) {
            return self.authorize(cred, access);
        }
        match self.required_cap {
            Some(cap) if !cred.caps.holds(cap) => Err(VfsError::PermissionDenied),
            _ => Ok(()),
        }
    }

    /// Decide whether `cred` may remove, rename away or replace the entry this
    /// inode occupies in a directory whose metadata is `dir`, beyond the
    /// write permission on `dir` the caller already holds.
    ///
    /// The capability gate guards the name as well as the inode: otherwise a
    /// writer of `dir` could unlink a gated node and plant an ungated one the
    /// gate's own service would walk into. In a sticky directory only the
    /// entry's owner or the directory's owner may touch the name, which is
    /// what a shared drop folder relies on.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::PermissionDenied`] when either rule refuses.
    pub fn admits_name_removal(&self, dir: &Self, cred: &Credentials<'_>) -> Result<(), VfsError> {
        if self.required_cap.is_some_and(|cap| !cred.caps.holds(cap)) {
            return Err(VfsError::PermissionDenied);
        }
        if dir.mode.has(Mode::STICKY) && cred.uid != self.owner && cred.uid != dir.owner {
            return Err(VfsError::PermissionDenied);
        }
        Ok(())
    }

    /// Resolve the ACL for `access`: `Some(false)` if any matching entry
    /// denies, `Some(true)` if one allows (and none deny), `None` if no
    /// entry matches.
    fn acl_decision(&self, cred: &Credentials<'_>, access: Access) -> Option<bool> {
        let mut allowed = None;
        for entry in &self.acl {
            if entry.access != access || !Self::acl_matches(cred, entry.who) {
                continue;
            }
            if !entry.allow {
                return Some(false);
            }
            allowed = Some(true);
        }
        allowed
    }

    /// What a POSIX access ACL grants `cred`, in POSIX.1e's order: the owner
    /// by the owner triad alone; a named user by its entry under the mask; a
    /// caller in the owning group or a named group by the union of those
    /// entries under the mask, which is then final; anyone else by the other
    /// triad.
    fn posix_rwx(&self, acl: &PosixAcl, cred: &Credentials<'_>) -> u16 {
        if cred.uid == self.owner {
            return self.mode.owner_rwx();
        }
        let granted = |names: &dyn Fn(AclWho) -> bool| -> Option<u16> {
            acl.named
                .iter()
                .flatten()
                .filter(|entry| names(entry.who))
                .map(|entry| entry.rwx)
                .reduce(|a, b| a | b)
        };
        if let Some(rwx) = granted(&|who| matches!(who, AclWho::User(uid) if uid == cred.uid)) {
            return rwx & acl.mask;
        }
        let owning = cred.is_in_group(self.group).then_some(acl.group_obj);
        let named = granted(&|who| matches!(who, AclWho::Group(gid) if cred.is_in_group(gid)));
        match owning.into_iter().chain(named).reduce(|a, b| a | b) {
            Some(rwx) => rwx & acl.mask,
            None => self.mode.other_rwx(),
        }
    }

    /// `true` if `who` names a principal the caller acts as.
    fn acl_matches(cred: &Credentials<'_>, who: AclWho) -> bool {
        match who {
            AclWho::User(uid) => cred.uid == uid,
            AclWho::Group(gid) => cred.is_in_group(gid),
        }
    }

    /// Select the owner / group / other mode triad for `cred`.
    fn mode_triad(&self, cred: &Credentials<'_>) -> u16 {
        if cred.uid == self.owner {
            self.mode.owner_rwx()
        } else if cred.is_in_group(self.group) {
            self.mode.group_rwx()
        } else {
            self.mode.other_rwx()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_caps::CapabilitySet;

    fn creds<'a>(
        uid: u32,
        gid: u32,
        sup: &'a [GroupId],
        caps: &'a CapabilitySet,
    ) -> Credentials<'a> {
        Credentials {
            uid: UserId(uid),
            gid: GroupId(gid),
            supplementary_gids: sup,
            caps,
        }
    }

    #[test]
    fn a_create_mode_lets_through_what_the_umask_does_not_withhold() {
        let file = CreateMode::new(tairix_abi::FS_NEW_FILE_MODE, tairix_abi::FS_INITIAL_UMASK)
            .expect("in range");
        assert_eq!(file.bits(), 0o644);
        assert_eq!(
            CreateMode::new(0o4777, 0o077).map(CreateMode::bits),
            Ok(0o4700),
            "a mask never withholds a special bit"
        );
        assert_eq!(CreateMode::LINK.bits(), 0o777);
        assert_eq!(CreateMode::NONE.bits(), 0);
    }

    #[test]
    fn a_create_mode_past_its_bounds_is_refused() {
        assert_eq!(CreateMode::new(FS_MODE_MASK + 1, 0), Err(Errno::OutOfRange));
        assert_eq!(CreateMode::new(0o644, 0o1000), Err(Errno::OutOfRange));
        assert_eq!(
            CreateMode::new(FS_MODE_MASK, 0o777).map(CreateMode::bits),
            Ok(0o7000)
        );
    }

    #[test]
    fn mode_triads_decode_correctly() {
        let m = Mode::from_bits(0o751);
        assert_eq!(m.owner_rwx(), 0b111);
        assert_eq!(m.group_rwx(), 0b101);
        assert_eq!(m.other_rwx(), 0b001);
        assert!(Mode::from_bits(0o4755).has(Mode::SETUID));
    }

    #[test]
    fn owner_uses_owner_triad() {
        let caps = CapabilitySet::empty();
        let meta = Metadata::new(UserId(7), GroupId(3), Mode::from_bits(0o700));
        let cred = creds(7, 3, &[], &caps);
        assert!(meta.authorize(&cred, Access::Read).is_ok());
        assert!(meta.authorize(&cred, Access::Write).is_ok());
        // A non-owner, non-group caller gets the (empty) other triad.
        let other = creds(8, 9, &[], &caps);
        assert_eq!(
            meta.authorize(&other, Access::Read),
            Err(VfsError::PermissionDenied)
        );
    }

    #[test]
    fn group_membership_uses_group_triad() {
        let caps = CapabilitySet::empty();
        let meta = Metadata::new(UserId(7), GroupId(3), Mode::from_bits(0o070));
        let sup = [GroupId(3)];
        let member = creds(8, 1, &sup, &caps);
        assert!(meta.authorize(&member, Access::Read).is_ok());
        let nonmember = creds(8, 1, &[], &caps);
        assert_eq!(
            meta.authorize(&nonmember, Access::Read),
            Err(VfsError::PermissionDenied)
        );
    }

    #[test]
    fn capability_gate_overrides_open_mode_bits() {
        // mode 0644 would allow the read, but the capability gate refuses.
        let mut meta = Metadata::new(UserId(7), GroupId(3), Mode::from_bits(0o644));
        meta.required_cap = Some(CapabilityId::AUDIT_READ);

        let without = CapabilitySet::empty();
        let cred = creds(7, 3, &[], &without);
        assert_eq!(
            meta.authorize(&cred, Access::Read),
            Err(VfsError::PermissionDenied)
        );

        let mut with = CapabilitySet::empty();
        with.insert(CapabilityId::AUDIT_READ);
        let cred = creds(7, 3, &[], &with);
        assert!(meta.authorize(&cred, Access::Read).is_ok());
    }

    /// A held node's owner passes its own mode through a descriptor that is not
    /// a grant, as POSIX's `open(O_CREAT, 0444)` then `write` needs; a grant,
    /// another principal, and the capability gate are judged as ever.
    #[test]
    fn a_held_nodes_owner_passes_its_mode_but_not_its_gate() {
        let none = CapabilitySet::empty();
        let mut meta = Metadata::new(UserId(7), GroupId(3), Mode::from_bits(0o444));
        let owner = creds(7, 3, &[], &none);
        assert!(meta.authorize_held(&owner, Access::Write, true).is_ok());
        assert_eq!(
            meta.authorize_held(&owner, Access::Write, false),
            Err(VfsError::PermissionDenied),
            "a grant has no exemption"
        );
        let other = creds(8, 3, &[], &none);
        assert_eq!(
            meta.authorize_held(&other, Access::Write, true),
            Err(VfsError::PermissionDenied)
        );
        meta.required_cap = Some(CapabilityId::AUDIT_READ);
        assert_eq!(
            meta.authorize_held(&owner, Access::Read, true),
            Err(VfsError::PermissionDenied),
            "the gate binds the owner too"
        );
    }

    #[test]
    fn acl_deny_beats_permissive_mode() {
        let caps = CapabilitySet::empty();
        let mut meta = Metadata::new(UserId(7), GroupId(3), Mode::from_bits(0o777));
        meta.acl.push(AclEntry {
            who: AclWho::User(UserId(8)),
            access: Access::Write,
            allow: false,
        });
        let cred = creds(8, 9, &[], &caps);
        assert!(meta.authorize(&cred, Access::Read).is_ok());
        assert_eq!(
            meta.authorize(&cred, Access::Write),
            Err(VfsError::PermissionDenied)
        );
    }

    #[test]
    fn acl_allow_grants_where_mode_would_deny() {
        let caps = CapabilitySet::empty();
        let mut meta = Metadata::new(UserId(7), GroupId(3), Mode::from_bits(0o600));
        meta.acl.push(AclEntry {
            who: AclWho::Group(GroupId(42)),
            access: Access::Read,
            allow: true,
        });
        let sup = [GroupId(42)];
        let cred = creds(8, 1, &sup, &caps);
        assert!(meta.authorize(&cred, Access::Read).is_ok());
        // No allow entry for Write: falls through to the (denying) mode bits.
        assert_eq!(
            meta.authorize(&cred, Access::Write),
            Err(VfsError::PermissionDenied)
        );
    }

    #[test]
    fn uid_zero_gets_no_special_treatment() {
        let caps = CapabilitySet::empty();
        let meta = Metadata::new(UserId(7), GroupId(3), Mode::from_bits(0o000));
        let root = creds(0, 0, &[], &caps);
        assert_eq!(
            meta.authorize(&root, Access::Read),
            Err(VfsError::PermissionDenied)
        );
    }

    #[test]
    fn from_node_security_carries_owner_mode_cap_and_acl() {
        use tairix_abi::driver::filesystem::{NodeSecurity, SecurityAcl, SecuritySubject};

        let mut sec = NodeSecurity::new(0o600, 7, 3);
        sec.required_cap = Some(CapabilityId::AUDIT_READ);
        // A group ACL grant of read+write (0b110) for gid 42.
        sec.push_acl(SecurityAcl {
            subject: SecuritySubject::Group(42),
            perms: 0b110,
        })
        .expect("acl");

        let meta = Metadata::from_node_security(&sec);
        assert_eq!(meta.owner, UserId(7));
        assert_eq!(meta.group, GroupId(3));
        assert_eq!(meta.mode, Mode::from_bits(0o600));
        assert_eq!(meta.required_cap, Some(CapabilityId::AUDIT_READ));
        // The single rw grant expands into one allow entry per bit.
        assert_eq!(meta.acl.len(), 2);

        // A gid-42 member, without the capability, is still gated out.
        let without = CapabilitySet::empty();
        let denied = creds(8, 1, &[GroupId(42)], &without);
        assert_eq!(
            meta.authorize(&denied, Access::Read),
            Err(VfsError::PermissionDenied)
        );

        // With the capability the ACL grant lets the group member read and
        // write, where the owner-only mode `0o600` would otherwise deny.
        let mut with = CapabilitySet::empty();
        with.insert(CapabilityId::AUDIT_READ);
        let member = creds(8, 1, &[GroupId(42)], &with);
        assert!(meta.authorize(&member, Access::Read).is_ok());
        assert!(meta.authorize(&member, Access::Write).is_ok());
        // No grant for execute: falls through to the denying mode bits.
        assert_eq!(
            meta.authorize(&member, Access::Execute),
            Err(VfsError::PermissionDenied)
        );
    }

    /// A POSIX access ACL as `setfacl` would leave it: owner `rw-`, owning
    /// group `r--`, named user 20 `rwx`, named group 30 `-w-`, mask `r-x`,
    /// other `r--` — so the mode's group triad mirrors the mask.
    fn posix_meta() -> Metadata {
        use tairix_abi::driver::filesystem::{NodeSecurity, SecurityAcl, SecuritySubject};

        let mut sec = NodeSecurity::new(0o654, 7, 3);
        sec.acl_model = AclModel::Posix {
            group_obj: 0b100,
            mask: 0b101,
        };
        for (subject, perms) in [
            (SecuritySubject::User(20), 0b111),
            (SecuritySubject::Group(30), 0b010),
        ] {
            sec.push_acl(SecurityAcl { subject, perms }).expect("acl");
        }
        Metadata::from_node_security(&sec)
    }

    fn rwx(meta: &Metadata, cred: &Credentials<'_>) -> [bool; 3] {
        [Access::Read, Access::Write, Access::Execute].map(|a| meta.authorize(cred, a).is_ok())
    }

    #[test]
    fn a_posix_named_user_is_held_to_the_mask_not_its_own_entry() {
        let caps = CapabilitySet::empty();
        let meta = posix_meta();
        // rwx & mask r-x: write is masked away.
        assert_eq!(rwx(&meta, &creds(20, 99, &[], &caps)), [true, false, true]);
    }

    #[test]
    fn a_posix_owner_is_decided_by_the_owner_triad_alone() {
        let caps = CapabilitySet::empty();
        let meta = posix_meta();
        // In the named group too, the owner still gets exactly `rw-`.
        assert_eq!(
            rwx(&meta, &creds(7, 3, &[GroupId(30)], &caps)),
            [true, true, false]
        );
    }

    #[test]
    fn the_owning_group_gets_its_own_entry_under_the_mask_not_the_mask() {
        let caps = CapabilitySet::empty();
        let meta = posix_meta();
        // group_obj r-- & mask r-x = r--, though the mode's group triad is
        // r-x: the mirrored mask must not be read as the group's grant.
        assert_eq!(rwx(&meta, &creds(8, 3, &[], &caps)), [true, false, false]);
    }

    #[test]
    fn a_matched_group_class_never_falls_through_to_other() {
        let caps = CapabilitySet::empty();
        let meta = posix_meta();
        // Named group 30 has -w-, masked to nothing; other would grant r--
        // but a member of a matching group is refused rather than let
        // through as other.
        assert_eq!(
            rwx(&meta, &creds(8, 1, &[GroupId(30)], &caps)),
            [false, false, false]
        );
        // Anyone matching nothing gets the other triad.
        assert_eq!(rwx(&meta, &creds(8, 1, &[], &caps)), [true, false, false]);
    }

    /// A POSIX record with `u:21:---` and `g:31:---` beside an other triad of
    /// `r--`.
    fn posix_meta_with_empty_entries() -> Metadata {
        use tairix_abi::driver::filesystem::{NodeSecurity, SecurityAcl, SecuritySubject};

        let mut sec = NodeSecurity::new(0o644, 7, 3);
        sec.acl_model = AclModel::Posix {
            group_obj: 0b100,
            mask: 0b111,
        };
        for subject in [SecuritySubject::User(21), SecuritySubject::Group(31)] {
            sec.push_acl(SecurityAcl { subject, perms: 0 })
                .expect("acl");
        }
        Metadata::from_node_security(&sec)
    }

    /// An entry granting nothing is still the entry that names its user:
    /// dropping it let that user through as other.
    #[test]
    fn a_posix_named_user_granted_nothing_is_refused_not_let_through_as_other() {
        let caps = CapabilitySet::empty();
        let meta = posix_meta_with_empty_entries();
        assert_eq!(
            rwx(&meta, &creds(21, 99, &[], &caps)),
            [false, false, false]
        );
        assert_eq!(rwx(&meta, &creds(22, 99, &[], &caps)), [true, false, false]);
    }

    #[test]
    fn a_posix_named_group_granted_nothing_is_refused_not_let_through_as_other() {
        let caps = CapabilitySet::empty();
        let meta = posix_meta_with_empty_entries();
        assert_eq!(
            rwx(&meta, &creds(22, 99, &[GroupId(31)], &caps)),
            [false, false, false]
        );
        // In the owning group as well, its own entry still grants.
        assert_eq!(
            rwx(&meta, &creds(22, 3, &[GroupId(31)], &caps)),
            [true, false, false]
        );
    }

    #[test]
    fn a_posix_record_still_answers_to_the_capability_gate() {
        let caps = CapabilitySet::empty();
        let mut meta = posix_meta();
        meta.required_cap = Some(CapabilityId::AUDIT_READ);
        assert_eq!(rwx(&meta, &creds(7, 3, &[], &caps)), [false, false, false]);
    }
}
