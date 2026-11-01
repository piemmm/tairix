//! Laying out an empty volume, as AmigaDOS's formatter does: the boot block
//! naming the type, the root at the volume's middle, the bitmap after it.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::affs::{
    put_be32, root_block, seal, DateStamp, Offsets, BITMAP_CHECKSUM, BITMAP_PAGES, BITMAP_VALID,
    BLOCK_SIZES, BOOT_BLOCKS, CHECKSUM, ST_ROOT, T_HEADER,
};

use crate::bitmap::bits_per_page;
use crate::header::low32;
use crate::name::Name;

/// The fewest blocks a volume is laid out on: the boot blocks, a root, a
/// bitmap block and room for a few objects.
const MIN_BLOCKS: u64 = 8;

/// Where an empty volume's root, bitmap and bitmap extensions lie.
struct Layout {
    /// The block size, in bytes.
    size: u64,
    blocks: u64,
    root: u64,
    /// The bitmap's pages, which follow the root.
    pages: u64,
    /// The pages one extension block points at.
    per_extension: u64,
    /// The extension blocks, which follow the bitmap.
    extensions: u64,
}

impl Layout {
    /// The layout of a volume on `capacity` bytes in blocks laid out as `at`.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a device too small or too large
    /// for a volume.
    fn of(capacity: u64, at: Offsets) -> Result<Self, DriverError> {
        let size = at.block_size as u64;
        let blocks = capacity / size;
        if !(MIN_BLOCKS..=u64::from(u32::MAX) + 1).contains(&blocks) {
            return Err(DriverError::LengthOutOfRange);
        }
        let pages = (blocks - BOOT_BLOCKS).div_ceil(bits_per_page(at.block_size));
        let per_extension = (at.longs() - 1) as u64;
        let layout = Self {
            size,
            blocks,
            root: root_block(blocks),
            pages,
            per_extension,
            extensions: pages
                .saturating_sub(BITMAP_PAGES as u64)
                .div_ceil(per_extension),
        };
        if layout.end() > blocks {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(layout)
    }

    const fn first_page(&self) -> u64 {
        self.root + 1
    }

    const fn first_extension(&self) -> u64 {
        self.first_page() + self.pages
    }

    /// The block after the last extension: the root and every block of the
    /// bitmap lie below it, and from it on every block is free.
    const fn end(&self) -> u64 {
        self.first_extension() + self.extensions
    }
}

/// How an empty volume is laid out.
#[derive(Copy, Clone, Debug)]
pub struct FormatOptions<'a> {
    /// The type, the boot block's fourth byte: `DOS\0` to `DOS\3`.
    pub dos_type: u8,
    /// The block size: 512 for a floppy, up to 32768 for a partition.
    pub block_size: u32,
    /// The volume's name, ISO 8859-1.
    pub name: &'a [u8],
    /// When it is formatted: its creation and alteration dates.
    pub created: DateStamp,
}

/// Lay `options`' volume out on `device`, answering the device.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a type past `DOS\3` — a directory-cache
/// or long-filename volume this driver does not write — or a block size the
/// format has not, or a name no volume may take; [`DriverError::LengthOutOfRange`] for
/// a device too small or too large for a volume; the device's error.
pub(crate) fn format_volume<B: Block>(
    device: B,
    options: &FormatOptions<'_>,
) -> Result<B, DriverError> {
    if options.dos_type > 3 || !BLOCK_SIZES.contains(&options.block_size) {
        return Err(DriverError::OutOfRange);
    }
    let name = Name::recorded(options.name)
        .map_err(|_| DriverError::OutOfRange)?
        .given()?;
    let mut dev = ByteDevice::new(device)?;
    let at = Offsets::new(options.block_size as usize);
    let layout = Layout::of(dev.capacity(), at)?;
    let mut block =
        tairix_util::fallible::filled(at.block_size, 0u8).ok_or(DriverError::OutOfMemory)?;
    write_boot(&mut dev, &layout, options.dos_type, &block)?;
    write_root(
        &mut dev,
        &layout,
        at,
        name.as_bytes(),
        options.created,
        &mut block,
    )?;
    write_bitmap(&mut dev, &layout, &mut block)?;
    write_extensions(&mut dev, &layout, &mut block)?;
    dev.flush()?;
    Ok(dev.into_inner())
}

/// The boot blocks: zero but for the type and, for readers that look, the
/// root's block.
fn write_boot<B: Block>(
    dev: &mut ByteDevice<B>,
    layout: &Layout,
    dos_type: u8,
    zero: &[u8],
) -> Result<(), DriverError> {
    for boot in 0..BOOT_BLOCKS {
        dev.write(boot * layout.size, zero)?;
    }
    let mut boot = [0u8; 12];
    boot[..3].copy_from_slice(b"DOS");
    boot[3] = dos_type;
    put_be32(&mut boot, 8, low32(layout.root));
    dev.write(0, &boot)
}

/// The root: an empty table, where the bitmap lies, the dates and the name.
fn write_root<B: Block>(
    dev: &mut ByteDevice<B>,
    layout: &Layout,
    at: Offsets,
    name: &[u8],
    created: DateStamp,
    block: &mut [u8],
) -> Result<(), DriverError> {
    block.fill(0);
    put_be32(block, 0, T_HEADER);
    put_be32(block, 12, low32(at.table_len() as u64));
    put_be32(block, at.bm_flag(), BITMAP_VALID);
    let held = layout.pages.min(BITMAP_PAGES as u64);
    for (slot, page) in (0..held).enumerate() {
        put_be32(
            block,
            at.bm_pages() + 4 * slot,
            low32(layout.first_page() + page),
        );
    }
    if layout.extensions > 0 {
        put_be32(block, at.bm_ext(), low32(layout.first_extension()));
    }
    for date in [at.date(), at.volume_date(), at.created()] {
        created.write(block, date);
    }
    block[at.name()] = u8::try_from(name.len()).map_err(|_| DriverError::OutOfRange)?;
    block[at.name() + 1..at.name() + 1 + name.len()].copy_from_slice(name);
    put_be32(block, at.sec_type(), ST_ROOT);
    seal(block, CHECKSUM);
    dev.write(layout.root * layout.size, block)
}

/// The bitmap, a set bit a free block: every block is free but the root's,
/// the bitmap's own and its extensions', which lie together from the root.
fn write_bitmap<B: Block>(
    dev: &mut ByteDevice<B>,
    layout: &Layout,
    block: &mut [u8],
) -> Result<(), DriverError> {
    let bits = bits_per_page(block.len());
    for page in 0..layout.pages {
        let first = BOOT_BLOCKS + page * bits;
        for (word, long) in (0u64..).zip(block[4..].as_chunks_mut::<4>().0) {
            let lo = first + 32 * word;
            let free = span_bits(lo, BOOT_BLOCKS, layout.root)
                | span_bits(lo, layout.end(), layout.blocks);
            *long = free.to_be_bytes();
        }
        seal(block, BITMAP_CHECKSUM);
        dev.write((layout.first_page() + page) * layout.size, block)?;
    }
    Ok(())
}

/// The bits of the bitmap long whose bit 0 is block `lo` that lie in
/// `start..end`.
fn span_bits(lo: u64, start: u64, end: u64) -> u32 {
    let from = start.max(lo);
    let to = end.min(lo + 32);
    if from >= to {
        return 0;
    }
    let ones = u32::MAX >> (32 - (to - from));
    ones << (from - lo)
}

/// The extension blocks: the bitmap's pages past the root's own pointers,
/// each block's last long naming the next.
fn write_extensions<B: Block>(
    dev: &mut ByteDevice<B>,
    layout: &Layout,
    block: &mut [u8],
) -> Result<(), DriverError> {
    let next = 4 * usize::try_from(layout.per_extension).map_err(|_| DriverError::OutOfRange)?;
    for extension in 0..layout.extensions {
        block.fill(0);
        let first = BITMAP_PAGES as u64 + extension * layout.per_extension;
        let last = layout.pages.min(first + layout.per_extension);
        for (slot, page) in (first..last).enumerate() {
            put_be32(block, 4 * slot, low32(layout.first_page() + page));
        }
        if extension + 1 < layout.extensions {
            put_be32(block, next, low32(layout.first_extension() + extension + 1));
        }
        dev.write((layout.first_extension() + extension) * layout.size, block)?;
    }
    Ok(())
}
