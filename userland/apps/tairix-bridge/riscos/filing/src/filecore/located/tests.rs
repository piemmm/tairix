//! The search for what holds a byte, over a disc in memory.

use alloc::vec;
use alloc::vec::Vec;

use tairix_filecore::dir::Object;
use tairix_filecore::format::{lay_out, new_map_record, BootRecord, Variant};
use tairix_filecore::newmap::NewMap;
use tairix_filecore::volume::{Backing, Volume};
use tairix_filecore::Disc;
use tairix_fsmeta::preset::acorn::ATTR_DIRECTORY;

use super::{find, Answer, Name, OBJECT};

/// A disc held in memory, which refuses what lies past it.
struct Memory(Vec<u8>);

impl Memory {
    fn span(&self, at: u64, len: usize) -> Result<core::ops::Range<usize>, ()> {
        let start = usize::try_from(at).map_err(drop)?;
        let end = start.checked_add(len).ok_or(())?;
        (end <= self.0.len()).then_some(start..end).ok_or(())
    }
}

impl Disc for Memory {
    type Error = ();

    fn capacity(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), ()> {
        let span = self.span(at, out.len())?;
        out.copy_from_slice(&self.0[span]);
        Ok(())
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), ()> {
        let span = self.span(at, bytes.len())?;
        self.0[span].copy_from_slice(bytes);
        Ok(())
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), ()> {
        let span = self.span(at, usize::try_from(length).map_err(drop)?)?;
        self.0[span].fill(0);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ()> {
        Ok(())
    }
}

/// `name`, `size` bytes at `indaddr`, entered in the directory at `parent`.
fn enter(
    volume: &mut Volume<Memory>,
    parent: u32,
    name: &[u8],
    indaddr: u32,
    size: u32,
    dir: bool,
) {
    let mut object = Object::named(name).expect("a name");
    object.indaddr = indaddr;
    object.size = size;
    if dir {
        object.attr = ATTR_DIRECTORY;
    }
    volume.dir_insert(parent, &object).expect("entered");
}

#[test]
fn a_directory_named_from_within_itself_is_searched_once() {
    let size = 0x4_0000;
    let mut record = new_map_record(Variant::RamDisc, size).expect("a record");
    record.disc_name = *b"Probe     ";
    let mut disc = Memory(vec![0; usize::try_from(size).expect("a test size")]);
    let record = lay_out(&mut disc, record, BootRecord::Blank, b"$").expect("laid out");
    let map = NewMap::open(&mut disc, record).expect("its map");
    let boot_block = record.nzones > 1;
    let mut volume = Volume::open(disc, Backing::New { map, boot_block }).expect("its root");
    let root = volume.root_addr();
    let (a, bytes) = volume.new_directory(root, b"A").expect("a directory");
    enter(&mut volume, root, b"A", a, bytes, true);
    // Two entries naming A itself would double the search at every level.
    enter(&mut volume, a, b"X", a, bytes, true);
    enter(&mut volume, a, b"Y", a, bytes, true);
    let z = volume.place_object(a, 100).expect("placed");
    enter(&mut volume, a, b"Z", z, 100, false);
    let marker = b"held by Z alone";
    volume.object_write(z, 0, marker).expect("written");
    let offset = volume
        .disc()
        .0
        .windows(marker.len())
        .position(|bytes| bytes == marker)
        .expect("Z's data on the disc");
    let mut name = Name::new(u32::MAX);
    let answer = find(&mut volume, b"RAM", offset as u64, &mut name).expect("searched");
    assert!(matches!(answer, Answer::Kind(OBJECT)));
    assert_eq!(name.written(), b"RAM::Probe.$.A.Z\0");
}
