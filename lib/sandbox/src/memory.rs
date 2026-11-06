//! How much memory a sandbox worker may map.
//!
//! A worker is spawned under an `AddressSpaceBytes` ceiling on its one
//! thread, so a decoder whose allocation runs away — a malformed file it
//! under-forecast, or a worker an attacker now drives — ends as the
//! worker's own out-of-memory exit rather than squeezing the machine. The
//! ceiling is chosen here, once, from what the worker parses, so no caller
//! picks a number of its own.

use crate::imagerender::{MAX_DOCUMENT_BYTES, MAX_THUMBNAIL_PEAK_BYTES};
use crate::proto::MAX_FRAME;

/// What a worker needs whatever it parses: its heap's bookkeeping and the
/// stack and tables its service builds before the first request.
pub(crate) const WORKER_BASELINE_BYTES: u64 = 16 << 20;

/// The ceiling of a worker whose request and reply frames bound its work.
pub const MESSAGE_WORKER_BYTES: u64 = 2 * MAX_FRAME as u64 + WORKER_BASELINE_BYTES;

/// The least a document worker is given, however small the machine: one
/// whole document beside a message worker's frames.
pub const DOCUMENT_WORKER_FLOOR_BYTES: u64 = MESSAGE_WORKER_BYTES + MAX_DOCUMENT_BYTES as u64;

/// The ceiling of a worker drawing pictures at a tile's size: one whole
/// document beside a message worker's frames, and the most a thumbnail's
/// decode may cost.
pub const ARTWORK_WORKER_BYTES: u64 = DOCUMENT_WORKER_FLOOR_BYTES + MAX_THUMBNAIL_PEAK_BYTES;

/// What a sandbox worker parses, which decides how much it may map.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WorkerMemory {
    /// Messages its frames bound: a protocol packet, a help page, a batch
    /// of text to colour, a block of sound.
    Messages,
    /// Documents and the pictures decoded from them, whose working set
    /// follows the file rather than any frame.
    Documents,
    /// One document held whole and read where it lies, building nothing that
    /// grows with it: a structure report.
    HeldDocument,
    /// Icons and thumbnails: pictures drawn at a tile's size, whose decode is
    /// refused past a fixed peak on any machine.
    Artwork,
}

impl WorkerMemory {
    /// The most bytes a worker of this kind may map on a machine of
    /// `ram_bytes` (zero where unknown): a message parser its frames and
    /// baseline; a document decoder a quarter of the machine, so one parse
    /// leaves the rest to everything else, and never less than
    /// [`DOCUMENT_WORKER_FLOOR_BYTES`]; an artwork decoder
    /// [`ARTWORK_WORKER_BYTES`].
    #[must_use]
    pub const fn ceiling(self, ram_bytes: u64) -> u64 {
        match self {
            Self::Messages => MESSAGE_WORKER_BYTES,
            Self::Documents => {
                let share = ram_bytes / 4;
                if share > DOCUMENT_WORKER_FLOOR_BYTES {
                    share
                } else {
                    DOCUMENT_WORKER_FLOOR_BYTES
                }
            }
            Self::HeldDocument => DOCUMENT_WORKER_FLOOR_BYTES,
            Self::Artwork => ARTWORK_WORKER_BYTES,
        }
    }

    /// How many workers of this kind fit in what one document decoder may map
    /// on a machine of `ram_bytes`: the most a program pooling them runs at
    /// once, so a pool's ceilings together leave the machine what one decoder
    /// would. Never fewer than one, so where none fits the one a pool runs
    /// may map its own ceiling, beyond a document decoder's.
    #[must_use]
    pub fn pooled(self, ram_bytes: u64) -> usize {
        let fit = Self::Documents.ceiling(ram_bytes) / self.ceiling(ram_bytes);
        usize::try_from(fit).unwrap_or(usize::MAX).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorkerMemory, ARTWORK_WORKER_BYTES, DOCUMENT_WORKER_FLOOR_BYTES, MESSAGE_WORKER_BYTES,
    };

    #[test]
    fn a_message_worker_is_held_to_its_frames_on_any_machine() {
        for ram in [0, 64 << 20, 64 << 30] {
            assert_eq!(WorkerMemory::Messages.ceiling(ram), MESSAGE_WORKER_BYTES);
        }
    }

    #[test]
    fn a_document_worker_takes_a_quarter_of_the_machine_above_its_floor() {
        assert_eq!(WorkerMemory::Documents.ceiling(8 << 30), 2 << 30);
        assert_eq!(
            WorkerMemory::HeldDocument.ceiling(64 << 30),
            DOCUMENT_WORKER_FLOOR_BYTES,
            "a document read in place is held to one document whatever the machine"
        );
        assert_eq!(WorkerMemory::Documents.ceiling(64 << 30), 16 << 30);
        for small in [0, 1 << 20, 256 << 20] {
            assert_eq!(
                WorkerMemory::Documents.ceiling(small),
                DOCUMENT_WORKER_FLOOR_BYTES
            );
        }
    }

    #[test]
    fn an_artwork_worker_is_held_to_a_document_and_a_thumbnail_on_any_machine() {
        for ram in [0, 1 << 30, 64 << 30] {
            assert_eq!(WorkerMemory::Artwork.ceiling(ram), ARTWORK_WORKER_BYTES);
        }
    }

    /// A pool of more than one keeps its ceilings together within one document
    /// decoder's, and a pool always has one, even where none fits.
    #[test]
    fn a_pool_of_artwork_workers_fits_one_document_decoders_share() {
        for ram in [0, 256 << 20, 1 << 30, 8 << 30, 64 << 30] {
            let pooled = WorkerMemory::Artwork.pooled(ram);
            let together = u64::try_from(pooled).unwrap() * ARTWORK_WORKER_BYTES;
            assert!(
                pooled == 1 || together <= WorkerMemory::Documents.ceiling(ram),
                "{ram}"
            );
            assert!(pooled >= 1);
        }
        assert_eq!(
            WorkerMemory::Artwork.pooled(0),
            1,
            "a machine that cannot say"
        );
        assert_eq!(WorkerMemory::Artwork.pooled(1 << 30), 2);
        assert_eq!(WorkerMemory::Artwork.pooled(8 << 30), 16);
    }
}
