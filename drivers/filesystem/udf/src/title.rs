//! The volume's title: the logical volume identifier the logical volume
//! descriptor records, with the copies the logical volume information and
//! the file set descriptor keep, and the primary volume descriptor's shorter
//! volume identifier (ECMA-167 3/10.1, 3/10.6 and 4/14.1; OSTA UDF 2.60
//! 2.2.7.2).

use core::ops::Range;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{FilesystemLabel, VolumeTitle};
use tairix_abi::DriverError;
use tairix_fsprobe::udf::{
    ExtentAd, Tag, MAX_VDS_SECTORS, TAG_FSD, TAG_IUVD, TAG_LVD, TAG_PVD, TAG_TD, TAG_VDP,
};
pub(crate) use tairix_fsprobe::udf::{
    LOGICAL_ID_LEN, LVD_LOGICAL_ID, PVD_VOLUME_ID, VOLUME_ID_LEN,
};

use crate::fields::Fields;
use crate::{dir, zeroed, Udf};

/// Where the logical volume information records its copy.
pub(crate) const IUVD_LOGICAL_ID: Range<usize> = 116..116 + LOGICAL_ID_LEN;

/// Where a file set descriptor records its copy.
pub(crate) const FSD_LOGICAL_ID: Range<usize> = 112..112 + LOGICAL_ID_LEN;

/// The implementation identifier the logical volume information is recorded
/// under.
pub(crate) const LV_INFO: &[u8] = b"*UDF LV Info";

/// Where an implementation use descriptor's implementation identifier
/// begins.
const IMPLEMENTATION_ID_AT: usize = 21;

/// A title as its descriptors record it: whole as the logical volume
/// identifier, and as much of it as fits as the volume identifier.
pub(crate) struct Titles {
    /// The logical volume identifier.
    pub logical: [u8; LOGICAL_ID_LEN],
    /// The volume identifier.
    pub volume: [u8; VOLUME_ID_LEN],
}

impl Titles {
    /// The identifiers of `title`. The volume identifier's 32 bytes hold 30
    /// eight-bit characters or 15 UTF-16 units, so a longer title is recorded
    /// there as its longest leading part in whole characters, its closing
    /// spaces dropped; the title is the logical volume identifier, which every
    /// reader takes it from.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] for a title past the logical
    ///   volume identifier's 126 eight-bit characters or 63 UTF-16 units.
    /// * [`DriverError::OutOfRange`] for one closing with a space, which
    ///   readers take for padding.
    pub(crate) fn of(title: &str) -> Result<Self, DriverError> {
        if title.ends_with(' ') {
            return Err(DriverError::OutOfRange);
        }
        Ok(Self {
            logical: dstring(title)?,
            volume: dstring(leading(title, VOLUME_ID_LEN).trim_end_matches(' '))?,
        })
    }
}

/// A `dstring` of `N` bytes holding `text`: its OSTA Compressed Unicode,
/// zeros after it and its length in the last byte, or all zeros for none.
///
/// # Errors
///
/// [`DriverError::LengthOutOfRange`] for text the field cannot hold.
pub(crate) fn dstring<const N: usize>(text: &str) -> Result<[u8; N], DriverError> {
    let mut field = [0u8; N];
    if text.is_empty() {
        return Ok(field);
    }
    let identifier = dir::encode(text).ok_or(DriverError::LengthOutOfRange)?;
    if identifier.len() >= N {
        return Err(DriverError::LengthOutOfRange);
    }
    field[..identifier.len()].copy_from_slice(&identifier);
    field[N - 1] = u8::try_from(identifier.len()).map_err(|_| DriverError::LengthOutOfRange)?;
    Ok(field)
}

/// The longest leading part of `text`, in whole characters, a `dstring` of
/// `len` bytes holds: past its compression ID and length byte, a byte a
/// character while every character is narrow, and two a UTF-16 unit once one
/// is not.
pub(crate) fn leading(text: &str, len: usize) -> &str {
    let room = len.saturating_sub(2);
    let (mut chars, mut units, mut wide) = (0, 0, false);
    let mut end = 0;
    for (at, ch) in text.char_indices() {
        chars += 1;
        units += ch.len_utf16();
        wide |= !dir::is_narrow(ch);
        let bytes = if wide { 2 * units } else { chars };
        if bytes > room {
            break;
        }
        end = at + ch.len_utf8();
    }
    &text[..end]
}

/// Whether the implementation use descriptor `block` is the logical volume
/// information.
fn is_lv_info(block: &[u8]) -> bool {
    block.get(IMPLEMENTATION_ID_AT..IMPLEMENTATION_ID_AT + LV_INFO.len()) == Some(LV_INFO)
}

/// Seal the descriptor `block` again as `tag`, its CRC over the bytes its
/// own length field says it covers.
fn reseal(block: &mut [u8], tag: Tag) -> Result<(), DriverError> {
    let covered = usize::from(Fields(block).u16(10)?);
    tag.seal(block, covered)
}

impl<B: Block> Udf<B> {
    /// Record `identifiers` in every primary, logical volume and logical
    /// volume information descriptor of the sequence at `extent`, each
    /// resealed: followed through its continuations to its terminator,
    /// within the bound the probe reads a sequence in.
    fn retitle_sequence(
        &mut self,
        mut extent: ExtentAd,
        identifiers: &Titles,
    ) -> Result<(), DriverError> {
        let bs = u64::from(self.block_size);
        let mut block = zeroed(self.block_size)?;
        let mut read = 0;
        'sequence: loop {
            for index in 0..u64::from(extent.length) / bs {
                read += 1;
                if read > MAX_VDS_SECTORS {
                    break 'sequence;
                }
                let sector = u64::from(extent.location) + index;
                let location = u32::try_from(sector).map_err(|_| DriverError::DeviceFault)?;
                self.read_sector(sector, &mut block)?;
                let Some(tag) = Tag::verify(&block, location) else {
                    break 'sequence;
                };
                let (field, value) = match tag.id {
                    TAG_PVD => (PVD_VOLUME_ID, &identifiers.volume[..]),
                    TAG_LVD => (LVD_LOGICAL_ID, &identifiers.logical[..]),
                    TAG_IUVD if is_lv_info(&block) => (IUVD_LOGICAL_ID, &identifiers.logical[..]),
                    TAG_VDP => {
                        extent = ExtentAd::at(&block, 20).ok_or(DriverError::DeviceFault)?;
                        continue 'sequence;
                    }
                    TAG_TD => break 'sequence,
                    _ => continue,
                };
                block[field].copy_from_slice(value);
                reseal(&mut block, tag)?;
                self.write_descriptor(sector, &block)?;
            }
            break;
        }
        Ok(())
    }

    /// Record `logical` in the file set descriptor that prevails.
    fn retitle_file_set(&mut self, logical: &[u8]) -> Result<(), DriverError> {
        let (at, _) = self.file_set_root()?;
        let mut block = zeroed(self.block_size)?;
        self.read_block(at, &mut block)?;
        let tag = Tag::verify(&block, at.block)
            .filter(|tag| tag.id == TAG_FSD)
            .ok_or(DriverError::DeviceFault)?;
        block[FSD_LOGICAL_ID].copy_from_slice(logical);
        reseal(&mut block, tag)?;
        self.write_block(at, &block)
    }
}

/// A retitle is a change like any other, the integrity descriptor open
/// around it, and rewrites every copy of the identifiers in both descriptor
/// sequences and the file set descriptor; the mount then reads the volume
/// with the identifiers it recorded, as the probe reads them.
impl<B: Block> FilesystemLabel for Udf<B> {
    fn label(&mut self) -> Result<VolumeTitle, DriverError> {
        let (label, len) = self.volume.label();
        let text = core::str::from_utf8(&label[..len]).map_err(|_| DriverError::OutOfRange)?;
        VolumeTitle::new(text).map_err(DriverError::from_errno)
    }

    fn set_label(&mut self, title: &VolumeTitle) -> Result<(), DriverError> {
        let identifiers = Titles::of(title.as_str())?;
        let anchor = self.volume.anchor;
        self.change(|fs| {
            fs.retitle_sequence(anchor.main, &identifiers)?;
            fs.retitle_sequence(anchor.reserve, &identifiers)?;
            fs.retitle_file_set(&identifiers.logical)
        })?;
        self.volume
            .retitle(&identifiers.logical, &identifiers.volume);
        Ok(())
    }
}
