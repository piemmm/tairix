//! The display-service IPC protocol (`plans/DISPLAY.md` D7b,
//! `plans/FIX-DISPLAY-ACCELERATION.md` B): the reserved rendezvous a
//! display-driver service binds and the fixed-width, fail-closed requests a
//! seat owner presents through.
//!
//! The driver allocates every buffer its device reads and grants each to the
//! presenter that asked (`shm_grant_peer`), so a present names a buffer by id
//! and no pixel crosses the IPC. `Configure` answers the output frames the
//! presenter rotates through; `AllocBuffer` answers a further buffer, for a
//! layer or the pointer. Every request that acts *for a seat* carries that
//! seat's id, and the service gates each one, `Query` included, on the
//! caller's live seat lease (`call_peer_seat`), so only the seat owner learns
//! the mode, holds buffers, or scans out.
//!
//! [`DisplayRequest::QueryStats`] is the one operation that acts for no seat:
//! it describes the *device*, for the monitor that draws graphics
//! utilisation, and is gated on the caller's `CAP_SYSINFO_HW` instead.
//!
//! One present is in flight at a time. A present the device takes at its next
//! frame start answers [`Presented::Queued`], and its latch is posted to the
//! configuration's port as [`DisplayEvent::Presented`].
//!
//! Every decode fails closed: an unknown magic, version, operation, format,
//! an out-of-bounds count, an empty rectangle, a zero buffer id or a dirty
//! reserved field refuses rather than guessing.

use crate::driver::display::{
    AccelCaps, AccelLayer, BufferId, CursorCaps, DamageList, DamageRect, DisplayDeviceReport,
    DisplayFormat, DisplayMode, DisplayPower, Presented, MAX_ACCEL_LAYERS, MAX_DAMAGE_RECTS,
    NO_RECT,
};
use crate::le::{put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64};
use crate::origin::{ProcId, PROC_ID_LEN};
use crate::Errno;

/// Reserved well-known call-endpoint id of the display service (`"DIS"`
/// hex-spelled prefix, mirroring [`crate::seat::SEATMGR_ENDPOINT`]'s
/// convention). Binding it requires `CAP_IPC_BIND_PRIVILEGED`
/// ([`crate::ipc::is_reserved_endpoint`]): a squatter claiming the
/// rendezvous first would be handed the desktop session's frames. One
/// endpoint serves every seat — requests carry the seat id in-protocol.
pub const DISPLAY_ENDPOINT: u64 = 0x0D15_1001;

/// Magic number identifying a display-service request (`"DSP1"`
/// little-endian).
pub const DISPLAY_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"DSP1");

/// Magic number identifying a display-service event (`"DSPE"`
/// little-endian).
pub const DISPLAY_EVENT_MAGIC: u32 = u32::from_le_bytes(*b"DSPE");

/// The `display-v1` protocol version.
pub const DISPLAY_VERSION_V1: u16 = 1;

/// Most output frames one configuration rotates through. A validation bound,
/// not a capacity: three serve a device that reads a frame while it shows and
/// a presenter drawing the next, and more buy no latency while letting a
/// hostile client pin memory.
pub const DISPLAY_MAX_FRAMES: u32 = 4;

/// Most buffers one configuration holds: its frames, two for each layer the
/// engine shows, and the pointer image. A containment bound on the table the
/// service keeps for a presenter, not what memory the device reaches, which
/// bounds them first.
pub const DISPLAY_MAX_BUFFERS: usize = DISPLAY_MAX_FRAMES as usize + 2 * MAX_ACCEL_LAYERS + 1;

/// Maximum request, in bytes, the [`DISPLAY_ENDPOINT`] accepts: exactly
/// one fixed-width [`DisplayRequest`].
pub const DISPLAY_MAX_REQUEST: usize = DisplayRequest::WIRE_LEN;

/// The layers of one [`DisplayRequest::PresentLayers`]: one to
/// [`MAX_ACCEL_LAYERS`], held inline so the request stays a plain value.
#[derive(Copy, Clone, Debug)]
pub struct LayerList {
    layers: [AccelLayer; MAX_ACCEL_LAYERS],
    len: usize,
}

impl LayerList {
    /// The list of `layers`, bottom-most first.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for no layer or more than
    /// [`MAX_ACCEL_LAYERS`].
    pub fn new(layers: &[AccelLayer]) -> Result<Self, Errno> {
        let (&first, _) = layers.split_first().ok_or(Errno::LengthOutOfRange)?;
        if layers.len() > MAX_ACCEL_LAYERS {
            return Err(Errno::LengthOutOfRange);
        }
        let mut slots = [first; MAX_ACCEL_LAYERS];
        slots[..layers.len()].copy_from_slice(layers);
        Ok(Self {
            layers: slots,
            len: layers.len(),
        })
    }

    /// The layers, bottom-most first.
    #[must_use]
    pub fn layers(&self) -> &[AccelLayer] {
        &self.layers[..self.len]
    }
}

impl PartialEq for LayerList {
    fn eq(&self, other: &Self) -> bool {
        self.layers() == other.layers()
    }
}

impl Eq for LayerList {}

/// One display-service operation.
///
/// Every variant that acts for a seat names it, and the service derives the
/// right to perform it from the caller's **live lease** on that seat
/// (`call_peer_seat`), never from a claimed handle. [`Self::QueryStats`] acts
/// for no seat and carries its own authority.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DisplayRequest {
    /// Report the output of `seat_id`'s display: the mode it runs, or none
    /// while nothing is connected.
    Query {
        /// The seat whose display output is requested.
        seat_id: u64,
    },
    /// Take the output frames of `seat_id`'s display at the mode the caller
    /// expects, replacing any earlier configuration by the same lease, with
    /// events posted to `events_port`, a port the caller owns. A mode that is
    /// not the output's is refused [`Errno::Stale`].
    Configure {
        /// The seat the frames will be presented on.
        seat_id: u64,
        /// The output width the caller expects.
        width_px: u32,
        /// The output height the caller expects.
        height_px: u32,
        /// The pixel encoding the caller expects.
        format: DisplayFormat,
        /// The caller's port the service posts [`DisplayEvent`]s to.
        events_port: u64,
    },
    /// Report the device's own graphics statistics. Names no seat; its
    /// authority is `CAP_SYSINFO_HW`.
    QueryStats,
    /// Show output frame `buffer`, of which only `damage` changed since the
    /// previous present.
    Present {
        /// The seat the frame is presented on.
        seat_id: u64,
        /// The frame shown.
        buffer: BufferId,
        /// The changed rectangles; a single [`DamageRect::full`] presents
        /// the whole frame. Never empty.
        damage: DamageList,
    },
    /// Switch `seat_id`'s display on or off, for as long as the configuration
    /// that asked stands.
    SetPower {
        /// The seat whose display is switched.
        seat_id: u64,
        /// The state asked for.
        power: DisplayPower,
    },
    /// Report what the device composites and draws in hardware.
    QueryPlanes {
        /// The seat whose display is asked about.
        seat_id: u64,
    },
    /// Allocate a further buffer of `width_px` × `height_px` for the
    /// configuration: a layer's or the pointer's.
    AllocBuffer {
        /// The seat the buffer is for.
        seat_id: u64,
        /// Width in pixels.
        width_px: u32,
        /// Height in pixels.
        height_px: u32,
    },
    /// Free a buffer the configuration allocated.
    FreeBuffer {
        /// The seat the buffer is for.
        seat_id: u64,
        /// The buffer freed.
        buffer: BufferId,
    },
    /// Composite `layers` in hardware and show them.
    PresentLayers {
        /// The seat the layers are presented on.
        seat_id: u64,
        /// The layers, bottom-most first.
        layers: LayerList,
    },
    /// Show `buffer` as the pointer, its hot spot `(hot_x, hot_y)` pixels in
    /// from its top-left.
    SetCursor {
        /// The seat the pointer is shown on.
        seat_id: u64,
        /// The pointer image.
        buffer: BufferId,
        /// The hot spot's column.
        hot_x: u32,
        /// The hot spot's row.
        hot_y: u32,
    },
    /// Put the pointer's hot spot at `(x, y)` on the output.
    MoveCursor {
        /// The seat the pointer is shown on.
        seat_id: u64,
        /// Column, possibly off the left edge.
        x: i32,
        /// Row, possibly off the top edge.
        y: i32,
    },
    /// Take the pointer off the screen.
    HideCursor {
        /// The seat the pointer is shown on.
        seat_id: u64,
    },
}

/// Wire operation discriminant of [`DisplayRequest::Query`].
const OP_QUERY: u16 = 1;
/// Wire operation discriminant of [`DisplayRequest::Configure`].
const OP_CONFIGURE: u16 = 2;
/// Wire operation discriminant of [`DisplayRequest::Present`].
const OP_PRESENT: u16 = 3;
/// Wire operation discriminant of [`DisplayRequest::QueryStats`].
const OP_QUERY_STATS: u16 = 4;
/// Wire operation discriminant of [`DisplayRequest::SetPower`].
const OP_SET_POWER: u16 = 5;
/// Wire operation discriminant of [`DisplayRequest::QueryPlanes`].
const OP_QUERY_PLANES: u16 = 6;
/// Wire operation discriminant of [`DisplayRequest::AllocBuffer`].
const OP_ALLOC_BUFFER: u16 = 7;
/// Wire operation discriminant of [`DisplayRequest::FreeBuffer`].
const OP_FREE_BUFFER: u16 = 8;
/// Wire operation discriminant of [`DisplayRequest::PresentLayers`].
const OP_PRESENT_LAYERS: u16 = 9;
/// Wire operation discriminant of [`DisplayRequest::SetCursor`].
const OP_SET_CURSOR: u16 = 10;
/// Wire operation discriminant of [`DisplayRequest::MoveCursor`].
const OP_MOVE_CURSOR: u16 = 11;
/// Wire operation discriminant of [`DisplayRequest::HideCursor`].
const OP_HIDE_CURSOR: u16 = 12;

/// Offset of the operation block, after the magic, version, op and seat.
const BODY_AT: usize = 16;

/// Offset of a `Present`'s first damage rectangle.
const PRESENT_RECTS_AT: usize = 24;

/// Offset of a `PresentLayers`' first layer.
const LAYERS_AT: usize = 24;

/// Encoded size of one [`DamageRect`].
const DAMAGE_RECT_LEN: usize = DamageRect::WIRE_LEN;

/// Encoded size of one layer: the buffer (4), its source rectangle (16), the
/// destination origin (8) and size (8), the opacity (1) and a reserved tail
/// (3).
const LAYER_LEN: usize = 40;

impl DisplayRequest {
    /// Encoded size on the wire: magic (4), version (2), op (2), seat id
    /// (8), and an operation block whose unused tail must be zero. The
    /// widest block is `PresentLayers`' count and its [`MAX_ACCEL_LAYERS`]
    /// inline layers.
    pub const WIRE_LEN: usize = LAYERS_AT + MAX_ACCEL_LAYERS * LAYER_LEN;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, DISPLAY_REQUEST_MAGIC);
        put_u16(&mut out, 4, DISPLAY_VERSION_V1);
        let (op, seat_id) = match *self {
            Self::Query { seat_id } => (OP_QUERY, seat_id),
            Self::Configure {
                seat_id,
                width_px,
                height_px,
                format,
                events_port,
            } => {
                put_u32(&mut out, BODY_AT, width_px);
                put_u32(&mut out, BODY_AT + 4, height_px);
                out[BODY_AT + 8] = format.as_u8();
                put_u64(&mut out, BODY_AT + 16, events_port);
                (OP_CONFIGURE, seat_id)
            }
            Self::QueryStats => (OP_QUERY_STATS, 0),
            Self::Present {
                seat_id,
                buffer,
                damage,
            } => {
                put_u32(&mut out, BODY_AT, buffer.get());
                put_u32(&mut out, BODY_AT + 4, wire_count(damage.len()));
                for (index, rect) in damage.rects().iter().enumerate() {
                    let at = PRESENT_RECTS_AT + index * DAMAGE_RECT_LEN;
                    out[at..at + DAMAGE_RECT_LEN].copy_from_slice(&rect.to_le_bytes());
                }
                (OP_PRESENT, seat_id)
            }
            Self::SetPower { seat_id, power } => {
                out[BODY_AT] = power.as_u8();
                (OP_SET_POWER, seat_id)
            }
            Self::QueryPlanes { seat_id } => (OP_QUERY_PLANES, seat_id),
            Self::AllocBuffer {
                seat_id,
                width_px,
                height_px,
            } => {
                put_u32(&mut out, BODY_AT, width_px);
                put_u32(&mut out, BODY_AT + 4, height_px);
                (OP_ALLOC_BUFFER, seat_id)
            }
            Self::FreeBuffer { seat_id, buffer } => {
                put_u32(&mut out, BODY_AT, buffer.get());
                (OP_FREE_BUFFER, seat_id)
            }
            Self::PresentLayers { seat_id, layers } => {
                put_u32(&mut out, BODY_AT, wire_count(layers.layers().len()));
                for (index, layer) in layers.layers().iter().enumerate() {
                    put_layer(&mut out, LAYERS_AT + index * LAYER_LEN, layer);
                }
                (OP_PRESENT_LAYERS, seat_id)
            }
            Self::SetCursor {
                seat_id,
                buffer,
                hot_x,
                hot_y,
            } => {
                put_u32(&mut out, BODY_AT, buffer.get());
                put_u32(&mut out, BODY_AT + 4, hot_x);
                put_u32(&mut out, BODY_AT + 8, hot_y);
                (OP_SET_CURSOR, seat_id)
            }
            Self::MoveCursor { seat_id, x, y } => {
                put_i32(&mut out, BODY_AT, x);
                put_i32(&mut out, BODY_AT + 4, y);
                (OP_MOVE_CURSOR, seat_id)
            }
            Self::HideCursor { seat_id } => (OP_HIDE_CURSOR, seat_id),
        };
        put_u16(&mut out, 6, op);
        put_u64(&mut out, 8, seat_id);
        out
    }

    /// Decode from `bytes`, failing closed on any malformed input.
    ///
    /// Bounds a decoder can see are enforced here — a non-empty damage and
    /// layer list within its bound, non-empty rectangles, non-zero buffer ids
    /// and extents — so no accepted request carries a value the server would
    /// have to re-reject structurally. Bounds only the server knows (the
    /// output, the buffers it allocated, what the device admits) stay there.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole request.
    /// * [`Errno::BadMagic`] — wrong magic or a dirty reserved tail.
    /// * [`Errno::AbiVersionUnsupported`] — not `display-v1`.
    /// * [`Errno::OutOfRange`] — an operation, pixel format or power state
    ///   outside the closed set, or a zero buffer id.
    /// * [`Errno::LengthOutOfRange`] — a zero extent, an empty or overlong
    ///   damage or layer list, or an empty rectangle.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != DISPLAY_REQUEST_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != DISPLAY_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let seat_id = read_u64(bytes, 8);
        let at = |offset: usize| read_u32(bytes, BODY_AT + offset);
        match read_u16(bytes, 6) {
            OP_QUERY => {
                reserved_zero(bytes, BODY_AT)?;
                Ok(Self::Query { seat_id })
            }
            OP_CONFIGURE => decode_configure(bytes, seat_id),
            OP_QUERY_STATS => {
                // A stats read names no seat, so a seat smuggled into one is
                // refused rather than ignored.
                reserved_zero(bytes, 8)?;
                Ok(Self::QueryStats)
            }
            OP_PRESENT => decode_present(bytes, seat_id),
            OP_SET_POWER => {
                reserved_zero(bytes, BODY_AT + 1)?;
                Ok(Self::SetPower {
                    seat_id,
                    power: DisplayPower::from_u8(bytes[BODY_AT])?,
                })
            }
            OP_QUERY_PLANES => {
                reserved_zero(bytes, BODY_AT)?;
                Ok(Self::QueryPlanes { seat_id })
            }
            OP_ALLOC_BUFFER => {
                reserved_zero(bytes, BODY_AT + 8)?;
                let (width_px, height_px) = extent(at(0), at(4))?;
                Ok(Self::AllocBuffer {
                    seat_id,
                    width_px,
                    height_px,
                })
            }
            OP_FREE_BUFFER => {
                reserved_zero(bytes, BODY_AT + 4)?;
                Ok(Self::FreeBuffer {
                    seat_id,
                    buffer: buffer_id(at(0))?,
                })
            }
            OP_PRESENT_LAYERS => decode_layers(bytes, seat_id),
            OP_SET_CURSOR => {
                reserved_zero(bytes, BODY_AT + 12)?;
                Ok(Self::SetCursor {
                    seat_id,
                    buffer: buffer_id(at(0))?,
                    hot_x: at(4),
                    hot_y: at(8),
                })
            }
            OP_MOVE_CURSOR => {
                reserved_zero(bytes, BODY_AT + 8)?;
                Ok(Self::MoveCursor {
                    seat_id,
                    x: read_i32(bytes, BODY_AT),
                    y: read_i32(bytes, BODY_AT + 4),
                })
            }
            OP_HIDE_CURSOR => {
                reserved_zero(bytes, BODY_AT)?;
                Ok(Self::HideCursor { seat_id })
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// The body of a `Configure`.
fn decode_configure(bytes: &[u8], seat_id: u64) -> Result<DisplayRequest, Errno> {
    reserved_zero(bytes, BODY_AT + 24)?;
    if bytes[BODY_AT + 9..BODY_AT + 16].iter().any(|&b| b != 0) {
        return Err(Errno::BadMagic);
    }
    let (width_px, height_px) = extent(read_u32(bytes, BODY_AT), read_u32(bytes, BODY_AT + 4))?;
    Ok(DisplayRequest::Configure {
        seat_id,
        width_px,
        height_px,
        format: DisplayFormat::from_u8(bytes[BODY_AT + 8])?,
        events_port: read_u64(bytes, BODY_AT + 16),
    })
}

/// The body of a `Present`.
fn decode_present(bytes: &[u8], seat_id: u64) -> Result<DisplayRequest, Errno> {
    let count = bounded(read_u32(bytes, BODY_AT + 4), MAX_DAMAGE_RECTS)?;
    // Slots past the live count are reserved: a rectangle smuggled behind the
    // count is refused, never ignored.
    reserved_zero(bytes, PRESENT_RECTS_AT + count * DAMAGE_RECT_LEN)?;
    let mut rects = [NO_RECT; MAX_DAMAGE_RECTS];
    for (index, slot) in rects.iter_mut().take(count).enumerate() {
        let from = PRESENT_RECTS_AT + index * DAMAGE_RECT_LEN;
        *slot = DamageRect::from_bytes(&bytes[from..])?;
    }
    Ok(DisplayRequest::Present {
        seat_id,
        buffer: buffer_id(read_u32(bytes, BODY_AT))?,
        damage: DamageList::new(&rects[..count])?,
    })
}

/// The body of a `PresentLayers`.
fn decode_layers(bytes: &[u8], seat_id: u64) -> Result<DisplayRequest, Errno> {
    let count = bounded(read_u32(bytes, BODY_AT), MAX_ACCEL_LAYERS)?;
    if read_u32(bytes, BODY_AT + 4) != 0 {
        return Err(Errno::BadMagic);
    }
    reserved_zero(bytes, LAYERS_AT + count * LAYER_LEN)?;
    let mut layers = [read_layer(bytes, LAYERS_AT)?; MAX_ACCEL_LAYERS];
    for (index, slot) in layers.iter_mut().enumerate().take(count).skip(1) {
        *slot = read_layer(bytes, LAYERS_AT + index * LAYER_LEN)?;
    }
    Ok(DisplayRequest::PresentLayers {
        seat_id,
        layers: LayerList::new(&layers[..count])?,
    })
}

/// A list's length as its wire count; lists are bounded far below `u32`.
fn wire_count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// `count` as a list length: one to `max`.
fn bounded(count: u32, max: usize) -> Result<usize, Errno> {
    usize::try_from(count)
        .ok()
        .filter(|count| (1..=max).contains(count))
        .ok_or(Errno::LengthOutOfRange)
}

/// A buffer id read off the wire; `0` names none.
fn buffer_id(raw: u32) -> Result<BufferId, Errno> {
    BufferId::new(raw).ok_or(Errno::OutOfRange)
}

/// A width and height read off the wire, neither zero.
fn extent(width_px: u32, height_px: u32) -> Result<(u32, u32), Errno> {
    if width_px == 0 || height_px == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    Ok((width_px, height_px))
}

/// Write one layer at `at`.
fn put_layer(out: &mut [u8], at: usize, layer: &AccelLayer) {
    put_u32(out, at, layer.buffer.get());
    out[at + 4..at + 4 + DAMAGE_RECT_LEN].copy_from_slice(&layer.src.to_le_bytes());
    put_i32(out, at + 20, layer.dst_x);
    put_i32(out, at + 24, layer.dst_y);
    put_u32(out, at + 28, layer.dst_width_px);
    put_u32(out, at + 32, layer.dst_height_px);
    out[at + 36] = layer.opacity;
}

/// Read one layer from `at`: a real buffer, a non-empty source and a
/// non-empty destination. Whether the device admits it is the server's to
/// decide.
fn read_layer(bytes: &[u8], at: usize) -> Result<AccelLayer, Errno> {
    if bytes[at + 37..at + LAYER_LEN].iter().any(|&b| b != 0) {
        return Err(Errno::BadMagic);
    }
    let src = DamageRect::from_bytes(&bytes[at + 4..])?;
    if src.width_px == 0 || src.height_px == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    let (dst_width_px, dst_height_px) = extent(read_u32(bytes, at + 28), read_u32(bytes, at + 32))?;
    Ok(AccelLayer {
        buffer: buffer_id(read_u32(bytes, at))?,
        src,
        dst_x: read_i32(bytes, at + 20),
        dst_y: read_i32(bytes, at + 24),
        dst_width_px,
        dst_height_px,
        opacity: bytes[at + 36],
    })
}

/// The width, height and stride at `at`, in `format`, refused
/// [`Errno::LengthOutOfRange`] unless they describe a drawable surface: no
/// zero extent, and a stride that holds one scanline.
fn read_mode(bytes: &[u8], at: usize, format: DisplayFormat) -> Result<DisplayMode, Errno> {
    let mode = DisplayMode {
        width_px: read_u32(bytes, at),
        height_px: read_u32(bytes, at + 4),
        stride_bytes: read_u32(bytes, at + 8),
        format,
    };
    let scanline = u64::from(mode.width_px) * u64::from(format.bytes_per_pixel());
    if mode.width_px == 0 || mode.height_px == 0 || u64::from(mode.stride_bytes) < scanline {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(mode)
}

/// Refuse a request whose reserved tail (from `from` to the end of the
/// fixed frame) carries any non-zero byte — wire corruption or a smuggled
/// field, never silently ignored.
fn reserved_zero(bytes: &[u8], from: usize) -> Result<(), Errno> {
    if bytes[from..DisplayRequest::WIRE_LEN]
        .iter()
        .any(|&b| b != 0)
    {
        return Err(Errno::BadMagic);
    }
    Ok(())
}

/// The status word of a reply frame: a negative [`Errno`] discriminant on
/// refusal, `0` otherwise.
fn put_status(out: &mut [u8], result: Result<(), Errno>) {
    out[..4].copy_from_slice(&crate::reply::encode_status_reply(result));
}

/// Check a reply frame's length and status word, and that its bytes from
/// `tail` to `len` are zero.
fn read_status(bytes: &[u8], len: usize, tail: usize) -> Result<(), Errno> {
    if bytes.len() < len {
        return Err(Errno::BufferTooSmall);
    }
    crate::reply::decode_status_reply(&bytes[..4])?;
    if bytes[tail..len].iter().any(|&b| b != 0) {
        return Err(Errno::BadMagic);
    }
    Ok(())
}

/// Reply length, in bytes, of a `Query`: the status word followed by the
/// output record — width (4), height (4), stride (4), format (1), whether an
/// output runs that mode (1), and a reserved tail (2) that must be zero. With
/// no output the whole record is zero.
pub const DISPLAY_MODE_REPLY_LEN: usize = 20;

/// The byte of a `Query` reply that is `1` when an output runs the mode the
/// reply carries and `0` when there is none.
const QUERY_OUTPUT_AT: usize = 17;

/// Encode a `Query` outcome. A refusal is the status word zero-padded to the
/// same length, so a client always issues one fixed-size receive.
#[must_use]
pub fn encode_mode_reply(
    result: Result<Option<DisplayMode>, Errno>,
) -> [u8; DISPLAY_MODE_REPLY_LEN] {
    let mut out = [0u8; DISPLAY_MODE_REPLY_LEN];
    match result {
        Ok(Some(mode)) => {
            put_u32(&mut out, 4, mode.width_px);
            put_u32(&mut out, 8, mode.height_px);
            put_u32(&mut out, 12, mode.stride_bytes);
            out[16] = mode.format.as_u8();
            out[QUERY_OUTPUT_AT] = 1;
        }
        Ok(None) => {}
        Err(err) => put_status(&mut out, Err(err)),
    }
    out
}

/// Decode a `Query` reply frame: the mode the output runs, or [`None`]
/// while there is no output.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole reply.
/// * [`Errno::OutOfRange`] — a corrupt status word or an unknown pixel
///   format.
/// * [`Errno::BadMagic`] — a dirty reserved tail, an output byte other than
///   `0` or `1`, or a mode carried beside a cleared one.
/// * [`Errno::LengthOutOfRange`] — a mode with a zero extent or a stride
///   too small for one scanline.
/// * The decoded [`Errno`] itself, when the service refused the query.
pub fn decode_mode_reply(bytes: &[u8]) -> Result<Option<DisplayMode>, Errno> {
    read_status(bytes, DISPLAY_MODE_REPLY_LEN, QUERY_OUTPUT_AT + 1)?;
    match bytes[QUERY_OUTPUT_AT] {
        0 if bytes[4..QUERY_OUTPUT_AT].iter().all(|&b| b == 0) => Ok(None),
        1 => read_mode(bytes, 4, DisplayFormat::from_u8(bytes[16])?).map(Some),
        _ => Err(Errno::BadMagic),
    }
}

/// One buffer the service granted a presenter: the driver's id for it and the
/// kernel's handle for the presenter's mapping.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BufferGrant {
    /// The buffer.
    pub buffer: BufferId,
    /// The `shm_grant_peer` handle the presenter maps it through.
    pub grant: u64,
}

impl BufferGrant {
    /// Encoded size: the buffer (4), a reserved word (4), the grant (8).
    const WIRE_LEN: usize = 16;

    fn put(&self, out: &mut [u8], at: usize) {
        put_u32(out, at, self.buffer.get());
        put_u64(out, at + 8, self.grant);
    }

    fn read(bytes: &[u8], at: usize) -> Result<Self, Errno> {
        if read_u32(bytes, at + 4) != 0 {
            return Err(Errno::BadMagic);
        }
        let grant = read_u64(bytes, at + 8);
        if grant == 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            buffer: buffer_id(read_u32(bytes, at))?,
            grant,
        })
    }
}

/// The output frames a `Configure` answered: the stride every frame has, the
/// driver instance that granted them, and the frames in rotation order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredFrames {
    /// Bytes from one row to the next of every frame.
    pub stride_bytes: u32,
    /// The driver instance that granted the frames, which the presenter names
    /// when it maps them.
    pub grantor: ProcId,
    frames: [Option<BufferGrant>; DISPLAY_MAX_FRAMES as usize],
}

impl ConfiguredFrames {
    /// The frames `frames`, granted by `grantor`, `stride_bytes` per row.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for no frame or more than
    /// [`DISPLAY_MAX_FRAMES`].
    pub fn new(stride_bytes: u32, grantor: ProcId, frames: &[BufferGrant]) -> Result<Self, Errno> {
        if frames.is_empty() || frames.len() > DISPLAY_MAX_FRAMES as usize {
            return Err(Errno::LengthOutOfRange);
        }
        let mut slots = [None; DISPLAY_MAX_FRAMES as usize];
        for (slot, frame) in slots.iter_mut().zip(frames) {
            *slot = Some(*frame);
        }
        Ok(Self {
            stride_bytes,
            grantor,
            frames: slots,
        })
    }

    /// The frames, in the order a presenter rotates through them.
    pub fn frames(&self) -> impl Iterator<Item = BufferGrant> + '_ {
        self.frames.iter().flatten().copied()
    }

    /// How many frames there are.
    #[must_use]
    pub fn count(&self) -> usize {
        self.frames().count()
    }
}

/// Offset of a `Configure` reply's grantor.
const CONFIGURE_GRANTOR_AT: usize = 16;
/// Offset of a `Configure` reply's first frame.
const CONFIGURE_FRAMES_AT: usize = CONFIGURE_GRANTOR_AT + PROC_ID_LEN;

/// Reply length, in bytes, of a `Configure`: the status word, the frame count
/// (4), the stride (4), a reserved word (4), the grantor (16), and
/// [`DISPLAY_MAX_FRAMES`] frame slots, the unused ones zero.
pub const DISPLAY_CONFIGURE_REPLY_LEN: usize =
    CONFIGURE_FRAMES_AT + DISPLAY_MAX_FRAMES as usize * BufferGrant::WIRE_LEN;

/// Encode a `Configure` outcome; a refusal is the zero-padded status word.
#[must_use]
pub fn encode_configure_reply(
    result: Result<ConfiguredFrames, Errno>,
) -> [u8; DISPLAY_CONFIGURE_REPLY_LEN] {
    let mut out = [0u8; DISPLAY_CONFIGURE_REPLY_LEN];
    match result {
        Ok(frames) => {
            put_u32(&mut out, 4, wire_count(frames.count()));
            put_u32(&mut out, 8, frames.stride_bytes);
            out[CONFIGURE_GRANTOR_AT..CONFIGURE_FRAMES_AT]
                .copy_from_slice(&frames.grantor.to_le_bytes());
            for (index, frame) in frames.frames().enumerate() {
                frame.put(
                    &mut out,
                    CONFIGURE_FRAMES_AT + index * BufferGrant::WIRE_LEN,
                );
            }
        }
        Err(err) => put_status(&mut out, Err(err)),
    }
    out
}

/// Decode a `Configure` reply frame.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole reply.
/// * [`Errno::BadMagic`] — a dirty reserved word or a frame smuggled past the
///   count.
/// * [`Errno::LengthOutOfRange`] — no frame or more than
///   [`DISPLAY_MAX_FRAMES`], or a zero stride.
/// * [`Errno::OutOfRange`] — a zero buffer id or grant, a corrupt status
///   word, or the kernel named as grantor.
/// * The decoded [`Errno`] itself, when the service refused the configure.
pub fn decode_configure_reply(bytes: &[u8]) -> Result<ConfiguredFrames, Errno> {
    if bytes.len() < DISPLAY_CONFIGURE_REPLY_LEN {
        return Err(Errno::BufferTooSmall);
    }
    crate::reply::decode_status_reply(&bytes[..4])?;
    let count = bounded(read_u32(bytes, 4), DISPLAY_MAX_FRAMES as usize)?;
    let stride_bytes = read_u32(bytes, 8);
    if stride_bytes == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    let used = CONFIGURE_FRAMES_AT + count * BufferGrant::WIRE_LEN;
    if read_u32(bytes, 12) != 0
        || bytes[used..DISPLAY_CONFIGURE_REPLY_LEN]
            .iter()
            .any(|&b| b != 0)
    {
        return Err(Errno::BadMagic);
    }
    let grantor = ProcId::from_bytes(&bytes[CONFIGURE_GRANTOR_AT..CONFIGURE_FRAMES_AT])?;
    if grantor.is_kernel() {
        return Err(Errno::OutOfRange);
    }
    let mut frames = [None; DISPLAY_MAX_FRAMES as usize];
    for (index, slot) in frames.iter_mut().take(count).enumerate() {
        *slot = Some(BufferGrant::read(
            bytes,
            CONFIGURE_FRAMES_AT + index * BufferGrant::WIRE_LEN,
        )?);
    }
    Ok(ConfiguredFrames {
        stride_bytes,
        grantor,
        frames,
    })
}

/// A buffer an `AllocBuffer` answered: the grant, its stride and the driver
/// instance that granted it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GrantedBuffer {
    /// The buffer and the presenter's handle for it.
    pub grant: BufferGrant,
    /// Bytes from one row to the next.
    pub stride_bytes: u32,
    /// The driver instance that granted it.
    pub grantor: ProcId,
}

/// Offset of an `AllocBuffer` reply's grantor.
const BUFFER_GRANTOR_AT: usize = 8 + BufferGrant::WIRE_LEN;

/// Reply length, in bytes, of an `AllocBuffer`: the status word, the stride
/// (4), the buffer's grant (16) and the grantor (16).
pub const DISPLAY_BUFFER_REPLY_LEN: usize = BUFFER_GRANTOR_AT + PROC_ID_LEN;

/// Encode an `AllocBuffer` outcome; a refusal is the zero-padded status word.
#[must_use]
pub fn encode_buffer_reply(result: Result<GrantedBuffer, Errno>) -> [u8; DISPLAY_BUFFER_REPLY_LEN] {
    let mut out = [0u8; DISPLAY_BUFFER_REPLY_LEN];
    match result {
        Ok(granted) => {
            put_u32(&mut out, 4, granted.stride_bytes);
            granted.grant.put(&mut out, 8);
            out[BUFFER_GRANTOR_AT..].copy_from_slice(&granted.grantor.to_le_bytes());
        }
        Err(err) => put_status(&mut out, Err(err)),
    }
    out
}

/// Decode an `AllocBuffer` reply frame.
///
/// # Errors
///
/// As [`decode_configure_reply`], for one buffer.
pub fn decode_buffer_reply(bytes: &[u8]) -> Result<GrantedBuffer, Errno> {
    if bytes.len() < DISPLAY_BUFFER_REPLY_LEN {
        return Err(Errno::BufferTooSmall);
    }
    crate::reply::decode_status_reply(&bytes[..4])?;
    let stride_bytes = read_u32(bytes, 4);
    if stride_bytes == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    let grantor = ProcId::from_bytes(&bytes[BUFFER_GRANTOR_AT..DISPLAY_BUFFER_REPLY_LEN])?;
    if grantor.is_kernel() {
        return Err(Errno::OutOfRange);
    }
    Ok(GrantedBuffer {
        grant: BufferGrant::read(bytes, 8)?,
        stride_bytes,
        grantor,
    })
}

/// What a `Present` or `PresentLayers` answered: whether the frame is shown or
/// queued, and the present's sequence number within its configuration, which
/// a queued present's [`DisplayEvent::Presented`] carries.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PresentReply {
    /// Whether the frame is on screen or waits for the device's next frame.
    pub presented: Presented,
    /// The present's number within its configuration.
    pub seq: u64,
}

/// A present's wire byte for [`Presented::Shown`].
const PRESENTED_SHOWN: u8 = 1;
/// A present's wire byte for [`Presented::Queued`].
const PRESENTED_QUEUED: u8 = 2;

/// Reply length, in bytes, of a `Present` or `PresentLayers`: the status
/// word, how the present ended (1), a reserved tail (3) and the sequence
/// number (8).
pub const DISPLAY_PRESENT_REPLY_LEN: usize = 16;

/// Encode a present's outcome; a refusal is the zero-padded status word.
#[must_use]
pub fn encode_present_reply(
    result: Result<PresentReply, Errno>,
) -> [u8; DISPLAY_PRESENT_REPLY_LEN] {
    let mut out = [0u8; DISPLAY_PRESENT_REPLY_LEN];
    match result {
        Ok(reply) => {
            out[4] = match reply.presented {
                Presented::Shown => PRESENTED_SHOWN,
                Presented::Queued => PRESENTED_QUEUED,
            };
            put_u64(&mut out, 8, reply.seq);
        }
        Err(err) => put_status(&mut out, Err(err)),
    }
    out
}

/// Decode a present's reply frame.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole reply.
/// * [`Errno::BadMagic`] — a dirty reserved tail or an unknown outcome.
/// * The decoded [`Errno`] itself, when the service refused the present.
pub fn decode_present_reply(bytes: &[u8]) -> Result<PresentReply, Errno> {
    if bytes.len() < DISPLAY_PRESENT_REPLY_LEN {
        return Err(Errno::BufferTooSmall);
    }
    crate::reply::decode_status_reply(&bytes[..4])?;
    if bytes[5..8].iter().any(|&b| b != 0) {
        return Err(Errno::BadMagic);
    }
    let presented = match bytes[4] {
        PRESENTED_SHOWN => Presented::Shown,
        PRESENTED_QUEUED => Presented::Queued,
        _ => return Err(Errno::BadMagic),
    };
    Ok(PresentReply {
        presented,
        seq: read_u64(bytes, 8),
    })
}

/// What a device composites and draws in hardware, as `QueryPlanes` answers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Planes {
    /// The layer compositor, if any.
    pub accel: Option<AccelCaps>,
    /// The cursor plane, if any.
    pub cursor: Option<CursorCaps>,
}

/// Set when a planes reply carries a layer compositor.
const PLANES_ACCEL: u32 = 1 << 0;
/// Set when that compositor applies a constant per-layer opacity.
const PLANES_PER_LAYER_OPACITY: u32 = 1 << 1;
/// Set when that compositor scales a layer.
const PLANES_HW_SCALING: u32 = 1 << 2;
/// Set when a planes reply carries a cursor plane.
const PLANES_CURSOR: u32 = 1 << 3;
/// Every flag this ABI defines.
const PLANES_KNOWN: u32 =
    PLANES_ACCEL | PLANES_PER_LAYER_OPACITY | PLANES_HW_SCALING | PLANES_CURSOR;

/// Reply length, in bytes, of a `QueryPlanes`: the status word, the flags
/// (4), the compositor's limits (12) and the cursor plane's (8), each block
/// zero where its flag is clear.
pub const DISPLAY_PLANES_REPLY_LEN: usize = 28;

/// Encode a `QueryPlanes` outcome; a refusal is the zero-padded status word.
#[must_use]
pub fn encode_planes_reply(result: Result<Planes, Errno>) -> [u8; DISPLAY_PLANES_REPLY_LEN] {
    let mut out = [0u8; DISPLAY_PLANES_REPLY_LEN];
    match result {
        Ok(planes) => {
            let mut flags = 0;
            if let Some(caps) = planes.accel {
                flags |= PLANES_ACCEL;
                if caps.per_layer_opacity {
                    flags |= PLANES_PER_LAYER_OPACITY;
                }
                if caps.hw_scaling {
                    flags |= PLANES_HW_SCALING;
                }
                put_u32(&mut out, 8, caps.max_layers);
                put_u32(&mut out, 12, caps.max_width_px);
                put_u32(&mut out, 16, caps.max_height_px);
            }
            if let Some(cursor) = planes.cursor {
                flags |= PLANES_CURSOR;
                put_u32(&mut out, 20, cursor.max_width_px);
                put_u32(&mut out, 24, cursor.max_height_px);
            }
            put_u32(&mut out, 4, flags);
        }
        Err(err) => put_status(&mut out, Err(err)),
    }
    out
}

/// Decode a `QueryPlanes` reply frame.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole reply.
/// * [`Errno::BadMagic`] — an unknown flag.
/// * [`Errno::LengthOutOfRange`] — a block that contradicts its flag, a
///   compositor of no layer, more than [`MAX_ACCEL_LAYERS`] or no pixel, or
///   a cursor of no pixel.
/// * The decoded [`Errno`] itself, when the service refused the query.
pub fn decode_planes_reply(bytes: &[u8]) -> Result<Planes, Errno> {
    if bytes.len() < DISPLAY_PLANES_REPLY_LEN {
        return Err(Errno::BufferTooSmall);
    }
    crate::reply::decode_status_reply(&bytes[..4])?;
    let flags = read_u32(bytes, 4);
    if flags & !PLANES_KNOWN != 0 {
        return Err(Errno::BadMagic);
    }
    let block =
        |at: usize, words: usize| (0..words).map(move |word| read_u32(bytes, at + 4 * word));
    let accel = if flags & PLANES_ACCEL != 0 {
        let caps = AccelCaps {
            max_layers: read_u32(bytes, 8),
            max_width_px: read_u32(bytes, 12),
            max_height_px: read_u32(bytes, 16),
            per_layer_opacity: flags & PLANES_PER_LAYER_OPACITY != 0,
            hw_scaling: flags & PLANES_HW_SCALING != 0,
        };
        let layers_fit = usize::try_from(caps.max_layers)
            .is_ok_and(|layers| (1..=MAX_ACCEL_LAYERS).contains(&layers));
        if !layers_fit || caps.max_width_px == 0 || caps.max_height_px == 0 {
            return Err(Errno::LengthOutOfRange);
        }
        Some(caps)
    } else {
        if flags & (PLANES_PER_LAYER_OPACITY | PLANES_HW_SCALING) != 0
            || block(8, 3).any(|w| w != 0)
        {
            return Err(Errno::LengthOutOfRange);
        }
        None
    };
    let cursor = if flags & PLANES_CURSOR != 0 {
        let cursor = CursorCaps {
            max_width_px: read_u32(bytes, 20),
            max_height_px: read_u32(bytes, 24),
        };
        if cursor.max_width_px == 0 || cursor.max_height_px == 0 {
            return Err(Errno::LengthOutOfRange);
        }
        Some(cursor)
    } else {
        if block(20, 2).any(|w| w != 0) {
            return Err(Errno::LengthOutOfRange);
        }
        None
    };
    Ok(Planes { accel, cursor })
}

/// What the service posts to a configuration's port.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DisplayEvent {
    /// Queued present `seq` latched: the frame it replaced is free.
    Presented {
        /// The present's sequence number, as its reply named it.
        seq: u64,
    },
}

/// Wire kind of [`DisplayEvent::Presented`].
const EVENT_PRESENTED: u16 = 1;

impl DisplayEvent {
    /// Encoded size: magic (4), version (2), kind (2), sequence number (8).
    pub const WIRE_LEN: usize = 16;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, DISPLAY_EVENT_MAGIC);
        put_u16(&mut out, 4, DISPLAY_VERSION_V1);
        match *self {
            Self::Presented { seq } => {
                put_u16(&mut out, 6, EVENT_PRESENTED);
                put_u64(&mut out, 8, seq);
            }
        }
        out
    }

    /// Decode from `bytes`, exactly one event.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a wrong length, [`Errno::BadMagic`]
    /// for a wrong magic, [`Errno::AbiVersionUnsupported`] for another
    /// version, and [`Errno::OutOfRange`] for an unknown kind.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != Self::WIRE_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        if read_u32(bytes, 0) != DISPLAY_EVENT_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != DISPLAY_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        match read_u16(bytes, 6) {
            EVENT_PRESENTED => Ok(Self::Presented {
                seq: read_u64(bytes, 8),
            }),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// A display device's own graphics statistics, as
/// [`DisplayRequest::QueryStats`] answers them.
///
/// Composed of the types that already define its parts — the
/// [`DisplayDeviceReport`] and the [`DisplayMode`] it scans out — plus the
/// service-measured occupancy, so no field here is a second spelling of one
/// defined elsewhere.
///
/// `busy_ns` and `idle_ns` partition the window since the device was first
/// driven: the same busy/idle vocabulary the CPU reading uses. Both are
/// cumulative and never reset, so a reader takes a two-sample delta over its
/// own interval; a device nothing has presented to reports both as `0`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DisplayStats {
    /// The seat whose frames the device is currently configured for, or `0`
    /// when nothing is configured. A fact about the device, not a key.
    pub seat_id: u64,
    /// Cumulative nanoseconds the device had a present in flight.
    pub busy_ns: u64,
    /// Cumulative nanoseconds it did not, over the same window.
    pub idle_ns: u64,
    /// What the device is.
    pub device: DisplayDeviceReport,
    /// The mode being scanned out, or [`None`] while there is no output.
    pub mode: Option<DisplayMode>,
}

/// Set in a statistics record when the device has a layer compositor.
const STATS_FLAG_ACCELERATED: u16 = 1 << 0;
/// Set when that compositor applies a constant per-layer opacity.
const STATS_FLAG_PER_LAYER_OPACITY: u16 = 1 << 1;
/// Set when an output is connected, so the mode block carries its mode.
const STATS_FLAG_OUTPUT: u16 = 1 << 2;
/// Set when the compositor scales a layer.
const STATS_FLAG_HW_SCALING: u16 = 1 << 3;
/// Set when the device has a cursor plane.
const STATS_FLAG_CURSOR: u16 = 1 << 4;
/// Every flag bit this ABI defines; any other set bit fails the decode
/// closed rather than being ignored.
const STATS_FLAGS_KNOWN: u16 = STATS_FLAG_ACCELERATED
    | STATS_FLAG_PER_LAYER_OPACITY
    | STATS_FLAG_OUTPUT
    | STATS_FLAG_HW_SCALING
    | STATS_FLAG_CURSOR;

impl DisplayStats {
    /// Encoded size on the wire, and the size of one packed
    /// `GPU_DEVICE_STATS` record.
    ///
    /// Layout: `busy_ns` (8), `idle_ns` (8), `mem_resident_bytes` (8),
    /// `mem_total_bytes` (8), `seat_id` (8), `max_layers` (4),
    /// `max_width_px` (4), `max_height_px` (4), mode width (4), height (4),
    /// stride (4), format (1), reserved (1), flags (2), reserved (4), cursor
    /// width (4) and height (4). Every reserved byte must be zero.
    pub const WIRE_LEN: usize = 80;

    /// Byte offset of the accelerated-capability block.
    const ACCEL_AT: usize = 40;
    /// Byte offset of the mode block.
    const MODE_AT: usize = 52;
    /// Byte offset of the flags word.
    const FLAGS_AT: usize = 66;
    /// Byte offset of the cursor block.
    const CURSOR_AT: usize = 72;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        let mut flags = 0u16;
        if let Some(caps) = self.device.accel {
            flags |= STATS_FLAG_ACCELERATED;
            if caps.per_layer_opacity {
                flags |= STATS_FLAG_PER_LAYER_OPACITY;
            }
            if caps.hw_scaling {
                flags |= STATS_FLAG_HW_SCALING;
            }
            put_u32(&mut out, Self::ACCEL_AT, caps.max_layers);
            put_u32(&mut out, Self::ACCEL_AT + 4, caps.max_width_px);
            put_u32(&mut out, Self::ACCEL_AT + 8, caps.max_height_px);
        }
        if let Some(mode) = self.mode {
            flags |= STATS_FLAG_OUTPUT;
            put_u32(&mut out, Self::MODE_AT, mode.width_px);
            put_u32(&mut out, Self::MODE_AT + 4, mode.height_px);
            put_u32(&mut out, Self::MODE_AT + 8, mode.stride_bytes);
            out[Self::MODE_AT + 12] = mode.format.as_u8();
        }
        if let Some(cursor) = self.device.cursor {
            flags |= STATS_FLAG_CURSOR;
            put_u32(&mut out, Self::CURSOR_AT, cursor.max_width_px);
            put_u32(&mut out, Self::CURSOR_AT + 4, cursor.max_height_px);
        }
        put_u16(&mut out, Self::FLAGS_AT, flags);
        put_u64(&mut out, 0, self.busy_ns);
        put_u64(&mut out, 8, self.idle_ns);
        put_u64(&mut out, 16, self.device.mem_resident_bytes);
        put_u64(&mut out, 24, self.device.mem_total_bytes);
        put_u64(&mut out, 32, self.seat_id);
        out
    }

    /// Decode from `bytes`.
    ///
    /// Every relation the producer must have honoured is re-checked here, so
    /// a reader never renders a service's arithmetic: a device with no memory
    /// of its own cannot claim resident bytes, an unaccelerated device cannot
    /// carry compositor limits, and an accelerated one must be able to
    /// composite at least one layer of at least one pixel.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — `bytes` is shorter than
    ///   [`Self::WIRE_LEN`].
    /// * [`Errno::OutOfRange`] — an unknown pixel format.
    /// * [`Errno::BadMagic`] — an unknown flag bit or a dirty reserved field.
    /// * [`Errno::LengthOutOfRange`] — a nonsensical mode, resident bytes
    ///   above the memory the device owns, or a block that contradicts its
    ///   flag.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        let flags = read_u16(bytes, Self::FLAGS_AT);
        if flags & !STATS_FLAGS_KNOWN != 0
            || bytes[Self::MODE_AT + 13] != 0
            || bytes[Self::FLAGS_AT + 2..Self::CURSOR_AT]
                .iter()
                .any(|&b| b != 0)
        {
            return Err(Errno::BadMagic);
        }
        let mem_resident_bytes = read_u64(bytes, 16);
        let mem_total_bytes = read_u64(bytes, 24);
        if mem_resident_bytes > mem_total_bytes {
            return Err(Errno::LengthOutOfRange);
        }
        let max_layers = read_u32(bytes, Self::ACCEL_AT);
        let max_width_px = read_u32(bytes, Self::ACCEL_AT + 4);
        let max_height_px = read_u32(bytes, Self::ACCEL_AT + 8);
        let accel = if flags & STATS_FLAG_ACCELERATED != 0 {
            if max_layers == 0 || max_width_px == 0 || max_height_px == 0 {
                return Err(Errno::LengthOutOfRange);
            }
            Some(AccelCaps {
                max_layers,
                max_width_px,
                max_height_px,
                per_layer_opacity: flags & STATS_FLAG_PER_LAYER_OPACITY != 0,
                hw_scaling: flags & STATS_FLAG_HW_SCALING != 0,
            })
        } else {
            if max_layers != 0
                || max_width_px != 0
                || max_height_px != 0
                || flags & (STATS_FLAG_PER_LAYER_OPACITY | STATS_FLAG_HW_SCALING) != 0
            {
                return Err(Errno::LengthOutOfRange);
            }
            None
        };
        let mode = if flags & STATS_FLAG_OUTPUT != 0 {
            let format = DisplayFormat::from_u8(bytes[Self::MODE_AT + 12])?;
            Some(read_mode(bytes, Self::MODE_AT, format)?)
        } else {
            if bytes[Self::MODE_AT..=Self::MODE_AT + 12]
                .iter()
                .any(|&b| b != 0)
            {
                return Err(Errno::LengthOutOfRange);
            }
            None
        };
        let cursor_width = read_u32(bytes, Self::CURSOR_AT);
        let cursor_height = read_u32(bytes, Self::CURSOR_AT + 4);
        let cursor = if flags & STATS_FLAG_CURSOR != 0 {
            if cursor_width == 0 || cursor_height == 0 {
                return Err(Errno::LengthOutOfRange);
            }
            Some(CursorCaps {
                max_width_px: cursor_width,
                max_height_px: cursor_height,
            })
        } else {
            if cursor_width != 0 || cursor_height != 0 {
                return Err(Errno::LengthOutOfRange);
            }
            None
        };
        Ok(Self {
            seat_id: read_u64(bytes, 32),
            busy_ns: read_u64(bytes, 0),
            idle_ns: read_u64(bytes, 8),
            device: DisplayDeviceReport {
                mem_resident_bytes,
                mem_total_bytes,
                accel,
                cursor,
            },
            mode,
        })
    }
}

/// Reply length, in bytes, of a `QueryStats`: the status word followed by one
/// [`DisplayStats`] record.
pub const DISPLAY_STATS_REPLY_LEN: usize = 4 + DisplayStats::WIRE_LEN;

/// Encode a `QueryStats` outcome; a refusal is the zero-padded status word.
#[must_use]
pub fn encode_stats_reply(result: Result<DisplayStats, Errno>) -> [u8; DISPLAY_STATS_REPLY_LEN] {
    let mut out = [0u8; DISPLAY_STATS_REPLY_LEN];
    put_status(&mut out, result.map(|_| ()));
    if let Ok(stats) = result {
        out[4..].copy_from_slice(&stats.to_le_bytes());
    }
    out
}

/// Decode a `QueryStats` reply frame.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole reply.
/// * [`Errno::OutOfRange`] — a corrupt status word.
/// * The decoded [`Errno`] itself, when the service refused the read.
/// * Everything [`DisplayStats::from_bytes`] can return.
pub fn decode_stats_reply(bytes: &[u8]) -> Result<DisplayStats, Errno> {
    if bytes.len() < DISPLAY_STATS_REPLY_LEN {
        return Err(Errno::BufferTooSmall);
    }
    crate::reply::decode_status_reply(&bytes[..4])?;
    DisplayStats::from_bytes(&bytes[4..DISPLAY_STATS_REPLY_LEN])
}

#[cfg(test)]
#[path = "display_ipc_tests.rs"]
mod tests;
