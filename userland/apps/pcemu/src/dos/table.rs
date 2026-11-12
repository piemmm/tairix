//! The kernel's routines, by the number their frames record: one list,
//! from which the numbers, their decoding and the dispatch are made.

use super::engine::{Kernel, Routine};
use crate::frames::{routines, Identity};

routines! {
    Kernel<'_, '_>, Routine;
    Int21 => super::int21::Int21,
    Boot => super::boot::Boot,
    Int25 => super::int21::Int25,
    Int2F => super::int21::Int2F,
    Device => super::device::Device,
    BlockIo => super::fs::BlockIo,
    Critical => super::fs::Critical,
    Buffer => super::fs::Buffer,
    Flush => super::fs::Flush,
    Media => super::fs::Media,
    MediaLabel => super::fs::MediaLabel,
    FatGet => super::fs::FatGet,
    FatSet => super::fs::FatSet,
    Walk => super::fs::Walk,
    Alloc => super::fs::Alloc,
    FreeChain => super::fs::FreeChain,
    Free => super::fs::Free,
    DirScan => super::dir::DirScan,
    Resolve => super::dir::Resolve,
    Slot => super::dir_write::Slot,
    Grow => super::dir_write::Grow,
    Clear => super::dir_write::Clear,
    Store => super::dir_write::Store,
    Open => super::file::Open,
    MakeFile => super::file::MakeFile,
    Create => super::file::Create,
    Read => super::file::Read,
    Write => super::file::Write,
    DirUpdate => super::file::DirUpdate,
    Close => super::file::Close,
    Commit => super::file::Commit,
    CharIo => super::char::CharIo,
    Break => super::char::BreakCheck,
    Idle => super::char::Idle,
    PutChar => super::char::PutChar,
    GetChar => super::char::GetChar,
    InputStatus => super::char::InputStatus,
    LineInput => super::char::LineInput,
    Clock => super::char::Clock,
    Exec => super::exec::Exec,
    Terminate => super::exec::Terminate,
    MakeDir => super::dirops::MakeDir,
    RemoveDir => super::dirops::RemoveDir,
    ChangeDir => super::dirops::ChangeDir,
    CurrentDir => super::dirops::CurrentDir,
    Delete => super::dirops::Delete,
    Attributes => super::dirops::Attributes,
    Find => super::dirops::Find,
    Rename => super::dirops::Rename,
    TempFile => super::dirops::TempFile,
    ExtendedOpen => super::dirops::ExtendedOpen,
    FcbLookup => super::fcb::Lookup,
    FcbOpen => super::fcb::FcbOpen,
    FcbClose => super::fcb::FcbClose,
    FcbIo => super::fcb::FcbIo,
    FcbSearch => super::fcb::FcbSearch,
    FcbDelete => super::fcb::FcbDelete,
    FcbRename => super::fcb::FcbRename,
    FcbSize => super::fcb::FcbSize,
    Ioctl => super::ioctl::Ioctl,
}

impl Identity for Id {
    fn number(self) -> u8 {
        Self::number(self)
    }

    fn of(byte: u8) -> Option<Self> {
        Self::of(byte)
    }

    /// Calls made to the kernel are roots.
    fn is_root(self) -> bool {
        matches!(self, Self::Int21 | Self::Boot | Self::Int25 | Self::Int2F)
    }
}
