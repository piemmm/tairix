extern crate std;

use alloc::sync::Arc;
use alloc::vec;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::volume::{VolumeCtl, VolumeCtlOp};
use tairix_abi::{DriverError, Errno};

use super::ServedVolume;
use crate::test_support::{RamBlock, BLOCK_SIZE};

/// A RAM device that says when it is let go.
struct Tracked {
    block: RamBlock,
    dropped: Arc<AtomicBool>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl Block for Tracked {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.block.geometry()
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.block.read_blocks(lba, buf)
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.block.write_blocks(lba, buf)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn volume(blocks: u64, read_only: bool) -> (ServedVolume<Tracked>, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    let device = Tracked {
        block: RamBlock::new(blocks),
        dropped: Arc::clone(&dropped),
    };
    (
        ServedVolume::new(7, device, read_only).expect("a served volume"),
        dropped,
    )
}

fn read(volume: &ServedVolume<Tracked>, lba: u64, out: &mut [u8]) -> Result<(), Errno> {
    let len = out.len();
    volume.read(lba, len, &mut |offset, chunk| {
        out[offset..offset + chunk.len()].copy_from_slice(chunk);
        Ok(())
    })
}

fn write(volume: &ServedVolume<Tracked>, lba: u64, data: &[u8]) -> Result<(), Errno> {
    volume.write(lba, data.len(), &mut |offset, chunk| {
        let len = chunk.len();
        chunk.copy_from_slice(&data[offset..offset + len]);
        Ok(())
    })
}

#[test]
fn whole_blocks_of_the_volume_cross_in_staged_chunks() {
    let (volume, _) = volume(512, false);
    let body: std::vec::Vec<u8> = (0..200 * BLOCK_SIZE)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    write(&volume, 3, &body).expect("a write past one staged chunk");
    let mut back = vec![0u8; body.len()];
    read(&volume, 3, &mut back).expect("read back");
    assert_eq!(back, body);
}

#[test]
fn a_transfer_outside_whole_blocks_of_the_volume_is_refused() {
    let (volume, _) = volume(16, false);
    let mut buf = vec![0u8; BLOCK_SIZE];
    assert_eq!(read(&volume, 16, &mut buf), Err(Errno::LengthOutOfRange));
    assert_eq!(
        read(&volume, u64::MAX, &mut buf),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        read(&volume, 0, &mut buf[..BLOCK_SIZE - 1]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(read(&volume, 0, &mut []), Err(Errno::LengthOutOfRange));
    let discard = |lba, blocks| {
        volume.ctl(&VolumeCtl {
            op: VolumeCtlOp::Discard,
            lba,
            blocks,
        })
    };
    assert_eq!(discard(10, 7), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_read_only_volume_refuses_every_write_and_describes_itself() {
    let (volume, _) = volume(16, true);
    assert_eq!(write(&volume, 0, &[0; BLOCK_SIZE]), Err(Errno::ReadOnly));
    assert_eq!(
        volume.ctl(&VolumeCtl {
            op: VolumeCtlOp::Discard,
            lba: 0,
            blocks: 1,
        }),
        Err(Errno::ReadOnly)
    );
    let info = volume
        .ctl(&VolumeCtl {
            op: VolumeCtlOp::Info,
            lba: 0,
            blocks: 0,
        })
        .expect("described")
        .expect("a description");
    assert!(info.read_only);
    assert_eq!(info.block_count, 16);
    assert_eq!(info.block_size as usize, BLOCK_SIZE);
}

#[test]
fn a_volume_retired_under_a_transfer_lets_its_window_go_with_its_last_holder() {
    let (volume, dropped) = volume(16, false);
    let volume = Arc::new(volume);
    let (inside, entered) = mpsc::channel();
    let (go, gate) = mpsc::channel::<()>();
    let transfer = {
        let volume = Arc::clone(&volume);
        thread::spawn(move || {
            let mut buf = vec![0u8; BLOCK_SIZE];
            let len = buf.len();
            volume.read(0, len, &mut |offset, chunk| {
                inside.send(()).unwrap();
                gate.recv().unwrap();
                buf[offset..offset + chunk.len()].copy_from_slice(chunk);
                Ok(())
            })
        })
    };
    entered.recv().unwrap();
    volume.retire();
    assert!(volume.retired());
    go.send(()).unwrap();
    assert_eq!(
        transfer.join().unwrap(),
        Ok(()),
        "the transfer under way ends"
    );
    let mut buf = vec![0u8; BLOCK_SIZE];
    assert_eq!(
        read(&volume, 0, &mut buf),
        Err(Errno::DeviceOffline),
        "every later one fails closed"
    );
    assert!(!dropped.load(Ordering::SeqCst), "held while referenced");
    drop(volume);
    assert!(
        dropped.load(Ordering::SeqCst),
        "the window goes with the last holder"
    );
}
