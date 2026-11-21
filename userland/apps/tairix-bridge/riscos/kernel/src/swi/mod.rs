//! SWIs (`plans/RISCOS-EMULATOR.md` §4.1): a number's fields, the kernel's
//! table, and how a SWI hands its results and errors back.
//!
//! The processor stops at a `SWI` rather than taking the exception, and the
//! kernel performs it in place: the caller's registers are read, the SWI
//! run, and its outputs written back with the caller's flags but those it
//! sets. An error sets V with R0 pointing at its block, or, from the form
//! without X, goes to the environment's error handler.

pub mod names;

use tairix_riscos_arm::cpu::Exception;
use tairix_riscos_arm::psr;
use tairix_riscos_arm::reg::Reg;

use crate::os::{len32, Fail, Os, Regs, Unwind};

/// The X bit: the SWI returns its error rather than raising it.
pub const X: u32 = 1 << 17;
/// `OS_WriteI`'s first number: `&100` to `&1FF` write their low byte.
pub const WRITE_I: u32 = 0x100;
/// The SWIs in a module's chunk.
pub const CHUNK: u32 = 64;
/// How far a chain of `OS_CallASWI`s is followed.
const CALL_A_SWI_HOPS: u32 = 16;
/// A number no SWI has, for a chain followed too far.
const UNKNOWN: u32 = 0x80;

/// The SWI chunks of the modules the kernel builds in.
pub mod chunk {
    /// `MessageTrans`.
    pub const MESSAGE_TRANS: u32 = 0x4_1500;
    /// `ResourceFS`.
    pub const RESOURCE_FS: u32 = 0x4_1B40;
    /// `Territory`.
    pub const TERRITORY: u32 = 0x4_3040;
    /// `SoundDMA`.
    pub const SOUND_DMA: u32 = 0x4_0140;
    /// `SoundChannels`.
    pub const SOUND_CHANNELS: u32 = 0x4_0180;
    /// `SoundScheduler`.
    pub const SOUND_SCHEDULER: u32 = 0x4_01C0;
}

/// The sound modules' SWIs.
pub mod sound {
    use super::chunk::{SOUND_CHANNELS, SOUND_DMA, SOUND_SCHEDULER};

    /// `Sound_Configure`.
    pub const CONFIGURE: u32 = SOUND_DMA;
    /// `Sound_Enable`.
    pub const ENABLE: u32 = SOUND_DMA + 1;
    /// `Sound_Stereo`.
    pub const STEREO: u32 = SOUND_DMA + 2;
    /// `Sound_ControlPacked`.
    pub const CONTROL_PACKED: u32 = SOUND_CHANNELS + 6;
    /// `Sound_Control`.
    pub const CONTROL: u32 = SOUND_CHANNELS + 9;
    /// `Sound_AttachNamedVoice`.
    pub const ATTACH_NAMED_VOICE: u32 = SOUND_CHANNELS + 10;
    /// `Sound_QInit`.
    pub const Q_INIT: u32 = SOUND_SCHEDULER;
    /// `Sound_QTempo`.
    pub const Q_TEMPO: u32 = SOUND_SCHEDULER + 5;
    /// `Sound_QBeat`.
    pub const Q_BEAT: u32 = SOUND_SCHEDULER + 6;
}

/// ResourceFS's SWIs.
pub mod resource_fs {
    /// `ResourceFS_RegisterFiles`.
    pub const REGISTER_FILES: u32 = super::chunk::RESOURCE_FS;
    /// `ResourceFS_DeregisterFiles`.
    pub const DEREGISTER_FILES: u32 = super::chunk::RESOURCE_FS + 1;
}

/// The OS SWIs' numbers, and the reasons and flags the kernel and its
/// modules call them with.
pub mod os {
    /// `OS_WriteC`.
    pub const WRITE_C: u32 = 0x00;
    /// `OS_WriteS`.
    pub const WRITE_S: u32 = 0x01;
    /// `OS_Write0`.
    pub const WRITE_0: u32 = 0x02;
    /// `OS_NewLine`.
    pub const NEW_LINE: u32 = 0x03;
    /// `OS_ReadC`.
    pub const READ_C: u32 = 0x04;
    /// `OS_CLI`.
    pub const CLI: u32 = 0x05;
    /// `OS_Byte`.
    pub const BYTE: u32 = 0x06;
    /// `OS_Word`.
    pub const WORD: u32 = 0x07;
    /// `OS_File`.
    pub const FILE: u32 = 0x08;
    /// `OS_Args`.
    pub const ARGS: u32 = 0x09;
    /// `OS_BGet`.
    pub const BGET: u32 = 0x0A;
    /// `OS_BPut`.
    pub const BPUT: u32 = 0x0B;
    /// `OS_GBPB`.
    pub const GBPB: u32 = 0x0C;
    /// `OS_Find`.
    pub const FIND: u32 = 0x0D;
    /// `OS_ReadLine`.
    pub const READ_LINE: u32 = 0x0E;
    /// `OS_Control`.
    pub const CONTROL: u32 = 0x0F;
    /// `OS_GetEnv`.
    pub const GET_ENV: u32 = 0x10;
    /// `OS_Exit`.
    pub const EXIT: u32 = 0x11;
    /// `OS_SetEnv`.
    pub const SET_ENV: u32 = 0x12;
    /// `OS_IntOn`.
    pub const INT_ON: u32 = 0x13;
    /// `OS_IntOff`.
    pub const INT_OFF: u32 = 0x14;
    /// `OS_CallBack`.
    pub const CALL_BACK: u32 = 0x15;
    /// `OS_EnterOS`.
    pub const ENTER_OS: u32 = 0x16;
    /// `OS_BreakPt`.
    pub const BREAK_PT: u32 = 0x17;
    /// `OS_BreakCtrl`.
    pub const BREAK_CTRL: u32 = 0x18;
    /// `OS_UnusedSWI`.
    pub const UNUSED_SWI: u32 = 0x19;
    /// `OS_UpdateMEMC`.
    pub const UPDATE_MEMC: u32 = 0x1A;
    /// `OS_SetCallBack`.
    pub const SET_CALLBACK: u32 = 0x1B;
    /// `OS_Mouse`.
    pub const MOUSE: u32 = 0x1C;
    /// `OS_Heap`.
    pub const HEAP: u32 = 0x1D;
    /// `OS_Module`.
    pub const MODULE: u32 = 0x1E;
    /// `OS_Claim`.
    pub const CLAIM: u32 = 0x1F;
    /// `OS_Release`.
    pub const RELEASE: u32 = 0x20;
    /// `OS_ReadUnsigned`.
    pub const READ_UNSIGNED: u32 = 0x21;
    /// `OS_GenerateEvent`.
    pub const GENERATE_EVENT: u32 = 0x22;
    /// `OS_ReadVarVal`.
    pub const READ_VAR_VAL: u32 = 0x23;
    /// `OS_SetVarVal`.
    pub const SET_VAR_VAL: u32 = 0x24;
    /// `OS_GSInit`.
    pub const GS_INIT: u32 = 0x25;
    /// `OS_GSRead`.
    pub const GS_READ: u32 = 0x26;
    /// `OS_GSTrans`.
    pub const GS_TRANS: u32 = 0x27;
    /// `OS_BinaryToDecimal`.
    pub const BINARY_TO_DECIMAL: u32 = 0x28;
    /// `OS_FSControl`.
    pub const FS_CONTROL: u32 = 0x29;
    /// `OS_ChangeDynamicArea`.
    pub const CHANGE_DYNAMIC_AREA: u32 = 0x2A;
    /// `OS_GenerateError`.
    pub const GENERATE_ERROR: u32 = 0x2B;
    /// `OS_ReadEscapeState`.
    pub const READ_ESCAPE_STATE: u32 = 0x2C;
    /// `OS_EvaluateExpression`.
    pub const EVALUATE_EXPRESSION: u32 = 0x2D;
    /// `OS_SpriteOp`.
    pub const SPRITE_OP: u32 = 0x2E;
    /// `OS_ReadPalette`.
    pub const READ_PALETTE: u32 = 0x2F;
    /// `OS_ServiceCall`.
    pub const SERVICE_CALL: u32 = 0x30;
    /// `OS_ReadVduVariables`.
    pub const READ_VDU_VARIABLES: u32 = 0x31;
    /// `OS_ReadPoint`.
    pub const READ_POINT: u32 = 0x32;
    /// `OS_UpCall`.
    pub const UPCALL: u32 = 0x33;
    /// `OS_CallAVector`.
    pub const CALL_A_VECTOR: u32 = 0x34;
    /// `OS_ReadModeVariable`.
    pub const READ_MODE_VARIABLE: u32 = 0x35;
    /// `OS_RemoveCursors`.
    pub const REMOVE_CURSORS: u32 = 0x36;
    /// `OS_RestoreCursors`.
    pub const RESTORE_CURSORS: u32 = 0x37;
    /// `OS_SWINumberToString`.
    pub const SWI_NUMBER_TO_STRING: u32 = 0x38;
    /// `OS_SWINumberFromString`.
    pub const SWI_NUMBER_FROM_STRING: u32 = 0x39;
    /// `OS_ValidateAddress`.
    pub const VALIDATE_ADDRESS: u32 = 0x3A;
    /// `OS_CallAfter`.
    pub const CALL_AFTER: u32 = 0x3B;
    /// `OS_CallEvery`.
    pub const CALL_EVERY: u32 = 0x3C;
    /// `OS_RemoveTickerEvent`.
    pub const REMOVE_TICKER_EVENT: u32 = 0x3D;
    /// `OS_InstallKeyHandler`.
    pub const INSTALL_KEY_HANDLER: u32 = 0x3E;
    /// `OS_CheckModeValid`.
    pub const CHECK_MODE_VALID: u32 = 0x3F;
    /// `OS_ChangeEnvironment`.
    pub const CHANGE_ENVIRONMENT: u32 = 0x40;
    /// `OS_ClaimScreenMemory`.
    pub const CLAIM_SCREEN_MEMORY: u32 = 0x41;
    /// `OS_ReadMonotonicTime`.
    pub const READ_MONOTONIC_TIME: u32 = 0x42;
    /// `OS_SubstituteArgs`.
    pub const SUBSTITUTE_ARGS: u32 = 0x43;
    /// `OS_PrettyPrint`.
    pub const PRETTY_PRINT: u32 = 0x44;
    /// `OS_Plot`.
    pub const PLOT: u32 = 0x45;
    /// `OS_WriteN`.
    pub const WRITE_N: u32 = 0x46;
    /// `OS_AddToVector`.
    pub const ADD_TO_VECTOR: u32 = 0x47;
    /// `OS_WriteEnv`.
    pub const WRITE_ENV: u32 = 0x48;
    /// `OS_ReadArgs`.
    pub const READ_ARGS: u32 = 0x49;
    /// `OS_ReadRAMFsLimits`.
    pub const READ_RAM_FS_LIMITS: u32 = 0x4A;
    /// `OS_ClaimDeviceVector`.
    pub const CLAIM_DEVICE_VECTOR: u32 = 0x4B;
    /// `OS_ReleaseDeviceVector`.
    pub const RELEASE_DEVICE_VECTOR: u32 = 0x4C;
    /// `OS_DelinkApplication`.
    pub const DELINK_APPLICATION: u32 = 0x4D;
    /// `OS_RelinkApplication`.
    pub const RELINK_APPLICATION: u32 = 0x4E;
    /// `OS_HeapSort`.
    pub const HEAP_SORT: u32 = 0x4F;
    /// `OS_ExitAndDie`.
    pub const EXIT_AND_DIE: u32 = 0x50;
    /// `OS_ReadMemMapInfo`.
    pub const READ_MEM_MAP_INFO: u32 = 0x51;
    /// `OS_ReadMemMapEntries`.
    pub const READ_MEM_MAP_ENTRIES: u32 = 0x52;
    /// `OS_SetMemMapEntries`.
    pub const SET_MEM_MAP_ENTRIES: u32 = 0x53;
    /// `OS_AddCallBack`.
    pub const ADD_CALLBACK: u32 = 0x54;
    /// `OS_ReadDefaultHandler`.
    pub const READ_DEFAULT_HANDLER: u32 = 0x55;
    /// `OS_SetECFOrigin`.
    pub const SET_ECF_ORIGIN: u32 = 0x56;
    /// `OS_SerialOp`.
    pub const SERIAL_OP: u32 = 0x57;
    /// `OS_ReadSysInfo`.
    pub const READ_SYS_INFO: u32 = 0x58;
    /// `OS_Confirm`.
    pub const CONFIRM: u32 = 0x59;
    /// `OS_ChangedBox`.
    pub const CHANGED_BOX: u32 = 0x5A;
    /// `OS_CRC`.
    pub const CRC: u32 = 0x5B;
    /// `OS_ReadDynamicArea`.
    pub const READ_DYNAMIC_AREA: u32 = 0x5C;
    /// `OS_PrintChar`.
    pub const PRINT_CHAR: u32 = 0x5D;
    /// `OS_ChangeRedirection`.
    pub const CHANGE_REDIRECTION: u32 = 0x5E;
    /// `OS_RemoveCallBack`.
    pub const REMOVE_CALLBACK: u32 = 0x5F;
    /// `OS_FindMemMapEntries`.
    pub const FIND_MEM_MAP_ENTRIES: u32 = 0x60;
    /// `OS_SetColour`.
    pub const SET_COLOUR: u32 = 0x61;
    /// `OS_Pointer`.
    pub const POINTER: u32 = 0x64;
    /// `OS_ScreenMode`.
    pub const SCREEN_MODE: u32 = 0x65;
    /// `OS_DynamicArea`.
    pub const DYNAMIC_AREA: u32 = 0x66;
    /// `OS_AbortTrap`.
    pub const ABORT_TRAP: u32 = 0x67;
    /// `OS_Memory`.
    pub const MEMORY: u32 = 0x68;
    /// `OS_ClaimProcessorVector`.
    pub const CLAIM_PROCESSOR_VECTOR: u32 = 0x69;
    /// `OS_Reset`.
    pub const RESET: u32 = 0x6A;
    /// `OS_MMUControl`.
    pub const MMU_CONTROL: u32 = 0x6B;
    /// `OS_ResyncTime`.
    pub const RESYNC_TIME: u32 = 0x6C;
    /// `OS_PlatformFeatures`.
    pub const PLATFORM_FEATURES: u32 = 0x6D;
    /// `OS_SynchroniseCodeAreas`.
    pub const SYNCHRONISE_CODE_AREAS: u32 = 0x6E;
    /// `OS_CallASWI`.
    pub const CALL_A_SWI: u32 = 0x6F;
    /// `OS_AMBControl`.
    pub const AMB_CONTROL: u32 = 0x70;
    /// `OS_CallASWIR12`.
    pub const CALL_A_SWI_R12: u32 = 0x71;
    /// `OS_NVMemory`.
    pub const NV_MEMORY: u32 = 0x76;
    /// `OS_TaskControl`.
    pub const TASK_CONTROL: u32 = 0x78;
    /// `OS_Hardware`.
    pub const HARDWARE: u32 = 0x7A;
    /// `OS_IICOp`.
    pub const IIC_OP: u32 = 0x7B;
    /// `OS_LeaveOS`.
    pub const LEAVE_OS: u32 = 0x7C;
    /// `OS_ReadLine32`.
    pub const READ_LINE32: u32 = 0x7D;
    /// `OS_SubstituteArgs32`.
    pub const SUBSTITUTE_ARGS32: u32 = 0x7E;
    /// `OS_HeapSort32`.
    pub const HEAP_SORT32: u32 = 0x7F;

    /// `OS_ChangeEnvironment`'s handlers.
    pub mod environment {
        pub use crate::env::{ERROR, ESCAPE, EXIT, UPCALL};
    }

    /// `OS_Exit`'s word beside a return code.
    pub mod exit {
        pub use crate::env::ABEX;
    }

    /// `OS_SetColour`'s flags.
    pub mod set_colour {
        /// The GCOL action.
        pub const ACTION: u32 = 0xF;
        /// The background colour rather than the foreground.
        pub const BACKGROUND: u32 = 1 << 4;
        /// R1 points at an ECF pattern's eight words.
        pub const PATTERN: u32 = 1 << 5;
        /// The text colour rather than the graphics.
        pub const TEXT: u32 = 1 << 6;
        /// The colour read rather than set.
        pub const READ: u32 = 1 << 7;
    }

    /// `OS_ReadUnsigned`'s flags, beside its base.
    pub mod read_unsigned {
        /// The number must end at a space or a control character.
        pub const TERMINATED: u32 = 1 << 31;
        /// The number is at most 255.
        pub const BYTE_SIZED: u32 = 1 << 30;
        /// The number is at most R2.
        pub const BOUNDED: u32 = 1 << 29;
        /// The base a reading with flags is most often in.
        pub const DECIMAL: u32 = 10;
        /// The base a disc address is given in.
        pub const HEXADECIMAL: u32 = 16;
    }

    /// `OS_Find`'s reason: its kind, the flags that refuse a directory and
    /// a missing object, and the path it searches.
    pub mod find {
        /// Close the file R1 names; with R1 0, the current filing system's
        /// every file.
        pub const CLOSE: u32 = 0;
        /// Open an existing file to read.
        pub const INPUT: u32 = 0x40;
        /// Create a file to write.
        pub const OUTPUT: u32 = 0x80;
        /// Open an existing file to read and write; as a mask, the kind.
        pub const UPDATE: u32 = INPUT | OUTPUT;
        /// Refuse a directory.
        pub const NO_DIRECTORY: u32 = 0x04;
        /// Refuse a missing object.
        pub const MUST_EXIST: u32 = 0x08;
        /// Refuse a missing file and a directory.
        pub const MUST_OPEN: u32 = NO_DIRECTORY | MUST_EXIST;
        /// The path searched: `File$Path`, a path, a path variable, none.
        pub const PATH: u32 = 0x03;
        /// [`PATH`] for no path at all.
        pub const NO_PATH: u32 = 0x03;
    }

    /// `OS_Args`'s reasons.
    pub mod args {
        /// Read a file's pointer.
        pub const READ_POINTER: u32 = 0;
        /// Write a file's pointer.
        pub const WRITE_POINTER: u32 = 1;
        /// Read a file's extent.
        pub const READ_EXTENT: u32 = 2;
        /// Write a file's extent.
        pub const WRITE_EXTENT: u32 = 3;
        /// Read the space allocated to a file.
        pub const READ_ALLOCATED: u32 = 4;
        /// Whether a file's pointer is at its end.
        pub const AT_END: u32 = 5;
        /// Ensure a file has space for at least a size.
        pub const ENSURE: u32 = 6;
        /// Read a file's canonical name.
        pub const NAME: u32 = 7;
        /// The image stamp of an image file changed.
        pub const IMAGE_STAMP: u32 = 8;
        /// An I/O control on a file.
        pub const IOCTL: u32 = 9;
        /// Read a file's status and its filing system's information word.
        pub const STATUS: u32 = 254;
        /// Flush a file's buffers.
        pub const FLUSH: u32 = 255;

        /// The stream status word [`STATUS`] reads.
        pub mod status {
            /// Opened to be read.
            pub const READ: u32 = 0x40;
            /// Opened to be written.
            pub const WRITE: u32 = 0x80;
            /// Written to since it was opened.
            pub const MODIFIED: u32 = 0x100;
            /// The next read at its end is the error.
            pub const AT_END: u32 = 0x200;
            /// Unbuffered: the filing system keeps its pointer and extent.
            pub const UNBUFFERED: u32 = 0x400;
            /// No file is open on the handle.
            pub const UNALLOCATED: u32 = 0x800;
        }
    }

    /// `OS_GBPB`'s reasons.
    pub mod gbpb {
        /// Bytes written to a file at its pointer.
        pub const WRITE: u32 = 2;
        /// Bytes read from a file from the offset given.
        pub const READ_AT: u32 = 3;
        /// The names in a directory that match a pattern.
        pub const READ_NAMES: u32 = 9;
        /// The objects in a directory that match a pattern, each with its
        /// catalogue information.
        pub const READ_ENTRIES: u32 = 10;
    }

    /// `OS_FSControl`'s reasons.
    pub mod fs_control {
        /// Set the current directory.
        pub const SET_CURRENT: u32 = 0;
        /// Set the library.
        pub const SET_LIBRARY: u32 = 1;
        /// An application is starting.
        pub const START_APPLICATION: u32 = 2;
        /// Reserved.
        pub const RESERVED: u32 = 3;
        /// `*Run`.
        pub const RUN: u32 = 4;
        /// `*Cat`.
        pub const CAT: u32 = 5;
        /// `*Ex`.
        pub const EX: u32 = 6;
        /// `*LCat`.
        pub const LIBRARY_CAT: u32 = 7;
        /// `*LEx`.
        pub const LIBRARY_EX: u32 = 8;
        /// `*Info`.
        pub const INFO: u32 = 9;
        /// `*Opt`.
        pub const OPT: u32 = 10;
        /// A filing system prefix or path variable at the start of a name.
        pub const PREFIX: u32 = 11;
        /// Add a filing system.
        pub const ADD: u32 = 12;
        /// Whether a filing system is present.
        pub const PRESENT: u32 = 13;
        /// Select a filing system.
        pub const SELECT: u32 = 14;
        /// Boot from a filing system.
        pub const BOOT: u32 = 15;
        /// Remove a filing system.
        pub const REMOVE: u32 = 16;
        /// Add a secondary module.
        pub const ADD_SECONDARY: u32 = 17;
        /// A file type's name.
        pub const TYPE_NAME: u32 = 18;
        /// Restore the current filing system.
        pub const RESTORE: u32 = 19;
        /// The temporary filing system's module.
        pub const TEMPORARY_MODULE: u32 = 20;
        /// A handle's filing system.
        pub const HANDLE: u32 = 21;
        /// Close every file.
        pub const CLOSE_ALL: u32 = 22;
        /// Shut every filing system down.
        pub const SHUT_DOWN: u32 = 23;
        /// `*Access`.
        pub const ACCESS: u32 = 24;
        /// `*Rename`.
        pub const RENAME: u32 = 25;
        /// `*Copy`.
        pub const COPY: u32 = 26;
        /// `*Wipe`.
        pub const WIPE: u32 = 27;
        /// `*Count`.
        pub const COUNT: u32 = 28;
        /// The secondary module's name and private word.
        pub const SECONDARY_MODULE: u32 = 30;
        /// A file type's number from its name.
        pub const TYPE_NUMBER: u32 = 31;
        /// `*FileInfo`.
        pub const FILE_INFO: u32 = 32;
        /// A filing system's name from its number.
        pub const NUMBER_TO_NAME: u32 = 33;
        /// Add an image filing system.
        pub const ADD_IMAGE_SYSTEM: u32 = 35;
        /// Remove an image filing system.
        pub const REMOVE_IMAGE_SYSTEM: u32 = 36;
        /// A name made canonical.
        pub const CANONICALISE: u32 = 37;
        /// A file type from catalogue information.
        pub const TYPE_OF_INFO: u32 = 38;
        /// Set the user root directory.
        pub const SET_USER_ROOT: u32 = 39;
        /// Exchange the current and previous directories.
        pub const BACK: u32 = 40;
        /// A disc's defect list.
        pub const DEFECT_LIST: u32 = 41;
        /// Add a defect to a disc.
        pub const ADD_DEFECT: u32 = 42;
        /// Unset the current directory.
        pub const UNSET_CURRENT: u32 = 43;
        /// Unset the user root directory.
        pub const UNSET_USER_ROOT: u32 = 44;
        /// Unset the library.
        pub const UNSET_LIBRARY: u32 = 45;
        /// A disc's used space map.
        pub const USED_SPACE_MAP: u32 = 46;
        /// A disc's boot option.
        pub const READ_BOOT_OPTION: u32 = 47;
        /// Set a disc's boot option.
        pub const WRITE_BOOT_OPTION: u32 = 48;
        /// A disc's free space.
        pub const FREE_SPACE: u32 = 49;
        /// Name a disc.
        pub const NAME_DISC: u32 = 50;
        /// An image's stamp updated.
        pub const STAMP_IMAGE: u32 = 51;
        /// The object at an offset on a disc.
        pub const OBJECT_AT_OFFSET: u32 = 52;
        /// Set a directory, unchecked.
        pub const SET_UNCHECKED: u32 = 53;
        /// A directory as it was set.
        pub const READ_PATH: u32 = 54;
        /// A disc's free space, in 64 bits.
        pub const FREE_SPACE_64: u32 = 55;
        /// A disc's defect list, in 64 bits.
        pub const DEFECT_LIST_64: u32 = 56;
        /// Add a defect to a disc, in 64 bits.
        pub const ADD_DEFECT_64: u32 = 57;
        /// The open file after a handle.
        pub const NEXT_OPEN: u32 = 58;
    }

    /// `OS_File`'s reasons.
    pub mod file {
        /// Save a block of memory as a file.
        pub const SAVE: u32 = 0;
        /// An object's catalogue information.
        pub const READ_INFO: u32 = 5;
        /// Delete an object.
        pub const DELETE: u32 = 6;
        /// Create an empty file.
        pub const CREATE: u32 = 7;
        /// Create a directory.
        pub const CREATE_DIRECTORY: u32 = 8;
        /// Stamp an object with the time.
        pub const STAMP: u32 = 9;
        /// Save a block of memory as a file of a type.
        pub const SAVE_TYPED: u32 = 10;
        /// Create an empty file of a type.
        pub const CREATE_TYPED: u32 = 11;
        /// An object's catalogue information, no path searched.
        pub const READ_INFO_NO_PATH: u32 = 17;
        /// Set a file's type.
        pub const SET_TYPE: u32 = 18;
        /// The error an object's kind gives.
        pub const MAKE_ERROR: u32 = 19;
        /// An object's catalogue information and file type.
        pub const READ_WITH_TYPE: u32 = 20;
        /// Load a file at its own load address or one given.
        pub const LOAD: u32 = 255;
    }
}

/// A SWI the kernel performs in Rust.
pub(crate) type Handler<'o> = for<'a, 'c> fn(&'a mut Os<'o>, &'c mut Regs) -> Result<(), Fail>;

/// The registers a SWI's caller passes, as the current mode sees them.
fn caller_regs(os: &mut Os<'_>) -> Regs {
    let cpu = os.world.processor.cpu();
    let mut regs = Regs::default();
    for (n, slot) in (0..).zip(regs.r.iter_mut()) {
        *slot = cpu.reg(Reg::field(n));
    }
    regs.flags = cpu.cpsr() & psr::NZCV;
    regs
}

impl Os<'_> {
    /// Performs the `SWI` the processor stopped at, from guest code.
    pub(crate) fn swi(&mut self, number: u32) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let width = if cpu.cpsr() & psr::T != 0 { 2 } else { 4 };
        let at = cpu.pc().wrapping_sub(width);
        if !self.world.swi_passing && self.swi_vector_claimed() {
            let pc = self.world.processor.cpu().pc();
            self.world.processor.cpu().enter(Exception::Swi, pc);
            return Ok(());
        }
        let outer = core::mem::replace(&mut self.world.swi_at, at);
        let outermost = self.world.depth == 0 && self.world.frame.is_none();
        let mut regs = caller_regs(self);
        if outermost {
            if self.world.resume.take() != Some((at, number)) {
                self.world.continuation = None;
            }
            self.world.guest_called = false;
            let back = self.world.processor.cpu().pc();
            self.world.frame = Some(crate::os::Frame {
                swi: Some(number),
                return_pc: back,
                upper: [regs.r[10], regs.r[11], regs.r[12]],
            });
        }
        let number = through_call_a_swi(number, &regs);
        let result = self.perform_resolved(number, &mut regs);
        let waiting = matches!(result, Err(Fail::Unwind(Unwind::Wait(_))));
        self.world.swi_at = outer;
        let completed = self.complete(number, &regs, result);
        if outermost {
            self.world.frame = None;
            if !waiting {
                self.world.continuation = None;
            }
        }
        completed
    }

    /// Performs SWI `number` where RISC OS ignores its error: whether it
    /// succeeded. Only an unwind comes back.
    pub(crate) fn perform_quietly(&mut self, number: u32, regs: &mut Regs) -> Result<bool, Unwind> {
        match self.perform(number, regs) {
            Ok(()) => Ok(true),
            Err(Fail::Error(_)) => Ok(false),
            Err(Fail::Unwind(unwind)) => Err(unwind),
        }
    }

    /// Hands a SWI's results to its caller: R0 to R9 and the flags it set,
    /// V clear; or its error, returned or raised as its X bit says.
    fn complete(
        &mut self,
        number: u32,
        regs: &Regs,
        result: Result<(), Fail>,
    ) -> Result<(), Unwind> {
        let error = match result {
            Ok(()) => None,
            Err(Fail::Unwind(unwind)) => return Err(unwind),
            Err(Fail::Error(block)) => {
                if number & X == 0 {
                    // An ErrorV claimant that swallows the error leaves the
                    // SWI to return as though it had worked.
                    self.raise(block)?;
                    None
                } else {
                    Some(block)
                }
            }
        };
        let cpu = self.world.processor.cpu();
        for (n, &value) in (0..).zip(&regs.r[..10]) {
            cpu.set_reg(Reg::field(n), value);
        }
        let mut flags = regs.flags & (psr::N | psr::Z | psr::C);
        if let Some(block) = error {
            cpu.set_reg(Reg::field(0), block);
            flags |= psr::V;
        }
        cpu.set_nzcv(flags);
        self.callbacks_after_swi()
    }

    /// Performs SWI `number` with `regs`, from guest code or the kernel; an
    /// error is returned, never raised.
    pub fn perform(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        let number = through_call_a_swi(number, regs);
        self.world.kernel_calls += 1;
        let result = self.perform_resolved(number, regs);
        self.world.kernel_calls -= 1;
        result
    }

    /// Performs SWI `number`, any `OS_CallASWI` already resolved, however it
    /// was issued. An error block that cannot be one is replaced, but for
    /// `XOS_GenerateError`'s, which is its caller's own to give.
    fn perform_resolved(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        match self.run_handler(number, regs) {
            Err(Fail::Error(block)) if number != X | os::GENERATE_ERROR => {
                Err(self.checked_error(number, block))
            }
            result => result,
        }
    }

    /// SWI `number`'s handler run.
    fn run_handler(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        let base = number & !X;
        if base < WRITE_I {
            let outer = core::mem::replace(&mut self.world.performing, base);
            let result = match table::os(base) {
                Some(handler) => handler(self, regs),
                None => Err(self.no_such_swi(base)),
            };
            self.world.performing = outer;
            return result;
        }
        if base < 2 * WRITE_I {
            return self.write_c((base & 0xFF) as u8);
        }
        if let Some(result) = self.module_swi(number, regs) {
            return result;
        }
        self.unknown_swi(base, regs)
    }

    /// "SWI &… not known", naming `number`.
    pub(crate) fn no_such_swi(&mut self, number: u32) -> Fail {
        let name = crate::env::hex(number);
        self.error_with(crate::env::NO_SUCH_SWI, &[&name])
    }

    /// A SWI no module provides: UKSWIV, with its number in R11; whatever
    /// claims it answers as a module's SWI handler would, every flag its
    /// own.
    fn unknown_swi(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        let mut call = *regs;
        call.r[11] = number;
        self.call_vector(
            crate::vectors::UKSWI,
            &mut call,
            crate::os::Entry::Supervisor,
        )?;
        regs.r[..10].copy_from_slice(&call.r[..10]);
        regs.flags = call.flags & (psr::N | psr::Z | psr::C);
        if call.flag(psr::V) {
            return Err(Fail::Error(call.r[0]));
        }
        Ok(())
    }
}

mod table;

impl Os<'_> {
    /// `OS_SWINumberToString`: R0's name into R1's buffer of R2 bytes, R2
    /// the bytes written with the terminator; a buffer too small holds what
    /// fitted, unterminated, and R0 points at "Buffer overflow" with V clear,
    /// as genuine RISC OS 5 returns it.
    pub(crate) fn swi_number_to_string(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        use crate::module::ChunkNames;
        let number = regs.r[0];
        let chunk = number & !X & !(CHUNK - 1);
        let chunk_names = if number & !X < 2 * WRITE_I {
            ChunkNames::User
        } else {
            self.chunk_names(chunk)?
        };
        let naming = match &chunk_names {
            ChunkNames::User => None,
            ChunkNames::Table(table) => Some(names::Naming::Table(table)),
            ChunkNames::Title(title) => Some(names::Naming::Title(title)),
            &ChunkNames::Code { target, private } => {
                return self.name_by_code(number, target, private, regs);
            }
        };
        let mut text = names::to_string(number, naming);
        if text.try_reserve(1).is_err() {
            return Err(self.no_memory());
        }
        text.push(0);
        let room = regs.r[2] as usize;
        if text.len() > room {
            self.write_bytes(regs.r[1], &text[..room])?;
            regs.r[2] = len32(room);
            if let Fail::Error(block) = self.buffer_overflow() {
                regs.r[0] = block;
            }
            return Ok(());
        }
        self.write_bytes(regs.r[1], &text)?;
        regs.r[2] = len32(text.len());
        Ok(())
    }

    /// `OS_SWINumberFromString`: the number of the SWI R1 names.
    pub(crate) fn swi_number_from_string(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let text = self.read_until(regs.r[1], 256, |b| b <= b' ')?.0;
        let name = names::name_of(&text);
        if let Some(number) = names::os_from_string(name) {
            regs.r[0] = number;
            return Ok(());
        }
        let (x, bare, at) = match name.strip_prefix(b"X") {
            Some(rest) => (X, rest, regs.r[1].wrapping_add(1)),
            None => (0, name, regs.r[1]),
        };
        match self.module_swi_named(bare, at)? {
            Some(number) => {
                regs.r[0] = x | number;
                Ok(())
            }
            None => Err(self.error(NO_SUCH_SWI_NAME)),
        }
    }
}

/// "No SWI has that name".
const NO_SUCH_SWI_NAME: crate::error::Error = crate::error::Error {
    number: crate::env::NO_SUCH_SWI.number,
    token: "UnknownSwiName",
};

/// The SWI `OS_CallASWI` and `OS_CallASWIR12` carry, in R10 and R12, in
/// their place, its own X bit deciding how its error returns. A chain of
/// them is followed a bounded way, past which it is not known, the last
/// one's X bit kept.
fn through_call_a_swi(number: u32, regs: &Regs) -> u32 {
    let target = |number: u32| match number & !X {
        os::CALL_A_SWI => Some(regs.r[10] & 0x00FF_FFFF),
        os::CALL_A_SWI_R12 => Some(regs.r[12] & 0x00FF_FFFF),
        _ => None,
    };
    let mut number = number;
    for _ in 0..CALL_A_SWI_HOPS {
        match target(number) {
            Some(next) => number = next,
            None => return number,
        }
    }
    if target(number).is_some() {
        UNKNOWN | (number & X)
    } else {
        number
    }
}

#[cfg(test)]
mod tests;
