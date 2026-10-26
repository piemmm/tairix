extern crate std;

use core::cell::RefCell;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::block::{Block, DiscardCapability};
use tairix_abi::driver::BufferClass;
use tairix_abi::volume::{VolumeCtl, VolumeCtlOp, VolumeInfo, VOLUME_IO_MAX};
use tairix_abi::{DriverError, Errno};

use super::{GrantedVolume, VolumeAccess};

const BLOCK: u32 = 4096;

/// What the fake kernel was asked: each transfer's first block and length,
/// and each control.
#[derive(Debug, Eq, PartialEq)]
enum Call {
    Read(u64, usize),
    Write(u64, usize),
    Ctl(VolumeCtlOp, u64, u64),
}

/// A kernel serving one in-memory volume, recording every call.
struct FakeKernel {
    disk: RefCell<Vec<u8>>,
    calls: RefCell<Vec<Call>>,
    info: Option<VolumeInfo>,
    refuse: Option<Errno>,
}

impl FakeKernel {
    fn new(blocks: u64) -> Self {
        Self {
            disk: RefCell::new(vec![0; usize::try_from(blocks).unwrap() * BLOCK as usize]),
            calls: RefCell::new(Vec::new()),
            info: Some(VolumeInfo {
                block_size: BLOCK,
                block_count: blocks,
                read_only: false,
                discard: DiscardCapability {
                    supported: true,
                    granularity_blocks: 1,
                    max_blocks_per_request: 0,
                },
                disc_layout: None,
                medium: None,
            }),
            refuse: None,
        }
    }

    fn span(lba: u64, len: usize) -> core::ops::Range<usize> {
        let at = usize::try_from(lba).unwrap() * BLOCK as usize;
        at..at + len
    }
}

impl VolumeAccess for &FakeKernel {
    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), Errno> {
        assert!(
            buf.len() <= VOLUME_IO_MAX,
            "one call moves at most the bound"
        );
        self.calls.borrow_mut().push(Call::Read(lba, buf.len()));
        if let Some(err) = self.refuse {
            return Err(err);
        }
        buf.copy_from_slice(&self.disk.borrow()[FakeKernel::span(lba, buf.len())]);
        Ok(())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<(), Errno> {
        assert!(
            buf.len() <= VOLUME_IO_MAX,
            "one call moves at most the bound"
        );
        self.calls.borrow_mut().push(Call::Write(lba, buf.len()));
        if let Some(err) = self.refuse {
            return Err(err);
        }
        self.disk.borrow_mut()[FakeKernel::span(lba, buf.len())].copy_from_slice(buf);
        Ok(())
    }

    fn ctl(&self, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno> {
        self.calls
            .borrow_mut()
            .push(Call::Ctl(ctl.op, ctl.lba, ctl.blocks));
        match ctl.op {
            VolumeCtlOp::Info => Ok(self.info),
            _ => self.refuse.map_or(Ok(None), Err),
        }
    }
}

#[test]
fn the_volume_is_the_device_the_kernel_describes() {
    let kernel = FakeKernel::new(64);
    let volume = GrantedVolume::open(&kernel).unwrap();
    let geometry = volume.geometry().unwrap();
    assert_eq!((geometry.block_size, geometry.block_count), (BLOCK, 64));
    assert!(volume.discard_capability().unwrap().supported);
    assert_eq!(volume.info().block_count, 64);
    assert_eq!(*kernel.calls.borrow(), [Call::Ctl(VolumeCtlOp::Info, 0, 0)]);
}

#[test]
fn an_answer_that_is_no_description_opens_nothing() {
    let mut kernel = FakeKernel::new(8);
    kernel.info = None;
    assert_eq!(
        GrantedVolume::open(&kernel).err(),
        Some(DriverError::DeviceFault)
    );
}

#[test]
fn a_transfer_past_one_call_crosses_in_whole_blocks_and_round_trips() {
    let kernel = FakeKernel::new(256);
    let mut volume = GrantedVolume::open(&kernel).unwrap();
    let len = VOLUME_IO_MAX * 2 + 3 * BLOCK as usize;
    let body: Vec<u8> = (0..len).map(|n| u8::try_from(n % 253).unwrap()).collect();
    volume.write_blocks(5, &body).unwrap();
    let mut back = vec![0u8; len];
    volume.read_blocks(5, &mut back).unwrap();
    assert_eq!(back, body);
    let per_call = (VOLUME_IO_MAX / BLOCK as usize) as u64;
    let calls = kernel.calls.borrow();
    assert_eq!(
        calls[1..4],
        [
            Call::Write(5, VOLUME_IO_MAX),
            Call::Write(5 + per_call, VOLUME_IO_MAX),
            Call::Write(5 + 2 * per_call, 3 * BLOCK as usize),
        ]
    );
    assert_eq!(calls.len(), 7, "the info, three writes and three reads");
}

#[test]
fn flush_and_discard_are_the_volume_controls() {
    let kernel = FakeKernel::new(16);
    let mut volume = GrantedVolume::open(&kernel).unwrap();
    volume.flush().unwrap();
    volume.discard(3, 4).unwrap();
    assert_eq!(
        kernel.calls.borrow()[1..],
        [
            Call::Ctl(VolumeCtlOp::Flush, 0, 0),
            Call::Ctl(VolumeCtlOp::Discard, 3, 4)
        ]
    );
}

#[test]
fn a_refusal_reaches_the_format_as_its_driver_error() {
    let mut kernel = FakeKernel::new(16);
    kernel.refuse = Some(Errno::ReadOnly);
    let mut volume = GrantedVolume::open(&kernel).unwrap();
    assert_eq!(
        volume.write_blocks(0, &[0; BLOCK as usize]),
        Err(DriverError::from_errno(Errno::ReadOnly))
    );
    assert_eq!(
        volume.flush(),
        Err(DriverError::from_errno(Errno::ReadOnly))
    );
}

#[test]
fn a_sensitive_payload_never_crosses_into_the_kernels_staging() {
    let kernel = FakeKernel::new(16);
    let mut volume = GrantedVolume::open(&kernel).unwrap();
    let mut buf = vec![0u8; BLOCK as usize];
    assert_eq!(
        volume.read_blocks_with_class(0, &mut buf, BufferClass::Sensitive),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        volume.write_blocks_with_class(0, &buf, BufferClass::Sensitive),
        Err(DriverError::Unsupported)
    );
    volume
        .write_blocks_with_class(0, &buf, BufferClass::NonSensitive)
        .unwrap();
    assert_eq!(
        kernel.calls.borrow().len(),
        2,
        "only the ordinary write left"
    );
}
