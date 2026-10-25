//! The pure execution model behind the management verbs
//! (`plans/NEW-FILEMANAGER.md` FM7b): how a planned paste is *carried out*.
//!
//! Where [`clipboard`](crate::clipboard) decides *what* moves where (a
//! [`PastePlan`](crate::clipboard::PastePlan) of source→dest items), this
//! module states *how* each item is carried out, touching no filesystem. The
//! `files.app` `Run` binary carries each item out as an operation of the
//! shared engine (`lib/fscopy`) under the user's own identity; composing the
//! model grants nothing and the read-only picker never runs it.
//!
//! # Move vs. copy
//!
//! A move within one volume is a single [`fs_rename`]; a move *across* volumes
//! cannot rename and must copy-then-delete, exactly as `mv` decides from
//! `st_dev`. [`paste_strategy`] states that decision from the clipboard
//! operation and the two items' [`VolumeId`]s, so the count of a paste can tell
//! an item a move renames, carrying nothing, from one it copies.
//!
//! [`fs_rename`]: tairix_abi::SyscallNumber::FS_RENAME

use crate::clipboard::ClipboardOp;

/// The stable system-wide identity of the mounted volume a path lives on — the
/// 16-byte [`volume`](tairix_abi::fs::FileId::volume) field the app reads from
/// `fs_stat`. Two paths can be moved with a single rename only when their
/// volume ids are equal; otherwise the move must copy then delete.
///
/// The engine only *compares* two ids — it never mints one, and the bytes are
/// opaque to it (the filesystem driver assigns them).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VolumeId([u8; 16]);

impl VolumeId {
    /// The volume id read from a node's `fs_stat` result.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The raw 16-byte identifier.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 16] {
        self.0
    }
}

/// How a single planned paste item must be carried out on the filesystem.
///
/// Exhaustive over the two clipboard operations crossed with the same-/
/// different-volume distinction: a `Copy` always streams; a `Cut` renames
/// within a volume and copies-then-deletes across volumes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PasteStrategy {
    /// A same-volume move: one `fs_rename` from the source to the destination.
    /// The source is gone once it succeeds; nothing is streamed.
    Rename,
    /// A cross-volume move: stream-copy the source to the destination, then
    /// remove the source once the copy has fully succeeded (never before, so a
    /// failed copy loses no data).
    CopyThenDelete,
    /// A copy: stream-copy the source to the destination and leave the source
    /// in place.
    Copy,
}

/// Decide how to carry out a paste item, given the clipboard `op` and the
/// [`VolumeId`]s of the source and its destination directory.
///
/// * [`ClipboardOp::Copy`] → [`PasteStrategy::Copy`] (the source stays).
/// * [`ClipboardOp::Cut`] on the same volume → [`PasteStrategy::Rename`].
/// * [`ClipboardOp::Cut`] across volumes → [`PasteStrategy::CopyThenDelete`].
#[must_use]
pub fn paste_strategy(op: ClipboardOp, source: VolumeId, dest: VolumeId) -> PasteStrategy {
    match op {
        ClipboardOp::Copy => PasteStrategy::Copy,
        ClipboardOp::Cut if source == dest => PasteStrategy::Rename,
        ClipboardOp::Cut => PasteStrategy::CopyThenDelete,
    }
}
