//! The virtio-gpu 2D command set (virtio 1.2 §5.7.6): the requests this
//! driver sends on the control and cursor queues, and the responses it
//! reads back, as little-endian byte images.
//!
//! Every response is decoded fail-closed: a header naming another response
//! type, or a body shorter than its type implies, is the device's error.

use tairix_abi::driver::display::DamageRect;
use tairix_abi::DriverError;

/// Bytes of every request's and response's header.
pub const HEADER_LEN: usize = 24;

/// The scanouts a display-info response describes.
pub const SCANOUTS_MAX: usize = 16;

/// Bytes of a display-info response: the header and one mode per scanout.
pub const DISPLAY_INFO_LEN: usize = HEADER_LEN + SCANOUTS_MAX * 24;

/// The most bytes of EDID a response carries.
pub const EDID_MAX: usize = 1024;

/// Bytes of an EDID response.
pub const EDID_LEN: usize = HEADER_LEN + 8 + EDID_MAX;

/// Bytes of the longest request this driver sends.
pub const REQUEST_MAX: usize = 56;

/// The device's feature: it answers `GET_EDID`.
pub const FEATURE_EDID: u64 = 1 << 1;

/// The configuration-space event the device raises when a display changed.
pub const EVENT_DISPLAY: u32 = 1;

/// Byte offset of the device configuration's events the device raised.
pub const CONFIG_EVENTS_READ: usize = 0;
/// Byte offset of the device configuration's events the driver clears.
pub const CONFIG_EVENTS_CLEAR: usize = 4;

/// The control queue's index.
pub const CONTROL_QUEUE: u16 = 0;
/// The cursor queue's index.
pub const CURSOR_QUEUE: u16 = 1;

/// A request type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
enum Command {
    GetDisplayInfo = 0x0100,
    ResourceCreate2d = 0x0101,
    ResourceUnref = 0x0102,
    SetScanout = 0x0103,
    ResourceFlush = 0x0104,
    TransferToHost2d = 0x0105,
    ResourceAttachBacking = 0x0106,
    ResourceDetachBacking = 0x0107,
    GetEdid = 0x010A,
    UpdateCursor = 0x0300,
    MoveCursor = 0x0301,
}

/// The response types this driver reads.
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const RESP_OK_EDID: u32 = 0x1104;
const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
const RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

/// A resource's pixel layout, in memory order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Format {
    /// Blue, green, red and alpha bytes: a pointer image.
    B8G8R8A8 = 1,
    /// Blue, green and red bytes and one ignored: a scanned-out frame.
    B8G8R8X8 = 2,
}

/// A request's bytes, and how many of them it is.
pub struct Request {
    bytes: [u8; REQUEST_MAX],
    len: usize,
}

impl Request {
    fn new(command: Command) -> Self {
        let mut request = Self {
            bytes: [0; REQUEST_MAX],
            len: HEADER_LEN,
        };
        request.put(0, command as u32);
        request
    }

    fn put(&mut self, at: usize, value: u32) {
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put64(&mut self, at: usize, value: u64) {
        self.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn rect(&mut self, at: usize, rect: &DamageRect) {
        self.put(at, rect.x);
        self.put(at + 4, rect.y);
        self.put(at + 8, rect.width_px);
        self.put(at + 12, rect.height_px);
    }

    fn with_len(mut self, len: usize) -> Self {
        self.len = len;
        self
    }

    /// The request's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Ask what every scanout shows.
#[must_use]
pub fn get_display_info() -> Request {
    Request::new(Command::GetDisplayInfo)
}

/// Ask for scanout `scanout`'s EDID.
#[must_use]
pub fn get_edid(scanout: u32) -> Request {
    let mut request = Request::new(Command::GetEdid).with_len(HEADER_LEN + 8);
    request.put(HEADER_LEN, scanout);
    request
}

/// Create resource `id`, `width` × `height` pixels laid out as `format`.
#[must_use]
pub fn resource_create_2d(id: u32, format: Format, width: u32, height: u32) -> Request {
    let mut request = Request::new(Command::ResourceCreate2d).with_len(HEADER_LEN + 16);
    request.put(HEADER_LEN, id);
    request.put(HEADER_LEN + 4, format as u32);
    request.put(HEADER_LEN + 8, width);
    request.put(HEADER_LEN + 12, height);
    request
}

/// Destroy resource `id`.
#[must_use]
pub fn resource_unref(id: u32) -> Request {
    let mut request = Request::new(Command::ResourceUnref).with_len(HEADER_LEN + 8);
    request.put(HEADER_LEN, id);
    request
}

/// Back resource `id` with the one span of `len` bytes the device reaches
/// at `addr`.
#[must_use]
pub fn attach_backing(id: u32, addr: u64, len: u32) -> Request {
    let mut request = Request::new(Command::ResourceAttachBacking).with_len(HEADER_LEN + 8 + 16);
    request.put(HEADER_LEN, id);
    request.put(HEADER_LEN + 4, 1);
    request.put64(HEADER_LEN + 8, addr);
    request.put(HEADER_LEN + 16, len);
    request
}

/// Take resource `id`'s backing away: the device reads it no more once the
/// response arrives.
#[must_use]
pub fn detach_backing(id: u32) -> Request {
    let mut request = Request::new(Command::ResourceDetachBacking).with_len(HEADER_LEN + 8);
    request.put(HEADER_LEN, id);
    request
}

/// Show `rect` of resource `id` on scanout `scanout`; resource `0` shows
/// nothing, which switches the scanout off.
#[must_use]
pub fn set_scanout(scanout: u32, id: u32, rect: &DamageRect) -> Request {
    let mut request = Request::new(Command::SetScanout).with_len(HEADER_LEN + 24);
    request.rect(HEADER_LEN, rect);
    request.put(HEADER_LEN + 16, scanout);
    request.put(HEADER_LEN + 20, id);
    request
}

/// Copy `rect` of resource `id` from its backing, where it starts `offset`
/// bytes in, to the host's copy.
#[must_use]
pub fn transfer_to_host_2d(id: u32, rect: &DamageRect, offset: u64) -> Request {
    let mut request = Request::new(Command::TransferToHost2d).with_len(HEADER_LEN + 32);
    request.rect(HEADER_LEN, rect);
    request.put64(HEADER_LEN + 16, offset);
    request.put(HEADER_LEN + 24, id);
    request
}

/// Show what changed of resource `id` within `rect` on every scanout
/// showing it.
#[must_use]
pub fn resource_flush(id: u32, rect: &DamageRect) -> Request {
    let mut request = Request::new(Command::ResourceFlush).with_len(HEADER_LEN + 24);
    request.rect(HEADER_LEN, rect);
    request.put(HEADER_LEN + 16, id);
    request
}

/// Show resource `id` as scanout `scanout`'s pointer, its hot spot
/// `(hot_x, hot_y)` in, at `(x, y)`; resource `0` hides it.
#[must_use]
pub fn update_cursor(scanout: u32, at: (u32, u32), id: u32, hot: (u32, u32)) -> Request {
    cursor(Command::UpdateCursor, scanout, at, id, hot)
}

/// Move scanout `scanout`'s pointer, resource `id` with its hot spot `hot`, to
/// `at`. A device takes any cursor command's resource as whether the pointer
/// is shown, so a move names the one shown, as Linux's driver sends it.
#[must_use]
pub fn move_cursor(scanout: u32, at: (u32, u32), id: u32, hot: (u32, u32)) -> Request {
    cursor(Command::MoveCursor, scanout, at, id, hot)
}

fn cursor(
    command: Command,
    scanout: u32,
    (x, y): (u32, u32),
    id: u32,
    (hot_x, hot_y): (u32, u32),
) -> Request {
    let mut request = Request::new(command).with_len(HEADER_LEN + 32);
    request.put(HEADER_LEN, scanout);
    request.put(HEADER_LEN + 4, x);
    request.put(HEADER_LEN + 8, y);
    request.put(HEADER_LEN + 16, id);
    request.put(HEADER_LEN + 20, hot_x);
    request.put(HEADER_LEN + 24, hot_y);
    request
}

fn read32(bytes: &[u8], at: usize) -> Result<u32, DriverError> {
    let word = bytes
        .get(at..at + 4)
        .and_then(|word| <[u8; 4]>::try_from(word).ok())
        .ok_or(DriverError::DeviceFault)?;
    Ok(u32::from_le_bytes(word))
}

/// What a response's header says of the request.
///
/// # Errors
///
/// The device's refusal, as the driver error it is.
pub fn status(response: &[u8], wanted: u32) -> Result<(), DriverError> {
    match read32(response, 0)? {
        found if found == wanted => Ok(()),
        RESP_ERR_OUT_OF_MEMORY => Err(DriverError::OutOfMemory),
        RESP_ERR_INVALID_SCANOUT_ID | RESP_ERR_INVALID_RESOURCE_ID => Err(DriverError::NotFound),
        RESP_ERR_INVALID_PARAMETER => Err(DriverError::LengthOutOfRange),
        _ => Err(DriverError::DeviceFault),
    }
}

/// Whether a no-data response says the request was done.
///
/// # Errors
///
/// As [`status`].
pub fn ok_nodata(response: &[u8]) -> Result<(), DriverError> {
    status(response, RESP_OK_NODATA)
}

/// One scanout's mode, as a display-info response states it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Scanout {
    /// Its width in pixels; `0` where the device names none.
    pub width: u32,
    /// Its height in pixels; `0` where the device names none.
    pub height: u32,
    /// Whether a display is connected to it.
    pub enabled: bool,
}

/// Every scanout a display-info response describes.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a response of another type or too short.
pub fn display_info(response: &[u8]) -> Result<[Scanout; SCANOUTS_MAX], DriverError> {
    status(response, RESP_OK_DISPLAY_INFO)?;
    if response.len() < DISPLAY_INFO_LEN {
        return Err(DriverError::DeviceFault);
    }
    let mut scanouts = [Scanout::default(); SCANOUTS_MAX];
    for (index, scanout) in scanouts.iter_mut().enumerate() {
        let at = HEADER_LEN + index * 24;
        *scanout = Scanout {
            width: read32(response, at + 8)?,
            height: read32(response, at + 12)?,
            enabled: read32(response, at + 16)? != 0,
        };
    }
    Ok(scanouts)
}

/// The EDID an EDID response carries.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a response of another type, or one
/// claiming more bytes than it holds.
pub fn edid(response: &[u8]) -> Result<&[u8], DriverError> {
    status(response, RESP_OK_EDID)?;
    let size = read32(response, HEADER_LEN)? as usize;
    if size > EDID_MAX {
        return Err(DriverError::DeviceFault);
    }
    response
        .get(HEADER_LEN + 8..HEADER_LEN + 8 + size)
        .ok_or(DriverError::DeviceFault)
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
