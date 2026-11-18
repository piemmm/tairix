//! The catalogue: the first two sectors of each side. The first holds the
//! title's first eight bytes, then each entry's name and directory; the
//! second the title's last four, the cycle number, the entries' end, the boot
//! option with the sector count's top bits, the count's low byte, then each
//! entry's addresses. DFS keeps the entries in order of their start sector,
//! the highest first.

use crate::image::{SECTOR_LEN, SECTOR_SHIFT};
use crate::name;

/// The catalogue's two sectors.
pub const CATALOGUE_LEN: usize = 2 * SECTOR_LEN;
/// The sectors the catalogue takes, ahead of every file.
pub const CATALOGUE_SECTORS: u16 = 2;
/// The files a catalogue holds.
pub const MAX_FILES: u8 = 31;
/// Bytes of an entry in each sector.
pub const ENTRY_LEN: u8 = 8;
/// Where the entries start in each sector.
pub const FIRST_ENTRY: u8 = 8;
/// The entries' end of a full catalogue.
pub const FULL_END: u8 = MAX_FILES * ENTRY_LEN;
/// The title's length.
pub const TITLE_LEN: usize = 12;
/// The part of the title the first sector holds.
pub const TITLE_HEAD_LEN: usize = 8;

/// The second sector's cycle number, in BCD.
pub const CYCLE: u8 = 4;
/// The second sector's entries' end: eight bytes a file.
pub const END: u8 = 5;
/// The second sector's option byte.
pub const OPTION: u8 = 6;
/// The second sector's sector count, its low byte.
pub const SECTORS: u8 = 7;
/// Where the option byte holds the boot option.
pub const BOOT_SHIFT: u8 = 4;
/// The option byte's boot option bits.
pub const BOOT_BITS: u8 = 0x30;
/// The option byte's bits holding the sector count's top two.
pub const SECTORS_HIGH_BITS: u8 = 0x03;

/// An entry's name's length.
pub const NAME_LEN: usize = 7;
/// The byte after the name: the directory, its top bit the lock.
pub const DIR: u8 = 7;
/// The directory byte's lock bit.
pub const LOCKED: u8 = 0x80;
/// An entry's load address, its low sixteen bits.
pub const LOAD: u8 = 0;
/// An entry's execution address, its low sixteen bits.
pub const EXEC: u8 = 2;
/// An entry's length, its low sixteen bits.
pub const LENGTH: u8 = 4;
/// The byte holding the high bits of every field.
pub const MIXED: u8 = 6;
/// An entry's start sector, its low byte.
pub const START: u8 = 7;
/// Where the mixed byte holds the start sector's high bits.
pub const START_SHIFT: u8 = 0;
/// Where the mixed byte holds the load address's high bits.
pub const LOAD_SHIFT: u8 = 2;
/// Where the mixed byte holds the length's high bits.
pub const LENGTH_SHIFT: u8 = 4;
/// Where the mixed byte holds the execution address's high bits.
pub const EXEC_SHIFT: u8 = 6;

/// The most sectors a side counts: ten bits.
pub const MAX_SECTORS: u16 = 0x3FF;
/// The eighteen bits an address or a length holds.
pub const MAX_FIELD: u32 = 0x3_FFFF;
/// An address field's top two bits, both set for the I/O processor's.
const IO_BITS: u32 = 0x3_0000;

/// The address an entry's eighteen-bit load or execution `field` is to the
/// OS: both top bits set mark the I/O processor's, the top of the address
/// space.
#[must_use]
pub const fn address(field: u32) -> u32 {
    if field & IO_BITS == IO_BITS {
        field | !MAX_FIELD
    } else {
        field & MAX_FIELD
    }
}

/// The eighteen-bit field that holds the OS address `address`, or `None` for
/// one no field reads back as.
#[must_use]
pub const fn field(address: u32) -> Option<u32> {
    if address | 0xFFFF == u32::MAX {
        Some(address & 0xFFFF | IO_BITS)
    } else if address < IO_BITS {
        Some(address)
    } else {
        None
    }
}

/// One file's entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    /// The name, padded with spaces, its bytes as the catalogue holds them.
    pub name: [u8; NAME_LEN],
    /// The directory, its lock bit clear.
    pub dir: u8,
    /// Whether the file is locked.
    pub locked: bool,
    /// The load address, eighteen bits.
    pub load: u32,
    /// The execution address, eighteen bits.
    pub exec: u32,
    /// The length, eighteen bits.
    pub length: u32,
    /// The first sector, ten bits.
    pub start: u16,
}

impl Entry {
    /// An entry of no file.
    pub const NONE: Self = Self {
        name: [name::PAD; NAME_LEN],
        dir: name::ROOT_DIR,
        locked: false,
        load: 0,
        exec: 0,
        length: 0,
        start: 0,
    };

    /// The sectors the file takes.
    #[must_use]
    pub const fn sectors(&self) -> u32 {
        (self.length >> SECTOR_SHIFT) + (self.length & ((1 << SECTOR_SHIFT) - 1) != 0) as u32
    }

    /// The sector after the file's last.
    #[must_use]
    pub const fn end(&self) -> u32 {
        self.start as u32 + self.sectors()
    }

    /// The entry whose name and directory are `names` and whose addresses
    /// are `info`, each the entry's eight bytes in its sector.
    #[must_use]
    pub fn decode(names: &[u8; ENTRY_LEN as usize], info: &[u8; ENTRY_LEN as usize]) -> Self {
        let mut name = [0; NAME_LEN];
        name.copy_from_slice(&names[..NAME_LEN]);
        let dir_byte = names[usize::from(DIR)];
        let mixed = info[usize::from(MIXED)];
        let high = |shift: u8| u32::from(mixed >> shift & 3) << 16;
        let low = |at: u8| {
            u32::from(u16::from_le_bytes([
                info[usize::from(at)],
                info[usize::from(at) + 1],
            ]))
        };
        Self {
            name,
            dir: dir_byte & !LOCKED,
            locked: dir_byte & LOCKED != 0,
            load: high(LOAD_SHIFT) | low(LOAD),
            exec: high(EXEC_SHIFT) | low(EXEC),
            length: high(LENGTH_SHIFT) | low(LENGTH),
            start: u16::from(mixed >> START_SHIFT & 3) << 8 | u16::from(info[usize::from(START)]),
        }
    }

    /// The entry's name and directory, and its addresses, as its sectors hold
    /// them; each field past its bits loses the rest.
    #[must_use]
    pub fn encode(&self) -> ([u8; ENTRY_LEN as usize], [u8; ENTRY_LEN as usize]) {
        let mut names = [0; ENTRY_LEN as usize];
        names[..NAME_LEN].copy_from_slice(&self.name);
        names[usize::from(DIR)] = self.dir & !LOCKED | if self.locked { LOCKED } else { 0 };
        let top = |value: u32, shift: u8| (value >> 16 & 3).to_le_bytes()[0] << shift;
        let mut info = [0; ENTRY_LEN as usize];
        info[usize::from(LOAD)..][..2].copy_from_slice(&self.load.to_le_bytes()[..2]);
        info[usize::from(EXEC)..][..2].copy_from_slice(&self.exec.to_le_bytes()[..2]);
        info[usize::from(LENGTH)..][..2].copy_from_slice(&self.length.to_le_bytes()[..2]);
        let [start_low, start_high] = self.start.to_le_bytes();
        info[usize::from(MIXED)] = top(self.exec, EXEC_SHIFT)
            | top(self.length, LENGTH_SHIFT)
            | top(self.load, LOAD_SHIFT)
            | (start_high & 3) << START_SHIFT;
        info[usize::from(START)] = start_low;
        (names, info)
    }

    /// Whether a catalogue of `sectors` sectors could hold this entry: a name
    /// and directory of printable bytes, and a file between the catalogue
    /// and the side's end.
    #[must_use]
    pub fn believable(&self, sectors: u16) -> bool {
        self.name.iter().all(|&byte| name::readable(byte))
            && self.name[0] & 0x7F != name::PAD
            && name::readable(self.dir)
            && self.dir != name::PAD
            && self.start >= CATALOGUE_SECTORS
            && self.end() <= u32::from(sectors)
    }
}

/// A side's catalogue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Catalogue {
    /// The title, as the catalogue holds it: padded with spaces or NULs.
    pub title: [u8; TITLE_LEN],
    /// The cycle number, in BCD, which DFS steps at every write of the
    /// catalogue.
    pub cycle: u8,
    /// The boot option *OPT 4 sets, 0 to 3.
    pub boot: u8,
    /// The sectors the side counts, the catalogue's among them.
    pub sectors: u16,
    entries: [Entry; MAX_FILES as usize],
    count: usize,
}

impl Catalogue {
    /// An empty catalogue of a side of `sectors` sectors, its title blank, as
    /// DFS formats one.
    #[must_use]
    pub const fn blank(sectors: u16) -> Self {
        Self {
            title: [name::PAD; TITLE_LEN],
            cycle: 0,
            boot: 0,
            sectors,
            entries: [Entry::NONE; MAX_FILES as usize],
            count: 0,
        }
    }

    /// The catalogue `bytes` holds, or `None` where it is not one a DFS disc
    /// could hold: an entries' end that is not a whole number of entries,
    /// option bits DFS 1.20 leaves clear set, a side of fewer
    /// sectors than its catalogue, a title holding a control code, or an
    /// entry [`Entry::believable`] refuses.
    #[must_use]
    pub fn read(bytes: &[u8; CATALOGUE_LEN]) -> Option<Self> {
        let (first, second) = bytes.split_at(SECTOR_LEN);
        let field = |at: u8| second[usize::from(at)];
        let end = field(END);
        let option = field(OPTION);
        // A whole number of entries in a byte is at most a full catalogue's.
        if end % ENTRY_LEN != 0 || option & !(BOOT_BITS | SECTORS_HIGH_BITS) != 0 {
            return None;
        }
        let sectors = u16::from(option & SECTORS_HIGH_BITS) << 8 | u16::from(field(SECTORS));
        if sectors < CATALOGUE_SECTORS {
            return None;
        }
        let mut title = [0; TITLE_LEN];
        title[..TITLE_HEAD_LEN].copy_from_slice(&first[..TITLE_HEAD_LEN]);
        title[TITLE_HEAD_LEN..].copy_from_slice(&second[..TITLE_LEN - TITLE_HEAD_LEN]);
        if title.iter().any(|&byte| matches!(byte, 0x01..=0x1F | 0x7F)) {
            return None;
        }
        let mut catalogue = Self {
            title,
            cycle: field(CYCLE),
            boot: (option & BOOT_BITS) >> BOOT_SHIFT,
            sectors,
            entries: [Entry::NONE; MAX_FILES as usize],
            count: usize::from(end / ENTRY_LEN),
        };
        for (index, entry) in catalogue.entries[..catalogue.count].iter_mut().enumerate() {
            let at = usize::from(FIRST_ENTRY) + index * usize::from(ENTRY_LEN);
            let names = first
                .get(at..at + usize::from(ENTRY_LEN))?
                .try_into()
                .ok()?;
            let info = second
                .get(at..at + usize::from(ENTRY_LEN))?
                .try_into()
                .ok()?;
            *entry = Entry::decode(names, info);
            if !entry.believable(sectors) {
                return None;
            }
        }
        Some(catalogue)
    }

    /// Write the catalogue into `bytes`, its entries in the order held and
    /// the slots past them zeroed.
    pub fn write(&self, bytes: &mut [u8; CATALOGUE_LEN]) {
        bytes.fill(0);
        let (first, second) = bytes.split_at_mut(SECTOR_LEN);
        first[..TITLE_HEAD_LEN].copy_from_slice(&self.title[..TITLE_HEAD_LEN]);
        second[..TITLE_LEN - TITLE_HEAD_LEN].copy_from_slice(&self.title[TITLE_HEAD_LEN..]);
        second[usize::from(CYCLE)] = self.cycle;
        // At most 31 entries of eight bytes each fit the byte.
        second[usize::from(END)] = (self.count * usize::from(ENTRY_LEN)).to_le_bytes()[0];
        let [sectors_low, sectors_high] = self.sectors.to_le_bytes();
        second[usize::from(OPTION)] =
            (self.boot << BOOT_SHIFT) & BOOT_BITS | sectors_high & SECTORS_HIGH_BITS;
        second[usize::from(SECTORS)] = sectors_low;
        for (index, entry) in self.files().iter().enumerate() {
            let at = usize::from(FIRST_ENTRY) + index * usize::from(ENTRY_LEN);
            let (names, info) = entry.encode();
            first[at..at + usize::from(ENTRY_LEN)].copy_from_slice(&names);
            second[at..at + usize::from(ENTRY_LEN)].copy_from_slice(&info);
        }
    }

    /// The entries, in the catalogue's order.
    #[must_use]
    pub fn files(&self) -> &[Entry] {
        &self.entries[..self.count]
    }

    /// Hold `files` as the entries, in their order; `None`, the catalogue
    /// unchanged, for more than it holds. The slots past them are emptied,
    /// so two catalogues of the same entries compare equal.
    pub fn set_files(&mut self, files: &[Entry]) -> Option<()> {
        let slots = self.entries.get_mut(..files.len())?;
        slots.copy_from_slice(files);
        self.entries[files.len()..].fill(Entry::NONE);
        self.count = files.len();
        Some(())
    }
}

/// The BCD number after `cycle`, 99 wrapping to 0, as DFS steps it.
#[must_use]
pub const fn next_cycle(cycle: u8) -> u8 {
    let (tens, units) = (cycle >> 4, cycle & 0x0F);
    if units < 9 {
        cycle + 1
    } else if tens < 9 {
        (tens + 1) << 4
    } else {
        0
    }
}

#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod tests;
