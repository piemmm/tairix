//! Acorn ADFS / RISC OS `FileCore` recognition, shared by the probe and the
//! ADFS driver so the two agree on what an ADFS volume is and which identity
//! it carries. Where the structures lie is `lib/filecore`'s to find.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
pub use tairix_filecore::identify::Layout;

use crate::content_identity;

/// Recognise an ADFS volume on `dev`, as `FileCore` finds one.
///
/// # Errors
///
/// Only a device fault: a structure that lies beyond the device is simply
/// absent.
pub fn identify<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<Layout>, DriverError> {
    tairix_filecore::identify::identify(dev).map_err(DriverError::from)
}

/// The volume's stable identity.
///
/// ADFS records no unique identifier — the disc id is cycled on mutation — so
/// the identity is a digest of what formatting fixes and neither an ordinary
/// write nor a retitle changes: a new map's disc record without its disc id,
/// boot option, name or big root's size, and an old map's size and directory
/// form. Two discs formatted alike share it.
#[must_use]
pub fn identity(layout: &Layout) -> [u8; 16] {
    match layout {
        Layout::NewMap { record, .. } => {
            let mut stable = record.encode();
            stable[0x14..0x16].fill(0);
            stable[0x16..0x20].fill(0);
            stable[7] = 0;
            stable[0x30..0x34].fill(0);
            content_identity(b"tairix.adfs.newmap", &[&stable])
        }
        Layout::OldMap {
            large_directories,
            disc_sectors,
            ..
        } => {
            let directories = [u8::from(*large_directories)];
            let sectors = disc_sectors.to_le_bytes();
            content_identity(b"tairix.adfs.oldmap", &[&directories, &sectors])
        }
    }
}
