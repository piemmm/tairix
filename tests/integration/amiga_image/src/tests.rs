use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::FilesystemRead;
use tairix_abi::DriverError;
use tairix_drv_fs_affs::Affs;
use tairix_fsprobe::affs::{be32, root_block, sums_to_zero, Offsets};

use super::*;

/// The disk a test lays a volume out on.
struct Disk(Vec<u8>);

impl Disk {
    fn span(&self, lba: u64, len: usize) -> Result<core::ops::Range<usize>, DriverError> {
        let at = usize::try_from(lba)
            .ok()
            .and_then(|lba| lba.checked_mul(BLOCK_SIZE as usize))
            .ok_or(DriverError::OutOfRange)?;
        let end = at.checked_add(len).ok_or(DriverError::OutOfRange)?;
        if end > self.0.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(at..end)
    }
}

impl Block for Disk {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: BLOCK_SIZE,
            block_count: self.0.len() as u64 / u64::from(BLOCK_SIZE),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        buf.copy_from_slice(&self.0[span]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, data: &[u8]) -> Result<(), DriverError> {
        let span = self.span(lba, data.len())?;
        self.0[span].copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

#[test]
fn a_floppy_reopens_holding_its_file_under_its_label() {
    let blocks = usize::try_from(FLOPPY_BLOCKS).expect("a floppy fits");
    let disk = ffs_volume(
        Disk(vec![0; blocks * BLOCK_SIZE as usize]),
        b"Workbench",
        b"ReadMe",
        b"held",
    )
    .expect("lays out");
    assert_eq!(&disk.0[..4], b"DOS\x01", "a Fast File System volume");
    let root = usize::try_from(root_block(FLOPPY_BLOCKS)).expect("on the floppy");
    let label = root * BLOCK_SIZE as usize + Offsets::new(BLOCK_SIZE as usize).name();
    assert_eq!(&disk.0[label..label + 10], b"\x09Workbench");
    let mut fs = Affs::open(disk).expect("reopens");
    let file = fs
        .lookup(fs.root(), b"ReadMe")
        .expect("the file is in the root");
    let mut text = [0u8; 8];
    let read = fs.read_at(file, 0, &mut text).expect("reads");
    assert_eq!(&text[..read], b"held");
}

#[test]
fn a_rigid_disk_block_is_where_it_was_asked_and_sums_to_zero() {
    let at = 3;
    let disk = rdb_disk(16, at);
    assert_eq!(disk.len(), 16 * BLOCK_SIZE as usize);
    let block = &disk[at * BLOCK_SIZE as usize..(at + 1) * BLOCK_SIZE as usize];
    assert_eq!(&block[..4], b"RDSK");
    assert_eq!(be32(block, 4), RDB_SUMMED_LONGS);
    assert_eq!(be32(block, 16), BLOCK_SIZE);
    assert!(sums_to_zero(&block[..RDB_SUMMED_LONGS as usize * 4]));
    assert!(
        disk[..at * BLOCK_SIZE as usize].iter().all(|&b| b == 0),
        "nothing ahead of it"
    );
}
