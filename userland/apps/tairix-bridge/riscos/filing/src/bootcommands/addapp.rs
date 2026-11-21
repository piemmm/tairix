//! `*AddApp [<directory>.]<pattern>`: each object the pattern matches added
//! to `Resources:$.Apps` as RISC OS 5's `BootCommands` adds one, so the
//! application can be found there and run from it.
//!
//! An added object is a directory of Obey files that hand on to it: `!Run`
//! runs it; `!Boot` runs its `!Boot`, or loads its `!Sprites` where it has
//! no `!Boot`; `!Help` opens its `!Help`. They are stamped with the time and
//! kept in a block of the RMA the module registers with ResourceFS, where
//! they stay for the session, through the module's restart, as on RISC OS
//! 5; adding the same leaf again replaces it.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::filetype::OBEY;
use tairix_riscos_kernel::filing::fs::attribute::{OWNER_READ, OWNER_WRITE};
use tairix_riscos_kernel::filing::fs::kind;
use tairix_riscos_kernel::filing::fs::stamp;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as swi, file, fs_control};
use tairix_riscos_kernel::swi::resource_fs;
use tairix_riscos_kernel::swi::X;
use tairix_riscos_kernel::word;

use super::objects::{object_kind, objects};
use super::State;
use crate::fileswitch::name::matches;

/// The directory added objects go in, below `Resources:$`.
const APPS: &[u8] = b"Apps.";
/// An added file's attributes: its owner able to read and write it.
const ATTRIBUTES: u32 = OWNER_READ | OWNER_WRITE;
/// The room a canonical name is read into.
const NAME_ROOM: u32 = 1024;

/// `*AddApp`.
pub(super) fn add_app(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (given, _) = os.read_until(regs.r[0], NAME_ROOM as usize, |b| b <= b' ')?;
    let (directory, pattern) = split(&given);
    let wild = pattern.iter().any(|&b| b == b'*' || b == b'#');
    let found = objects(os, directory)?;
    let mut added = false;
    for object in found.iter().filter(|object| matches(pattern, &object.leaf)) {
        let whole = tairix_util::fallible::joined(&[directory, b".", &object.leaf])
            .ok_or_else(|| os.no_memory())?;
        add(os, &object.leaf, &whole)?;
        added = true;
    }
    if added || wild {
        return Ok(());
    }
    os.with_text(&given, 0, |os, name| {
        let mut refused = Regs::default();
        refused.r[..3].copy_from_slice(&[file::MAKE_ERROR, name, 0]);
        os.perform(X | swi::FILE, &mut refused)
    })
}

/// `given` as its directory and the leaf pattern after its last full stop
/// or colon; the current directory where it has neither.
fn split(given: &[u8]) -> (&[u8], &[u8]) {
    match given.iter().rposition(|&b| b == b'.' || b == b':') {
        Some(at) if given[at] == b'.' => (&given[..at], &given[at + 1..]),
        Some(at) => (&given[..=at], &given[at + 1..]),
        None => (b"@", given),
    }
}

/// The object `whole`, of leaf `leaf`, added.
fn add(os: &mut Os<'_>, leaf: &[u8], whole: &[u8]) -> Result<(), Fail> {
    let canonical = canonical(os, whole)?;
    let mut files: Vec<(&[u8], Vec<u8>)> = Vec::new();
    files.try_reserve_exact(3).map_err(|_| os.no_memory())?;
    let joined = |os: &mut Os<'_>, parts: &[&[u8]]| {
        tairix_util::fallible::joined(parts).ok_or_else(|| os.no_memory())
    };
    let inside = |os: &mut Os<'_>, part: &[u8]| -> Result<bool, Fail> {
        let name = joined(os, &[&canonical, b".", part])?;
        Ok(object_kind(os, &name)? != kind::NOT_FOUND)
    };
    if inside(os, b"!Boot")? {
        files.push((b"!Boot", joined(os, &[b"/", &canonical, b".!Boot %*0\n"])?));
    } else if inside(os, b"!Sprites")? {
        files.push((
            b"!Boot",
            joined(os, &[b"IconSprites ", &canonical, b".!Sprites\n"])?,
        ));
    }
    if inside(os, b"!Help")? {
        files.push((
            b"!Help",
            joined(os, &[b"Filer_Run ", &canonical, b".!Help\n"])?,
        ));
    }
    files.push((b"!Run", joined(os, &[b"/", &canonical, b" %*0\n"])?));
    let (load, exec) = stamped(os)?;
    let block = build(os, leaf, &files, load, exec)?;
    let mut register = Regs::default();
    register.r[0] = block;
    if let Err(fail) = os.perform(X | resource_fs::REGISTER_FILES, &mut register) {
        os.rma_free(block)?;
        return Err(fail);
    }
    let Ok(replaced) = super::with(os, |state: &mut State| state.added(leaf, block))? else {
        let mut deregister = Regs::default();
        deregister.r[0] = block;
        os.perform(X | resource_fs::DEREGISTER_FILES, &mut deregister)?;
        os.rma_free(block)?;
        return Err(os.no_memory());
    };
    match replaced {
        Some(old) => {
            let mut deregister = Regs::default();
            deregister.r[0] = old;
            os.perform(X | resource_fs::DEREGISTER_FILES, &mut deregister)?;
            os.rma_free(old)
        }
        None => Ok(()),
    }
}

/// The canonical name of `name`.
fn canonical(os: &mut Os<'_>, name: &[u8]) -> Result<Vec<u8>, Fail> {
    os.with_text(name, 0, |os, at| {
        os.below_supervisor_stack(NAME_ROOM, |os, buffer| {
            let mut regs = Regs::default();
            regs.r[..6].copy_from_slice(&[fs_control::CANONICALISE, at, buffer, 0, 0, NAME_ROOM]);
            os.perform(X | swi::FS_CONTROL, &mut regs)?;
            os.read_zstring(buffer, NAME_ROOM as usize)
        })
    })
}

/// The load and execution words of an Obey file stamped with the time.
fn stamped(os: &mut Os<'_>) -> Result<(u32, u32), Fail> {
    os.below_supervisor_stack(8, |os, block| {
        os.write_byte(block, word::read::UTC)?;
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[word::READ_CLOCK, block]);
        os.perform(X | swi::WORD, &mut regs)?;
        let exec = os.read_word(block)?;
        let high = u32::from(os.read_byte(block.wrapping_add(4))?);
        Ok((stamp::typed(OBEY, high), exec))
    })
}

/// A ResourceFS block in the RMA holding `files` below `Apps.<leaf>.`.
fn build(
    os: &mut Os<'_>,
    leaf: &[u8],
    files: &[(&[u8], Vec<u8>)],
    load: u32,
    exec: u32,
) -> Result<u32, Fail> {
    let mut bytes = Vec::new();
    for (name, data) in files {
        let whole = tairix_util::fallible::joined(&[APPS, leaf, b".", name])
            .ok_or_else(|| os.no_memory())?;
        let name_room = (whole.len() + 1).next_multiple_of(4);
        let data_room = data.len().next_multiple_of(4);
        let size = 20 + name_room + 4 + data_room;
        if bytes.try_reserve(size).is_err() {
            return Err(os.no_memory());
        }
        for word in [len32(size), load, exec, len32(data.len()), ATTRIBUTES] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&whole);
        bytes.resize(bytes.len() + name_room - whole.len(), 0);
        bytes.extend_from_slice(&(len32(data.len()) + 4).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len() + data_room - data.len(), 0);
    }
    if bytes.try_reserve(4).is_err() {
        return Err(os.no_memory());
    }
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let block = os.rma_claim(len32(bytes.len()))?;
    match os.write_bytes(block, &bytes) {
        Ok(()) => Ok(block),
        Err(fail) => {
            os.rma_free(block)?;
            Err(fail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_pattern_splits_from_its_directory_at_the_last_full_stop_or_colon() {
        assert_eq!(split(b"RAM:$.Apps.!P*"), (&b"RAM:$.Apps"[..], &b"!P*"[..]));
        assert_eq!(split(b"RAM:!P*"), (&b"RAM:"[..], &b"!P*"[..]));
        assert_eq!(split(b"!Q*"), (&b"@"[..], &b"!Q*"[..]));
    }
}
