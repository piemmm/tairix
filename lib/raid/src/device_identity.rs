//! The identity a composed array states as one block device
//! (`docs/src/filesystem/drives.md` §8).
//!
//! A volume's writes held while its medium was away are replayed onto the
//! medium that returns only when its device proves it is the one they were
//! made on ([`Block::device_identity`]). What tells an array from every other
//! is its own identifier with what tells each of its members from every
//! other, slot by slot, so that is what it states — and nothing while any slot
//! holds no device or one that proves nothing, since no proof is better than a
//! guessed one.

use tairix_abi::blkio::{BlkDeviceIdentity, BlkDeviceName, BlkMedium, BlkReissue};
use tairix_abi::driver::block::{
    Block, BlockGeometry, DeviceHealth, DiscLayout, DiscardCapability,
};
use tairix_abi::driver::{BufferClass, DriverError};
use tairix_abi::sysinfo::MountAvailability;
use tairix_crypto::Fingerprint;

use crate::owned::OwnedRaidArray;
use crate::superblock::ArrayUuid;

#[cfg(test)]
mod tests;

/// The domain an array's identity is fingerprinted under, apart from every
/// other kind of thing a fingerprint names.
const ARRAY_DOMAIN: &[u8] = b"tairix.raid.array";

/// The identity of the array `array` whose slots' devices state `members`, in
/// slot order: `None` when any states none.
fn compose(
    array: &ArrayUuid,
    members: impl ExactSizeIterator<Item = Option<BlkDeviceIdentity>>,
) -> Option<BlkDeviceIdentity> {
    let count = u32::try_from(members.len()).ok()?;
    let mut fingerprint = Fingerprint::new(ARRAY_DOMAIN);
    fingerprint.field(array);
    fingerprint.field(&count.to_le_bytes());
    for member in members {
        fingerprint.field(&BlkDeviceIdentity::encode(Some(member?)));
    }
    BlkDeviceIdentity::new(fingerprint.finish())
}

impl<B: Block> OwnedRaidArray<B> {
    /// The identity this array states as one device: its identifier `array`
    /// fingerprinted with what each slot's device states, read by `stated`, in
    /// slot order; `None` while a slot holds no device or one stating none.
    ///
    /// Neither the order members arrived in, what each is doing, nor the
    /// array's generation enters it, so the same devices in the same slots of
    /// the same array state one identity however often the array is assembled,
    /// and another device in any slot, or the same devices in another array,
    /// never state it. `stated` is the caller's because a member may be reached
    /// through a layer that states nothing of its own, as a window past its
    /// reserved metadata is.
    #[must_use]
    pub fn composed_identity(
        &mut self,
        array: &ArrayUuid,
        mut stated: impl FnMut(&mut B) -> Option<BlkDeviceIdentity>,
    ) -> Option<BlkDeviceIdentity> {
        self.with_array(|view| {
            let count = view.member_count();
            compose(
                array,
                (0..count).map(|index| view.member_device_mut(index).and_then(&mut stated)),
            )
        })
    }

    /// This array as the block-service seam serves it, stating `identity`:
    /// what [`composed_identity`](Self::composed_identity) composed.
    #[must_use]
    pub fn stating(&mut self, identity: Option<BlkDeviceIdentity>) -> StatingArray<'_, B> {
        StatingArray {
            array: self,
            identity,
        }
    }
}

/// An owned composed array as the block-service seam serves it
/// ([`OwnedRaidArray::stating`]): every operation is the array's own, and the
/// identity it states is the one its members composed.
pub struct StatingArray<'a, B: Block> {
    array: &'a mut OwnedRaidArray<B>,
    identity: Option<BlkDeviceIdentity>,
}

impl<B: Block> Block for StatingArray<'_, B> {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.array.geometry()
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.array.read_blocks(lba, buf)
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.array.write_blocks(lba, buf)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.array.flush()
    }

    fn read_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &mut [u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        self.array.read_blocks_with_class(lba, buf, class)
    }

    fn write_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &[u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        self.array.write_blocks_with_class(lba, buf, class)
    }

    fn discard_capability(&self) -> Result<DiscardCapability, DriverError> {
        self.array.discard_capability()
    }

    fn discard(&mut self, lba: u64, blocks: u64) -> Result<(), DriverError> {
        self.array.discard(lba, blocks)
    }

    fn device_health(&self) -> Result<DeviceHealth, DriverError> {
        self.array.device_health()
    }

    fn medium(&self) -> Option<BlkMedium> {
        self.array.medium()
    }

    fn disc_layout(&self) -> Option<DiscLayout> {
        self.array.disc_layout()
    }

    fn device_name(&self) -> BlkDeviceName {
        self.array.device_name()
    }

    fn device_identity(&self) -> Option<BlkDeviceIdentity> {
        self.identity
    }

    fn backing_availability(&self) -> MountAvailability {
        self.array.backing_availability()
    }

    fn reissue(&mut self) -> BlkReissue {
        self.array.reissue()
    }
}
