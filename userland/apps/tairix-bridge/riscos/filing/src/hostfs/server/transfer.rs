//! `Transfer`: each file delivered to the session by a drop or an open, in a
//! directory of its own named by its number, so two deliveries of one name
//! stand apart. The disc's shape is the server's own: nothing on it is made,
//! deleted or renamed, and a delivered file is written only where it was
//! delivered writable, its type then fixed by its name.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_fscopy::tree::{Node, WriteAt};
use tairix_fscopy::{Source, Target};
use tairix_fsmeta::preset::acorn;

use super::{keep_first, object, push_decimal, refusal, typed_stamp, version_of, DATA, LOCKED};
use crate::hostfs::protocol::{Kind, Object, Refusal, Reply, Version, ENTRIES_MOST, NAME_MOST};

/// The name its disc is given, made unique among the session's.
pub(super) const DISC: &str = "Transfer";

/// The longest path beneath its root: a delivery's number and its leaf.
pub(super) const PATH_MOST: usize = DIGITS_MOST + 1 + NAME_MOST;

/// The most digits a delivery's number has: a `usize`'s.
const DIGITS_MOST: usize = 20;

/// A directory on it: read for all, and locked, since nothing on it may be
/// deleted.
const DIRECTORY_ATTRIBUTES: u8 = 0x19;

/// The attribute bits that let a file be written.
const WRITE_ATTRIBUTES: u8 = 0x22;

/// One delivered file: its leaf as RISC OS spells it, its own name, which
/// its type is read from, the file, and whether it may be written.
struct Delivery<F> {
    leaf: Vec<u8>,
    name: String,
    file: F,
    writable: bool,
}

/// What a path on the disc names.
#[derive(Clone, Copy)]
enum Spot {
    Root,
    Directory(usize),
    File(usize),
}

/// A delivered file opened: its number, extent and version, and whether it
/// was opened to write.
pub(super) struct Opened {
    pub(super) number: usize,
    pub(super) extent: u32,
    pub(super) writable: bool,
    pub(super) version: Option<Version>,
}

/// The files delivered.
pub(super) struct Transfer<F> {
    deliveries: Vec<Delivery<F>>,
}

impl<F: Node + Source + Target + WriteAt> Transfer<F> {
    pub(super) const fn new() -> Self {
        Self {
            deliveries: Vec::new(),
        }
    }

    /// `file`, delivered as `name`: its path beneath the root appended to
    /// `path`, which has room for it.
    pub(super) fn deliver(
        &mut self,
        name: &str,
        file: F,
        writable: bool,
        path: &mut Vec<u8>,
    ) -> Result<(), Refusal> {
        let leaf = acorn::leaf_to_riscos(name.as_bytes()).map_err(|_| Refusal::BadName)?;
        let leaf = tairix_util::fallible::copied(leaf.as_bytes()).ok_or(Refusal::Failed)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(name.len())
            .map_err(|_| Refusal::Failed)?;
        owned.push_str(name);
        self.deliveries
            .try_reserve(1)
            .map_err(|_| Refusal::Failed)?;
        push_decimal(path, self.deliveries.len() + 1);
        path.push(b'.');
        path.extend_from_slice(&leaf);
        self.deliveries.push(Delivery {
            leaf,
            name: owned,
            file,
            writable,
        });
        Ok(())
    }

    /// What `path` names: none where its directory holds nothing of that
    /// name.
    fn locate(&self, path: &[u8]) -> Result<Option<Spot>, Refusal> {
        if path.is_empty() {
            return Ok(Some(Spot::Root));
        }
        let mut leaves = path.split(|&byte| byte == b'.');
        let first = leaves.next().unwrap_or_default();
        let Some(number) =
            parse_decimal(first).filter(|&number| (1..=self.deliveries.len()).contains(&number))
        else {
            return nothing_beneath(leaves.next());
        };
        let index = number - 1;
        let Some(leaf) = leaves.next() else {
            return Ok(Some(Spot::Directory(index)));
        };
        if !acorn::names_match(leaf, &self.deliveries[index].leaf) {
            return nothing_beneath(leaves.next());
        }
        match leaves.next() {
            None => Ok(Some(Spot::File(index))),
            Some(_) => Err(Refusal::NotADirectory),
        }
    }

    pub(super) fn read(&self, path: &[u8]) -> Result<Object, Refusal> {
        match self.locate(path)? {
            None => Ok(Object::NOTHING),
            Some(Spot::Root) => directory(
                self.deliveries
                    .len()
                    .checked_sub(1)
                    .map(|index| self.stamp(index)),
            ),
            Some(Spot::Directory(index)) => directory(Some(self.stamp(index))),
            Some(Spot::File(index)) => self.file_object(index),
        }
    }

    pub(super) fn list(
        &self,
        path: &[u8],
        offset: u32,
        count: u32,
    ) -> Result<Reply<'static>, Refusal> {
        let (first, total) = (offset as usize, self.deliveries.len());
        let count = count.min(ENTRIES_MOST) as usize;
        let mut entries = Vec::new();
        let (end, of) = match self.locate(path)?.ok_or(Refusal::NotFound)? {
            Spot::Root => {
                let end = first.saturating_add(count).min(total);
                entries
                    .try_reserve_exact(end.saturating_sub(first))
                    .map_err(|_| Refusal::Failed)?;
                for index in first..end {
                    let mut name = Vec::new();
                    name.try_reserve_exact(DIGITS_MOST)
                        .map_err(|_| Refusal::Failed)?;
                    push_decimal(&mut name, index + 1);
                    entries.push((name, directory(Some(self.stamp(index)))?));
                }
                (end, total)
            }
            Spot::Directory(index) => {
                if first == 0 && count > 0 {
                    let leaf = tairix_util::fallible::copied(&self.deliveries[index].leaf)
                        .ok_or(Refusal::Failed)?;
                    entries.try_reserve_exact(1).map_err(|_| Refusal::Failed)?;
                    entries.push((leaf, self.file_object(index)?));
                }
                (first.saturating_add(entries.len()).min(1), 1)
            }
            Spot::File(_) => return Err(Refusal::NotADirectory),
        };
        let next = if end >= of {
            u32::MAX
        } else {
            u32::try_from(end).unwrap_or(u32::MAX)
        };
        Ok(Reply::Entries { next, entries })
    }

    pub(super) fn open(&self, path: &[u8], update: bool) -> Result<Opened, Refusal> {
        let index = match self.locate(path)? {
            None => return Err(Refusal::NotFound),
            Some(Spot::Root | Spot::Directory(_)) => return Err(Refusal::IsADirectory),
            Some(Spot::File(index)) => index,
        };
        let delivery = &self.deliveries[index];
        if update && !delivery.writable {
            return Err(Refusal::AccessViolation);
        }
        let stat = delivery.file.stat().map_err(refusal)?;
        Ok(Opened {
            number: index,
            extent: u32::try_from(stat.size).map_err(|_| Refusal::TooLarge)?,
            writable: update,
            version: version_of(&stat),
        })
    }

    /// The delivered file `number`.
    pub(super) fn file_mut(&mut self, number: usize) -> Option<&mut F> {
        self.deliveries
            .get_mut(number)
            .map(|delivery| &mut delivery.file)
    }

    /// A save over the delivered file `path` names: emptied to `length` once
    /// `load` and `exec` are known to be keepable, its attributes kept.
    pub(super) fn create(
        &mut self,
        path: &[u8],
        load: u32,
        exec: u32,
        length: u32,
    ) -> Result<Reply<'static>, Refusal> {
        let index = match self.locate(path)? {
            None => return Err(Refusal::WriteProtected),
            Some(Spot::Root | Spot::Directory(_)) => return Err(Refusal::IsADirectory),
            Some(Spot::File(index)) => index,
        };
        let attributes = self.writable_object(index)?.attributes;
        let delivery = &mut self.deliveries[index];
        let stamping = keep_first(&delivery.file, load, exec, attributes, Some(&delivery.name))?;
        delivery.file.set_len(0).map_err(refusal)?;
        delivery.file.set_len(u64::from(length)).map_err(refusal)?;
        stamping.finish(&delivery.file)?;
        Ok(Reply::Done)
    }

    pub(super) fn write_info(
        &mut self,
        path: &[u8],
        load: u32,
        exec: u32,
        attributes: u8,
    ) -> Result<Reply<'static>, Refusal> {
        let index = match self.locate(path)? {
            None => return Err(Refusal::NotFound),
            Some(Spot::Root | Spot::Directory(_)) => return Err(Refusal::WriteProtected),
            Some(Spot::File(index)) => index,
        };
        self.writable_object(index)?;
        let delivery = &self.deliveries[index];
        keep_first(&delivery.file, load, exec, attributes, Some(&delivery.name))?
            .finish(&delivery.file)?;
        Ok(Reply::Done)
    }

    /// A deletion at `path`: nothing where nothing is there, else refused, as
    /// a protected disc refuses it.
    pub(super) fn delete(&self, path: &[u8]) -> Result<Object, Refusal> {
        match self.locate(path)? {
            None => Ok(Object::NOTHING),
            Some(Spot::Root) => Err(Refusal::AccessViolation),
            Some(Spot::Directory(_) | Spot::File(_)) => Err(Refusal::WriteProtected),
        }
    }

    /// What the delivered file `index` is to RISC OS: never writable where it
    /// was not delivered so, and then locked.
    fn file_object(&self, index: usize) -> Result<Object, Refusal> {
        let delivery = &self.deliveries[index];
        let stat = delivery.file.stat().map_err(refusal)?;
        let mut object = object(&delivery.file, &stat, &delivery.name)?;
        if !delivery.writable {
            object.attributes = (object.attributes & !WRITE_ATTRIBUTES) | LOCKED;
        }
        Ok(object)
    }

    /// The delivered file `index`'s object, refused where it may not be
    /// written.
    fn writable_object(&self, index: usize) -> Result<Object, Refusal> {
        if !self.deliveries[index].writable {
            return Err(Refusal::AccessViolation);
        }
        self.file_object(index)
    }

    /// The stamp of delivery `index`'s directory: its file's modification
    /// time, the epoch where the file cannot say.
    fn stamp(&self, index: usize) -> Time64 {
        self.deliveries[index]
            .file
            .stat()
            .map_or(Time64::UNIX_EPOCH, |stat| stat.times.modified)
    }
}

/// What a name that is nothing in its directory answers: nothing, or "not
/// found" where the path goes on beneath it.
const fn nothing_beneath(below: Option<&[u8]>) -> Result<Option<Spot>, Refusal> {
    match below {
        None => Ok(None),
        Some(_) => Err(Refusal::NotFound),
    }
}

/// A directory of the disc, stamped `stamp`, or the epoch for none.
fn directory(stamp: Option<Time64>) -> Result<Object, Refusal> {
    let (load, exec) = typed_stamp(DATA, stamp.unwrap_or(Time64::UNIX_EPOCH))?;
    Ok(Object {
        kind: Kind::Directory,
        load,
        exec,
        length: 0,
        attributes: DIRECTORY_ATTRIBUTES,
    })
}

/// The number `leaf` spells in decimal, without a leading zero.
fn parse_decimal(leaf: &[u8]) -> Option<usize> {
    if leaf.is_empty() || leaf.len() > DIGITS_MOST || (leaf[0] == b'0' && leaf.len() > 1) {
        return None;
    }
    leaf.iter().try_fold(0usize, |number, &byte| {
        let digit = byte.checked_sub(b'0').filter(|&digit| digit < 10)?;
        number.checked_mul(10)?.checked_add(usize::from(digit))
    })
}

#[cfg(test)]
mod tests;
