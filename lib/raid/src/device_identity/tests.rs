//! Host tests for the identity a composed array states: its identifier and
//! what each slot's device states, in slot order, and none while any slot
//! proves nothing.

use alloc::vec;
use alloc::vec::Vec;

use crate::{ArrayUuid, MirrorMember, OwnedRaidArray, StripeMember};

use tairix_abi::blkio::{
    serve_request_recovering, Answer, BlkDeviceClass, BlkDeviceIdentity, BlkHealth, BlkOp,
    BlkPublication, BlkRequest, BLK_COMPLETION_LEN, BLK_DEVICE_IDENTITY_LEN, BLK_REQUEST_LEN,
};
use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::DriverError;

/// Bytes in one logical block of every member double.
const BLOCK_LEN: usize = 64;
/// Logical blocks in every member double.
const BLOCKS: usize = 8;
/// The stripe unit of the striped arrays here, in logical blocks.
const CHUNK: u32 = 2;

/// The array the identities here are composed for, and another.
const ARRAY: ArrayUuid = [0xA1; 16];
const OTHER_ARRAY: ArrayUuid = [0xB2; 16];

/// What member doubles state: two devices an array is composed of, and a
/// third that takes the place of one.
const UNIT_A: Option<BlkDeviceIdentity> = BlkDeviceIdentity::new([0x0A; BLK_DEVICE_IDENTITY_LEN]);
const UNIT_B: Option<BlkDeviceIdentity> = BlkDeviceIdentity::new([0x0B; BLK_DEVICE_IDENTITY_LEN]);
const UNIT_C: Option<BlkDeviceIdentity> = BlkDeviceIdentity::new([0x0C; BLK_DEVICE_IDENTITY_LEN]);

/// A member device holding its blocks in memory and stating `identity`, as
/// the driver of a device that proves its medium does.
struct Unit {
    identity: Option<BlkDeviceIdentity>,
    store: Vec<u8>,
}

impl Unit {
    fn stating(identity: Option<BlkDeviceIdentity>) -> Self {
        Self {
            identity,
            store: vec![0; BLOCK_LEN * BLOCKS],
        }
    }

    /// The bytes a transfer of `len` from block `lba` covers, or why not.
    fn span(&self, lba: u64, len: usize) -> Result<core::ops::Range<usize>, DriverError> {
        let start = usize::try_from(lba)
            .ok()
            .and_then(|lba| lba.checked_mul(BLOCK_LEN))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(len)
            .filter(|&end| end <= self.store.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        Ok(start..end)
    }
}

impl Block for Unit {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: u32::try_from(BLOCK_LEN).map_err(|_| DriverError::OutOfRange)?,
            block_count: u64::try_from(BLOCKS).map_err(|_| DriverError::OutOfRange)?,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        buf.copy_from_slice(&self.store[span]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        self.store[span].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    fn device_identity(&self) -> Option<BlkDeviceIdentity> {
        self.identity
    }
}

/// A whole two-copy mirror over devices stating `identities`, in slot order.
fn mirror(identities: [Option<BlkDeviceIdentity>; 2]) -> OwnedRaidArray<Unit> {
    let members = identities
        .into_iter()
        .map(|identity| MirrorMember::new(Unit::stating(identity)))
        .collect();
    OwnedRaidArray::assemble_mirror(members).expect("two healthy copies assemble")
}

/// What `array` states as the array `uuid`, each member stating what its own
/// device states.
fn stated(array: &mut OwnedRaidArray<Unit>, uuid: &ArrayUuid) -> Option<BlkDeviceIdentity> {
    array.composed_identity(uuid, |member| member.device_identity())
}

#[test]
fn the_same_devices_in_the_same_slots_state_one_identity_however_often_assembled() {
    let first = stated(&mut mirror([UNIT_A, UNIT_B]), &ARRAY);
    assert!(
        first.is_some(),
        "an array every member of which states one states one"
    );
    assert_eq!(stated(&mut mirror([UNIT_A, UNIT_B]), &ARRAY), first);
}

#[test]
fn another_member_another_order_or_another_array_states_another_identity() {
    let first = stated(&mut mirror([UNIT_A, UNIT_B]), &ARRAY);
    for other in [[UNIT_A, UNIT_C], [UNIT_C, UNIT_B], [UNIT_B, UNIT_A]] {
        assert_ne!(stated(&mut mirror(other), &ARRAY), first, "{other:?}");
    }
    assert_ne!(
        stated(&mut mirror([UNIT_A, UNIT_B]), &OTHER_ARRAY),
        first,
        "the same devices in another array"
    );
}

#[test]
fn a_member_that_states_none_leaves_the_array_stating_none() {
    for identities in [[UNIT_A, None], [None, UNIT_B], [None, None]] {
        assert_eq!(
            stated(&mut mirror(identities), &ARRAY),
            None,
            "{identities:?}"
        );
    }
}

#[test]
fn a_slot_with_no_device_states_none_until_a_device_fills_it() {
    let mut array = OwnedRaidArray::assemble_mirror(vec![
        MirrorMember::new(Unit::stating(UNIT_A)),
        MirrorMember::absent(),
    ])
    .expect("one copy of a two-copy mirror serves");
    assert_eq!(stated(&mut array, &ARRAY), None);
    array
        .add_member(1, Unit::stating(UNIT_B))
        .expect("a spare fills the empty slot");
    assert_eq!(
        stated(&mut array, &ARRAY),
        stated(&mut mirror([UNIT_A, UNIT_B]), &ARRAY),
        "how a slot came to hold its device, and what the device is doing, do not enter it"
    );
}

#[test]
fn a_stripe_states_what_its_members_do() {
    let stripe = |identities: [Option<BlkDeviceIdentity>; 2]| {
        let members = identities
            .into_iter()
            .map(|identity| StripeMember::new(Unit::stating(identity)))
            .collect();
        OwnedRaidArray::assemble_stripe(members, CHUNK).expect("two members stripe")
    };
    assert!(stated(&mut stripe([UNIT_A, UNIT_B]), &ARRAY).is_some());
    assert_eq!(stated(&mut stripe([UNIT_A, None]), &ARRAY), None);
}

#[test]
fn a_stating_array_states_what_it_was_given_and_serves_as_the_array() {
    let mut array = mirror([UNIT_A, UNIT_B]);
    let identity = stated(&mut array, &ARRAY);
    let mut frame = [0u8; BLK_REQUEST_LEN];
    BlkRequest {
        op: BlkOp::Identity,
        lba: 0,
        blocks: 0,
        binding: 0,
    }
    .encode(&mut frame)
    .expect("a request frame is exactly wide enough");
    let mut window = vec![0xEEu8; BLOCK_LEN];
    let mut reply = [0u8; BLK_COMPLETION_LEN];
    let mut health = BlkHealth::new(BlkDeviceClass::Virtual);
    let answer = serve_request_recovering(
        &mut array.stating(identity),
        BlkPublication::default(),
        &frame,
        &mut window,
        &mut reply,
        &mut health,
        0,
    );
    assert!(matches!(answer, Answer::Reply(_)));
    assert_eq!(
        &window[..BLK_DEVICE_IDENTITY_LEN],
        &BlkDeviceIdentity::encode(identity)
    );

    array
        .stating(identity)
        .write_blocks(1, &[0x5A; BLOCK_LEN])
        .expect("a write through it reaches the array");
    let mut read = [0u8; BLOCK_LEN];
    array
        .read_blocks(1, &mut read)
        .expect("and reads back from it");
    assert_eq!(read, [0x5A; BLOCK_LEN]);
    let geometry = array.stating(None).geometry();
    assert_eq!(geometry, array.geometry());
}
