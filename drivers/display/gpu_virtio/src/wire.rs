//! The virtio-gpu command set (virtio 1.2 §5.7.6): the 2D and 3D requests
//! this driver sends on the control and cursor queues, and the responses it
//! reads back, as little-endian byte images.
//!
//! Every response is decoded fail-closed: a header naming another response
//! type, or a body shorter than its type implies, is the device's error.
//!
//! The device may answer a command before it has run it unless the command
//! is fenced (§5.7.6.7): the driver fences every command it sends on the
//! control queue, and an answer that does not carry its fence is a fault.

use tairix_abi::driver::display::DamageRect;
use tairix_abi::driver::RegionCaching;
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

/// Bytes of the longest request this driver sends: a context's creation.
pub const REQUEST_MAX: usize = HEADER_LEN + 8 + CONTEXT_NAME_MAX;

/// Bytes of a capset-information response.
pub const CAPSET_INFO_LEN: usize = HEADER_LEN + 16;

/// Bytes of a map-information response.
pub const MAP_INFO_LEN: usize = HEADER_LEN + 8;

/// The most bytes of a capset the driver reads: a bound on what a device
/// states, far past the Venus capset's 160.
pub const CAPSET_MAX: usize = 1024;

/// The most bytes of a context's name.
pub const CONTEXT_NAME_MAX: usize = 64;

/// The fence rings a context has: a ring index is below this.
pub const RINGS: u8 = 64;

/// The device's feature: it takes 3D commands.
pub const FEATURE_VIRGL: u64 = 1;

/// The device's feature: it answers `GET_EDID`.
pub const FEATURE_EDID: u64 = 1 << 1;

/// The device's feature: it makes blob resources.
pub const FEATURE_RESOURCE_BLOB: u64 = 1 << 3;

/// The device's feature: a context is created for a capset, with fence rings
/// of its own.
pub const FEATURE_CONTEXT_INIT: u64 = 1 << 4;

/// The Venus capset: serialised Vulkan.
pub const CAPSET_VENUS: u32 = 4;

/// The shared-memory region the device maps host-visible blobs into (virtio
/// 1.2 §5.7, `VIRTIO_GPU_SHM_ID_HOST_VISIBLE`).
pub const SHM_HOST_VISIBLE: u8 = 1;

/// The configuration-space event the device raises when a display changed.
pub const EVENT_DISPLAY: u32 = 1;

/// Byte offset of the device configuration's events the device raised.
pub const CONFIG_EVENTS_READ: usize = 0;
/// Byte offset of the device configuration's events the driver clears.
pub const CONFIG_EVENTS_CLEAR: usize = 4;
/// Byte offset of the device configuration's count of capsets.
pub const CONFIG_NUM_CAPSETS: usize = 12;

/// The header flag asking the device to answer a command only once it has
/// run it, echoing the command's fence.
const FLAG_FENCE: u32 = 1;

/// The header flag placing a command's fence on one of its context's rings.
const FLAG_INFO_RING_IDX: u32 = 2;

/// A blob made by the host, not backed by guest memory.
const BLOB_MEM_HOST3D: u32 = 2;
/// A blob the host lets the driver map.
const BLOB_FLAG_USE_MAPPABLE: u32 = 1;
/// A blob the host lets be shared.
const BLOB_FLAG_USE_SHAREABLE: u32 = 2;

/// The caching a map-information response states, in its low bits.
const MAP_CACHE_MASK: u32 = 0x0f;
const MAP_CACHE_CACHED: u32 = 1;
const MAP_CACHE_UNCACHED: u32 = 2;
const MAP_CACHE_WC: u32 = 3;

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
    GetCapsetInfo = 0x0108,
    GetCapset = 0x0109,
    GetEdid = 0x010A,
    ResourceCreateBlob = 0x010C,
    CtxCreate = 0x0200,
    CtxDestroy = 0x0201,
    Submit3d = 0x0207,
    ResourceMapBlob = 0x0208,
    ResourceUnmapBlob = 0x0209,
    UpdateCursor = 0x0300,
    MoveCursor = 0x0301,
}

/// The response types this driver reads.
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const RESP_OK_CAPSET_INFO: u32 = 0x1102;
const RESP_OK_CAPSET: u32 = 0x1103;
const RESP_OK_EDID: u32 = 0x1104;
const RESP_OK_MAP_INFO: u32 = 0x1106;
const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
const RESP_ERR_INVALID_CONTEXT_ID: u32 = 0x1204;
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

    /// Address the request to context `context`.
    fn context(mut self, context: u32) -> Self {
        self.put(16, context);
        self
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
/// `at`. The specification has a device ignore all but a move's position
/// (virtio 1.2 §5.7.6), but QEMU's takes its resource as whether the pointer
/// is shown, so a move names the one shown.
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

/// Ask capset `index`'s identity, highest version and size.
#[must_use]
pub fn get_capset_info(index: u32) -> Request {
    let mut request = Request::new(Command::GetCapsetInfo).with_len(HEADER_LEN + 8);
    request.put(HEADER_LEN, index);
    request
}

/// Ask for capset `id` at `version`.
#[must_use]
pub fn get_capset(id: u32, version: u32) -> Request {
    let mut request = Request::new(Command::GetCapset).with_len(HEADER_LEN + 8);
    request.put(HEADER_LEN, id);
    request.put(HEADER_LEN + 4, version);
    request
}

/// Create context `context` for capset `capset`, named `name` in the host's
/// diagnostics; a name past [`CONTEXT_NAME_MAX`] bytes is cut there.
#[must_use]
pub fn ctx_create(context: u32, capset: u32, name: &[u8]) -> Request {
    let name = &name[..name.len().min(CONTEXT_NAME_MAX)];
    let mut request = Request::new(Command::CtxCreate)
        .with_len(REQUEST_MAX)
        .context(context);
    #[allow(clippy::cast_possible_truncation)] // At most `CONTEXT_NAME_MAX`.
    request.put(HEADER_LEN, name.len() as u32);
    request.put(HEADER_LEN + 4, capset);
    request.bytes[HEADER_LEN + 8..HEADER_LEN + 8 + name.len()].copy_from_slice(name);
    request
}

/// Destroy context `context`.
#[must_use]
pub fn ctx_destroy(context: u32) -> Request {
    Request::new(Command::CtxDestroy).context(context)
}

/// What blob resource the host makes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlobSpec {
    /// The host object the blob is, in the context's own protocol; `0` for
    /// fresh memory of the host's.
    pub blob_id: u64,
    /// Its bytes.
    pub size: u64,
    /// Whether the driver may map it into the host-visible region.
    pub mappable: bool,
    /// Whether it may be shared.
    pub shareable: bool,
}

/// Have context `context` make blob resource `id` as `spec` says, of host
/// memory.
#[must_use]
pub fn resource_create_blob(context: u32, id: u32, spec: &BlobSpec) -> Request {
    let flags = if spec.mappable {
        BLOB_FLAG_USE_MAPPABLE
    } else {
        0
    } | if spec.shareable {
        BLOB_FLAG_USE_SHAREABLE
    } else {
        0
    };
    let mut request = Request::new(Command::ResourceCreateBlob)
        .with_len(HEADER_LEN + 32)
        .context(context);
    request.put(HEADER_LEN, id);
    request.put(HEADER_LEN + 4, BLOB_MEM_HOST3D);
    request.put(HEADER_LEN + 8, flags);
    request.put64(HEADER_LEN + 16, spec.blob_id);
    request.put64(HEADER_LEN + 24, spec.size);
    request
}

/// Map blob resource `id` into the host-visible region at `offset`.
#[must_use]
pub fn resource_map_blob(id: u32, offset: u64) -> Request {
    let mut request = Request::new(Command::ResourceMapBlob).with_len(HEADER_LEN + 16);
    request.put(HEADER_LEN, id);
    request.put64(HEADER_LEN + 8, offset);
    request
}

/// Take blob resource `id` out of the host-visible region.
#[must_use]
pub fn resource_unmap_blob(id: u32) -> Request {
    let mut request = Request::new(Command::ResourceUnmapBlob).with_len(HEADER_LEN + 8);
    request.put(HEADER_LEN, id);
    request
}

/// Submit `len` bytes of command stream, which follow the request in its own
/// segment, to context `context`.
#[must_use]
pub fn submit_3d(context: u32, len: u32) -> Request {
    let mut request = Request::new(Command::Submit3d)
        .with_len(HEADER_LEN + 8)
        .context(context);
    request.put(HEADER_LEN, len);
    request
}

/// Fence the request whose header is `header` with `id`: the device answers
/// it only once it has run it, echoing `id`.
pub fn fence(header: &mut [u8; HEADER_LEN], id: u64) {
    header[4..8].copy_from_slice(&FLAG_FENCE.to_le_bytes());
    header[8..16].copy_from_slice(&id.to_le_bytes());
}

/// Fence the request whose header is `header` with `id` on ring `ring` of its
/// context: the device answers it once the ring's work up to it is done.
pub fn fence_on_ring(header: &mut [u8; HEADER_LEN], id: u64, ring: u8) {
    header[4..8].copy_from_slice(&(FLAG_FENCE | FLAG_INFO_RING_IDX).to_le_bytes());
    header[8..16].copy_from_slice(&id.to_le_bytes());
    header[20] = ring;
}

/// Whether `response` answers the command fenced with `id`.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for an answer that does not carry the fence:
/// it does not say the command has run.
pub fn fenced(response: &[u8], id: u64) -> Result<(), DriverError> {
    let echoed = response
        .get(8..16)
        .and_then(|word| <[u8; 8]>::try_from(word).ok())
        .map(u64::from_le_bytes);
    if read32(response, 4)? & FLAG_FENCE != 0 && echoed == Some(id) {
        Ok(())
    } else {
        Err(DriverError::DeviceFault)
    }
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
/// The device's refusal, as the driver error it is;
/// [`DriverError::DeviceFault`] for a response shorter than a header.
pub fn status(response: &[u8], wanted: u32) -> Result<(), DriverError> {
    if response.len() < HEADER_LEN {
        return Err(DriverError::DeviceFault);
    }
    match read32(response, 0)? {
        found if found == wanted => Ok(()),
        RESP_ERR_OUT_OF_MEMORY => Err(DriverError::OutOfMemory),
        RESP_ERR_INVALID_SCANOUT_ID
        | RESP_ERR_INVALID_RESOURCE_ID
        | RESP_ERR_INVALID_CONTEXT_ID => Err(DriverError::NotFound),
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

/// One capset as a capset-information response states it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CapsetInfo {
    /// Which capset it is.
    pub id: u32,
    /// The highest version the device takes.
    pub max_version: u32,
    /// Its bytes at that version.
    pub max_size: u32,
}

/// The capset a capset-information response states.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a response of another type or too short.
pub fn capset_info(response: &[u8]) -> Result<CapsetInfo, DriverError> {
    status(response, RESP_OK_CAPSET_INFO)?;
    Ok(CapsetInfo {
        id: read32(response, HEADER_LEN)?,
        max_version: read32(response, HEADER_LEN + 4)?,
        max_size: read32(response, HEADER_LEN + 8)?,
    })
}

/// The `len` bytes of capset a capset response carries.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a response of another type or shorter
/// than `len`.
pub fn capset(response: &[u8], len: usize) -> Result<&[u8], DriverError> {
    status(response, RESP_OK_CAPSET)?;
    response
        .get(HEADER_LEN..HEADER_LEN + len)
        .ok_or(DriverError::DeviceFault)
}

/// The caching a map-information response states the mapped blob takes, or
/// `None` where it states none the driver can map with or none at all: the
/// blob is mapped all the same.
///
/// # Errors
///
/// The device's refusal; [`DriverError::DeviceFault`] for a response of
/// another type or shorter than a header.
pub fn map_info(response: &[u8]) -> Result<Option<RegionCaching>, DriverError> {
    status(response, RESP_OK_MAP_INFO)?;
    Ok(read32(response, HEADER_LEN)
        .ok()
        .and_then(|info| match info & MAP_CACHE_MASK {
            MAP_CACHE_CACHED => Some(RegionCaching::WriteBack),
            MAP_CACHE_UNCACHED => Some(RegionCaching::Uncached),
            MAP_CACHE_WC => Some(RegionCaching::WriteCombine),
            _ => None,
        }))
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
