//! What a RISC OS archive — Spark, `ArcFS` — records of a member beside its
//! bytes: its load, exec and attribute words, read into the member's
//! metadata and written back from it.
//!
//! A typed object's load and exec words carry its filetype and its stamp, to
//! the centisecond, so the stamp is the member's modification time and is
//! written back from it. An untyped object's words are addresses, kept as
//! they are. A member without words — a plain ARC member, or one made here —
//! is written typed as Data, as RISC OS types a file it is told nothing
//! about.

use tairix_abi::driver::filesystem::{NodeKind, NodeSecurity, NodeTimes};
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fsmeta::preset::acorn::{self, Filetype, LoadExec};

use crate::tree::Meta;

/// A member's RISC OS words, as its archive records them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Words {
    pub(crate) load: u32,
    pub(crate) exec: u32,
    /// The `FileSwitch` attribute byte.
    pub(crate) attr: u8,
}

/// The metadata of a member of `kind` whose archive records `words`, and
/// `dated` where its words carry no stamp.
pub(crate) fn meta_of(words: Option<Words>, dated: Option<Time64>, kind: NodeKind) -> Meta {
    let stamped = words.and_then(
        |words| match acorn::decode_load_exec(words.load, words.exec) {
            LoadExec::Typed { centiseconds, .. } => {
                acorn::centiseconds_to_time64(centiseconds).ok()
            }
            LoadExec::Untyped { .. } => None,
        },
    );
    let modified = stamped.or(dated).unwrap_or(Time64::UNIX_EPOCH);
    let directory = if kind == NodeKind::Directory {
        acorn::ATTR_DIRECTORY
    } else {
        0
    };
    Meta {
        mode: NodeSecurity::unrecorded(kind).mode,
        owner: None,
        times: NodeTimes {
            created: Time64::UNIX_EPOCH,
            modified,
            accessed: Time64::UNIX_EPOCH,
            changed: Time64::UNIX_EPOCH,
        },
        acorn: words.map(|words| acorn::Object {
            load: words.load,
            exec: words.exec,
            attr: acorn::attr_from_fileswitch(words.attr) | directory,
        }),
    }
}

/// `object` dated `time`, where it is typed: a typed object keeps its one
/// time in its load and exec words.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a time a RISC OS stamp cannot hold.
pub(crate) fn dated(object: acorn::Object, time: Time64) -> Result<acorn::Object, DriverError> {
    let LoadExec::Typed { filetype, .. } = acorn::decode_load_exec(object.load, object.exec) else {
        return Ok(object);
    };
    let centiseconds = acorn::time64_to_centiseconds(time).map_err(|_| DriverError::OutOfRange)?;
    let (load, exec) =
        acorn::encode_typed(filetype, centiseconds).map_err(|_| DriverError::OutOfRange)?;
    Ok(acorn::Object {
        load,
        exec,
        ..object
    })
}

/// The time a typed `object`'s words date it.
pub(crate) fn date_of(object: &acorn::Object) -> Option<Time64> {
    match acorn::decode_load_exec(object.load, object.exec) {
        LoadExec::Typed { centiseconds, .. } => acorn::centiseconds_to_time64(centiseconds).ok(),
        LoadExec::Untyped { .. } => None,
    }
}

/// The words a member with `meta` is written with: a typed object's stamped
/// with its modification time, an untyped one's kept, and one with none
/// typed as `untyped` — Data for a file, the format's own type for a
/// directory.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a modification time a RISC OS stamp
/// cannot hold, 1900 to 2248.
pub(crate) fn words_of(meta: &Meta, untyped: Filetype) -> Result<Words, DriverError> {
    let centiseconds =
        acorn::time64_to_centiseconds(meta.times.modified).map_err(|_| DriverError::OutOfRange)?;
    let Some(object) = meta.acorn else {
        let (load, exec) =
            acorn::encode_typed(untyped, centiseconds).map_err(|_| DriverError::OutOfRange)?;
        return Ok(Words {
            load,
            exec,
            attr: acorn::attr_to_fileswitch(acorn::ATTR_OWNER_READ | acorn::ATTR_OWNER_WRITE),
        });
    };
    let (load, exec) = match acorn::decode_load_exec(object.load, object.exec) {
        LoadExec::Typed { filetype, .. } => {
            acorn::encode_typed(filetype, centiseconds).map_err(|_| DriverError::OutOfRange)?
        }
        LoadExec::Untyped { load, exec } => (load, exec),
    };
    Ok(Words {
        load,
        exec,
        attr: acorn::attr_to_fileswitch(object.attr),
    })
}

#[cfg(test)]
#[path = "riscos_tests.rs"]
mod tests;
