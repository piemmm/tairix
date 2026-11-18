//! The filing system interface: the numbers FileSwitch calls a filing
//! system's entries with and the words they answer in, which FileSwitch and
//! every filing system share.

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

/// An object's kind, as `FSEntry_File` 5 gives it.
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
}

/// An object's attributes: the owner's read and write, locked, and the
/// public's read and write.
pub mod attribute {
    /// The owner may read it.
    pub const OWNER_READ: u32 = 1 << 0;
    /// The owner may write it.
    pub const OWNER_WRITE: u32 = 1 << 1;
    /// It may not be deleted, written over or renamed.
    pub const LOCKED: u32 = 1 << 3;
    /// Others may read it.
    pub const PUBLIC_READ: u32 = 1 << 4;
    /// Others may write it.
    pub const PUBLIC_WRITE: u32 = 1 << 5;
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

/// `FSEntry_Func`'s reasons.
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
    /// Read a disc's boot option.
    pub const READ_BOOT_OPTION: u32 = 27;
}

/// `Service_FSRedeclare`: every filing system asked to register again, as
/// FileSwitch does at its start.
pub const REDECLARE: u32 = 0x40;

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
