use super::*;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::BlockGeometry;
use tairix_writeback::RUN_BYTES;

use crate::tests::{SizedBlock, ONE_GROUP_SECTORS, TEST_UUID};

/// A device of `sector`-byte sectors that records every request's length.
struct Recording {
    data: Vec<u8>,
    sector: u32,
    reads: Vec<usize>,
    writes: Vec<usize>,
}

impl Recording {
    fn new(bytes: usize, sector: u32) -> Self {
        Self {
            data: (0..bytes)
                .map(|i| u8::try_from(i % 251).expect("below 251"))
                .collect(),
            sector,
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    fn span(&self, lba: u64, len: usize) -> Result<core::ops::Range<usize>, DriverError> {
        let ss = self.sector as usize;
        if !len.is_multiple_of(ss) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba).map_err(|_| DriverError::OutOfRange)? * ss;
        let end = start + len;
        if end > self.data.len() {
            return Err(DriverError::OutOfRange);
        }
        Ok(start..end)
    }
}

impl Block for Recording {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.sector,
            block_count: (self.data.len() / self.sector as usize) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        self.reads.push(buf.len());
        buf.copy_from_slice(&self.data[span]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        self.writes.push(buf.len());
        self.data[span].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

#[test]
fn whole_blocks_move_in_requests_of_at_most_one_coalesced_transfer() {
    let mut dev = Device::new(Recording::new(1 << 20, 512)).expect("device");
    dev.set_fs_block(4096);
    let mut buf = vec![0u8; 200 * 1024];
    dev.read_blocks(3, &mut buf).expect("read");
    let disk = dev.bytes.device();
    assert_eq!(&buf[..], &disk.data[3 * 4096..3 * 4096 + buf.len()]);
    assert!(disk.reads.iter().all(|&len| len <= RUN_BYTES));
    assert_eq!(disk.reads.len(), buf.len().div_ceil(RUN_BYTES));
    dev.write_blocks(10, &buf).expect("write");
    assert_eq!(
        dev.bytes.device().writes.len(),
        buf.len().div_ceil(RUN_BYTES)
    );
}

#[test]
fn a_filesystem_block_smaller_than_a_sector_is_read_through_its_sector() {
    let mut dev = Device::new(Recording::new(64 * 1024, 4096)).expect("device");
    dev.set_fs_block(1024);
    let mut block = [0u8; 1024];
    dev.read_blocks(5, &mut block).expect("read");
    assert_eq!(&block[..], &dev.bytes.device().data[5 * 1024..6 * 1024]);
    let fresh = [7u8; 1024];
    dev.write_blocks(5, &fresh).expect("write");
    let data = &dev.bytes.device().data;
    assert_eq!(&data[5 * 1024..6 * 1024], &fresh[..]);
    assert_eq!(
        data[4 * 1024],
        (4 * 1024 % 251) as u8,
        "its sector-mate kept"
    );
}

#[test]
fn a_block_whose_place_overflows_or_leaves_the_device_is_refused() {
    let mut dev = Device::new(Recording::new(8192, 512)).expect("device");
    dev.set_fs_block(4096);
    let mut buf = [0u8; 4096];
    assert_eq!(
        dev.read_blocks(2, &mut buf),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        dev.write_blocks(u64::MAX / 1024, &buf),
        Err(DriverError::DeviceFault)
    );
    assert!(
        dev.bytes.device().writes.is_empty(),
        "nothing reached the device"
    );
}

#[test]
fn metadata_reads_see_the_transactions_own_changes_first() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let bs = fs.layout.block_size as usize;
    let block = 300;
    let mut buf = vec![0u8; bs];
    assert!(
        fs.read_meta(block, &mut buf).expect("read"),
        "from the device"
    );
    fs.put_meta(block, &[0x5A; 1024][..bs]).expect("staged");
    assert!(
        !fs.read_meta(block, &mut buf).expect("read"),
        "the staged copy"
    );
    assert!(buf.iter().all(|&b| b == 0x5A));
    assert_eq!(
        fs.read_meta(0, &mut buf),
        Err(DriverError::DeviceFault),
        "block 0"
    );
    let past = fs.layout.blocks_count;
    assert_eq!(fs.read_meta(past, &mut buf), Err(DriverError::DeviceFault));
    assert_eq!(fs.put_meta(past, &buf), Err(DriverError::DeviceFault));
}

#[test]
fn a_read_only_handle_stages_nothing_and_writes_no_data() {
    let fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let mut fs = Ext4::open_read_only(fs.into_block().expect("commit")).expect("opens");
    assert_eq!(fs.put_meta(300, &[0u8; 1024]), Err(DriverError::ReadOnly));
    assert_eq!(
        fs.write_data(300, 0, &[0u8; 1024]),
        Err(DriverError::ReadOnly)
    );
    assert!(fs.dirty.is_empty());
    assert_eq!(fs.write_refusal(), Some("opened_read_only"));
}

/// File data is written only inside the volume, wherever a write starts in
/// its first block: a mapping that runs past the end is damage.
#[test]
fn data_reaching_past_the_volume_is_refused() {
    let mut fs = Ext4::format(SizedBlock::new(ONE_GROUP_SECTORS), 256, TEST_UUID).expect("format");
    let last = fs.layout.blocks_count - 1;
    fs.write_data(last, 0, &[0u8; 1024])
        .expect("the last block");
    for (phys, within, len) in [(last, 0, 2048), (last, 1000, 100), (0, 0, 1024)] {
        assert_eq!(
            fs.write_data(phys, within, &vec![0u8; len]),
            Err(DriverError::DeviceFault),
            "{phys}+{within}+{len}"
        );
    }
}
