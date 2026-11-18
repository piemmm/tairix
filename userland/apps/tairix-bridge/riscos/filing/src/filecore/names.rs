//! Names as FileSwitch hands them to a filing system of FileCore's: a disc —
//! a drive's number or a disc's name — after a colon, then the path from the
//! disc's root, `$`; and the drive and object they name.

use alloc::vec::Vec;

use tairix_filecore::dir::Object;
use tairix_filecore::identify::trimmed;
use tairix_filecore::volume::Volume;
use tairix_filecore::{Disc, Error};
use tairix_fsmeta::preset::acorn;
use tairix_riscos_kernel::os::{Fail, Os};

use super::disc::{with_volume, ClientDisc};
use super::{code, fail, with_instance, Client, DRIVES};

/// The longest name FileSwitch hands a filing system: a disc's and a path's.
pub(crate) const NAME_MOST: usize = 1024;

/// A name split as FileSwitch hands it.
pub(crate) struct Named {
    /// The disc it names, if it names one.
    pub(crate) disc: Option<Vec<u8>>,
    /// The path below the disc's root: its parts, `.` between them; empty
    /// for the root.
    pub(crate) path: Vec<u8>,
}

impl Named {
    /// Each part of the path, in order.
    pub(crate) fn parts(&self) -> impl Iterator<Item = &[u8]> {
        self.path
            .split(|&b| b == b'.')
            .filter(|part| !part.is_empty())
    }
}

/// The name at `at`, split: none past the longest, not of the form
/// `[:disc.]$[.path]`, or with a part holding a sign FileCore keeps for
/// itself.
pub(crate) fn read(os: &mut Os<'_>, client: &Client, at: u32) -> Result<Named, Fail> {
    let (text, ended) = os.read_until(at, NAME_MOST, |b| b < b' ')?;
    if !ended {
        return Err(fail(os, Some(client), code::BAD_NAME));
    }
    split(&text).ok_or_else(|| fail(os, Some(client), code::BAD_NAME))
}

/// `text` split into its disc and path.
pub(crate) fn split(text: &[u8]) -> Option<Named> {
    let (disc, rest) = match text.strip_prefix(b":") {
        Some(rest) => {
            let end = rest.iter().position(|&b| b == b'.').unwrap_or(rest.len());
            let disc = tairix_util::fallible::copied(&rest[..end])?;
            (Some(disc), rest.get(end + 1..).unwrap_or_default())
        }
        None => (None, text),
    };
    let path = match rest {
        [] if disc.is_some() => &[][..],
        _ => match rest.strip_prefix(b"$")? {
            [] => &[][..],
            beneath => beneath.strip_prefix(b".")?,
        },
    };
    if path
        .split(|&b| b == b'.')
        .any(|part| part.iter().any(|b| b"@^\\&%".contains(b)))
    {
        return None;
    }
    Some(Named {
        disc,
        path: tairix_util::fallible::copied(path)?,
    })
}

/// The drive `disc` names for the instance whose workspace is `workspace`:
/// its current drive for none; for one character, the drive it numbers or
/// letters from `A`, as RISC OS 5 reads one; else the drive whose disc's name
/// it matches, wildcards and all — mounted first where none is yet.
pub(crate) fn drive_of(os: &mut Os<'_>, workspace: u32, disc: Option<&[u8]>) -> Result<u8, Fail> {
    let (client, current) = with_instance(os, workspace, |i| (i.client, i.drive))?;
    let Some(disc) = disc else {
        return Ok(current);
    };
    if let [single] = *disc {
        return match drive_named(single).filter(|&drive| client.has(drive)) {
            Some(drive) => Ok(drive),
            None => Err(fail(os, Some(&client), code::BAD_DRIVE)),
        };
    }
    for drive in (0..DRIVES).filter(|&drive| client.has(drive)) {
        let named = with_volume(os, workspace, drive, |volume, _| disc_name(volume));
        if let Ok((name, len)) = named {
            if acorn::names_match_wild(disc, &name[..len]) {
                return Ok(drive);
            }
        }
    }
    Err(fail(os, Some(&client), code::DISC_NOT_FOUND))
}

/// The drive one character names: its number, or its letter from `A`, as
/// RISC OS 5 reads one; none for any other, or past the last drive.
pub(crate) const fn drive_named(single: u8) -> Option<u8> {
    let drive = match single {
        b'0'..=b'9' => single - b'0',
        b'A'..=b'H' => single - b'A',
        b'a'..=b'h' => single - b'a',
        _ => return None,
    };
    if drive < DRIVES {
        Some(drive)
    } else {
        None
    }
}

/// A volume's disc name, trimmed of its padding.
pub(crate) fn disc_name<D: Disc>(
    volume: &mut Volume<D>,
) -> Result<([u8; tairix_filecore::record::NAME_LEN], usize), Error<D::Error>> {
    volume.disc_name().map(trimmed)
}

/// What a path names on a volume.
#[expect(
    clippy::large_enum_variant,
    reason = "a walk's answer, matched at once: boxing its entry would allocate on every walk"
)]
pub(crate) enum Found {
    /// The root.
    Root,
    /// An entry of a directory.
    Entry {
        /// The directory.
        parent: u32,
        /// Its place there.
        index: u32,
        /// The entry.
        object: Object,
    },
    /// Nothing, in a directory that is there.
    Missing {
        /// The directory.
        parent: u32,
    },
}

/// What `named`'s path names on `volume`, walked from the root: none where
/// no directory holds it, a part before the last being missing or no
/// directory, which reads as nothing there, as RISC OS 5 reads it.
pub(crate) fn walk(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    named: &Named,
) -> Result<Option<Found>, Error<Fail>> {
    let mut parent = volume.root_addr();
    let mut found = Found::Root;
    let mut parts = named.parts().peekable();
    while let Some(part) = parts.next() {
        if let Found::Entry { object, .. } = &found {
            if !object.is_dir() {
                return Ok(None);
            }
            parent = object.indaddr;
        }
        found = match volume.dir_lookup(parent, part)? {
            Some((index, object)) => Found::Entry {
                parent,
                index,
                object,
            },
            None if parts.peek().is_none() => Found::Missing { parent },
            None => return Ok(None),
        };
    }
    Ok(Some(found))
}

/// As [`walk`], FileCore's "not found" where no directory holds what
/// `named` names: what an operation that makes or moves an object answers.
pub(crate) fn reach(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    named: &Named,
) -> Result<Found, Error<Fail>> {
    match walk(volume, named)? {
        Some(found) => Ok(found),
        None => Err(Error::Disc(volume.disc_mut().fail(code::NOT_FOUND))),
    }
}

/// An entry's name, held by value.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EntryName {
    bytes: [u8; tairix_filecore::dir::MAX_NAME_LEN],
    length: usize,
}

impl EntryName {
    /// `object`'s name.
    pub(crate) const fn of(object: &tairix_filecore::dir::Object) -> Self {
        Self {
            bytes: object.name,
            length: object.name_len,
        }
    }

    /// The name.
    pub(crate) fn get(&self) -> &[u8] {
        self.bytes.get(..self.length).unwrap_or_default()
    }
}

/// The last part of `named`'s path: the leaf a new object takes.
pub(crate) fn leaf(named: &Named) -> &[u8] {
    named.parts().last().unwrap_or_default()
}
