//! The filing system interface, which FileSwitch and every filing system
//! share: the numbers FileSwitch calls a filing system's entries with, the
//! words they answer in, the entries a directory is listed as, and the
//! information block a filing system is declared to FileSwitch with.

use crate::module::Builtin;
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::{os, X};

/// A filing system's information word: its number in the low byte, and its
/// flags.
pub mod info {
    /// The filing system's number.
    pub const NUMBER: u32 = 0xFF;
    /// Special fields are supported.
    pub const SPECIAL_FIELDS: u32 = 1 << 31;
    /// Its streams are interactive.
    pub const INTERACTIVE: u32 = 1 << 30;
    /// It is called to open a file whether or not the file exists.
    pub const ALWAYS_OPEN: u32 = 1 << 28;
    /// `FSEntry_Args` 6 tells it of a flush.
    pub const FLUSH_NOTIFY: u32 = 1 << 27;
    /// It supports `FSEntry_File` 9.
    pub const FILE_9: u32 = 1 << 26;
    /// It supports the image filing system extensions, `FSEntry_Func` 23
    /// among them.
    pub const IMAGES: u32 = 1 << 23;
    /// A load is `Open`, `GetBytes` and `Close` rather than `File` 255.
    pub const LOAD_BY_OPEN: u32 = 1 << 20;
    /// A save is `Open`, `PutBytes` and `Close` rather than `File` 0.
    pub const SAVE_BY_OPEN: u32 = 1 << 19;
    /// The extra information word is present.
    pub const EXTRA: u32 = 1 << 17;
    /// It is read-only, which FileSwitch leaves the filing system to enforce.
    pub const READ_ONLY: u32 = 1 << 16;
}

/// A filing system's information block, by word: the offsets from its
/// module of its name, its start-up text and its entries, its information
/// word, and the extra word its `EXTRA` flag says is there.
pub mod block {
    /// The name's offset.
    pub const NAME: usize = 0;
    /// The start-up text's offset.
    pub const STARTUP: usize = 1;
    /// `FSEntry_Open`.
    pub const OPEN: usize = 2;
    /// `FSEntry_GetBytes`.
    pub const GET_BYTES: usize = 3;
    /// `FSEntry_PutBytes`.
    pub const PUT_BYTES: usize = 4;
    /// `FSEntry_Args`.
    pub const ARGS: usize = 5;
    /// `FSEntry_Close`.
    pub const CLOSE: usize = 6;
    /// `FSEntry_File`.
    pub const FILE: usize = 7;
    /// The information word.
    pub const INFO: usize = 8;
    /// `FSEntry_Func`.
    pub const FUNC: usize = 9;
    /// `FSEntry_GBPB`.
    pub const GBPB: usize = 10;
    /// The extra information word.
    pub const EXTRA: usize = 11;
    /// The block's words.
    pub const WORDS: usize = 12;
}

/// The entries every filing system has, by their words in its information
/// block, in the order its module's routines give them.
const ENTRIES: [usize; 7] = [
    block::OPEN,
    block::GET_BYTES,
    block::PUT_BYTES,
    block::ARGS,
    block::CLOSE,
    block::FILE,
    block::FUNC,
];

/// A filing system as its module declares it to FileSwitch.
#[derive(Clone, Copy, Debug)]
pub struct Declaration {
    /// The module whose filing system it is.
    pub module: &'static Builtin,
    /// The first of the module's routines that are its entries, its
    /// `FSEntry_Open`; the rest follow in their words' order.
    pub first_entry: usize,
    /// Where its information block is built.
    pub block: u32,
    /// Its name.
    pub name: u32,
    /// Its start-up text.
    pub startup: u32,
    /// Its information word.
    pub info: u32,
    /// What FileSwitch hands its entries in R12.
    pub workspace: u32,
}

/// The file information word `FSEntry_Open` answers with.
pub mod file_info {
    /// Writing is permitted.
    pub const WRITE: u32 = 1 << 31;
    /// Reading is permitted.
    pub const READ: u32 = 1 << 30;
    /// The object is a directory.
    pub const DIRECTORY: u32 = 1 << 29;
    /// The filing system's `FSEntry_GBPB` serves it.
    pub const GBPB: u32 = 1 << 28;
    /// It is interactive.
    pub const INTERACTIVE: u32 = 1 << 27;
}

/// An object's kind, as `OS_File` 5 and `FSEntry_File` 5 give it.
pub mod kind {
    /// Nothing is there.
    pub const NOT_FOUND: u32 = 0;
    /// A file.
    pub const FILE: u32 = 1;
    /// A directory.
    pub const DIRECTORY: u32 = 2;
    /// An image file, a file that is also a directory of an image filing
    /// system.
    pub const IMAGE: u32 = 3;
}

/// A load address that holds a file type and the top byte of a five-byte
/// date stamp, the execution address holding the rest.
pub mod stamp {
    /// The top twelve bits that mark an address as typed.
    pub const TYPED: u32 = 0xFFF0_0000;
    /// The type's twelve bits.
    pub const TYPE: u32 = 0x000F_FF00;
    /// The stamp's top byte.
    pub const HIGH: u32 = 0xFF;

    /// The file type the load address `load` holds, if it is typed.
    #[must_use]
    pub const fn type_of(load: u32) -> Option<u32> {
        if load & TYPED == TYPED {
            Some((load & TYPE) >> 8)
        } else {
            None
        }
    }

    /// The load address of a file of type `kind` whose stamp's top byte is
    /// `high`.
    #[must_use]
    pub const fn typed(kind: u32, high: u32) -> u32 {
        TYPED | ((kind << 8) & TYPE) | (high & HIGH)
    }

    /// The typed load address `load` given the type `kind`, its stamp kept.
    #[must_use]
    pub const fn retyped(load: u32, kind: u32) -> u32 {
        (load & !TYPE) | ((kind << 8) & TYPE)
    }

    /// The five-byte stamp the load address `load` and the execution address
    /// `exec` hold between them.
    #[must_use]
    pub const fn of(load: u32, exec: u32) -> u64 {
        (((load & HIGH) as u64) << 32) | exec as u64
    }
}

/// An object's attributes: the owner's read and write, locked, and the
/// public's read and write.
pub mod attribute {
    use tairix_fsmeta::preset::acorn::fileswitch;

    /// The owner may read it.
    pub const OWNER_READ: u32 = fileswitch::OWNER_READ as u32;
    /// The owner may write it.
    pub const OWNER_WRITE: u32 = fileswitch::OWNER_WRITE as u32;
    /// It may not be deleted, written over or renamed.
    pub const LOCKED: u32 = fileswitch::LOCKED as u32;
    /// Others may read it.
    pub const PUBLIC_READ: u32 = fileswitch::PUBLIC_READ as u32;
    /// Others may write it.
    pub const PUBLIC_WRITE: u32 = fileswitch::PUBLIC_WRITE as u32;
}

/// An object's catalogue information, as `FSEntry_File` 5 gives it: by
/// default, nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Info {
    /// Its kind.
    pub kind: u32,
    /// Its load address.
    pub load: u32,
    /// Its execution address.
    pub exec: u32,
    /// Its length.
    pub length: u32,
    /// Its attributes.
    pub attributes: u32,
}

/// The words of a listed entry's record, and its bytes: an object's load and
/// execution addresses, its length, its attributes and its kind.
const RECORD_WORDS: usize = 5;
const RECORD_BYTES: u32 = 4 * len32(RECORD_WORDS);
/// Where a full record holds, after the record, the object's internal name,
/// its five-byte stamp, and its name.
const INTERNAL_AT: u32 = RECORD_BYTES;
const STAMP_AT: u32 = INTERNAL_AT + 4;
const FULL_NAME_AT: u32 = STAMP_AT + 5;

impl Info {
    /// What R0 and R2 to R5 hold, as `FSEntry_File` 5 answers.
    #[must_use]
    pub const fn answered(regs: &Regs) -> Self {
        Self {
            kind: regs.r[0],
            load: regs.r[2],
            exec: regs.r[3],
            length: regs.r[4],
            attributes: regs.r[5],
        }
    }

    /// Into R0 and R2 to R5, as `FSEntry_File` 5 and `OS_File` 5 answer.
    pub fn answer(&self, regs: &mut Regs) {
        regs.r[0] = self.kind;
        regs.r[2] = self.load;
        regs.r[3] = self.exec;
        regs.r[4] = self.length;
        regs.r[5] = self.attributes;
    }

    /// Its five-byte stamp.
    #[must_use]
    pub const fn stamp(&self) -> u64 {
        stamp::of(self.load, self.exec)
    }

    /// Its record, which a listed entry's name follows.
    #[must_use]
    pub const fn record(&self) -> [u32; RECORD_WORDS] {
        [
            self.load,
            self.exec,
            self.length,
            self.attributes,
            self.kind,
        ]
    }

    /// The information `record` holds.
    const fn of_record([load, exec, length, attributes, kind]: [u32; RECORD_WORDS]) -> Self {
        Self {
            kind,
            load,
            exec,
            length,
            attributes,
        }
    }
}

/// How a directory's entries are laid out, as `FSEntry_Func` 14, 15 and 19
/// write them and `OS_GBPB` 9 to 11 give them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// Zero-terminated names.
    Names,
    /// A record before each name.
    Info,
    /// A full record before each name: the record, the object's internal
    /// name and its stamp.
    FullInfo,
}

impl Layout {
    /// The `FSEntry_Func` reason that lists entries so.
    #[must_use]
    pub const fn reason(self) -> u32 {
        match self {
            Self::Names => func::READ_NAMES,
            Self::Info => func::READ_ENTRIES,
            Self::FullInfo => func::READ_FULL_ENTRIES,
        }
    }

    /// Where an entry's name starts.
    #[must_use]
    pub const fn name_at(self) -> u32 {
        match self {
            Self::Names => 0,
            Self::Info => RECORD_BYTES,
            Self::FullInfo => FULL_NAME_AT,
        }
    }

    /// The room an entry of `name` takes: the name and its terminator after
    /// any record, padded to a word where there is one.
    #[must_use]
    pub fn size(self, name: &[u8]) -> u32 {
        let named = len32(name.len()).saturating_add(self.name_at() + 1);
        match self {
            Self::Names => named,
            Self::Info | Self::FullInfo => named.checked_next_multiple_of(4).unwrap_or(u32::MAX),
        }
    }
}

/// `FSEntry_Open`'s reasons.
pub mod open {
    /// Open an existing object to read.
    pub const READ: u32 = 0;
    /// Create a file to write.
    pub const CREATE: u32 = 1;
    /// Open an existing file to read and write.
    pub const UPDATE: u32 = 2;
}

/// `FSEntry_File`'s reasons.
pub mod file {
    /// Save a block of memory as a file.
    pub const SAVE: u32 = 0;
    /// Write an object's catalogue information.
    pub const WRITE_INFO: u32 = 1;
    /// Write an object's attributes.
    pub const WRITE_ATTRIBUTES: u32 = 4;
    /// Read an object's catalogue information.
    pub const READ_INFO: u32 = 5;
    /// Delete an object.
    pub const DELETE: u32 = 6;
    /// Create a file.
    pub const CREATE: u32 = 7;
    /// Create a directory.
    pub const CREATE_DIRECTORY: u32 = 8;
    /// Read an object's catalogue information but its length.
    pub const READ_INFO_NO_LENGTH: u32 = 9;
    /// Read a file's natural block size.
    pub const BLOCK_SIZE: u32 = 10;
    /// Load a file.
    pub const LOAD: u32 = 255;
}

/// `FSEntry_Args`'s reasons.
pub mod args {
    /// Read the pointer of an unbuffered file.
    pub const READ_POINTER: u32 = 0;
    /// Write the pointer of an unbuffered file.
    pub const WRITE_POINTER: u32 = 1;
    /// Read the extent of an unbuffered file.
    pub const READ_EXTENT: u32 = 2;
    /// Write a file's extent.
    pub const WRITE_EXTENT: u32 = 3;
    /// Read the space allocated to a file.
    pub const READ_ALLOCATED: u32 = 4;
    /// Whether an unbuffered file's pointer is at its end.
    pub const AT_END: u32 = 5;
    /// A file has been flushed.
    pub const FLUSH: u32 = 6;
    /// Ensure a file has space for at least a size.
    pub const ENSURE: u32 = 7;
    /// Write zeros into a file.
    pub const ZEROS: u32 = 8;
    /// Read a file's load and execution addresses.
    pub const READ_STAMP: u32 = 9;
    /// An image file's stamp changed.
    pub const IMAGE_STAMP: u32 = 10;
    /// An I/O control on a file.
    pub const IOCTL: u32 = 11;
}

/// `FSEntry_Func`'s reasons; 25 to 33 and 35 to 37 are a disc's, which
/// FileSwitch passes on from `OS_FSControl` 41, 42, 46 to 52 and 55 to 57.
pub mod func {
    /// Set the current directory.
    pub const SET_DIRECTORY: u32 = 0;
    /// `*Opt`.
    pub const OPT: u32 = 7;
    /// Rename an object.
    pub const RENAME: u32 = 8;
    /// `*Access`.
    pub const ACCESS: u32 = 9;
    /// Boot from the filing system.
    pub const BOOT: u32 = 10;
    /// Read a disc's name and boot option.
    pub const READ_DISC: u32 = 11;
    /// Read a directory's names.
    pub const READ_NAMES: u32 = 14;
    /// Read a directory's names and catalogue information.
    pub const READ_ENTRIES: u32 = 15;
    /// The filing system is being shut down.
    pub const SHUT_DOWN: u32 = 16;
    /// Read a directory's names, catalogue information and more.
    pub const READ_FULL_ENTRIES: u32 = 19;
    /// An image file is opened by its image filing system.
    pub const NEW_IMAGE: u32 = 21;
    /// An image file is closing.
    pub const CLOSING_IMAGE: u32 = 22;
    /// Make a special field and disc canonical.
    pub const CANONICAL: u32 = 23;
    /// A disc's defect list, a word each.
    pub const DEFECT_LIST: u32 = 25;
    /// A defect added to a disc's list.
    pub const ADD_DEFECT: u32 = 26;
    /// Read a disc's boot option.
    pub const READ_BOOT_OPTION: u32 = 27;
    /// Write a disc's boot option.
    pub const WRITE_BOOT_OPTION: u32 = 28;
    /// A disc's map of its used space.
    pub const USED_SPACE_MAP: u32 = 29;
    /// A disc's free space.
    pub const FREE_SPACE: u32 = 30;
    /// Name a disc.
    pub const NAME_DISC: u32 = 31;
    /// An image's disc stamped as changed.
    pub const STAMP_IMAGE: u32 = 32;
    /// The object at an offset into a disc.
    pub const OBJECT_AT_OFFSET: u32 = 33;
    /// A disc's free space, in 64 bits.
    pub const FREE_SPACE_64: u32 = 35;
    /// A disc's defect list, two words each.
    pub const DEFECT_LIST_64: u32 = 36;
    /// A defect at a 64-bit address added to a disc's list.
    pub const ADD_DEFECT_64: u32 = 37;
}

/// `*Opt`'s options: `FSEntry_Func` 7's R1.
pub mod opt {
    /// Every option reset.
    pub const RESET: u32 = 0;
    /// FileSwitch's messages.
    pub const MESSAGES: u32 = 1;
    /// A disc's boot option.
    pub const BOOT: u32 = 4;
}

/// `UpCall_ModifyingFile`, which comes before each change a call makes, and
/// the reasons in R9 beyond `FSEntry_File`'s own numbers.
pub mod modifying {
    /// The upcall's number.
    pub const UPCALL: u32 = 3;
    /// A file opened for update.
    pub const OPEN: u32 = 0x102;
    /// A written file being closed.
    pub const CLOSE: u32 = 0x103;
    /// A file's space ensured.
    pub const ENSURE: u32 = 0x200;
    /// A rename.
    pub const RENAME: u32 = 0x208;
}

impl Os<'_> {
    /// The information the record of the listed entry at `at` holds.
    ///
    /// # Errors
    ///
    /// The abort a read gives.
    pub fn read_listed(&mut self, at: u32) -> Result<Info, Fail> {
        let mut words = [0; RECORD_WORDS];
        self.read_words(at, &mut words)?;
        Ok(Info::of_record(words))
    }

    /// The entry named `name` written at `at` as `layout` lays it out:
    /// `info`'s record before the name, and in a full record the object's
    /// `internal` name and its stamp between them.
    ///
    /// # Errors
    ///
    /// The abort a write gives.
    pub fn write_listed(
        &mut self,
        at: u32,
        layout: Layout,
        info: &Info,
        internal: u32,
        name: &[u8],
    ) -> Result<(), Fail> {
        if layout != Layout::Names {
            self.write_words(at, &info.record())?;
        }
        if layout == Layout::FullInfo {
            self.write_word(at.wrapping_add(INTERNAL_AT), internal)?;
            self.write_five(at.wrapping_add(STAMP_AT), info.stamp())?;
        }
        self.write_terminated(at.wrapping_add(layout.name_at()), name, 0, name.len() + 1)
    }

    /// The filing system `declaration` describes registered with FileSwitch
    /// by `OS_FSControl` 12, from the information block built for it of
    /// offsets from its module; nothing while the module or one of its
    /// entries is not there.
    ///
    /// # Errors
    ///
    /// The abort writing the block gives, or FileSwitch's refusal of it.
    pub fn declare_filing_system(&mut self, declaration: Declaration) -> Result<(), Fail> {
        let Some(base) = self.module_of(declaration.module) else {
            return Ok(());
        };
        let mut words = [0; block::WORDS];
        words[block::NAME] = declaration.name.wrapping_sub(base);
        words[block::STARTUP] = declaration.startup.wrapping_sub(base);
        words[block::INFO] = declaration.info;
        for (offset, word) in ENTRIES.into_iter().enumerate() {
            let routine = declaration.first_entry.saturating_add(offset);
            let Some(at) = self.routine_of(declaration.module, routine) else {
                return Ok(());
            };
            words[word] = at.wrapping_sub(base);
        }
        self.write_words(declaration.block, &words)?;
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[
            os::fs_control::ADD,
            base,
            declaration.block.wrapping_sub(base),
            declaration.workspace,
        ]);
        self.perform(X | os::FS_CONTROL, &mut regs)
    }
}

#[cfg(test)]
mod tests;
