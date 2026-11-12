//! The programs' routines, by the number their frames record, and each
//! program's main routine, handlers and state.

use super::{Frame, Program, Routine};
use crate::dos::shell;
use crate::dos::utility::{
    self, attrib, chkdsk, deltree, diskcopy, doskey, edit, fc, fdisk, find, format, label, mem,
    mode, more, move_files, number, sort, sys, tree, xcopy,
};
use crate::frames::{routines, Identity};

routines! {
    Program<'_, '_>, Routine;
    Int => super::dos::Int,
    Key => super::dos::Key,
    Line => super::dos::Line,
    Shell => shell::Shell,
    ReadLine => shell::ReadLine,
    Prompt => shell::Prompt,
    Execute => shell::run::Execute,
    Stage => shell::run::Stage,
    Restore => shell::run::Restore,
    Release => shell::run::Release,
    Command => shell::run::Command,
    Remark => shell::run::Remark,
    External => shell::run::External,
    LoadHigh => shell::run::LoadHigh,
    BatchStart => shell::batch::BatchStart,
    BatchEnd => shell::batch::BatchEnd,
    BatchLine => shell::batch::BatchLine,
    Goto => shell::batch::Goto,
    Shift => shell::batch::Shift,
    Call => shell::batch::Call,
    Echo => shell::batch::Echo,
    Pause => shell::batch::Pause,
    IfCommand => shell::batch::IfCommand,
    ForLoop => shell::batch::ForLoop,
    Cd => shell::internal::Cd,
    MakeDir => shell::internal::MakeDir,
    RemoveDir => shell::internal::RemoveDir,
    SetCommand => shell::internal::SetCommand,
    PathCommand => shell::internal::PathCommand,
    PromptCommand => shell::internal::PromptCommand,
    Version => shell::internal::Version,
    Volume => shell::internal::Volume,
    VolumeHeader => shell::internal::VolumeHeader,
    DateCommand => shell::internal::DateCommand,
    TimeCommand => shell::internal::TimeCommand,
    Cls => shell::internal::Cls,
    Break => shell::internal::Break,
    Verify => shell::internal::Verify,
    Exit => shell::internal::Exit,
    Ctty => shell::internal::Ctty,
    Chcp => shell::internal::Chcp,
    Truename => shell::internal::Truename,
    TypeFile => shell::internal::TypeFile,
    Delete => shell::internal::Delete,
    Rename => shell::internal::Rename,
    Dir => shell::dir::Dir,
    DirList => shell::dir::DirList,
    Copy => shell::copy::Copy,
    CopyFile => shell::copy::CopyFile,
    Console => shell::handlers::Console,
    Critical => shell::handlers::Critical,
    Breaker => shell::handlers::Breaker,
    Walk => super::walk::Walk,
    NextLine => utility::NextLine,
    IgnoreBreak => utility::IgnoreBreak,
    AbandonOnBreak => utility::AbandonOnBreak,
    FailCritical => utility::FailCritical,
    Find => find::Find,
    FindSearch => find::Search,
    More => more::More,
    Sort => sort::Sort,
    Attrib => attrib::Attrib,
    Label => label::Label,
    Tree => tree::Tree,
    TreeFiles => tree::Files,
    Mem => mem::Mem,
    Mode => mode::Mode,
    ModeStatus => mode::ModeStatus,
    ModeResident => mode::ModeResident,
    ModePrinter => mode::ModePrinter,
    ModeReroute => mode::ModeReroute,
    ModeSerial => mode::ModeSerial,
    ModeVideo => mode::ModeVideo,
    ModeDisplay => mode::ModeDisplay,
    ModeDisplays => mode::ModeDisplays,
    ModeShift => mode::ModeShift,
    ModeAnsi => mode::ModeAnsi,
    Printer17 => mode::Printer17,
    Serial14 => mode::Serial14,
    Fc => fc::Fc,
    FcBytes => fc::FcBytes,
    FcLines => fc::FcLines,
    DelTree => deltree::DelTree,
    DeleteFiles => deltree::DeleteFiles,
    Move => move_files::Move,
    XCopy => xcopy::XCopy,
    XCopyFiles => xcopy::XCopyFiles,
    Chkdsk => chkdsk::Chkdsk,
    ChkdskCritical => chkdsk::ChkdskCritical,
    ChkdskAsk => chkdsk::Ask,
    ChkdskTell => chkdsk::Tell,
    ChkdskFix => chkdsk::FixEntry,
    ChkdskTree => chkdsk::ChkdskTree,
    ChkdskLost => chkdsk::ChkdskLost,
    ChkdskExtents => chkdsk::ChkdskExtents,
    Sys => sys::Sys,
    Format => format::Format,
    FormatBreak => format::FormatBreak,
    FormatAsk => format::FormatAsk,
    FormatDisk => format::FormatDisk,
    FormatLabel => format::FormatLabel,
    FormatTracks => format::FormatTracks,
    FormatBad => format::FormatBad,
    FormatWrite => format::FormatWrite,
    FormatSystem => format::FormatSystem,
    FormatSource => format::FormatSource,
    DiskCopy => diskcopy::DiskCopy,
    DiskCopyPress => diskcopy::DiskCopyPress,
    DiskCopyDisk => diskcopy::DiskCopyDisk,
    DiskCopyPass => diskcopy::DiskCopyPass,
    DiskCopyTrack => diskcopy::DiskCopyTrack,
    DiskCopySource => diskcopy::DiskCopySource,
    DiskCopyTarget => diskcopy::DiskCopyTarget,
    DiskCopyProbe => diskcopy::DiskCopyProbe,
    DiskCopyFormat => diskcopy::DiskCopyFormat,
    Fdisk => fdisk::Fdisk,
    FdiskRead => fdisk::FdiskRead,
    FdiskWrite => fdisk::FdiskWrite,
    FdiskSpace => fdisk::FdiskSpace,
    FdiskShow => fdisk::FdiskShow,
    FdiskTell => fdisk::FdiskTell,
    FdiskAnswer => fdisk::FdiskAnswer,
    FdiskSize => fdisk::FdiskSize,
    FdiskLabel => fdisk::FdiskLabel,
    FdiskWait => fdisk::FdiskWait,
    FdiskMenu => fdisk::FdiskMenu,
    FdiskCreate => fdisk::FdiskCreate,
    FdiskPrimary => fdisk::FdiskPrimary,
    FdiskExtended => fdisk::FdiskExtended,
    FdiskLogical => fdisk::FdiskLogical,
    FdiskActive => fdisk::FdiskActive,
    FdiskDelete => fdisk::FdiskDelete,
    FdiskDeletePrimary => fdisk::FdiskDeletePrimary,
    FdiskDeleteExtended => fdisk::FdiskDeleteExtended,
    FdiskDeleteLogical => fdisk::FdiskDeleteLogical,
    FdiskDisplay => fdisk::FdiskDisplay,
    DosKey => doskey::DosKey,
    DosKey21 => doskey::DosKey21,
    DosKey2F => doskey::DosKey2F,
    DosKeyEdit => doskey::DosKeyEdit,
    DosKeyWrite => doskey::DosKeyWrite,
    Edit => edit::Edit,
    EditScreen => edit::EditScreen,
    EditRestore => edit::EditRestore,
    EditHook => edit::EditHook,
    EditUnhook => edit::EditUnhook,
    EditPointer => edit::EditPointer,
    EditLoop => edit::EditLoop,
    EditVisit => edit::EditVisit,
    EditList => edit::EditList,
    EditOpen => edit::EditOpen,
    EditLoad => edit::EditLoad,
    EditSaveAs => edit::EditSaveAs,
    EditSave => edit::EditSave,
    EditPrint => edit::EditPrint,
    EditSettings => edit::EditSettings,
}

impl Identity for Id {
    fn number(self) -> u8 {
        Self::number(self)
    }

    fn of(byte: u8) -> Option<Self> {
        Self::of(byte)
    }

    /// A program's main routine and its handlers are roots.
    fn is_root(self) -> bool {
        matches!(
            self,
            Self::Shell
                | Self::IgnoreBreak
                | Self::AbandonOnBreak
                | Self::FailCritical
                | Self::Critical
                | Self::Breaker
                | Self::Find
                | Self::More
                | Self::Sort
                | Self::Attrib
                | Self::Label
                | Self::Tree
                | Self::Mem
                | Self::Mode
                | Self::Printer17
                | Self::Serial14
                | Self::Fc
                | Self::DelTree
                | Self::Move
                | Self::XCopy
                | Self::Chkdsk
                | Self::ChkdskCritical
                | Self::Sys
                | Self::Format
                | Self::FormatBreak
                | Self::DiskCopy
                | Self::Fdisk
                | Self::DosKey
                | Self::DosKey21
                | Self::DosKey2F
                | Self::Edit
        )
    }
}

/// The main routine of program `number`, if it is one.
pub fn main(number: u16) -> Option<Frame> {
    match number {
        shell::NUMBER => Some(Frame::of(&shell::Shell::default())),
        number::FIND => Some(Frame::of(&find::Find::default())),
        number::MORE => Some(Frame::of(&more::More {})),
        number::SORT => Some(Frame::of(&sort::Sort {})),
        number::ATTRIB => Some(Frame::of(&attrib::Attrib {})),
        number::LABEL => Some(Frame::of(&label::Label {})),
        number::TREE => Some(Frame::of(&tree::Tree {})),
        number::MEM => Some(Frame::of(&mem::Mem {})),
        number::MODE => Some(Frame::of(&mode::Mode {})),
        number::FC => Some(Frame::of(&fc::Fc {})),
        number::DELTREE => Some(Frame::of(&deltree::DelTree::default())),
        number::MOVE => Some(Frame::of(&move_files::Move::default())),
        number::XCOPY => Some(Frame::of(&xcopy::XCopy::default())),
        number::CHKDSK => Some(Frame::of(&chkdsk::Chkdsk {})),
        number::SYS => Some(Frame::of(&sys::Sys {})),
        number::FORMAT => Some(Frame::of(&format::Format {})),
        number::DISKCOPY => Some(Frame::of(&diskcopy::DiskCopy {})),
        number::FDISK => Some(Frame::of(&fdisk::Fdisk {})),
        number::DOSKEY => Some(Frame::of(&doskey::DosKey {})),
        number::EDIT => Some(Frame::of(&edit::Edit {})),
        _ => None,
    }
}

/// Handler `n` of program `number`, if it installs one.
pub fn handler(number: u8, n: u16) -> Option<Frame> {
    match (u16::from(number), n) {
        (shell::NUMBER, shell::handler::BREAK) => {
            Some(Frame::of(&shell::handlers::Breaker::default()))
        }
        (shell::NUMBER, shell::handler::CRITICAL) => {
            Some(Frame::of(&shell::handlers::Critical::default()))
        }
        (number::MODE, mode::handler::PRINTER) => Some(Frame::of(&mode::Printer17::default())),
        (number::MODE, mode::handler::SERIAL) => Some(Frame::of(&mode::Serial14::default())),
        (number::MODE, mode::handler::BREAK)
        | (number::XCOPY, xcopy::handler::BREAK)
        | (number::CHKDSK, chkdsk::handler::BREAK)
        | (number::DISKCOPY, diskcopy::handler::BREAK) => {
            Some(Frame::of(&utility::AbandonOnBreak {}))
        }
        (number::CHKDSK, chkdsk::handler::CRITICAL) => Some(Frame::of(&chkdsk::ChkdskCritical {})),
        (number::FORMAT, format::handler::BREAK) => Some(Frame::of(&format::FormatBreak {})),
        (number::FDISK, fdisk::handler::BREAK) | (number::EDIT, edit::handler::BREAK) => {
            Some(Frame::of(&utility::IgnoreBreak {}))
        }
        (number::FDISK, fdisk::handler::CRITICAL) | (number::EDIT, edit::handler::CRITICAL) => {
            Some(Frame::of(&utility::FailCritical {}))
        }
        (number::DOSKEY, doskey::handler::EDIT) => Some(Frame::of(&doskey::DosKey21 {})),
        (number::DOSKEY, doskey::handler::MULTIPLEX) => Some(Frame::of(&doskey::DosKey2F {})),
        _ => None,
    }
}

/// Bytes of program `number`'s own state.
pub fn state_len(number: u16) -> Option<u16> {
    match number {
        shell::NUMBER => Some(shell::at::LEN),
        number::FIND => Some(find::at::LEN),
        number::MORE => Some(more::at::LEN),
        number::SORT => Some(sort::at::LEN),
        number::ATTRIB => Some(attrib::at::LEN),
        number::LABEL => Some(label::at::LEN),
        number::TREE => Some(tree::at::LEN),
        number::MEM => Some(mem::at::LEN),
        number::MODE => Some(mode::at::LEN),
        number::FC => Some(fc::at::LEN),
        number::DELTREE => Some(deltree::at::LEN),
        number::MOVE => Some(move_files::at::LEN),
        number::XCOPY => Some(xcopy::at::LEN),
        number::CHKDSK => Some(chkdsk::at::LEN),
        number::SYS => Some(sys::at::LEN),
        number::FORMAT => Some(format::at::LEN),
        number::DISKCOPY => Some(diskcopy::at::LEN),
        number::FDISK => Some(fdisk::at::LEN),
        number::DOSKEY => Some(doskey::at::LEN),
        number::EDIT => Some(edit::at::LEN),
        _ => None,
    }
}
