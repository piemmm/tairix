//! Attachments: host-visible memory copied straight into and out of memory of
//! the caller's own, on a device in this process and on one another serves.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::gpu::{GPU_ATTACHED_MAX, MAX_CLIENT_ATTACHMENTS};

use crate::device_tests::{contents, context, device, memory};
use crate::testing::Loopback;
use crate::{Attachable, Error};

/// Bytes the served tests' device holds.
const MEMORY: u64 = 8 << 20;
/// The served tests' client region.
const REGION: usize = 1 << 20;

/// Memory a loopback grant names. The bytes kept here are not the region's,
/// which the test reads through the loopback as the server's mapping holds
/// them.
struct Granted {
    grant: u64,
    bytes: Vec<u8>,
}

impl Attachable for Granted {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    fn region(&self) -> Option<u64> {
        Some(self.grant)
    }
}

/// Bytes `0, 1, 2, …` of `len`.
fn counting(len: usize) -> Vec<u8> {
    (0..len)
        .map(|n| u8::try_from(n % 251).expect("small"))
        .collect()
}

#[test]
fn memory_in_this_process_is_copied_straight_into_and_out_of_an_attachment() {
    let (device, _) = device();
    let context = context(&device);
    let memory = memory(&context, 64);
    memory.write(0, &counting(64)).expect("written");
    let mut attached = device.attach(vec![0u8; 32]).expect("attached");
    memory
        .read_into(8, &mut attached, (4, 16))
        .expect("read into it");
    let mut want = vec![0u8; 32];
    want[4..20].copy_from_slice(&counting(64)[8..24]);
    assert_eq!(
        attached.memory(),
        &want,
        "only the bytes named, where named"
    );
    attached.memory_mut()[..8].copy_from_slice(&[0xA5; 8]);
    memory
        .write_from(40, &attached, (0, 8))
        .expect("written from it");
    let held = contents(&memory);
    assert_eq!(held[40..48], [0xA5; 8]);
    assert_eq!(held[..40], counting(64)[..40], "nothing else moved");
}

#[test]
fn a_transfer_past_either_end_is_refused_and_moves_nothing() {
    let (device, _) = device();
    let context = context(&device);
    let memory = memory(&context, 64);
    memory.write(0, &counting(64)).expect("written");
    let mut attached = device.attach(vec![0u8; 16]).expect("attached");
    for (offset, span) in [(0, (8, 9)), (0, (usize::MAX, 2)), (56, (0, 16))] {
        assert_eq!(
            memory.read_into(offset, &mut attached, span),
            Err(Error::OutOfRange),
            "{offset} {span:?}"
        );
        assert_eq!(
            memory.write_from(offset, &attached, span),
            Err(Error::OutOfRange),
            "{offset} {span:?}"
        );
    }
    assert_eq!(attached.memory(), &[0u8; 16]);
    assert_eq!(contents(&memory), counting(64));
}

#[test]
fn an_attachment_of_no_bytes_or_of_another_device_is_refused() {
    let (other, _) = device();
    let (device, _) = device();
    assert_eq!(device.attach(Vec::new()).err(), Some(Error::Empty));
    let context = context(&device);
    let memory = memory(&context, 16);
    let mut foreign = other.attach(vec![0u8; 16]).expect("attached");
    assert_eq!(
        memory.read_into(0, &mut foreign, (0, 4)),
        Err(Error::Foreign)
    );
    assert_eq!(memory.write_from(0, &foreign, (0, 4)), Err(Error::Foreign));
}

#[test]
fn a_served_device_copies_straight_into_and_out_of_the_region_it_maps() {
    let loopback = Loopback::new(MEMORY, Arc::new(tairix_parallel::Serial), None).expect("served");
    let device = loopback.connect(1, REGION).expect("connected");
    let context = context(&device);
    let memory = memory(&context, 64);
    memory.write(0, &counting(64)).expect("written");
    let grant = loopback.grant(1, 32);
    let mut attached = device
        .attach(Granted {
            grant,
            bytes: vec![0; 32],
        })
        .expect("attached");
    memory
        .read_into(16, &mut attached, (8, 16))
        .expect("read into it");
    let mut held = [0u8; 32];
    loopback.read_region(grant, 0, &mut held);
    let mut want = [0u8; 32];
    want[8..24].copy_from_slice(&counting(64)[16..32]);
    assert_eq!(held, want, "the driver copied into the region it maps");
    loopback.write_region(grant, 0, &[0x5A; 4]);
    memory
        .write_from(60, &attached, (0, 4))
        .expect("written from it");
    assert_eq!(contents(&memory)[60..], [0x5A; 4]);
    assert_eq!(
        memory.read_into(0, &mut attached, (24, 9)),
        Err(Error::OutOfRange),
        "past the attachment's end, checked before the device is asked"
    );
}

#[test]
fn served_attachments_are_bounded_their_own_and_let_go() {
    let loopback = Loopback::new(MEMORY, Arc::new(tairix_parallel::Serial), None).expect("served");
    let device = loopback.connect(1, REGION).expect("connected");
    assert_eq!(
        device.attach(vec![0u8; 8]).err(),
        Some(Error::Unsupported),
        "memory only this process reaches"
    );
    let theirs = loopback.grant(2, 8);
    assert_eq!(
        device
            .attach(Granted {
                grant: theirs,
                bytes: vec![0; 8],
            })
            .err(),
        Some(Error::Foreign),
        "a grant another process made"
    );
    let past = loopback.grant(1, 8);
    let huge = usize::try_from(GPU_ATTACHED_MAX + 1).expect("a 64-bit host");
    assert_eq!(
        device
            .attach(Granted {
                grant: past,
                bytes: vec![0; huge],
            })
            .err(),
        Some(Error::TooLarge),
        "past the bytes one client may attach"
    );
    let grants: Vec<u64> = (0..MAX_CLIENT_ATTACHMENTS)
        .map(|_| loopback.grant(1, 8))
        .collect();
    let held: Vec<_> = grants
        .iter()
        .map(|&grant| {
            device
                .attach(Granted {
                    grant,
                    bytes: vec![0; 8],
                })
                .expect("within the bound")
        })
        .collect();
    let one_more = loopback.grant(1, 8);
    assert_eq!(
        device
            .attach(Granted {
                grant: one_more,
                bytes: vec![0; 8],
            })
            .err(),
        Some(Error::TooMany)
    );
    assert_eq!(loopback.region_holders(grants[0]), 1, "the server maps it");
    drop(held);
    assert_eq!(
        loopback.region_holders(grants[0]),
        0,
        "let go with its handle"
    );
    let again = device
        .attach(Granted {
            grant: one_more,
            bytes: vec![0; 8],
        })
        .expect("room again");
    drop(loopback.exit(1));
    assert_eq!(
        loopback.region_holders(one_more),
        0,
        "a client's exit lets its attachments go"
    );
    drop(again);
}
