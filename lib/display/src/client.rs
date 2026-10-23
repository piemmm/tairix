//! The client half: the presenter a compositor presents through.
//!
//! [`DisplayClient`] speaks the wire protocol over an injected
//! [`DisplayTransport`] (the `ipc_call` syscall in production, a mock in
//! tests). [`RemoteDisplay`] is a [`Presenter`] over it: the driver
//! allocated the frames and granted them, the presenter maps them once, and a
//! present copies only the changed pixels of the compositor's frame into the
//! back frame and names it by id — no pixel crosses the IPC.
//!
//! With more than one frame the copy also refreshes the target frame's
//! **stale region** (the damage accumulated while other frames were
//! presented), which keeps every frame current without copying the whole
//! surface. A frame is presented **once**, carrying every rectangle it
//! changed.
//!
//! A present the device queued holds the next: [`RemoteDisplay`] refuses it
//! [`DriverError::Busy`] until the driver posts the latch
//! ([`RemoteDisplay::latched`]), so the frame the device still reads is never
//! written.

use tairix_abi::display_ipc::{
    decode_buffer_reply, decode_configure_reply, decode_mode_reply, decode_planes_reply,
    decode_present_reply, ConfiguredFrames, DisplayEvent, DisplayRequest, GrantedBuffer, LayerList,
    Planes, PresentReply, DISPLAY_BUFFER_REPLY_LEN, DISPLAY_CONFIGURE_REPLY_LEN,
    DISPLAY_MAX_BUFFERS, DISPLAY_MAX_FRAMES, DISPLAY_MODE_REPLY_LEN, DISPLAY_PLANES_REPLY_LEN,
    DISPLAY_PRESENT_REPLY_LEN,
};
use tairix_abi::driver::display::{
    AccelCaps, AccelLayer, BufferId, CursorCaps, DamageList, DamageRect, DisplayMode, DisplayPower,
    OutputMode, Presented, MAX_ACCEL_LAYERS, MAX_DAMAGE_RECTS,
};
use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_geometry::{Rect, Region};
use tairix_inline::ArrayVec;

use crate::driver_error_from_errno;
use crate::memory::copy_rows;
use crate::presenter::{Base, CursorPlane, DrawInPlace, LayerPresenter, Presenter};
use crate::scanout::as_damage;

/// One request/reply exchange with the display service.
pub trait DisplayTransport {
    /// Send `request` and receive the reply into `reply`, returning its
    /// length.
    ///
    /// # Errors
    ///
    /// The transport's own failure, as an [`Errno`].
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;
}

/// Maps a buffer a display driver granted this presenter.
pub trait BufferMapper {
    /// A mapped buffer, unmapped when dropped.
    type Mapping: AsMut<[u8]>;

    /// Map the region `grantor` granted as `grant`, at least `least` bytes.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, or [`Errno::LengthOutOfRange`] for a region
    /// smaller than `least`.
    fn map(&mut self, grantor: ProcId, grant: u64, least: usize) -> Result<Self::Mapping, Errno>;
}

/// The request half: one seat's requests and their decoded replies.
pub struct DisplayClient<T: DisplayTransport> {
    transport: T,
    seat_id: u64,
}

impl<T: DisplayTransport> DisplayClient<T> {
    /// A client presenting on `seat_id` over `transport`.
    pub const fn new(transport: T, seat_id: u64) -> Self {
        Self { transport, seat_id }
    }

    /// The seat this client presents on.
    #[must_use]
    pub const fn seat_id(&self) -> u64 {
        self.seat_id
    }

    /// The mode the seat's output runs and its rate, or [`None`] with no
    /// output.
    ///
    /// # Errors
    ///
    /// The service's refusal, or a malformed reply.
    pub fn query(&mut self) -> Result<Option<OutputMode>, Errno> {
        let mut reply = [0u8; DISPLAY_MODE_REPLY_LEN];
        let len = self.call(
            &DisplayRequest::Query {
                seat_id: self.seat_id,
            },
            &mut reply,
        )?;
        decode_mode_reply(&reply[..len])
    }

    /// Take the output frames for `mode`, events posted to `events_port`.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`] when the output no longer runs `mode`, or the
    /// service's refusal.
    pub fn configure(
        &mut self,
        mode: &DisplayMode,
        events_port: u64,
    ) -> Result<ConfiguredFrames, Errno> {
        let request = DisplayRequest::Configure {
            seat_id: self.seat_id,
            width_px: mode.width_px,
            height_px: mode.height_px,
            format: mode.format,
            events_port,
        };
        let mut reply = [0u8; DISPLAY_CONFIGURE_REPLY_LEN];
        let len = self.call(&request, &mut reply)?;
        decode_configure_reply(&reply[..len])
    }

    /// What the device composites and draws in hardware.
    ///
    /// # Errors
    ///
    /// The service's refusal, or a malformed reply.
    pub fn query_planes(&mut self) -> Result<Planes, Errno> {
        let mut reply = [0u8; DISPLAY_PLANES_REPLY_LEN];
        let len = self.call(
            &DisplayRequest::QueryPlanes {
                seat_id: self.seat_id,
            },
            &mut reply,
        )?;
        decode_planes_reply(&reply[..len])
    }

    /// Allocate a further buffer of `width_px` × `height_px`.
    ///
    /// # Errors
    ///
    /// The service's refusal, or a malformed reply.
    pub fn alloc_buffer(&mut self, width_px: u32, height_px: u32) -> Result<GrantedBuffer, Errno> {
        let request = DisplayRequest::AllocBuffer {
            seat_id: self.seat_id,
            width_px,
            height_px,
        };
        let mut reply = [0u8; DISPLAY_BUFFER_REPLY_LEN];
        let len = self.call(&request, &mut reply)?;
        decode_buffer_reply(&reply[..len])
    }

    /// Free a buffer allocated beyond the frames.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    pub fn free_buffer(&mut self, buffer: BufferId) -> Result<(), Errno> {
        self.status_call(&DisplayRequest::FreeBuffer {
            seat_id: self.seat_id,
            buffer,
        })
    }

    /// Show frame `buffer`, of which only `damage` changed.
    ///
    /// # Errors
    ///
    /// The service's refusal, or a malformed reply.
    pub fn present(
        &mut self,
        buffer: BufferId,
        damage: &[DamageRect],
    ) -> Result<PresentReply, Errno> {
        let request = DisplayRequest::Present {
            seat_id: self.seat_id,
            buffer,
            damage: DamageList::new(damage)?,
        };
        self.present_call(&request)
    }

    /// Composite `layers` in hardware.
    ///
    /// # Errors
    ///
    /// The service's refusal, or a malformed reply.
    pub fn present_layers(&mut self, layers: &[AccelLayer]) -> Result<PresentReply, Errno> {
        let request = DisplayRequest::PresentLayers {
            seat_id: self.seat_id,
            layers: LayerList::new(layers)?,
        };
        self.present_call(&request)
    }

    /// Show `buffer` as the pointer, hot spot `(hot_x, hot_y)`.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    pub fn set_cursor(&mut self, buffer: BufferId, hot_x: u32, hot_y: u32) -> Result<(), Errno> {
        self.status_call(&DisplayRequest::SetCursor {
            seat_id: self.seat_id,
            buffer,
            hot_x,
            hot_y,
        })
    }

    /// Put the pointer's hot spot at `(x, y)`.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    pub fn move_cursor(&mut self, x: i32, y: i32) -> Result<(), Errno> {
        self.status_call(&DisplayRequest::MoveCursor {
            seat_id: self.seat_id,
            x,
            y,
        })
    }

    /// Take the pointer off the screen.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    pub fn hide_cursor(&mut self) -> Result<(), Errno> {
        self.status_call(&DisplayRequest::HideCursor {
            seat_id: self.seat_id,
        })
    }

    /// Switch the seat's display on or off.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    pub fn set_power(&mut self, power: DisplayPower) -> Result<(), Errno> {
        self.status_call(&DisplayRequest::SetPower {
            seat_id: self.seat_id,
            power,
        })
    }

    fn call(&mut self, request: &DisplayRequest, reply: &mut [u8]) -> Result<usize, Errno> {
        self.transport.call(&request.to_le_bytes(), reply)
    }

    fn present_call(&mut self, request: &DisplayRequest) -> Result<PresentReply, Errno> {
        let mut reply = [0u8; DISPLAY_PRESENT_REPLY_LEN];
        let len = self.call(request, &mut reply)?;
        decode_present_reply(&reply[..len])
    }

    fn status_call(&mut self, request: &DisplayRequest) -> Result<(), Errno> {
        let mut reply = [0u8; STATUS_REPLY_LEN];
        let len = self.call(request, &mut reply)?;
        decode_status_reply(&reply[..len])
    }
}

/// One output frame the driver granted: its id, this presenter's mapping of
/// it, and what it lacks of the current frame.
struct Frame<B> {
    id: BufferId,
    memory: B,
    stale: Region,
}

/// A buffer allocated beyond the frames, and its mapping.
struct Extra<B> {
    id: BufferId,
    memory: B,
    width_px: u32,
    height_px: u32,
    stride_bytes: u32,
}

/// Most layer buffers a presenter holds: what the configuration's table leaves
/// beside its frames and the pointer image.
const LAYER_BUFFERS: usize = DISPLAY_MAX_BUFFERS - DISPLAY_MAX_FRAMES as usize - 1;

/// A display a compositor presents to over [`DisplayClient`], in frames the
/// driver allocated.
pub struct RemoteDisplay<M: BufferMapper, T: DisplayTransport> {
    client: DisplayClient<T>,
    mapper: M,
    grantor: ProcId,
    mode: DisplayMode,
    surface: Rect,
    frames: ArrayVec<Frame<M::Mapping>, { DISPLAY_MAX_FRAMES as usize }>,
    /// Bytes from one row to the next of every frame.
    stride: u32,
    back: usize,
    /// The sequence number of a present the device queued and has not
    /// latched.
    in_flight: Option<u64>,
    planes: Planes,
    layers: ArrayVec<Extra<M::Mapping>, LAYER_BUFFERS>,
    /// The pointer image, which the device has taken by the time a show
    /// returns.
    cursor: Option<Extra<M::Mapping>>,
    /// Whether the plane shows it.
    cursor_shown: bool,
    /// The frame the device shows, once one was presented.
    shown: Option<usize>,
    /// What a present changed, kept for its buffer.
    changed: Region,
}

impl<M: BufferMapper, T: DisplayTransport> RemoteDisplay<M, T> {
    /// Take the output frames for `mode` from `client`'s display, map them
    /// through `mapper`, and learn what the device draws in hardware.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`] when the output no longer runs `mode`,
    /// [`Errno::LengthOutOfRange`] for a mode or frame layout this presenter
    /// cannot copy into, or the service's or the kernel's refusal.
    pub fn configure(
        mut client: DisplayClient<T>,
        mut mapper: M,
        mode: DisplayMode,
        events_port: u64,
    ) -> Result<Self, Errno> {
        let scanline = u64::from(mode.width_px) * u64::from(mode.format.bytes_per_pixel());
        if i32::try_from(mode.width_px).is_err()
            || i32::try_from(mode.height_px).is_err()
            || u64::from(mode.stride_bytes) < scanline
        {
            return Err(Errno::LengthOutOfRange);
        }
        let configured = client.configure(&mode, events_port)?;
        if u64::from(configured.stride_bytes) < scanline {
            return Err(Errno::LengthOutOfRange);
        }
        let frame_len =
            usize::try_from(u64::from(configured.stride_bytes) * u64::from(mode.height_px))
                .map_err(|_| Errno::LengthOutOfRange)?;
        let surface = Rect::new(0, 0, mode.width_px, mode.height_px);
        let mut frames = ArrayVec::new();
        for grant in configured.frames() {
            let memory = mapper.map(configured.grantor, grant.grant, frame_len)?;
            let mut stale = Region::with_budget(stale_budget(configured.count()));
            stale.add(surface);
            frames
                .try_push(Frame {
                    id: grant.buffer,
                    memory,
                    stale,
                })
                .map_err(|_| Errno::LengthOutOfRange)?;
        }
        let planes = client.query_planes()?;
        Ok(Self {
            client,
            mapper,
            grantor: configured.grantor,
            mode,
            surface,
            frames,
            stride: configured.stride_bytes,
            back: 0,
            in_flight: None,
            planes,
            layers: ArrayVec::new(),
            cursor: None,
            cursor_shown: false,
            shown: None,
            changed: Region::new(),
        })
    }

    /// Take a [`DisplayEvent`] the driver posted: a queued present that
    /// latched frees the frames for the next. Answers whether a present was
    /// waiting on it.
    pub fn latched(&mut self, event: DisplayEvent) -> bool {
        let DisplayEvent::Presented { seq } = event;
        if self.in_flight == Some(seq) {
            self.in_flight = None;
            return true;
        }
        false
    }

    /// Note how a present ended: one the device queued holds the next.
    fn settle(&mut self, reply: PresentReply) {
        if reply.presented == Presented::Queued {
            self.in_flight = Some(reply.seq);
        }
    }

    /// Copy the parts of `frame` the back frame lacks, `damage` among them,
    /// into it, and owe `damage` to every other frame — before the present
    /// is sent, so a refused one is still owed to them.
    fn stage(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<BufferId, DriverError> {
        let source_len =
            usize::try_from(self.mode.len_bytes()).map_err(|_| DriverError::LengthOutOfRange)?;
        if frame.len() < source_len {
            return Err(DriverError::BufferTooSmall);
        }
        let mut changed = core::mem::take(&mut self.changed);
        changed.clear();
        mark(&mut changed, damage, self.surface);
        let refresh = self.refresh_back(&changed);
        for &rect in refresh.rects() {
            self.copy_rect(frame, self.back, rect);
        }
        let id = self.settle_back(refresh, &changed);
        self.changed = changed;
        Ok(id)
    }

    /// What the back frame needs brought up to date for a present of which
    /// `changed` changed: that, and what the frame missed since it was shown.
    fn refresh_back(&mut self, changed: &Region) -> Region {
        let mut refresh = core::mem::take(&mut self.frames[self.back].stale);
        refresh.add_region(changed);
        refresh.clip(self.surface);
        refresh
    }

    /// The back frame is up to date: give it back its emptied `refresh`, owe
    /// `changed` to every other frame, and name the back frame.
    fn settle_back(&mut self, mut refresh: Region, changed: &Region) -> BufferId {
        let (surface, back) = (self.surface, self.back);
        refresh.clear();
        self.frames[back].stale = refresh;
        for (index, other) in self.frames.iter_mut().enumerate() {
            if index != back {
                other.stale.add_region(changed);
                other.stale.clip(surface);
            }
        }
        self.frames[back].id
    }

    /// The staged back frame went to the display, so the next is drawn into.
    fn rotate(&mut self, reply: PresentReply) {
        self.settle(reply);
        self.shown = Some(self.back);
        self.back = (self.back + 1) % self.frames.len();
    }

    fn copy_rect(&mut self, frame: &[u8], index: usize, rect: Rect) {
        let rect = rect.intersection(&self.surface);
        let (Ok(left), Ok(top)) = (usize::try_from(rect.left()), usize::try_from(rect.top()))
        else {
            return;
        };
        if rect.is_empty() {
            return;
        }
        let bpp = self.mode.format.bytes_per_pixel() as usize;
        let (from_stride, to_stride) = (self.mode.stride_bytes as usize, self.stride as usize);
        let span = rect.width as usize * bpp;
        let memory = self.frames[index].memory.as_mut();
        for row in top..top + rect.height as usize {
            let from = row * from_stride + left * bpp;
            let to = row * to_stride + left * bpp;
            // Both lengths were checked against the mode when the frames were
            // mapped and the frame handed in, so a short slice is never met.
            if let (Some(dst), Some(src)) =
                (memory.get_mut(to..to + span), frame.get(from..from + span))
            {
                dst.copy_from_slice(src);
            }
        }
    }

    /// Frame `index`'s bytes, as this presenter mapped them.
    #[cfg(test)]
    pub(crate) fn frame(&mut self, index: usize) -> &[u8] {
        self.frames[index].memory.as_mut()
    }

    fn layer(&mut self, id: BufferId) -> Option<&mut Extra<M::Mapping>> {
        self.layers.iter_mut().find(|layer| layer.id == id)
    }

    /// Map a buffer the driver granted beyond the frames.
    fn take(
        &mut self,
        granted: GrantedBuffer,
        width_px: u32,
        height_px: u32,
    ) -> Result<Extra<M::Mapping>, Errno> {
        let least = usize::try_from(u64::from(granted.stride_bytes) * u64::from(height_px))
            .map_err(|_| Errno::LengthOutOfRange)?;
        let scanline = u64::from(width_px) * u64::from(self.mode.format.bytes_per_pixel());
        if u64::from(granted.stride_bytes) < scanline || granted.grantor != self.grantor {
            let _ = self.client.free_buffer(granted.grant.buffer);
            return Err(Errno::LengthOutOfRange);
        }
        match self.mapper.map(granted.grantor, granted.grant.grant, least) {
            Ok(memory) => Ok(Extra {
                id: granted.grant.buffer,
                memory,
                width_px,
                height_px,
                stride_bytes: granted.stride_bytes,
            }),
            Err(err) => {
                let _ = self.client.free_buffer(granted.grant.buffer);
                Err(err)
            }
        }
    }
}

/// Copy `height_px` rows of `row` bytes of `image`, `stride_bytes` apart, into
/// the pointer buffer `held`.
fn draw_image<B: AsMut<[u8]>>(
    held: &mut Extra<B>,
    image: &[u8],
    stride_bytes: u32,
    row: u32,
    height_px: u32,
) -> Result<(), DriverError> {
    // Whatever a larger pointer left beyond this one is cleared.
    held.memory.as_mut().fill(0);
    copy_rows(
        image,
        stride_bytes as usize,
        held.memory.as_mut(),
        held.stride_bytes as usize,
        row as usize,
        height_px as usize,
    )
}

/// The rectangles one frame's stale region may hold before it coarsens.
fn stale_budget(frames: usize) -> usize {
    frames.saturating_mul(MAX_DAMAGE_RECTS)
}

/// Add `damage`, clipped to `surface`, to `region`.
fn mark(region: &mut Region, damage: &[DamageRect], surface: Rect) {
    for rect in damage {
        match (i32::try_from(rect.x), i32::try_from(rect.y)) {
            (Ok(x), Ok(y)) => {
                region.add(Rect::new(x, y, rect.width_px, rect.height_px).intersection(&surface));
            }
            _ => region.add(surface),
        }
    }
}

impl<M: BufferMapper, T: DisplayTransport> Presenter for RemoteDisplay<M, T> {
    fn mode(&self) -> DisplayMode {
        self.mode
    }

    fn ready(&self) -> bool {
        self.in_flight.is_none()
    }

    fn present(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<(), DriverError> {
        if self.in_flight.is_some() {
            return Err(DriverError::Busy);
        }
        DamageRect::validate_list(damage, &self.mode)?;
        let id = self.stage(frame, damage)?;
        let reply = self
            .client
            .present(id, damage)
            .map_err(driver_error_from_errno)?;
        self.rotate(reply);
        Ok(())
    }

    fn present_drawn(
        &mut self,
        damage: &Region,
        draw: &mut DrawInPlace<'_>,
    ) -> Result<(), DriverError> {
        if self.in_flight.is_some() {
            return Err(DriverError::Busy);
        }
        let mut changed = core::mem::take(&mut self.changed);
        changed.clone_from(damage);
        changed.clip(self.surface);
        let listed = DamageList::fitted(changed.rects().iter().filter_map(as_damage));
        let Some(listed) = listed else {
            self.changed = changed;
            return Ok(());
        };
        let refresh = self.refresh_back(&changed);
        let back = &mut self.frames[self.back];
        if let Err(refused) = draw(back.memory.as_mut(), self.stride, &refresh) {
            // What the draw left is unknown, so all of it stays owed.
            back.stale = refresh;
            self.changed = changed;
            return Err(refused);
        }
        let id = self.settle_back(refresh, &changed);
        self.changed = changed;
        let reply = self
            .client
            .present(id, listed.rects())
            .map_err(driver_error_from_errno)?;
        self.rotate(reply);
        Ok(())
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        self.client
            .set_power(power)
            .map_err(driver_error_from_errno)
    }

    fn layers(&mut self) -> Option<&mut dyn LayerPresenter> {
        if self.planes.accel.is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn cursor(&mut self) -> Option<&mut dyn CursorPlane> {
        if self.planes.cursor.is_some() {
            Some(self)
        } else {
            None
        }
    }
}

impl<M: BufferMapper, T: DisplayTransport> LayerPresenter for RemoteDisplay<M, T> {
    fn caps(&self) -> AccelCaps {
        self.planes.accel.unwrap_or(AccelCaps {
            max_layers: 0,
            max_width_px: 0,
            max_height_px: 0,
            per_layer_opacity: false,
            hw_scaling: false,
        })
    }

    fn alloc(&mut self, width_px: u32, height_px: u32) -> Result<BufferId, DriverError> {
        if self.layers.is_full() {
            return Err(DriverError::NoSpace);
        }
        let granted = self
            .client
            .alloc_buffer(width_px, height_px)
            .map_err(driver_error_from_errno)?;
        let layer = self
            .take(granted, width_px, height_px)
            .map_err(driver_error_from_errno)?;
        let id = layer.id;
        if self.layers.try_push(layer).is_err() {
            let _ = self.client.free_buffer(id);
            return Err(DriverError::NoSpace);
        }
        Ok(id)
    }

    fn free(&mut self, id: BufferId) -> Result<(), DriverError> {
        let at = self
            .layers
            .iter()
            .position(|layer| layer.id == id)
            .ok_or(DriverError::NotFound)?;
        self.client
            .free_buffer(id)
            .map_err(driver_error_from_errno)?;
        self.layers.swap_remove(at);
        Ok(())
    }

    fn holds(&self, id: BufferId) -> bool {
        self.layers.iter().any(|layer| layer.id == id)
    }

    fn pixels(&mut self, id: BufferId) -> Option<(&mut [u8], u32)> {
        self.layer(id).map(|layer| {
            let stride = layer.stride_bytes;
            (layer.memory.as_mut(), stride)
        })
    }

    fn present_layers(
        &mut self,
        base: Option<Base<'_>>,
        layers: &[AccelLayer],
    ) -> Result<(), DriverError> {
        if self.in_flight.is_some() {
            return Err(DriverError::Busy);
        }
        let Some(base) = base else {
            let reply = self
                .client
                .present_layers(layers)
                .map_err(driver_error_from_errno)?;
            self.settle(reply);
            return Ok(());
        };
        // A base with nothing new is the frame on screen, named again; before
        // any was shown, a base is the whole frame.
        let whole = DamageRect::full(&self.mode);
        let unchanged = self.shown.filter(|_| base.damage.is_empty());
        let damage = if base.damage.is_empty() {
            core::slice::from_ref(&whole)
        } else {
            base.damage
        };
        if unchanged.is_none() {
            DamageRect::validate_list(damage, &self.mode)?;
        }
        let frame_at = unchanged.unwrap_or(self.back);
        let mut stack: ArrayVec<AccelLayer, MAX_ACCEL_LAYERS> = ArrayVec::new();
        let frame = AccelLayer {
            buffer: self.frames[frame_at].id,
            src: whole,
            dst_x: 0,
            dst_y: 0,
            dst_width_px: whole.width_px,
            dst_height_px: whole.height_px,
            opacity: u8::MAX,
        };
        for layer in core::iter::once(&frame).chain(layers) {
            stack
                .try_push(*layer)
                .map_err(|_| DriverError::LengthOutOfRange)?;
        }
        if unchanged.is_none() {
            self.stage(base.frame, damage)?;
        }
        let reply = self
            .client
            .present_layers(&stack)
            .map_err(driver_error_from_errno)?;
        if unchanged.is_some() {
            self.settle(reply);
        } else {
            self.rotate(reply);
        }
        Ok(())
    }
}

impl<M: BufferMapper, T: DisplayTransport> CursorPlane for RemoteDisplay<M, T> {
    fn caps(&self) -> CursorCaps {
        self.planes.cursor.unwrap_or(CursorCaps {
            max_width_px: 0,
            max_height_px: 0,
        })
    }

    fn show(
        &mut self,
        image: &[u8],
        width_px: u32,
        height_px: u32,
        stride_bytes: u32,
        hot_x: u32,
        hot_y: u32,
    ) -> Result<(), DriverError> {
        let caps = CursorPlane::caps(self);
        let bpp = self.mode.format.bytes_per_pixel();
        let row = width_px
            .checked_mul(bpp)
            .filter(|row| *row <= stride_bytes)
            .ok_or(DriverError::LengthOutOfRange)?;
        let needed =
            u64::from(stride_bytes) * u64::from(height_px.saturating_sub(1)) + u64::from(row);
        if width_px == 0
            || height_px == 0
            || width_px > caps.max_width_px
            || height_px > caps.max_height_px
            || hot_x >= width_px
            || hot_y >= height_px
            || (image.len() as u64) < needed
        {
            return Err(DriverError::LengthOutOfRange);
        }
        // One buffer the plane's largest image fits serves every pointer, so a
        // pointer changing size asks for no buffer.
        let (side_x, side_y) = (caps.max_width_px, caps.max_height_px);
        let fits = self
            .cursor
            .as_ref()
            .is_some_and(|held| (held.width_px, held.height_px) == (side_x, side_y));
        if fits {
            let Some(held) = self.cursor.as_mut() else {
                return Err(DriverError::NoSpace);
            };
            draw_image(held, image, stride_bytes, row, height_px)?;
            let id = held.id;
            self.client
                .set_cursor(id, hot_x, hot_y)
                .map_err(driver_error_from_errno)?;
            self.cursor_shown = true;
            return Ok(());
        }
        // A plane whose largest image changed is drawn into a buffer of its own
        // before the one shown is given back, so it never goes blank between.
        let granted = self
            .client
            .alloc_buffer(side_x, side_y)
            .map_err(driver_error_from_errno)?;
        let mut fresh = self
            .take(granted, side_x, side_y)
            .map_err(driver_error_from_errno)?;
        let shown = draw_image(&mut fresh, image, stride_bytes, row, height_px).and_then(|()| {
            self.client
                .set_cursor(fresh.id, hot_x, hot_y)
                .map_err(driver_error_from_errno)
        });
        let unwanted = match shown {
            Ok(()) => {
                self.cursor_shown = true;
                self.cursor.replace(fresh)
            }
            Err(_) => Some(fresh),
        };
        if let Some(unwanted) = unwanted {
            self.client
                .free_buffer(unwanted.id)
                .map_err(driver_error_from_errno)?;
        }
        shown
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
        self.client
            .move_cursor(x, y)
            .map_err(driver_error_from_errno)
    }

    fn hide(&mut self) -> Result<(), DriverError> {
        self.client.hide_cursor().map_err(driver_error_from_errno)?;
        self.cursor_shown = false;
        Ok(())
    }

    fn showing(&self) -> bool {
        self.cursor_shown
    }
}
