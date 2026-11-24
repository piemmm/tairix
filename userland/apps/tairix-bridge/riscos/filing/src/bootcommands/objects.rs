//! Objects as `BootCommands` and the boot sequence find them: whether one is
//! there, as `OS_File` 17 reads it, and a directory's objects read through
//! `OS_GBPB` 10, each object's leaf and catalogue words, in the order its
//! filing system lists them, read whole before any is acted on.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::fs::{stamp, Layout};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as swi, file, gbpb};
use tairix_riscos_kernel::swi::X;

pub(crate) use tairix_riscos_kernel::filing::fs::kind::{DIRECTORY, FILE, IMAGE};

/// The room one read of a listing is given: a record's catalogue words and
/// a leaf of the longest a filing system gives.
const ROOM: u32 = Layout::Info.name_at() + 256;
/// The most objects a listing holds: a containment bound on what one
/// directory's walk costs.
const MOST: usize = 1 << 16;

/// An object in a directory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Object {
    /// Its leaf name.
    pub(crate) leaf: Vec<u8>,
    /// What it is: a file, a directory or an image.
    pub(crate) kind: u32,
    /// Its load and execution words.
    pub(crate) load: u32,
    pub(crate) exec: u32,
}

impl Object {
    /// Its file type: none for a directory or an untyped file.
    pub(crate) const fn file_type(&self) -> Option<u32> {
        if self.kind == DIRECTORY {
            None
        } else {
            stamp::type_of(self.load)
        }
    }
}

/// What `name` is, as `OS_File` 17 reads it: nothing where it finds none or
/// refuses the name.
pub(crate) fn object_kind(os: &mut Os<'_>, name: &[u8]) -> Result<u32, Fail> {
    os.with_text(name, 0, |os, at| {
        let mut read = Regs::default();
        read.r[..2].copy_from_slice(&[file::READ_INFO_NO_PATH, at]);
        match os.perform(X | swi::FILE, &mut read) {
            Ok(()) => Ok(read.r[0]),
            Err(Fail::Error(_)) => Ok(0),
            Err(unwind) => Err(unwind),
        }
    })
}

/// The objects in `directory`, one read at a time.
///
/// # Errors
///
/// FileSwitch's error for a directory it cannot list, or "no memory" for a
/// listing past [`MOST`] objects or that never ends.
pub(crate) fn objects(os: &mut Os<'_>, directory: &[u8]) -> Result<Vec<Object>, Fail> {
    os.with_text(directory, 0, |os, name| {
        os.below_supervisor_stack(ROOM, |os, record| {
            let mut found = Vec::new();
            let mut offset = 0;
            for _ in 0..2 * MOST {
                if offset == u32::MAX {
                    return Ok(found);
                }
                let mut read = Regs::default();
                read.r[..7].copy_from_slice(&[
                    gbpb::READ_ENTRIES,
                    name,
                    record,
                    1,
                    offset,
                    ROOM,
                    0,
                ]);
                os.perform(X | swi::GBPB, &mut read)?;
                offset = read.r[4];
                if read.r[3] == 0 {
                    continue;
                }
                if found.len() >= MOST || found.try_reserve(1).is_err() {
                    return Err(os.no_memory());
                }
                let info = os.read_listed(record)?;
                found.push(Object {
                    leaf: os.read_zstring(record.wrapping_add(Layout::Info.name_at()), 256)?,
                    kind: info.kind,
                    load: info.load,
                    exec: info.exec,
                });
            }
            Err(os.no_memory())
        })
    })
}
