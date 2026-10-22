use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::DriverError;
use tairix_writeback::RUN_BYTES;

use super::ByteDevice;

/// An in-memory device that records every request as `(write, lba, bytes)`.
struct Recording {
    block_size: u32,
    data: Vec<u8>,
    requests: Vec<(bool, u64, usize)>,
}

impl Recording {
    fn new(block_size: u32, blocks: usize) -> Self {
        let data = (0..block_size as usize * blocks).map(pattern).collect();
        Self {
            block_size,
            data,
            requests: Vec::new(),
        }
    }

    fn span(&self, lba: u64, len: usize) -> core::ops::Range<usize> {
        let at = usize::try_from(lba).expect("a test-sized device") * self.block_size as usize;
        assert_eq!(len % self.block_size as usize, 0, "whole blocks only");
        at..at + len
    }
}

impl Block for Recording {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: (self.data.len() / self.block_size as usize) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.requests.push((false, lba, buf.len()));
        let span = self.span(lba, buf.len());
        buf.copy_from_slice(&self.data[span]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.requests.push((true, lba, buf.len()));
        let span = self.span(lba, buf.len());
        self.data[span].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// The byte a fresh [`Recording`] holds at `i`.
fn pattern(i: usize) -> u8 {
    u8::try_from(i % 251).expect("below 251")
}

fn expected(offset: usize, len: usize) -> Vec<u8> {
    (offset..offset + len).map(pattern).collect()
}

#[test]
fn an_aligned_range_reads_straight_through_in_one_request() {
    let mut dev = ByteDevice::new(Recording::new(512, 64)).expect("valid geometry");
    let mut buf = vec![0u8; 8 * 512];
    dev.read(4 * 512, &mut buf).expect("in range");
    assert_eq!(buf, expected(4 * 512, 8 * 512));
    assert_eq!(dev.device().requests, [(false, 4, 8 * 512)]);
}

#[test]
fn an_unaligned_range_stages_only_its_ends() {
    let mut dev = ByteDevice::new(Recording::new(2048, 16)).expect("valid geometry");
    let mut buf = vec![0u8; 2048 * 3 + 100];
    dev.read(1000, &mut buf).expect("in range");
    assert_eq!(buf, expected(1000, buf.len()));
    // The partial head, the two whole blocks after it in one run, the tail.
    assert_eq!(
        dev.device().requests,
        [(false, 0, 2048), (false, 1, 2 * 2048), (false, 3, 2048)]
    );
}

#[test]
fn a_long_read_is_split_into_runs() {
    let blocks = 3 * RUN_BYTES / 512;
    let mut dev = ByteDevice::new(Recording::new(512, blocks)).expect("valid geometry");
    let mut buf = vec![0u8; 3 * RUN_BYTES];
    dev.read(0, &mut buf).expect("in range");
    assert_eq!(buf, expected(0, buf.len()));
    let runs = &dev.device().requests;
    assert_eq!(runs.len(), 3);
    assert!(runs.iter().all(|&(_, _, len)| len == RUN_BYTES));
}

#[test]
fn a_partial_write_keeps_the_bytes_around_it() {
    let mut dev = ByteDevice::new(Recording::new(512, 8)).expect("valid geometry");
    dev.write(700, &[0xEE; 900]).expect("in range");
    let data = &dev.device().data;
    assert_eq!(&data[..700], &expected(0, 700)[..]);
    assert!(data[700..1600].iter().all(|&b| b == 0xEE));
    assert_eq!(&data[1600..], &expected(1600, data.len() - 1600)[..]);
}

#[test]
fn a_range_outside_the_device_is_refused_before_any_transfer() {
    let mut dev = ByteDevice::new(Recording::new(512, 4)).expect("valid geometry");
    let mut buf = [0u8; 16];
    assert_eq!(
        dev.read(4 * 512 - 8, &mut buf),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        dev.write(u64::MAX - 4, &buf),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(dev.zero(0, 4 * 512 + 1), Err(DriverError::LengthOutOfRange));
    assert!(dev.device().requests.is_empty());
}

#[test]
fn zeroing_clears_exactly_its_range_and_coalesces_the_middle() {
    let mut dev = ByteDevice::new(Recording::new(512, 16)).expect("valid geometry");
    dev.zero(300, 4 * 512).expect("in range");
    let data = &dev.device().data;
    assert_eq!(&data[..300], &expected(0, 300)[..]);
    assert!(data[300..300 + 4 * 512].iter().all(|&b| b == 0));
    assert_eq!(
        &data[300 + 4 * 512..],
        &expected(300 + 4 * 512, data.len() - 300 - 4 * 512)[..]
    );
    let whole_writes: Vec<_> = dev
        .device()
        .requests
        .iter()
        .filter(|&&(write, _, len)| write && len > 512)
        .collect();
    assert_eq!(whole_writes, [&(true, 1, 3 * 512)]);
}

#[test]
fn zeroing_inside_one_block_touches_only_that_block() {
    let mut dev = ByteDevice::new(Recording::new(512, 4)).expect("valid geometry");
    dev.zero(520, 10).expect("in range");
    let data = &dev.device().data;
    assert!(data[520..530].iter().all(|&b| b == 0));
    assert_eq!(data[519], expected(519, 1)[0]);
    assert_eq!(data[530], expected(530, 1)[0]);
    assert_eq!(dev.device().requests, [(false, 1, 512), (true, 1, 512)]);
}

#[test]
fn a_geometry_the_stack_cannot_stage_is_refused() {
    for block_size in [0, 256, 768, 8192] {
        assert_eq!(
            ByteDevice::new(Recording::new(block_size.max(1), 0)).err(),
            Some(DriverError::Unsupported),
            "block size {block_size}"
        );
    }
}
