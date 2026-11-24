//! The Font Manager's 255 handles, as RISC OS 5.30 keeps them. Each found
//! font is a handle for its size, resolution and matrix, and the size's
//! master — one per font and encoding — takes a handle of its own, which
//! `Font_ReadDefn` reads as the font at no size. A size whose uses all went
//! keeps its handle, cached, until a new font needs the room: a font found
//! again comes back on the handle it had, and new fonts take handles never
//! used before reclaiming the least recently used cached ones.

use alloc::vec::Vec;

use crate::identifier::Matrix;
use crate::scan::DataFiles;

/// How many handles there are, numbered from 1.
pub(crate) const HANDLES: usize = LAST as usize;
/// The last handle: a number past it is none.
pub(crate) const LAST: u8 = u8::MAX;

/// Where a font's data comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// One of TAIRiX's own faces under a RISC OS family's name.
    Face,
    /// Files the guest supplies.
    Files(DataFiles),
}

/// A font in one encoding: what every size of it shares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Master {
    /// Its identifier, spelt as the catalogue lists it.
    pub(crate) identifier: Vec<u8>,
    /// Its name in the current territory, where a Messages file names it.
    pub(crate) name: Option<Vec<u8>>,
    /// Its encoding as the Font Manager keeps it, for a font whose encoding
    /// follows the alphabet; none for one with an encoding of its own.
    pub(crate) encoding: Option<Vec<u8>>,
    /// Whether that encoding was asked for and is not the one it has by
    /// default, so that its bitmap files are that encoding's own.
    pub(crate) encoded: bool,
    /// The directory it was found in, its trailing full stop included.
    pub(crate) prefix: Vec<u8>,
    /// The file its encoding is in.
    pub(crate) encoding_file: Vec<u8>,
    /// Where its data comes from.
    pub(crate) source: Source,
    /// What its data is kept under, never another master's.
    pub(crate) serial: u64,
    /// The sizes of it in use.
    uses: u32,
}

/// Where the bitmap files of a font in `prefix` are: its encoding's
/// directory inside it where an encoding other than its own is asked for,
/// the leaf of `encoding_file` naming it, as such files are each one
/// encoding's; else the font's own.
pub(crate) fn bitmap_directory(prefix: &[u8], encoded: bool, encoding_file: &[u8]) -> Vec<u8> {
    let mut directory = prefix.to_vec();
    if encoded {
        let leaf = encoding_file
            .rsplit(|&b| b == b'.')
            .next()
            .unwrap_or_default();
        directory.extend_from_slice(leaf);
        directory.push(b'.');
    }
    directory
}

impl Master {
    /// A master no size uses yet.
    pub(crate) fn new(
        identifier: Vec<u8>,
        name: Option<Vec<u8>>,
        encoding: Option<Vec<u8>>,
        encoded: bool,
        prefix: Vec<u8>,
        encoding_file: Vec<u8>,
        source: Source,
    ) -> Self {
        Self {
            identifier,
            name,
            encoding,
            encoded,
            prefix,
            encoding_file,
            source,
            serial: 0,
            uses: 0,
        }
    }

    fn is(&self, identifier: &[u8], encoding: Option<&[u8]>) -> bool {
        self.identifier.eq_ignore_ascii_case(identifier) && self.encoding.as_deref() == encoding
    }
}

/// What a size is: its point sizes in sixteenths, its resolution in dots an
/// inch and its matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Size {
    pub(crate) points: (i32, i32),
    pub(crate) resolution: (i32, i32),
    pub(crate) matrix: Option<Matrix>,
}

/// A font at a size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sized {
    /// The handle of its master.
    pub(crate) master: u8,
    pub(crate) size: Size,
    /// `Font_FindFont`s less `Font_LoseFont`s.
    pub(crate) uses: u32,
    /// When it was last found, by the table's clock.
    last_found: u64,
}

/// A handle's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Free,
    Sized(Sized),
    Master(Master),
}

/// What a handle holds: its font's master, its size unless it is the
/// master, and its uses.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Held<'a> {
    pub(crate) master: &'a Master,
    pub(crate) size: Option<Size>,
    pub(crate) uses: u32,
}

/// There is no handle for a new font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Full;

/// The handles.
#[derive(Debug)]
pub(crate) struct Handles {
    slots: Vec<Slot>,
    clock: u64,
    /// The serial the next master is given.
    serial: u64,
    /// The serials of masters let go of, whose data is to be let go too.
    freed: Vec<u64>,
}

impl Handles {
    pub(crate) fn new() -> Self {
        Self {
            slots: alloc::vec![Slot::Free; HANDLES],
            clock: 0,
            serial: 1,
            freed: Vec::new(),
        }
    }

    /// `master` with a serial of its own.
    fn numbered(&mut self, mut master: Master) -> Master {
        master.serial = self.serial;
        self.serial += 1;
        master
    }

    /// The serials of the masters let go of since this was last asked.
    pub(crate) fn take_freed(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.freed)
    }

    /// Handle `handle`'s state; none outside 1 to 255.
    pub(crate) fn get(&self, handle: u32) -> Option<&Slot> {
        let index = usize::try_from(handle).ok()?.checked_sub(1)?;
        self.slots.get(index)
    }

    /// What `handle` holds: a size and its master, or a master at no size;
    /// none for a free handle or one out of range.
    pub(crate) fn held(&self, handle: u32) -> Option<Held<'_>> {
        match self.get(handle)? {
            Slot::Sized(sized) => match self.get(u32::from(sized.master))? {
                Slot::Master(master) => Some(Held {
                    master,
                    size: Some(sized.size),
                    uses: sized.uses,
                }),
                _ => None,
            },
            Slot::Master(master) => Some(Held {
                master,
                size: None,
                uses: master.uses,
            }),
            Slot::Free => None,
        }
    }

    /// The handle of `identifier`'s master in `encoding`, if it has one.
    pub(crate) fn master_of(&self, identifier: &[u8], encoding: Option<&[u8]>) -> Option<u8> {
        self.slots
            .iter()
            .zip(1..=LAST)
            .find_map(|(slot, handle)| match slot {
                Slot::Master(master) if master.is(identifier, encoding) => Some(handle),
                _ => None,
            })
    }

    /// A use of the font `master` makes at `size`: the size's handle, found
    /// again or made, and the master's, made where `master` is a font the
    /// table has no master for.
    pub(crate) fn find(&mut self, master: Master, size: Size) -> Result<u8, Full> {
        self.clock += 1;
        let existing = self.master_of(&master.identifier, master.encoding.as_deref());
        if let Some(master_handle) = existing {
            if let Some(handle) = self.size_of(master_handle, &size) {
                self.use_size(handle);
                return Ok(handle);
            }
        }
        let wanted = if existing.is_some() { 1 } else { 2 };
        if !self.make_room(wanted, existing) {
            return Err(Full);
        }
        let mut free = self
            .slots
            .iter()
            .zip(1..=LAST)
            .filter_map(|(slot, handle)| matches!(slot, Slot::Free).then_some(handle));
        let size_handle = free.next().ok_or(Full)?;
        let master_handle = if let Some(handle) = existing {
            handle
        } else {
            let handle = free.next().ok_or(Full)?;
            let master = self.numbered(master);
            self.slots[usize::from(handle) - 1] = Slot::Master(master);
            handle
        };
        self.slots[usize::from(size_handle) - 1] = Slot::Sized(Sized {
            master: master_handle,
            size,
            uses: 0,
            last_found: self.clock,
        });
        self.use_size(size_handle);
        Ok(size_handle)
    }

    /// A use of the font `master` at no size: its master's handle, made
    /// where the table has none, as RISC OS 5.30 answers a negative size.
    pub(crate) fn find_master(&mut self, master: Master) -> Result<u8, Full> {
        self.clock += 1;
        let existing = self.master_of(&master.identifier, master.encoding.as_deref());
        let handle = if let Some(handle) = existing {
            handle
        } else {
            if !self.make_room(1, None) {
                return Err(Full);
            }
            let handle = self
                .slots
                .iter()
                .zip(1..=LAST)
                .find_map(|(slot, handle)| matches!(slot, Slot::Free).then_some(handle))
                .ok_or(Full)?;
            let master = self.numbered(master);
            self.slots[usize::from(handle) - 1] = Slot::Master(master);
            handle
        };
        if let Slot::Master(master) = &mut self.slots[usize::from(handle) - 1] {
            master.uses = master.uses.saturating_add(1);
        }
        Ok(handle)
    }

    /// The handle of the size `size` of the master at `master`.
    fn size_of(&self, master: u8, size: &Size) -> Option<u8> {
        self.slots
            .iter()
            .zip(1..=LAST)
            .find_map(|(slot, handle)| match slot {
                Slot::Sized(sized) if sized.master == master && sized.size == *size => Some(handle),
                _ => None,
            })
    }

    /// One more use of the size at `handle`: its master's too, if it was
    /// cached.
    fn use_size(&mut self, handle: u8) {
        let clock = self.clock;
        let Slot::Sized(sized) = &mut self.slots[usize::from(handle) - 1] else {
            return;
        };
        sized.last_found = clock;
        sized.uses = sized.uses.saturating_add(1);
        if sized.uses == 1 {
            let master = sized.master;
            if let Slot::Master(master) = &mut self.slots[usize::from(master) - 1] {
                master.uses = master.uses.saturating_add(1);
            }
        }
    }

    /// `Font_LoseFont`: one fewer use of what `handle` holds — of a size,
    /// and of its master too where that was the size's last; of a master
    /// itself, as RISC OS 5.30 counts one down. Any other handle is let be.
    pub(crate) fn lose(&mut self, handle: u32) {
        let Some(index) = usize::try_from(handle)
            .ok()
            .and_then(|h| h.checked_sub(1))
            .filter(|&i| i < HANDLES)
        else {
            return;
        };
        let emptied = match &mut self.slots[index] {
            Slot::Sized(sized) if sized.uses > 0 => {
                sized.uses -= 1;
                (sized.uses == 0).then_some(sized.master)
            }
            Slot::Master(master) => {
                master.uses = master.uses.saturating_sub(1);
                None
            }
            _ => None,
        };
        if let Some(master) = emptied {
            if let Slot::Master(master) = &mut self.slots[usize::from(master) - 1] {
                master.uses = master.uses.saturating_sub(1);
            }
        }
    }

    /// Whether `wanted` handles are free, reclaiming the least recently
    /// found cached sizes — and a master once nothing refers to it — where
    /// too few were never used or were freed; nothing changes where even that
    /// leaves too few. `keep` is a master not to reclaim.
    fn make_room(&mut self, wanted: usize, keep: Option<u8>) -> bool {
        let mut reclaimed = [false; HANDLES];
        let mut room = self
            .slots
            .iter()
            .filter(|slot| matches!(slot, Slot::Free))
            .count();
        while room < wanted {
            let Some((index, master)) = self
                .slots
                .iter()
                .enumerate()
                .filter(|&(i, _)| !reclaimed[i])
                .filter_map(|(i, slot)| match slot {
                    Slot::Sized(sized) if sized.uses == 0 => {
                        Some((i, usize::from(sized.master) - 1, sized.last_found))
                    }
                    _ => None,
                })
                .min_by_key(|&(_, _, last)| last)
                .map(|(i, master, _)| (i, master))
            else {
                return false;
            };
            reclaimed[index] = true;
            room += 1;
            let referred = self.slots.iter().enumerate().any(|(i, slot)| {
                !reclaimed[i]
                    && matches!(slot, Slot::Sized(s) if usize::from(s.master) - 1 == master)
            });
            let kept = keep.is_some_and(|k| usize::from(k) - 1 == master);
            if !referred && !kept && !reclaimed[master] {
                reclaimed[master] = true;
                room += 1;
            }
        }
        for (slot, _) in self.slots.iter_mut().zip(reclaimed).filter(|&(_, r)| r) {
            if let Slot::Master(master) = slot {
                self.freed.push(master.serial);
            }
            *slot = Slot::Free;
        }
        true
    }

    /// Every handle's state, by handle.
    pub(crate) fn slots(&self) -> impl Iterator<Item = (u8, &Slot)> {
        (1..=LAST).zip(&self.slots)
    }

    /// Whether any size is in use.
    pub(crate) fn claimed(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, Slot::Sized(sized) if sized.uses > 0))
    }

    /// A table of `masters` and `sizes` at their handles, every size cached;
    /// none where a handle is out of range or taken twice, a font has two
    /// masters or a size two handles, or a size's master is no master.
    pub(crate) fn restored(masters: Vec<(u8, Master)>, sizes: Vec<(u8, u8, Size)>) -> Option<Self> {
        let mut table = Self::new();
        for (handle, master) in masters {
            if table
                .master_of(&master.identifier, master.encoding.as_deref())
                .is_some()
            {
                return None;
            }
            let master = table.numbered(master);
            table.place(handle, Slot::Master(master))?;
        }
        for (handle, master, size) in sizes {
            if !matches!(table.get(u32::from(master)), Some(Slot::Master(_)))
                || table.size_of(master, &size).is_some()
            {
                return None;
            }
            table.place(
                handle,
                Slot::Sized(Sized {
                    master,
                    size,
                    uses: 0,
                    last_found: 0,
                }),
            )?;
        }
        Some(table)
    }

    /// `slot` put at the free handle `handle`; none where it is out of range
    /// or not free.
    fn place(&mut self, handle: u8, slot: Slot) -> Option<()> {
        let at = self.slots.get_mut(usize::from(handle).checked_sub(1)?)?;
        if !matches!(at, Slot::Free) {
            return None;
        }
        *at = slot;
        Some(())
    }
}

#[cfg(test)]
mod tests;
