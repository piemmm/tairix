//! The **ownership-edit** model (`plans/NEW-FILEMANAGER.md` FM8b): the pure,
//! host-tested core of committing a new owning user and/or group to the
//! selected node (the `chown(2)` / `chgrp(2)` shape).
//!
//! Changing a node's owner is modelled here so the one rule that decides
//! whether a requested `(uid, gid)` change is *well-formed* — each field is
//! either left unchanged or set to a real id, never the reserved
//! [`FS_OWNER_UNCHANGED`](tairix_abi::fs::FS_OWNER_UNCHANGED) sentinel as an
//! explicit target — runs in `cargo test` with no kernel. The app asks for the
//! `fs_set_owner` call with the ids [`owner_ids`] spells.
//!
//! Authority is the kernel's, not the engine's. Unlike a rename, mode, or
//! `mkdir` change — which are the user's own permission-checked writes needing
//! no new capability — reassigning the **owner** is a privileged operation: the
//! secured VFS requires `CAP_FS_CHOWN` to change the uid or to set a group the
//! caller is not a member of, and clears the set-*id* bits on any change. The
//! engine models none of that policy; it names *what* to change and lets the
//! kernel decide, so composing it grants nothing and the trusted read-only
//! picker never calls the write path. A change this module accepts may still be
//! refused by the VFS (the caller lacks `CAP_FS_CHOWN`, the group is not
//! theirs, a read-only mount, a lost race), which surfaces as
//! [`OwnerError::Refused`] with the kernel's own [`Errno`].

use tairix_abi::Errno;

/// A requested ownership change: each field is either left as it is
/// ([`None`]) or set to a specific id ([`Some`]).
///
/// Modelling "leave unchanged" as [`None`] keeps the reserved
/// [`FS_OWNER_UNCHANGED`](tairix_abi::fs::FS_OWNER_UNCHANGED) sentinel an
/// *encoding* detail of the syscall boundary, not a value a caller has to
/// know: [`owner_ids`] maps [`None`] onto the sentinel.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct OwnerChange {
    /// The new owning user id, or [`None`] to leave the owner unchanged.
    pub uid: Option<u32>,
    /// The new owning group id, or [`None`] to leave the group unchanged.
    pub gid: Option<u32>,
}

impl OwnerChange {
    /// A change that sets only the owning user.
    #[must_use]
    pub const fn user(uid: u32) -> Self {
        Self {
            uid: Some(uid),
            gid: None,
        }
    }

    /// A change that sets only the owning group.
    #[must_use]
    pub const fn group(gid: u32) -> Self {
        Self {
            uid: None,
            gid: Some(gid),
        }
    }

    /// Whether the change leaves both fields unchanged (a no-op).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.uid.is_none() && self.gid.is_none()
    }
}

/// Why an ownership change was not applied.
///
/// [`Invalid`](Self::Invalid) is decided *before* any syscall, so nothing is
/// changed. [`Refused`](Self::Refused) carries the kernel's own reason for a
/// failure at the VFS call — including the [`Errno::PermissionDenied`] a caller
/// without `CAP_FS_CHOWN` (or setting a group that is not theirs) receives.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OwnerError {
    /// A requested id is the reserved "unchanged" sentinel — not a real,
    /// assignable id. Refused before any syscall rather than misread as a
    /// change.
    Invalid,
    /// The VFS refused the change (the caller lacks `CAP_FS_CHOWN`, the group
    /// is not one of theirs, a read-only mount, a lost race); the node's
    /// ownership is unchanged.
    Refused(Errno),
}

impl OwnerError {
    /// A terse, human-readable reason for the in-UI refusal line (a denied
    /// action is an honest answer, never a silent failure). It names no path
    /// and carries no secret.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Invalid => "That owner or group id is not valid.",
            Self::Refused(_) => "The ownership change was refused.",
        }
    }
}

/// Validate an [`OwnerChange`] as a well-formed request.
///
/// Pure and fail-closed: a field set to the reserved
/// [`FS_OWNER_UNCHANGED`](tairix_abi::fs::FS_OWNER_UNCHANGED) sentinel as an
/// explicit target is refused with [`OwnerError::Invalid`] rather than misread
/// as "leave unchanged", so the change a caller commits is always exactly the
/// one it asked for. It performs no I/O and makes no permission decision —
/// that is the secured VFS's, at commit time.
///
/// # Errors
///
/// [`OwnerError::Invalid`] if `change.uid` or `change.gid` is
/// `Some(FS_OWNER_UNCHANGED)`.
pub const fn validate_owner(change: OwnerChange) -> Result<(), OwnerError> {
    if let Some(uid) = change.uid {
        if uid == tairix_abi::fs::FS_OWNER_UNCHANGED {
            return Err(OwnerError::Invalid);
        }
    }
    if let Some(gid) = change.gid {
        if gid == tairix_abi::fs::FS_OWNER_UNCHANGED {
            return Err(OwnerError::Invalid);
        }
    }
    Ok(())
}

/// The `(uid, gid)` pair `fs_set_owner` is called with for `change`, a field
/// it leaves alone spelt as the reserved
/// [`FS_OWNER_UNCHANGED`](tairix_abi::fs::FS_OWNER_UNCHANGED) sentinel.
///
/// This is the one **privileged** write verb here: the secured VFS requires
/// `CAP_FS_CHOWN` to reassign the owner or to set a group the caller is not a
/// member of, and strips the set-*id* bits on any change. Nothing here makes
/// that decision.
///
/// # Errors
///
/// [`OwnerError::Invalid`] for the sentinel named as a target, decided before
/// any call is made.
pub const fn owner_ids(change: OwnerChange) -> Result<(u32, u32), OwnerError> {
    if let Err(err) = validate_owner(change) {
        return Err(err);
    }
    let unchanged = tairix_abi::fs::FS_OWNER_UNCHANGED;
    let uid = match change.uid {
        Some(uid) => uid,
        None => unchanged,
    };
    let gid = match change.gid {
        Some(gid) => gid,
        None => unchanged,
    };
    Ok((uid, gid))
}
