//! A directory's entries, read as RISC OS 5 reads them through
//! `FSEntry_Func` 15: one at a time for an operation that may change the
//! directory as it goes, in batches for one that only looks.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::fs::{self, Layout};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::call::{self, Info};
use super::image;
use super::name::{self, Resolved};

/// The room one entry is read into, as RISC OS 5 gives it.
const ENTRY_ROOM: u32 = 280;
/// The room, and the count asked for, of a batch.
const BATCH_ROOM: u32 = 284;
/// The most reads of one directory: a containment bound on a filing system
/// that never ends its listing.
pub(crate) const READS_MOST: u32 = 1 << 16;

/// An entry of a directory: its leaf and its information.
pub(crate) type Entry = (Vec<u8>, Info);

/// The entry of `directory` at `offset` read by `FSEntry_Func` 15, and the
/// offset after it.
pub(crate) fn read_entry(
    os: &mut Os<'_>,
    directory: &Resolved,
    offset: u32,
) -> Result<(Option<Entry>, u32), Fail> {
    let mut regs = Regs::default();
    regs.r[3] = 1;
    regs.r[4] = offset;
    regs.r[5] = ENTRY_ROOM;
    let mut entry = None;
    call::func(
        os,
        &directory.calls,
        fs::func::READ_ENTRIES,
        directory.named(),
        ENTRY_ROOM,
        &mut regs,
        |os, regs, at| {
            if regs.r[3] != 0 {
                entry = record(os, at)?;
            }
            Ok(())
        },
    )?;
    let entry = match entry {
        Some((leaf, info)) => Some((leaf, image::kind_of(os, info)?)),
        None => None,
    };
    Ok((entry, regs.r[4]))
}

/// The record `FSEntry_Func` 15 wrote at `at`, none where its name runs on.
fn record(os: &mut Os<'_>, at: u32) -> Result<Option<Entry>, Fail> {
    let info = os.read_listed(at)?;
    let (leaf, ended) = os.read_until(at + Layout::Info.name_at(), name::NAME_MOST, |b| b == 0)?;
    if !ended || leaf.is_empty() {
        return Ok(None);
    }
    Ok(Some((leaf, info)))
}

/// What became of an entry: whether it is gone, so the same offset is read
/// next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fate {
    /// Still there: the next offset is read next.
    Kept,
    /// Gone: the same offset is read again.
    Gone,
    /// Still there, and the rest of the directory left as it is.
    Abandoned,
}

/// Each entry of `directory` handed to `each` as RISC OS 5 walks one: an
/// entry at a time, the same offset read again after one is gone. Whether
/// every entry went.
pub(crate) fn walk(
    os: &mut Os<'_>,
    directory: &Resolved,
    mut each: impl FnMut(&mut Os<'_>, Entry) -> Result<Fate, Fail>,
) -> Result<bool, Fail> {
    let mut offset = 0;
    let mut emptied = true;
    for _ in 0..READS_MOST {
        let (entry, next) = read_entry(os, directory, offset)?;
        let fate = match entry {
            Some(entry) => {
                let fate = each(os, entry)?;
                emptied &= fate == Fate::Gone;
                fate
            }
            None => Fate::Kept,
        };
        if fate == Fate::Abandoned {
            return Ok(false);
        }
        if next == u32::MAX || (fate == Fate::Kept && next == offset) {
            return Ok(emptied);
        }
        if fate == Fate::Kept {
            offset = next;
        }
    }
    Ok(false)
}

/// The entries of `directory` from `offset` that one batch holds, read as
/// RISC OS 5 reads one for an access or a listing, and the offset after
/// them.
pub(crate) fn batch(
    os: &mut Os<'_>,
    directory: &Resolved,
    offset: u32,
) -> Result<(Vec<Entry>, u32), Fail> {
    let mut regs = Regs::default();
    regs.r[3] = BATCH_ROOM;
    regs.r[4] = offset;
    regs.r[5] = BATCH_ROOM;
    let mut found = Vec::new();
    call::func(
        os,
        &directory.calls,
        fs::func::READ_ENTRIES,
        directory.named(),
        BATCH_ROOM,
        &mut regs,
        |os, regs, room| {
            let mut at = room;
            for _ in 0..regs.r[3] {
                if at - room >= BATCH_ROOM {
                    break;
                }
                let Some((leaf, info)) = record(os, at)? else {
                    break;
                };
                at += Layout::Info.size(&leaf);
                if found.try_reserve(1).is_err() {
                    return Err(os.no_memory());
                }
                found.push((leaf, info));
            }
            Ok(())
        },
    )?;
    Ok((found, regs.r[4]))
}
