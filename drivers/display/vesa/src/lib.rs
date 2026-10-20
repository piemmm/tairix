//! TAIRiX VESA (VBE) linear-framebuffer display driver.
//!
//! On a legacy x86_64 PC the kernel cannot re-enter real mode to issue VBE
//! BIOS calls, so the boot stub asks the firmware for a 32-bit direct-colour
//! linear-framebuffer mode and hands the resulting **VBE `ModeInfoBlock`**
//! (the 256-byte record VBE function `0x4F01` fills in) to the driver host.
//! This driver owns the VBE-specific decode — the linear-framebuffer
//! attribute, the direct-colour memory model, the channel mask sizes and
//! positions — that a block needs before its surface can be trusted, and
//! drives the surface through the shared linear-framebuffer engine
//! ([`tairix_display::Framebuffer`]).
//!
//! # Public surface
//!
//! [`register`], and [`open`], through which the driver host instantiates the
//! surface from a boot-supplied block.
//!
//! # Capabilities
//!
//! Loading requires [`CapabilityId::DRV_LOAD`]; mapping the surface requires
//! [`CapabilityId::MMIO_MAP`], through the capability-gated mapper, never a
//! pointer synthesised from the block's `PhysBasePtr`. The driver runs in
//! user space and does not request `CAP_DRV_KERNEL`.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

use tairix_abi::driver::display::DisplayFormat;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_display::{BufferMemory, Framebuffer, FramebufferConfig};

#[cfg(test)]
mod tests;

/// Per-driver `DriverHandle` marker returned by [`register`].
///
/// Mirrors the convention the framebuffer and bus drivers use: the host
/// re-issues a host-local handle when binding the driver into its load
/// table; this constant is the on-the-wire signal that every load-time
/// gate cleared. The low bytes spell `"VESA"`.
const REGISTER_HANDLE_MARKER: u64 = 0x5645_5341_0000_0001;

/// Wire length in bytes of a VBE `ModeInfoBlock` (VBE 3.0 §4.3.2).
pub const VBE_MODE_INFO_BLOCK_LEN: usize = 256;

/// `ModeAttributes` bit 0 — the mode is supported by the hardware.
const MODE_ATTR_SUPPORTED: u16 = 1 << 0;
/// `ModeAttributes` bit 7 — a linear (flat) framebuffer is available.
const MODE_ATTR_LINEAR_FRAMEBUFFER: u16 = 1 << 7;
/// `MemoryModel` value 6 — direct colour (the only model abi-v1 presents).
const MEMORY_MODEL_DIRECT_COLOUR: u8 = 6;
/// The only bit depth the abi-v1 [`DisplayFormat`] surface covers.
const SUPPORTED_BITS_PER_PIXEL: u8 = 32;
/// Channel mask width every supported 32-bpp direct-colour mode uses.
const SUPPORTED_CHANNEL_MASK_BITS: u8 = 8;

/// Byte offsets of the `ModeInfoBlock` fields this driver reads
/// (VBE 3.0 §4.3.2). Fields the driver does not consume are skipped.
mod field {
    /// `ModeAttributes` (u16).
    pub const MODE_ATTRIBUTES: usize = 0x00;
    /// `BytesPerScanLine` (u16) — the surface stride.
    pub const BYTES_PER_SCAN_LINE: usize = 0x10;
    /// `XResolution` (u16) — surface width in pixels.
    pub const X_RESOLUTION: usize = 0x12;
    /// `YResolution` (u16) — surface height in pixels.
    pub const Y_RESOLUTION: usize = 0x14;
    /// `BitsPerPixel` (u8).
    pub const BITS_PER_PIXEL: usize = 0x19;
    /// `MemoryModel` (u8).
    pub const MEMORY_MODEL: usize = 0x1B;
    /// `RedMaskSize` (u8).
    pub const RED_MASK_SIZE: usize = 0x1F;
    /// `RedFieldPosition` (u8).
    pub const RED_FIELD_POSITION: usize = 0x20;
    /// `GreenMaskSize` (u8).
    pub const GREEN_MASK_SIZE: usize = 0x21;
    /// `GreenFieldPosition` (u8).
    pub const GREEN_FIELD_POSITION: usize = 0x22;
    /// `BlueMaskSize` (u8).
    pub const BLUE_MASK_SIZE: usize = 0x23;
    /// `BlueFieldPosition` (u8).
    pub const BLUE_FIELD_POSITION: usize = 0x24;
    /// `PhysBasePtr` (u32) — physical base of the linear framebuffer
    /// (VBE 2.0+).
    pub const PHYS_BASE_PTR: usize = 0x28;
}

/// Driver entry point.
///
/// # Errors
///
/// * [`DriverError::PermissionDenied`] if the host did not grant
///   [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

fn read_u16(block: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([block[off], block[off + 1]])
}

fn read_u32(block: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([block[off], block[off + 1], block[off + 2], block[off + 3]])
}

/// Decode a VBE `ModeInfoBlock` into the surface it describes.
///
/// The geometry itself is validated where the surface is opened.
///
/// # Errors
///
/// * [`DriverError::BufferTooSmall`] for a block shorter than
///   [`VBE_MODE_INFO_BLOCK_LEN`].
/// * [`DriverError::Unsupported`] for a mode that is not supported, not a
///   linear framebuffer, not 32-bit direct colour, or not a known 8-bit
///   channel layout.
/// * [`DriverError::DeviceFault`] for a block naming no surface.
pub fn parse_mode_info(block: &[u8]) -> Result<FramebufferConfig, DriverError> {
    if block.len() < VBE_MODE_INFO_BLOCK_LEN {
        return Err(DriverError::BufferTooSmall);
    }
    let attributes = read_u16(block, field::MODE_ATTRIBUTES);
    if attributes & MODE_ATTR_SUPPORTED == 0
        || attributes & MODE_ATTR_LINEAR_FRAMEBUFFER == 0
        || block[field::MEMORY_MODEL] != MEMORY_MODEL_DIRECT_COLOUR
        || block[field::BITS_PER_PIXEL] != SUPPORTED_BITS_PER_PIXEL
    {
        return Err(DriverError::Unsupported);
    }
    let format = parse_format(block)?;
    let phys_base = u64::from(read_u32(block, field::PHYS_BASE_PTR));
    if phys_base == 0 {
        return Err(DriverError::DeviceFault);
    }
    Ok(FramebufferConfig {
        phys_base,
        width_px: u32::from(read_u16(block, field::X_RESOLUTION)),
        height_px: u32::from(read_u16(block, field::Y_RESOLUTION)),
        stride_bytes: u32::from(read_u16(block, field::BYTES_PER_SCAN_LINE)),
        format,
    })
}

fn parse_format(block: &[u8]) -> Result<DisplayFormat, DriverError> {
    let masks_are_8bit = block[field::RED_MASK_SIZE] == SUPPORTED_CHANNEL_MASK_BITS
        && block[field::GREEN_MASK_SIZE] == SUPPORTED_CHANNEL_MASK_BITS
        && block[field::BLUE_MASK_SIZE] == SUPPORTED_CHANNEL_MASK_BITS;
    if !masks_are_8bit {
        return Err(DriverError::Unsupported);
    }
    match (
        block[field::RED_FIELD_POSITION],
        block[field::GREEN_FIELD_POSITION],
        block[field::BLUE_FIELD_POSITION],
    ) {
        (16, 8, 0) => Ok(DisplayFormat::Bgra8888),
        (0, 8, 16) => Ok(DisplayFormat::Rgba8888),
        _ => Err(DriverError::Unsupported),
    }
}

/// Decode `mode_info_block` and open the surface it describes, its
/// presenter's buffers allocated in `memory`.
///
/// # Errors
///
/// Everything [`parse_mode_info`] and [`Framebuffer::open`] can return.
pub fn open<'h, M: BufferMemory>(
    host: &'h dyn DriverHost,
    mode_info_block: &[u8],
    memory: M,
) -> Result<Framebuffer<'h, M>, DriverError> {
    Framebuffer::open(host, parse_mode_info(mode_info_block)?, memory)
}
