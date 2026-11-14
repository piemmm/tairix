//! The library's blobs on the application's own store, read and written by a
//! worker off the loop that owes a window its frames: the record, each
//! machine's state, and the parts of a machine the store keeps besides
//! (feature `rt`).
//!
//! Every answer that can fail says why in words the user is shown.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::appdata_ipc::BlobMode;
use tairix_appdata::{open_blob, read_blob, read_blob_span, write_blob, RtHost};
use tairix_crt::screen::thumbnail;
use tairix_fscopy::{DataCopy, Sparse};

use crate::record::{part_blob, state_blob, Kept, Record, RECORD_BLOBS};
use crate::state::{self, Bounds, LastScreen, StateError, Suspended};
use crate::{Library, Loaded};

/// Bytes a blob is copied through at a time.
const COPY_CHUNK: usize = 64 * 1024;

/// The library as the store holds it: every machine, each one's last screen
/// drawn at a card's size, which are suspended, and how many blobs no machine
/// owns could not be swept.
#[derive(Debug)]
pub struct Shelved<M> {
    /// The machines and how the record was found.
    pub loaded: Loaded<M>,
    /// Each machine's card picture that could be drawn.
    pub pictures: Vec<(u32, Vec<u8>)>,
    /// The machines suspended.
    pub suspended: Vec<u32>,
    /// Blobs of no machine left behind.
    pub unswept: usize,
}

/// What a suspended machine resumes from.
#[derive(Debug)]
pub enum Resumable {
    /// It was not suspended.
    Nothing,
    /// Its last screen and its snapshot.
    Kept(Box<(LastScreen, Suspended)>),
    /// Its state does not check out: it starts afresh.
    Damaged,
}

/// The library the store holds, what no machine owns swept away, each card's
/// picture drawn at `picture` where a size is given.
///
/// # Errors
///
/// A store that cannot be listed.
pub fn load<M: Kept>(picture: Option<(u32, u32)>, bounds: &Bounds) -> Result<Shelved<M>, String> {
    let listing = tairix_appdata::blobs::list(&mut RtHost)
        .map_err(|err| alloc::format!("the store could not be listed ({err})"))?;
    let listed = |name: &str| listing.iter().any(|(held, _)| held == name);
    let copies = RECORD_BLOBS.map(|name| {
        listed(name)
            .then(|| read_blob(name, M::MAX_RECORD).ok())
            .flatten()
    });
    let loaded = Library::<M>::load(&listing, [copies[0].as_deref(), copies[1].as_deref()]);
    let unswept = loaded
        .orphans
        .iter()
        .filter(|name| tairix_appdata::blobs::remove(&mut RtHost, name).is_err())
        .count();
    let (mut pictures, mut suspended) = (Vec::new(), Vec::new());
    for machine in loaded.library.machines() {
        if !listed(&state_blob(machine.id())) {
            continue;
        }
        let Some((drawn, asleep)) = card_state(machine.id(), picture, bounds) else {
            continue;
        };
        if asleep && suspended.try_reserve(1).is_ok() {
            suspended.push(machine.id());
        }
        if let Some(drawn) = drawn {
            if pictures.try_reserve(1).is_ok() {
                pictures.push((machine.id(), drawn));
            }
        }
    }
    Ok(Shelved {
        loaded,
        pictures,
        suspended,
        unswept,
    })
}

/// Write `record`: its generation, once it is on the store.
///
/// # Errors
///
/// Why it could not be written.
pub fn write_record(record: &Record) -> Result<u64, String> {
    write_blob(record.blob, &record.bytes)?;
    Ok(record.generation)
}

/// What machine `id`'s card shows of its state, if it has one that reads: its
/// last screen drawn at `size`, where a size is given, and whether it is
/// suspended. Only the header and the screen are read.
#[must_use]
pub fn card_state(
    id: u32,
    size: Option<(u32, u32)>,
    bounds: &Bounds,
) -> Option<(Option<Vec<u8>>, bool)> {
    let name = state_blob(id);
    let file = open_blob(&name, BlobMode::Read).ok()?;
    let head = state::head(&read_blob_span(&file, &name, 0..state::HEAD).ok()?, bounds).ok()?;
    let drawn = size.and_then(|size| {
        let section = read_blob_span(&file, &name, head.screen_span()).ok()?;
        let screen = state::screen(&head, &section, bounds).ok()?;
        thumbnail(screen.width, screen.height, &screen.pixels, size)
    });
    Some((drawn, head.suspended()))
}

/// Each of machines `ids`' last screens that reads, drawn at `size`.
#[must_use]
pub fn pictures(ids: &[u32], size: (u32, u32), bounds: &Bounds) -> Vec<(u32, Vec<u8>)> {
    ids.iter()
        .filter_map(|&id| Some((id, card_state(id, Some(size), bounds)?.0?)))
        .collect()
}

/// What machine `id`, which is suspended, resumes from.
///
/// # Errors
///
/// A state that could not be read at all.
pub fn resumable(id: u32, bounds: &Bounds) -> Result<Resumable, String> {
    let bytes = read_blob(&state_blob(id), state::most_bytes(bounds))?;
    Ok(match state::decode(&bytes, bounds) {
        Ok((screen, Some(suspended))) => Resumable::Kept(Box::new((screen, suspended))),
        Ok((_, None)) => Resumable::Nothing,
        Err(StateError::Malformed) => Resumable::Damaged,
        Err(StateError::OutOfMemory) => {
            return Err(String::from("there is no memory to read its saved state"))
        }
    })
}

/// Machine `id`'s state kept: `screen`, and its snapshot while it is
/// `suspended`.
///
/// # Errors
///
/// A state past `bounds`, or one that could not be written.
pub fn keep(
    id: u32,
    screen: &LastScreen,
    suspended: Option<&Suspended>,
    bounds: &Bounds,
) -> Result<(), String> {
    let bytes = state::encode(screen, suspended, bounds)
        .map_err(|err| alloc::format!("its state could not be kept ({err:?})"))?;
    Ok(write_blob(&state_blob(id), &bytes)?)
}

/// Every blob of machine `id` removed: one it never had is no error.
///
/// # Errors
///
/// The first that could not be removed.
pub fn remove<M: Kept>(id: u32) -> Result<(), String> {
    let parts = M::PARTS
        .iter()
        .flat_map(|&(word, count)| (0..count).map(move |index| part_blob(word, id, index)));
    for name in core::iter::once(state_blob(id)).chain(parts) {
        tairix_appdata::blobs::remove(&mut RtHost, &name)
            .map_err(|err| alloc::format!("{name} could not be removed ({err})"))?;
    }
    Ok(())
}

/// Machine `from`'s state, if it has one, copied to machine `to`.
///
/// # Errors
///
/// A store that cannot be listed, or a copy that failed.
pub fn copy_state(from: u32, to: u32) -> Result<(), String> {
    let state = state_blob(from);
    let held = tairix_appdata::blobs::list(&mut RtHost)
        .map_err(|err| alloc::format!("the store could not be listed ({err})"))?
        .iter()
        .any(|(name, _)| *name == state);
    if held {
        copy_blob(&state, &state_blob(to))?;
    }
    Ok(())
}

/// The host's calendar now, in seconds since 1970, when its clock is set: what
/// a suspend is stamped with.
#[must_use]
pub fn calendar_secs() -> Option<i64> {
    tairix_rt::wall_time()
        .ok()
        .filter(|reading| reading.state() != tairix_abi::time::WallTimeState::Unset)
        .map(|reading| reading.time().secs())
}

/// Blob `from` copied to blob `to`, its holes kept and its runs of zeros left
/// unwritten.
///
/// # Errors
///
/// Why it could not be copied.
pub fn copy_blob(from: &str, to: &str) -> Result<(), String> {
    let mut source = open_blob(from, BlobMode::Read)?;
    let len = source
        .regular_len()
        .ok()
        .flatten()
        .ok_or_else(|| alloc::format!("{from} could not be measured"))?;
    let mut target = open_blob(to, BlobMode::ReadWrite)?;
    let failed = |err: i64| {
        alloc::format!(
            "{to} could not be written ({})",
            tairix_abi::Errno::from_syscall(err)
        )
    };
    target.truncate(0).map_err(failed)?;
    let mut chunk = Vec::new();
    chunk
        .try_reserve_exact(COPY_CHUNK)
        .map_err(|_| alloc::format!("there is no memory to copy {from} through"))?;
    chunk.resize(COPY_CHUNK, 0);
    DataCopy::new(len, Sparse::Always)
        .run(&mut source, &mut target, &mut chunk)
        .map_err(|err| alloc::format!("{from} could not be copied ({err:?})"))?;
    target.sync().map_err(failed)
}
