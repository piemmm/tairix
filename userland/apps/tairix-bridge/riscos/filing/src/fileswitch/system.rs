//! The filing systems registered with FileSwitch: each one's name, number,
//! information words and entry points, copied out of the block `OS_FSControl`
//! 12 was given with the entries made absolute, and the call into one of
//! them.

use alloc::vec::Vec;

use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{len32, Entry, Fail, Os, Regs};

/// The longest filing system name FileSwitch takes: a containment bound on a
/// block the guest wrote, not a capacity.
pub const NAME_MOST: usize = 32;

use tairix_riscos_kernel::filing::fs::block;
pub use tairix_riscos_kernel::filing::fs::info;

/// An entry point, by its word in the information block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Point {
    /// `FSEntry_Open`.
    Open,
    /// `FSEntry_GetBytes`.
    GetBytes,
    /// `FSEntry_PutBytes`.
    PutBytes,
    /// `FSEntry_Args`.
    Args,
    /// `FSEntry_Close`.
    Close,
    /// `FSEntry_File`.
    File,
    /// `FSEntry_Func`.
    Func,
    /// `FSEntry_GBPB`.
    Gbpb,
}

impl Point {
    /// Its word in the information block; its discriminant indexes a
    /// filing system's entries.
    const fn word(self) -> usize {
        match self {
            Self::Open => block::OPEN,
            Self::GetBytes => block::GET_BYTES,
            Self::PutBytes => block::PUT_BYTES,
            Self::Args => block::ARGS,
            Self::Close => block::CLOSE,
            Self::File => block::FILE,
            Self::Func => block::FUNC,
            Self::Gbpb => block::GBPB,
        }
    }

    const ALL: [Self; 8] = [
        Self::Open,
        Self::GetBytes,
        Self::PutBytes,
        Self::Args,
        Self::Close,
        Self::File,
        Self::Func,
        Self::Gbpb,
    ];
}

/// The information block's words.
const BLOCK_WORDS: u32 = len32(block::WORDS);

/// A registered filing system.
#[derive(Clone, Debug)]
pub struct System {
    /// Its number, the information word's low byte.
    pub number: u8,
    /// Its name.
    pub name: Vec<u8>,
    /// Its start-up text's address.
    pub startup: u32,
    /// Its information word.
    pub info: u32,
    /// Its extra information word, zero where it gives none.
    pub extra: u32,
    /// Each entry's address, by [`Point`]; none for an optional one it has
    /// not got.
    entries: [Option<u32>; 8],
    /// R12 for its entries.
    pub workspace: u32,
    /// The profile its entries run in.
    pub bits: Bits,
    /// Its module's address, as it registered.
    pub base: u32,
    /// FileSwitch's copy of its information block in the RMA, its entries
    /// absolute, which `OS_FSControl` 13 hands out.
    pub block: u32,
    /// The secondary module `OS_FSControl` 17 added for it: the address its
    /// name was given at, which `OS_FSControl` 30 hands back as it was
    /// given, and its private word.
    pub secondary: Option<(u32, u32)>,
    /// `*Opt 1`'s level for it: what a load, save or creation tells.
    pub messages: u8,
}

/// Where a call into a filing system goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Target {
    address: u32,
    workspace: u32,
    bits: Bits,
}

/// What a call into a filing system needs of it, copied out of FileSwitch's
/// state so the call can be made while that state may change under it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Calls {
    /// Its number.
    pub number: u8,
    /// Its information word.
    pub info: u32,
    targets: [Option<Target>; 8],
}

impl Calls {
    /// Whether its information word has `flag`.
    #[must_use]
    pub const fn has(&self, flag: u32) -> bool {
        self.info & flag != 0
    }

    /// Where a call to `point` goes, if it has the entry.
    #[must_use]
    pub const fn target(&self, point: Point) -> Option<Target> {
        self.targets[point as usize]
    }
}

/// Why a block could not be registered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Its name is empty, too long, or holds more than letters, digits and
    /// `_`.
    BadName,
    /// Its number is none.
    NoNumber,
}

/// Whether `byte` may be in a filing system's name: what a prefix is read
/// up to.
pub(crate) const fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A block's `words` with each offset from `base` made an address, but for
/// the words `kept` picks out and those of nought, which name nothing.
fn made_absolute<const N: usize>(
    base: u32,
    mut words: [u32; N],
    kept: impl Fn(usize) -> bool,
) -> [u32; N] {
    for (i, word) in words.iter_mut().enumerate() {
        if !kept(i) && *word != 0 {
            *word = base.wrapping_add(*word);
        }
    }
    words
}

impl System {
    /// The filing system whose information block is at `offset` from its
    /// module at `base`, its entries given `workspace` in R12 and run in
    /// `bits`.
    ///
    /// # Errors
    ///
    /// A fault reading the block, or what [`Refused`] says of it.
    pub fn read(
        os: &mut Os<'_>,
        base: u32,
        offset: u32,
        workspace: u32,
        bits: Bits,
    ) -> Result<Result<Self, Refused>, Fail> {
        let block = base.wrapping_add(offset);
        let mut words = [0; BLOCK_WORDS as usize];
        os.read_words(block, &mut words[..block::EXTRA])?;
        let info = words[block::INFO];
        if info & info::EXTRA != 0 {
            words[block::EXTRA] = os.read_word(block.wrapping_add(4 * len32(block::EXTRA)))?;
        }
        let name_at = base.wrapping_add(words[block::NAME]);
        let (name, ended) = os.read_until(name_at, NAME_MOST, |b| b == 0)?;
        if !ended || name.is_empty() || !name.iter().all(|&b| name_byte(b)) {
            return Ok(Err(Refused::BadName));
        }
        let Ok(number) = u8::try_from(info & info::NUMBER) else {
            return Ok(Err(Refused::NoNumber));
        };
        if number == 0 {
            return Ok(Err(Refused::NoNumber));
        }
        let mut entries = [None; 8];
        for (slot, point) in entries.iter_mut().zip(Point::ALL) {
            let offset = words[point.word()];
            *slot = (offset != 0).then(|| base.wrapping_add(offset));
        }
        let copy = os.rma_claim(4 * BLOCK_WORDS)?;
        let absolute = made_absolute(base, words, |i| matches!(i, block::INFO | block::EXTRA));
        os.write_words(copy, &absolute)?;
        Ok(Ok(Self {
            number,
            name,
            startup: base.wrapping_add(words[block::STARTUP]),
            info,
            extra: words[block::EXTRA],
            entries,
            workspace,
            bits,
            base,
            block: copy,
            secondary: None,
            messages: 0,
        }))
    }

    /// Whether its information word has `flag`.
    #[must_use]
    pub const fn has(&self, flag: u32) -> bool {
        self.info & flag != 0
    }

    /// Where a call to `point` goes, if it has the entry.
    #[must_use]
    pub fn target(&self, point: Point) -> Option<Target> {
        let address = self.entries[point as usize]?;
        Some(Target {
            address,
            workspace: self.workspace,
            bits: self.bits,
        })
    }

    /// What a call into it needs.
    #[must_use]
    pub fn calls(&self) -> Calls {
        let mut targets = [None; 8];
        for (slot, point) in targets.iter_mut().zip(Point::ALL) {
            *slot = self.target(point);
        }
        Calls {
            number: self.number,
            info: self.info,
            targets,
        }
    }

    /// Whether `name` names it: its name, ignoring case.
    #[must_use]
    pub fn named(&self, name: &[u8]) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// An image filing system registered with FileSwitch: the file type whose
/// files it reads as directories, its information word, and its entries,
/// copied out of its block with `OS_FSControl` 35.
#[derive(Clone, Debug)]
pub struct ImageSystem {
    /// The file type.
    pub kind: u32,
    /// Its information word.
    pub info: u32,
    targets: [Option<Target>; 8],
    /// FileSwitch's copy of its block in the RMA.
    pub block: u32,
}

/// The image block's words: the information word, the file type, then the
/// entries, by [`Point`] — an image filing system has no `GBPB`.
const IMAGE_WORDS: u32 = 9;
const IMAGE_ENTRIES: [Point; 7] = [
    Point::Open,
    Point::GetBytes,
    Point::PutBytes,
    Point::Args,
    Point::Close,
    Point::File,
    Point::Func,
];

impl ImageSystem {
    /// The image filing system whose block is at `offset` from its module at
    /// `base`, its entries given `workspace` in R12 and run in `bits`.
    ///
    /// # Errors
    ///
    /// A fault reading the block, or the RMA refusing its copy.
    pub fn read(
        os: &mut Os<'_>,
        base: u32,
        offset: u32,
        workspace: u32,
        bits: Bits,
    ) -> Result<Self, Fail> {
        let block = base.wrapping_add(offset);
        let mut words = [0; IMAGE_WORDS as usize];
        os.read_words(block, &mut words)?;
        let mut targets = [None; 8];
        for (&point, &offset) in IMAGE_ENTRIES.iter().zip(&words[2..]) {
            targets[point as usize] = (offset != 0).then(|| Target {
                address: base.wrapping_add(offset),
                workspace,
                bits,
            });
        }
        let copy = os.rma_claim(4 * IMAGE_WORDS)?;
        os.write_words(copy, &made_absolute(base, words, |i| i < 2))?;
        Ok(Self {
            kind: words[1] & 0xFFF,
            info: words[0],
            targets,
            block: copy,
        })
    }

    /// What a call into it needs, for an image on the filing system
    /// `number`: an image's canonical name is its host's.
    #[must_use]
    pub const fn calls(&self, number: u8) -> Calls {
        Calls {
            number,
            info: self.info,
            targets: self.targets,
        }
    }
}

/// Calls `target` with `regs`, R12 its workspace, in Supervisor mode.
///
/// # Errors
///
/// The error the filing system gives, or the unwind leaving it.
pub fn enter(os: &mut Os<'_>, target: Target, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[12] = target.workspace;
    os.call_routine(target.address, target.bits, Entry::Supervisor, regs)
}

#[cfg(test)]
mod tests {
    use super::made_absolute;

    #[test]
    fn offsets_become_addresses_but_kept_words_and_noughts_stay() {
        let made = made_absolute(0x20, [0x40, 0x10, 0, 0xFFFF_FFF0], |i| i == 1);
        assert_eq!(made, [0x60, 0x10, 0, 0x10]);
    }
}
