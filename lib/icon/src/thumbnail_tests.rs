//! Unit tests for drawing a picture file as its own content.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::fs::FileId;
use tairix_abi::time::Time64;

use super::{ArtworkDocument, DocumentStamp, Reading, Thumbnail, MAX_THUMBNAIL_BYTES};
use crate::artwork::{
    render_artwork, ArtworkKey, ArtworkRasteriser, ArtworkReader, ArtworkResolver, InlineArtwork,
    Resolved,
};
use crate::desk::{ArtworkDesk, ArtworkJob};

const PATH: &str = "/Users/ann/UserFiles/Pictures/cat.png";
const SIDE: u32 = 4;

/// When the fixture file was last written.
const WRITTEN: Time64 = Time64::from_secs(1_700_000_000);

/// The file the fixture's listing named.
const LISTED: FileId = FileId {
    volume: [7; 16],
    node: 42,
};

/// An in-memory picture file.
struct Memory {
    bytes: Vec<u8>,
    modified: Time64,
    id: FileId,
}

impl ArtworkDocument for Memory {
    fn stamp(&self) -> DocumentStamp {
        DocumentStamp {
            size: self.bytes.len() as u64,
            modified: self.modified,
            id: self.id,
        }
    }

    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> Option<usize> {
        let start = usize::try_from(offset).ok()?;
        let held = self.bytes.get(start..)?;
        let len = held.len().min(into.len());
        into[..len].copy_from_slice(&held[..len]);
        Some(len)
    }
}

/// A reader holding one picture file, counting each open.
struct Files {
    bytes: Vec<u8>,
    modified: Time64,
    id: FileId,
    opens: usize,
}

impl Files {
    fn holding(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            modified: WRITTEN,
            id: LISTED,
            opens: 0,
        }
    }
}

impl ArtworkReader for Files {
    fn read(&mut self, _path: &str) -> Option<Vec<u8>> {
        None
    }

    fn open(&mut self, path: &str) -> Option<Box<dyn ArtworkDocument + '_>> {
        self.opens += 1;
        (path == PATH).then(|| {
            Box::new(Memory {
                bytes: self.bytes.clone(),
                modified: self.modified,
                id: self.id,
            }) as Box<dyn ArtworkDocument>
        })
    }
}

/// A rasteriser whose thumbnail streams the whole document and answers
/// `side`×`side` pixels, recording what it was handed.
#[derive(Default)]
struct Decoder {
    streamed: Vec<u8>,
    readings: Vec<Reading>,
    short: bool,
}

impl ArtworkRasteriser for Decoder {
    fn rasterise(&mut self, _side: u32, _bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn thumbnail(
        &mut self,
        side: u32,
        reading: Reading,
        document: &mut dyn ArtworkDocument,
    ) -> Option<Vec<u8>> {
        self.readings.push(reading);
        let length = document.stamp().size;
        let mut offset = 0;
        let mut run = [0u8; 3];
        while offset < length {
            let got = document.read_at(offset, &mut run)?;
            self.streamed.extend_from_slice(&run[..got]);
            offset += got as u64;
        }
        let side = side as usize;
        Some(vec![0x80; if self.short { 3 } else { side * side * 4 }])
    }
}

fn key(size: u64, modified: Time64, reading: Reading) -> ArtworkKey {
    key_of(size, modified, LISTED, reading)
}

fn key_of(size: u64, modified: Time64, id: FileId, reading: Reading) -> ArtworkKey {
    ArtworkKey::Thumbnail(Thumbnail {
        path: String::from(PATH),
        size,
        modified,
        id,
        reading,
    })
}

#[test]
fn a_thumbnail_streams_its_file_to_the_rasteriser_and_draws_its_reply() {
    let mut files = Files::holding(vec![1, 2, 3, 4, 5, 6, 7]);
    let mut decoder = Decoder::default();
    let picture = render_artwork(
        &mut files,
        &mut decoder,
        &key(7, WRITTEN, Reading::Sprite),
        SIDE,
    )
    .expect("a picture");
    assert_eq!((picture.width(), picture.height()), (SIDE, SIDE));
    assert_eq!(
        decoder.streamed,
        [1, 2, 3, 4, 5, 6, 7],
        "streamed whole, in order"
    );
    assert_eq!(decoder.readings, [Reading::Sprite], "read as the key says");
}

/// A file changed since it was listed is not the picture its key names, so it
/// is not drawn under that key.
#[test]
fn a_file_changed_since_its_listing_is_not_drawn() {
    let mut decoder = Decoder::default();
    let mut grown = Files::holding(vec![0; 8]);
    assert!(render_artwork(
        &mut grown,
        &mut decoder,
        &key(7, WRITTEN, Reading::Signature),
        SIDE
    )
    .is_none());
    let mut touched = Files::holding(vec![0; 7]);
    let later = Time64::from_secs(1_700_000_001);
    assert!(render_artwork(
        &mut touched,
        &mut decoder,
        &key(7, later, Reading::Signature),
        SIDE
    )
    .is_none());
    assert!(decoder.readings.is_empty(), "neither reached the decode");
}

/// A name now naming another file — swapped since the listing for one of the
/// same length and time — is not the picture its key names, and is not read.
#[test]
fn a_file_replaced_since_its_listing_is_not_drawn() {
    let mut decoder = Decoder::default();
    let mut swapped = Files::holding(vec![0; 7]);
    swapped.id = FileId {
        volume: [7; 16],
        node: 43,
    };
    assert!(render_artwork(
        &mut swapped,
        &mut decoder,
        &key(7, WRITTEN, Reading::Signature),
        SIDE
    )
    .is_none());
    assert!(decoder.readings.is_empty() && decoder.streamed.is_empty());
}

/// A listing naming no file gives an open nothing to be checked against, so
/// nothing is opened.
#[test]
fn a_listing_naming_no_file_opens_nothing() {
    let mut decoder = Decoder::default();
    let mut files = Files::holding(vec![0; 7]);
    files.id = FileId::NONE;
    assert!(render_artwork(
        &mut files,
        &mut decoder,
        &key_of(7, WRITTEN, FileId::NONE, Reading::Signature),
        SIDE
    )
    .is_none());
    assert_eq!(files.opens, 0);
}

#[test]
fn a_file_past_the_bound_is_not_streamed() {
    let mut files = Files::holding(Vec::new());
    let mut decoder = Decoder::default();
    let over = MAX_THUMBNAIL_BYTES + 1;
    // The stamp is the open handle's, so a file that claims the bound and is
    // over it is refused without a byte being read.
    files.bytes = vec![0; usize::try_from(over).expect("fits")];
    assert!(render_artwork(
        &mut files,
        &mut decoder,
        &key(over, WRITTEN, Reading::Signature),
        SIDE
    )
    .is_none());
    assert!(decoder.streamed.is_empty());
}

#[test]
fn a_reply_of_the_wrong_length_draws_nothing() {
    let mut files = Files::holding(vec![9; 4]);
    let mut decoder = Decoder {
        short: true,
        ..Decoder::default()
    };
    assert!(render_artwork(
        &mut files,
        &mut decoder,
        &key(4, WRITTEN, Reading::Signature),
        SIDE
    )
    .is_none());
}

/// A reader and a rasteriser of icons alone draw no thumbnail: every picture
/// file falls to its class picture.
#[test]
fn the_default_seams_draw_no_thumbnail() {
    struct Icons;
    impl ArtworkReader for Icons {
        fn read(&mut self, _path: &str) -> Option<Vec<u8>> {
            None
        }
    }
    impl ArtworkRasteriser for Icons {
        fn rasterise(&mut self, _side: u32, _bytes: &[u8]) -> Option<Vec<u8>> {
            None
        }
    }
    let thumbnail = key(7, WRITTEN, Reading::Signature);
    assert!(render_artwork(&mut Icons, &mut Decoder::default(), &thumbnail, SIDE).is_none());
    let mut files = Files::holding(vec![0; 7]);
    assert!(render_artwork(&mut files, &mut Icons, &thumbnail, SIDE).is_none());
}

/// A thread that owes a frame cannot afford a whole file's read and decode, so
/// the inline resolver declines a thumbnail without opening the file.
#[test]
fn the_inline_resolver_declines_a_thumbnail_without_opening_it() {
    let mut files = Files::holding(vec![0; 7]);
    let mut decoder = Decoder::default();
    let mut inline = InlineArtwork::new(&mut files, &mut decoder);
    assert!(matches!(
        inline.resolve(&key(7, WRITTEN, Reading::Signature), SIDE),
        Resolved::Done(None)
    ));
    assert_eq!(files.opens, 0);
}

fn icon(path: &str) -> ArtworkKey {
    ArtworkKey::Asset(String::from(path))
}

/// Every icon is handed out before any thumbnail, however they were asked.
#[test]
fn a_thumbnail_waits_behind_every_icon() {
    let mut desk = ArtworkDesk::new();
    let thumbnail = key(7, WRITTEN, Reading::Signature);
    desk.want(&thumbnail, SIDE);
    desk.want(&icon("/a.png"), SIDE);
    assert!(desk.has_work());
    let first = desk.next_job().expect("the icon");
    assert_eq!(first.key, icon("/a.png"));
    assert_eq!(desk.next_job(), None, "a thumbnail is never an icon job");
    assert_eq!(
        desk.next_thumbnail(),
        Some(ArtworkJob {
            key: thumbnail,
            side: SIDE
        })
    );
    assert!(!desk.has_work());
}

/// Icons land as a batch, but a waiting thumbnail does not hold that batch
/// back, and each thumbnail is shown as it lands.
#[test]
fn an_icon_batch_is_not_held_back_by_waiting_thumbnails() {
    let mut desk = ArtworkDesk::new();
    let (first, second) = (
        key(7, WRITTEN, Reading::Signature),
        key(8, WRITTEN, Reading::Signature),
    );
    desk.want(&first, SIDE);
    desk.want(&second, SIDE);
    desk.want(&icon("/a.png"), SIDE);
    let job = desk.next_job().expect("the icon");
    assert!(desk.deliver(&job, None).wake(), "the icons drained");
    let job = desk.next_thumbnail().expect("a thumbnail");
    assert!(
        desk.deliver(&job, None).wake(),
        "a thumbnail is shown though another waits"
    );
}
