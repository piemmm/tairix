//! The checks every soak body shares: a driver result tagged with the
//! reproducing seed, an expected refusal, and a directory listing.

use tairix_abi::driver::filesystem::{DirVisit, FilesystemRead, NodeId};
use tairix_abi::DriverError;

/// Map a driver result into a descriptive soak error tagged with `what`
/// and the reproducing `seed`.
pub(crate) fn ck<T>(r: Result<T, DriverError>, what: &str, seed: u64) -> Result<T, String> {
    r.map_err(|e| format!("seed {seed:#x}: {what}: unexpected {e:?}"))
}

/// Assert that an operation failed with exactly `want`. Callers pass the
/// operation's `.err()` so the success payload is dropped, keeping this
/// free of a moved generic value (clippy `needless_pass_by_value`).
pub(crate) fn want_err(
    got: Option<DriverError>,
    want: DriverError,
    what: &str,
    seed: u64,
) -> Result<(), String> {
    match got {
        Some(e) if e == want => Ok(()),
        Some(e) => Err(format!(
            "seed {seed:#x}: {what}: expected {want:?}, got {e:?}"
        )),
        None => Err(format!("seed {seed:#x}: {what}: expected {want:?}, got Ok")),
    }
}

/// Entries one listing call reads before the walk resumes from its cursor,
/// so every soak listing exercises resumption, not only one whole read.
const LIST_BATCH: usize = 7;

/// Most entries one listing may yield before it is judged not to terminate.
const LIST_MAX: usize = 10_000_000;

/// The names `dir` lists, read a few entries per call, skipping the `.`/`..`
/// links a driver may surface.
pub(crate) fn list_names<F: FilesystemRead>(
    fs: &mut F,
    dir: NodeId,
    seed: u64,
) -> Result<Vec<Vec<u8>>, String> {
    let mut names = Vec::new();
    let mut cursor = 0u64;
    let mut after = Vec::new();
    let mut taken = 0usize;
    loop {
        let mut batch = 0usize;
        let mut last = None;
        let listed = fs.read_dir(dir, cursor, &after, &mut |entry, name| {
            if batch == LIST_BATCH {
                return DirVisit::Stop;
            }
            batch += 1;
            if name != b"." && name != b".." {
                names.push(name.to_vec());
            }
            last = Some((entry.next_cursor, name.to_vec()));
            DirVisit::Take
        });
        ck(listed, "read_dir", seed)?;
        taken += batch;
        let Some((next, name)) = last else {
            return Ok(names);
        };
        if batch < LIST_BATCH {
            return Ok(names);
        }
        if next == cursor {
            return Err(format!("seed {seed:#x}: read_dir cursor did not advance"));
        }
        if taken > LIST_MAX {
            return Err(format!("seed {seed:#x}: read_dir did not terminate"));
        }
        cursor = next;
        after = name;
    }
}
