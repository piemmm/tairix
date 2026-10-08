//! The persistent thumbnail store: what it keeps, what it refuses, and what a
//! torn or foreign blob costs.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::fs::FileId;
use tairix_abi::time::Time64;
use tairix_geometry::Rect;

use super::{StoreFile, StoreKey, ThumbnailStore, HEADER_LEN, WAYS};
use crate::picture::Fitted;
use crate::thumbnail::{ArtworkDocument, DocumentStamp, Reading};

const SIDE: u32 = 4;
const REVISION: u32 = 7;
const PAYLOAD: usize = (SIDE * SIDE * 4) as usize;

/// A blob in memory that can be told to stop writing after some bytes.
#[derive(Clone, Default)]
struct Blob {
    bytes: Vec<u8>,
    /// Bytes a write may still put down before the power goes.
    budget: Option<usize>,
}

impl StoreFile for Blob {
    fn read_exact_at(&mut self, offset: u64, into: &mut [u8]) -> bool {
        let Ok(at) = usize::try_from(offset) else {
            return false;
        };
        match self.bytes.get(at..at + into.len()) {
            Some(held) => {
                into.copy_from_slice(held);
                true
            }
            None => false,
        }
    }

    fn write_all_at(&mut self, offset: u64, from: &[u8]) -> bool {
        let Ok(at) = usize::try_from(offset) else {
            return false;
        };
        let room = self
            .budget
            .map_or(from.len(), |budget| budget.min(from.len()));
        let Some(slot) = self.bytes.get_mut(at..at + room) else {
            return false;
        };
        slot.copy_from_slice(&from[..room]);
        if let Some(budget) = self.budget.as_mut() {
            *budget -= room;
        }
        room == from.len()
    }

    fn set_len(&mut self, len: u64) -> bool {
        usize::try_from(len).is_ok_and(|len| {
            self.bytes.resize(len, 0);
            true
        })
    }

    fn byte_len(&mut self) -> Option<u64> {
        u64::try_from(self.bytes.len()).ok()
    }
}

/// A ceiling holding `sets` sets of pictures `SIDE` square.
fn ceiling(sets: u64) -> u64 {
    HEADER_LEN as u64 + 4096 + sets * u64::from(WAYS) * (64 + PAYLOAD as u64)
}

fn stamp(node: u64, content_gen: u64) -> DocumentStamp {
    DocumentStamp {
        size: 100,
        modified: Time64::from_secs(1_700_000_000),
        id: FileId {
            volume: [9; 16],
            node,
        },
        content_gen,
    }
}

fn key(node: u64, content_gen: u64) -> StoreKey {
    StoreKey::of(stamp(node, content_gen), Reading::Signature).expect("an exact version")
}

fn picture(fill: u8) -> Fitted {
    Fitted {
        pixels: vec![fill; PAYLOAD],
        bounds: Rect::new(0, 1, SIDE, 2),
    }
}

/// A stored picture comes back whole, with its bounds, and survives the
/// store being opened again over the same blob.
#[test]
fn a_stored_picture_comes_back_across_a_reopen() {
    let mut store =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(8)).expect("formats");
    assert!(store.insert(key(1, 5), &picture(0x40)));
    assert_eq!(store.find(key(1, 5)), Some(picture(0x40)));
    assert_eq!(
        store.find(key(1, 6)),
        None,
        "another version is another key"
    );
    assert_eq!(store.find(key(2, 5)), None, "another file is another key");
    let blob = store.file.clone();
    let mut reopened = ThumbnailStore::open(blob, SIDE, REVISION, ceiling(8)).expect("adopts");
    assert_eq!(reopened.find(key(1, 5)), Some(picture(0x40)));
}

/// A file whose volume names no exact version is never keyed, and a picture
/// not this store's shape is refused.
#[test]
fn only_an_exact_version_of_the_stores_shape_is_kept() {
    assert_eq!(StoreKey::of(stamp(1, 0), Reading::Signature), None);
    let mut nameless = stamp(1, 3);
    nameless.id = FileId::NONE;
    assert_eq!(StoreKey::of(nameless, Reading::Signature), None);
    let mut store =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(2)).expect("formats");
    let short = Fitted {
        pixels: vec![0; PAYLOAD - 1],
        bounds: Rect::new(0, 0, SIDE, SIDE),
    };
    let outside = Fitted {
        pixels: vec![0; PAYLOAD],
        bounds: Rect::new(1, 0, SIDE, SIDE),
    };
    assert!(!store.insert(key(1, 1), &short));
    assert!(!store.insert(key(1, 1), &outside));
    assert_eq!(store.find(key(1, 1)), None);
}

/// A write cut short — the payload down and not its header, or the header
/// torn — reads as nothing rather than as a picture of the wrong pixels.
#[test]
fn a_torn_insert_reads_as_nothing() {
    let mut store =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(1)).expect("formats");
    assert!(store.insert(key(1, 1), &picture(0x10)));
    // The power goes part-way through overwriting the same picture.
    store.file.budget = Some(PAYLOAD + 10);
    assert!(!store.insert(key(1, 1), &picture(0x20)));
    store.file.budget = None;
    assert_eq!(
        store.find(key(1, 1)),
        None,
        "a torn header vouches for nothing"
    );
    store.file.budget = Some(PAYLOAD / 2);
    assert!(!store.insert(key(1, 1), &picture(0x30)));
    store.file.budget = None;
    assert_eq!(
        store.find(key(1, 1)),
        None,
        "a torn payload fails its checksum"
    );
    assert!(store.insert(key(1, 1), &picture(0x50)));
    assert_eq!(
        store.find(key(1, 1)),
        Some(picture(0x50)),
        "and the way is reused"
    );
}

/// Two instances evicting the same way at once leave its header from one and
/// its payload from the other, which serves neither picture.
#[test]
fn two_instances_writing_one_way_serve_neither_picture() {
    let mut shared =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(1)).expect("formats");
    for node in 1..=u64::from(WAYS) {
        assert!(shared.insert(key(node, 1), &picture(0x11)));
    }
    let mut first =
        ThumbnailStore::open(shared.file.clone(), SIDE, REVISION, ceiling(1)).expect("adopts");
    let mut second =
        ThumbnailStore::open(shared.file.clone(), SIDE, REVISION, ceiling(1)).expect("adopts");
    assert!(first.insert(key(50, 1), &picture(0x50)));
    assert!(second.insert(key(60, 1), &picture(0x60)));
    let table_end = usize::try_from(first.payload_base).expect("a small blob");
    for header_from_second in [true, false] {
        let mut mixed = first.file.clone();
        for (at, (mine, theirs)) in mixed.bytes.iter_mut().zip(&second.file.bytes).enumerate() {
            if (at < table_end) == header_from_second {
                *mine = *theirs;
            }
        }
        let mut store = ThumbnailStore::open(mixed, SIDE, REVISION, ceiling(1)).expect("adopts");
        assert_eq!(store.find(key(50, 1)), None);
        assert_eq!(store.find(key(60, 1)), None);
        for node in 2..=u64::from(WAYS) {
            assert_eq!(
                store.find(key(node, 1)),
                Some(picture(0x11)),
                "the other ways stand"
            );
        }
    }
}

/// A blob whose pictures another decoder revision drew, or one cut short of
/// its layout, is formatted afresh rather than served from.
#[test]
fn a_blob_from_another_revision_or_cut_short_is_formatted_afresh() {
    let mut store =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(2)).expect("formats");
    assert!(store.insert(key(1, 1), &picture(0x30)));
    let blob = store.file.clone();
    let mut newer =
        ThumbnailStore::open(blob.clone(), SIDE, REVISION + 1, ceiling(2)).expect("reformats");
    assert_eq!(newer.find(key(1, 1)), None, "an older decoder's picture");
    let mut short = blob;
    short.bytes.truncate(super::HEADER_LEN);
    let mut cut = ThumbnailStore::open(short, SIDE, REVISION, ceiling(2)).expect("reformats");
    assert_eq!(cut.find(key(1, 1)), None);
    assert!(
        cut.insert(key(1, 1), &picture(0x30)),
        "and keeps pictures again"
    );
    assert_eq!(cut.find(key(1, 1)), Some(picture(0x30)));
}

/// A full set gives up its oldest picture to a new one.
#[test]
fn a_full_set_gives_up_its_oldest() {
    let mut store =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(1)).expect("formats");
    let ways = u8::try_from(WAYS).expect("a handful of ways");
    for node in 1..=ways {
        assert!(store.insert(key(node.into(), 1), &picture(node)));
    }
    assert!(store.insert(key(99, 1), &picture(99)));
    assert_eq!(store.find(key(1, 1)), None, "the oldest made room");
    for node in 2..=ways {
        assert_eq!(store.find(key(node.into(), 1)), Some(picture(node)));
    }
    assert_eq!(store.find(key(99, 1)), Some(picture(99)));
}

/// A blob laid out for another side — or holding anything but this version's
/// header — is formatted afresh and serves nothing it held.
#[test]
fn a_blob_for_another_side_is_formatted_afresh() {
    let mut store =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(4)).expect("formats");
    assert!(store.insert(key(1, 1), &picture(7)));
    let blob = store.file.clone();
    let larger =
        ThumbnailStore::open(blob.clone(), SIDE * 2, REVISION, ceiling(16)).expect("reformats");
    assert_eq!(larger.side(), SIDE * 2);
    let mut back =
        ThumbnailStore::open(larger.file, SIDE, REVISION, ceiling(4)).expect("reformats again");
    assert_eq!(back.find(key(1, 1)), None, "the old layout did not survive");
    let mut garbage = blob;
    garbage.bytes[0] ^= 0xFF;
    let mut fresh = ThumbnailStore::open(garbage, SIDE, REVISION, ceiling(4)).expect("reformats");
    assert_eq!(fresh.find(key(1, 1)), None);
    assert!(
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, HEADER_LEN as u64).is_none(),
        "no room"
    );
}

/// The open document a serve decodes from.
struct Document {
    stamp: DocumentStamp,
    now: Option<DocumentStamp>,
}

impl ArtworkDocument for Document {
    fn stamp(&self) -> DocumentStamp {
        self.stamp
    }

    fn restamp(&mut self) -> Option<DocumentStamp> {
        self.now
    }

    fn read_at(&mut self, _offset: u64, _into: &mut [u8]) -> Option<usize> {
        None
    }
}

/// A serve decodes once per version: a hit costs no decode, a decode that
/// raced a write is drawn but not kept, and a volume with no generations is
/// decoded every time.
#[test]
fn a_serve_decodes_once_per_version_and_keeps_only_one_it_read_whole() {
    let mut store =
        ThumbnailStore::open(Blob::default(), SIDE, REVISION, ceiling(8)).expect("formats");
    let mut decodes = 0;
    let mut decode = |_: &mut dyn ArtworkDocument| {
        decodes += 1;
        Some(picture(0x60))
    };
    let mut steady = Document {
        stamp: stamp(1, 4),
        now: Some(stamp(1, 4)),
    };
    assert_eq!(
        store.serve(Reading::Signature, &mut steady, &mut decode),
        Some(picture(0x60))
    );
    assert_eq!(
        store.serve(Reading::Signature, &mut steady, &mut decode),
        Some(picture(0x60))
    );
    let mut raced = Document {
        stamp: stamp(2, 4),
        now: Some(stamp(2, 5)),
    };
    assert!(store
        .serve(Reading::Signature, &mut raced, &mut decode)
        .is_some());
    assert!(store
        .serve(Reading::Signature, &mut raced, &mut decode)
        .is_some());
    let mut inexact = Document {
        stamp: stamp(3, 0),
        now: Some(stamp(3, 0)),
    };
    assert!(store
        .serve(Reading::Signature, &mut inexact, &mut decode)
        .is_some());
    assert_eq!(
        decodes, 4,
        "one for the steady file, two raced, one inexact"
    );
}
