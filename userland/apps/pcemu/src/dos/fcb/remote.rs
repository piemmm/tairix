//! The FCB calls on a shared drive, each answered by the share in one step as
//! the handle calls are (`dos::redirect`). A search keeps its place where a
//! local one does, over the same fields of the caller's FCB, its drive marked
//! remote as a DTA's is.

use super::{field, result, EXTENDED};
use crate::dos::dir::HIDDEN_FILES;
use crate::dos::engine::{Far, Fault, Kernel};
use crate::dos::file::access;
use crate::dos::fs::error;
use crate::dos::name::upper;
use crate::dos::redirect::{self, Remote, LABEL_GIVEN, REMOTE_SEARCH};
use crate::folder::{Creation, DosName, Facts, From, SharePath};
use tairix_fat::dir::{attr, ShortEntry, ENTRY};
use tairix_fat::stamp::{Stamp, Stamps};

/// The name field `at` of the FCB `fcb`, in the upper case a share's names
/// are.
fn name_of(k: &mut Kernel<'_, '_>, fcb: Far, at: u16) -> [u8; 11] {
    k.fcb_name(fcb, at).map(upper)
}

/// The FCB's name in drive `drive`'s current directory, which a share is.
fn named(k: &mut Kernel<'_, '_>, fcb: Far, drive: u8, wild: bool) -> Result<Remote, Fault> {
    let name = name_of(k, fcb, field::NAME);
    redirect::in_current(k, drive, name, wild)?.ok_or(Fault::Error(error::INVALID_DRIVE))
}

/// `remote`'s directory with `name` last.
fn beside(remote: &Remote, name: DosName) -> Remote {
    Remote {
        last: Some(name),
        ..*remote
    }
}

/// The first plain file the FCB `fcb`'s name matches in drive `drive`'s
/// current directory, a hidden or system one only where `attributes` asks:
/// what function `0Fh` opens.
///
/// # Errors
///
/// `file not found`; the share's refusal.
pub(super) fn find_file(
    k: &mut Kernel<'_, '_>,
    fcb: Far,
    attributes: u8,
    drive: u8,
) -> Result<Facts, Fault> {
    let remote = named(k, fcb, drive, true)?;
    let pattern = remote.last.ok_or(Fault::Error(error::FILE_NOT_FOUND))?;
    let asked = attributes & HIDDEN_FILES;
    if !pattern.is_wild() {
        let facts = redirect::look_up(k, &remote)?;
        let admitted = !facts.directory && (!facts.hidden || asked & attr::HIDDEN != 0);
        return admitted
            .then_some(facts)
            .ok_or(Fault::Error(error::FILE_NOT_FOUND));
    }
    match redirect::found(
        k,
        remote.slot,
        pattern,
        asked,
        From::Start { dir: remote.dir },
    ) {
        Ok((facts, _, _)) => Ok(facts),
        Err(Fault::Error(error::NO_MORE_FILES)) => Err(Fault::Error(error::FILE_NOT_FOUND)),
        Err(fault) => Err(fault),
    }
}

/// Open the file `name` of drive `drive`'s current directory in SFT entry
/// `entry` — made first, empty, where `create` — for both reading and
/// writing, or for reading alone where the share keeps it so.
///
/// # Errors
///
/// The share's refusal.
pub(super) fn open(
    k: &mut Kernel<'_, '_>,
    drive: u8,
    name: [u8; 11],
    create: bool,
    writable: bool,
    entry: Far,
) -> Result<(), Fault> {
    let remote =
        redirect::in_current(k, drive, name, false)?.ok_or(Fault::Error(error::INVALID_DRIVE))?;
    let (mode, creation) = match (create, writable) {
        (true, _) => (access::BOTH, Creation::Truncate),
        (false, true) => (access::BOTH, Creation::Existing),
        (false, false) => (access::READ, Creation::Existing),
    };
    redirect::open(k, &remote, mode, creation, entry).map(|_| ())
}

/// Functions `11h` and `12h` on a shared drive: the next entry the FCB
/// `fcb`'s search finds, copied to the DTA as an unopened FCB — extended
/// where the search's was — on the drive `drive` it began on.
///
/// # Errors
///
/// The share's refusal; finding nothing more is the result `FFh`.
pub(super) fn search(
    k: &mut Kernel<'_, '_>,
    fcb: Far,
    extended: bool,
    attributes: u8,
    drive: u8,
    next: bool,
) -> Result<u32, Fault> {
    let remote = named(k, fcb, drive, true)?;
    let pattern = remote.last.ok_or(Fault::Error(error::FILE_NOT_FOUND))?;
    let from = if next {
        let (listing, place) = redirect::place_at(k, fcb.add(field::SEARCH_INDEX));
        match place {
            LABEL_GIVEN if attributes == attr::VOLUME_ID => return Ok(result::FAILED),
            LABEL_GIVEN => From::Start {
                dir: SharePath::ROOT,
            },
            _ => From::Next {
                table: listing,
                index: place,
            },
        }
    } else {
        k.set_fcb8(fcb, field::SEARCH_ATTRIBUTES, attributes);
        k.set_fcb8(fcb, field::SEARCH_DRIVE, REMOTE_SEARCH | (drive + 1));
        // A search that finds nothing leaves no place a next one goes on from.
        redirect::put_place(k, fcb.add(field::SEARCH_INDEX), u32::MAX, 0);
        if attributes & attr::VOLUME_ID != 0 && remote.dir.is_root() {
            let label = redirect::label(k, remote.slot)?;
            if pattern.matches(&label.name) {
                redirect::put_place(k, fcb.add(field::SEARCH_INDEX), 0, LABEL_GIVEN);
                report(k, extended, attributes, drive, &label, attr::VOLUME_ID);
                return Ok(result::DONE);
            }
        }
        if attributes == attr::VOLUME_ID {
            return Ok(result::FAILED);
        }
        From::Start { dir: remote.dir }
    };
    match redirect::found(k, remote.slot, pattern, attributes, from) {
        Ok((facts, table, place)) => {
            redirect::put_place(k, fcb.add(field::SEARCH_INDEX), table, place);
            let bits = redirect::attributes(&facts);
            report(k, extended, attributes, drive, &facts, bits);
            Ok(result::DONE)
        }
        Err(Fault::Error(error::NO_MORE_FILES)) => Ok(result::FAILED),
        Err(fault) => Err(fault),
    }
}

/// The drive an FCB's search of a shared drive began on, if its search was
/// one.
pub(super) fn search_drive(k: &mut Kernel<'_, '_>, fcb: Far) -> Option<u8> {
    let byte = k.fcb8(fcb, field::SEARCH_DRIVE);
    (byte & REMOTE_SEARCH != 0).then(|| (byte & !REMOTE_SEARCH).wrapping_sub(1))
}

/// Copy `facts`, with attribute byte `bits`, to the DTA as the unopened FCB
/// a search finds.
fn report(
    k: &mut Kernel<'_, '_>,
    extended: bool,
    attributes: u8,
    drive: u8,
    facts: &Facts,
    bits: u8,
) {
    let stamp = redirect::stamp(facts.modified);
    let entry = ShortEntry {
        name: *facts.name.bytes(),
        attributes: bits,
        case: 0,
        stamps: Stamps {
            written: Stamp {
                date: stamp.date,
                time: stamp.time,
                centis: 0,
            },
            ..Stamps::NONE
        },
        cluster: 0,
        size: facts.size,
    };
    let mut out = [0u8; 7 + 1 + ENTRY];
    let mut at = 0;
    if extended {
        out[0] = EXTENDED;
        out[6] = attributes;
        at = 7;
    }
    out[at] = drive + 1;
    out[at + 1..at + 1 + ENTRY].copy_from_slice(&entry.encode());
    let dta = k.dta();
    k.g.write_bytes(dta.segment, dta.offset, &out[..at + 1 + ENTRY]);
}

/// Function `13h` on a shared drive: every file of the current directory the
/// FCB `fcb`'s name matches deleted, but those the share keeps from being
/// changed; a share's label is the name it was shared under, and stays.
///
/// # Errors
///
/// The share's refusal.
pub(super) fn delete(
    k: &mut Kernel<'_, '_>,
    fcb: Far,
    attributes: u8,
    drive: u8,
) -> Result<u32, Fault> {
    if attributes == attr::VOLUME_ID {
        return Ok(result::FAILED);
    }
    let remote = named(k, fcb, drive, true)?;
    let pattern = remote.last.ok_or(Fault::Error(error::FILE_NOT_FOUND))?;
    let asked = attributes & HIDDEN_FILES;
    let mut from = From::Start { dir: remote.dir };
    let mut deleted = false;
    loop {
        let facts = match redirect::found(k, remote.slot, pattern, asked, from) {
            Ok((facts, table, place)) => {
                from = From::Next {
                    table,
                    index: place,
                };
                facts
            }
            Err(Fault::Error(error::NO_MORE_FILES)) => break,
            Err(fault) => return Err(fault),
        };
        if facts.writable {
            redirect::remove_file(k, &beside(&remote, facts.name))?;
            deleted = true;
        }
    }
    Ok(if deleted {
        result::DONE
    } else {
        result::FAILED
    })
}

/// Function `17h` on a shared drive: every entry of the current directory the
/// FCB `fcb`'s name matches renamed to its second name, a `?` there keeping
/// the old name's byte; a name already taken, or one DOS could not hold,
/// ends the call.
///
/// # Errors
///
/// The share's refusal.
pub(super) fn rename(
    k: &mut Kernel<'_, '_>,
    fcb: Far,
    attributes: u8,
    drive: u8,
) -> Result<u32, Fault> {
    let remote = named(k, fcb, drive, true)?;
    let pattern = remote.last.ok_or(Fault::Error(error::FILE_NOT_FOUND))?;
    let template = name_of(k, fcb, field::NEW_NAME);
    let mut from = From::Start { dir: remote.dir };
    let mut renamed = false;
    loop {
        let facts = match redirect::found(k, remote.slot, pattern, attributes, from) {
            Ok((facts, table, place)) => {
                from = From::Next {
                    table,
                    index: place,
                };
                facts
            }
            Err(Fault::Error(error::NO_MORE_FILES)) => break,
            Err(fault) => return Err(fault),
        };
        let mut new = *facts.name.bytes();
        for (byte, &want) in new.iter_mut().zip(&template) {
            if want != b'?' {
                *byte = want;
            }
        }
        let Some(new) = DosName::new(new, false) else {
            return Ok(result::FAILED);
        };
        match redirect::rename(k, &beside(&remote, facts.name), &beside(&remote, new)) {
            Ok(()) => renamed = true,
            Err(Fault::Error(error::ACCESS_DENIED)) => return Ok(result::FAILED),
            Err(fault) => return Err(fault),
        }
    }
    Ok(if renamed {
        result::DONE
    } else {
        result::FAILED
    })
}

/// Function `23h` on a shared drive: the size, in the FCB `fcb`'s records, of
/// the first file its name matches.
///
/// # Errors
///
/// The share's refusal.
pub(super) fn size(
    k: &mut Kernel<'_, '_>,
    fcb: Far,
    attributes: u8,
    drive: u8,
) -> Result<u32, Fault> {
    match find_file(k, fcb, attributes, drive) {
        Ok(facts) => {
            let records = facts.size.div_ceil(u32::from(k.record_size(fcb)));
            k.set_random_record(fcb, records);
            Ok(result::DONE)
        }
        Err(Fault::Error(error::FILE_NOT_FOUND)) => Ok(result::FAILED),
        Err(fault) => Err(fault),
    }
}
