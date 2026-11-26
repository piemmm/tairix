//! A window whose frames a [`tairix_gpu`] device draws (`plans/GPU.md` GP6.3).
//!
//! The renderer draws the swapchain's image. A present copies the rows the
//! window's next frame lacks from the image into a staging buffer, on the
//! renderer's queue after the work queued before it. The application's worker
//! owns the window's frame region, attached to the device: it waits on that
//! copy and reads the rows straight into the frame — one copy, made by the
//! driver of a device another process serves — and the loop presents the
//! frame once the worker answers, so no wait on the device runs on the loop.
//!
//! The frames go through the ring every window presents through
//! ([`FrameRing`]): a frame the session still reads is never filled, and what
//! a frame lacks is filled with it.

use alloc::sync::Arc;

use tairix_abi::driver::display::{DamageList, DisplayFormat, DisplayMode};
use tairix_abi::window_ipc::{WindowEvent, WindowSizing};
use tairix_abi::{Duration64, Errno};
use tairix_geometry::{Rect, Region};
use tairix_gpu::{
    Allocation, AllocationDesc, Aspects, Attachable, Attachment, Buffer, BufferDesc,
    BufferImageCopy, BufferUsage, Context, Device, Error, Extent3d, Fence, Format, FormatFeatures,
    Image, ImageDesc, ImageDimension, ImageSubresource, ImageUsage, MemoryProperties, Origin3d,
    Queue, Submission,
};
use tairix_rt::sync::WorkerWake;
use tairix_rt::work::Worker;
use tairix_util::defer::JobQueue;

use crate::client::{PresentState, WindowClient, WindowTransport};
use crate::frames::{region_bytes, FrameRing, FRAME_COUNT};

/// The jobs the worker holds at once: a fill, and the geometry a resize or a
/// re-attach hands it, twice over.
const WORKER_JOBS: usize = 4;

/// How a swapchain paces the frames it hands out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentMode {
    /// The vsync flip: a frame is handed out once the last presented has
    /// finished, so one is on its way to the screen and every one is shown.
    Fifo,
    /// The newest frame wins: one is handed out whenever the session has a
    /// frame to give, and a renderer faster than the display has its
    /// intermediate frames shown to nobody, for the least latency.
    Mailbox,
}

/// What became of the newest present the session finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameFate {
    /// Its serial.
    pub serial: u64,
    /// When, on the monotonic clock: the flip that showed it, or the moment
    /// it was set aside.
    pub at_ns: u64,
    /// Whether the window's pixels are on screen.
    pub shown: bool,
}

/// Why a swapchain refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refused {
    /// The window channel's refusal, [`Errno::WouldBlock`] while no frame may
    /// be presented.
    Window(Errno),
    /// The device's.
    Device(Error),
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Window(errno) => write!(f, "the window was refused ({errno})"),
            Self::Device(error) => write!(f, "the device refused ({error})"),
        }
    }
}

impl From<Errno> for Refused {
    fn from(errno: Errno) -> Self {
        Self::Window(errno)
    }
}

impl From<Error> for Refused {
    fn from(error: Error) -> Self {
        Self::Device(error)
    }
}

/// Where a swapchain's fills are answered: the application, for what it
/// reports, and the wait-set and token its fill wake is added under. The loop
/// tells [`Swapchain::service`] whether its wait reported that token.
#[derive(Clone, Copy, Debug)]
pub struct Waits<'a> {
    /// The application's name.
    pub name: &'a str,
    /// Its loop's wait-set.
    pub set: u64,
    /// The token the fill wake fires under.
    pub token: u64,
}

/// The window a swapchain opens.
#[derive(Clone, Copy, Debug)]
pub struct WindowSpec<'a> {
    /// The endpoint its events go to.
    pub event_endpoint: u64,
    /// Its title.
    pub title: &'a str,
    /// What the window manager may make of its size.
    pub sizing: WindowSizing,
    /// Its client size, in pixels.
    pub size: (u32, u32),
    /// Its frames' encoding.
    pub format: DisplayFormat,
}

/// What the worker owns: the window's frames attached to the device, and the
/// staging memory a present's copy lands in. `None` while released.
struct Filler<B: Attachable> {
    frames: Option<Attachment<B>>,
    staging: Option<Allocation>,
}

/// A job for the worker.
enum Job<B: Attachable> {
    /// Read `len` bytes at `staged` of the staging memory into the frames at
    /// `at`, once `fence` signals: given up past `deadline`, and the frame
    /// drawn again whole.
    Fill {
        fence: Fence,
        deadline: Duration64,
        staged: u64,
        at: usize,
        len: usize,
    },
    /// The frames and staging memory of a new geometry, in place of the last.
    Adopt(Option<(Attachment<B>, Allocation)>),
}

/// A fill's answer; an adoption answers `None`.
type Filled = Option<Result<(), Error>>;

/// The worker the frames are filled on.
type Filling<B> = Worker<Filler<B>, Job<B>, Filled, JobQueue<Job<B>, Filled>>;

/// The worker's whole job: take a new geometry, or fill a frame.
fn work<B: Attachable>(filler: &mut Filler<B>, job: &mut Job<B>) -> Filled {
    match job {
        Job::Adopt(adopted) => {
            let (frames, staging) = adopted.take().unzip();
            filler.frames = frames;
            filler.staging = staging;
            None
        }
        Job::Fill {
            fence,
            deadline,
            staged,
            at,
            len,
        } => Some(fill(filler, (fence, *deadline), *staged, (*at, *len))),
    }
}

/// Wait on `fence` up to `deadline`, then read `len` bytes at `staged` of the
/// staging memory into the frames at `at`.
fn fill<B: Attachable>(
    filler: &mut Filler<B>,
    (fence, deadline): (&Fence, Duration64),
    staged: u64,
    span: (usize, usize),
) -> Result<(), Error> {
    fence.wait(deadline)?;
    let (Some(frames), Some(staging)) = (filler.frames.as_mut(), filler.staging.as_ref()) else {
        return Err(Error::Stale);
    };
    staging.read_into(staged, frames, span)
}

/// What a geometry's frames are drawn from: the image and the staging buffer
/// a present's copy lands in.
struct Target {
    image: Image,
    _memory: Allocation,
    staging: Buffer,
}

/// A window whose frames a device draws.
pub struct Swapchain<B: Attachable> {
    window: u64,
    mode: DisplayMode,
    present_mode: PresentMode,
    /// How long a present's copy may run, and its fill wait: the longest the
    /// device lets a job run.
    deadline: Duration64,
    target: Target,
    worker: Arc<Filling<B>>,
    ring: FrameRing,
    /// The frame the worker is filling and the damage it presents.
    filling: Option<(usize, DamageList)>,
    /// A frame filled whose present waits for one of the session's to return.
    filled: Option<(usize, DamageList)>,
    /// A fill answered for a geometry since replaced, owed no present.
    discarded: usize,
    /// The window's frames are released: drawn again only once re-attached.
    released: bool,
    /// The serial of the newest present the session has yet to finish.
    unfinished: Option<u64>,
    /// The newest present's fate.
    fate: Option<FrameFate>,
}

impl<B: Attachable + 'static> Swapchain<B> {
    /// Open a window as `spec` asks, its frames drawn on `device` in
    /// `context`, paced as `present_mode` says, and start the worker that
    /// fills them, its answers waking `waits`. A machine that grants no
    /// worker has each fill made where it is asked: slower, never wrong.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a frame encoding no image format draws, the
    /// device's refusal of the image, the staging memory or the attachment,
    /// and the window channel's of the region or the window.
    pub fn open<T: WindowTransport<Region = B>>(
        client: &mut WindowClient<T>,
        (device, context): (&Device, &Context),
        (spec, present_mode, waits): (&WindowSpec<'_>, PresentMode, Waits<'_>),
    ) -> Result<Self, Refused> {
        let mode = frames_mode(spec.size, spec.format)?;
        let (frames, target, staging, grant) = geometry(client, (device, context), &mode)?;
        let window = client.create(
            grant,
            spec.event_endpoint,
            FRAME_COUNT,
            &mode,
            spec.title,
            spec.sizing,
        )?;
        let filler = Filler {
            frames: Some(frames),
            staging: Some(staging),
        };
        let worker = Worker::queued(work::<B>, filler, WorkerWake::create(), WORKER_JOBS)
            .map_err(|_| Error::OutOfMemory)?;
        let worker = Arc::new(worker);
        start_filling(&worker, waits);
        Ok(Self {
            window,
            mode,
            present_mode,
            deadline: device.info().limits.max_job_time,
            target,
            worker,
            ring: FrameRing::new(&mode, None),
            filling: None,
            filled: None,
            discarded: 0,
            released: false,
            unfinished: None,
            fate: None,
        })
    }

    /// The window.
    #[must_use]
    pub const fn window(&self) -> u64 {
        self.window
    }

    /// The frames' shape: the window's size, stride and encoding.
    #[must_use]
    pub const fn mode(&self) -> DisplayMode {
        self.mode
    }

    /// The newest present's fate, once the session finished one.
    #[must_use]
    pub const fn fate(&self) -> Option<FrameFate> {
        self.fate
    }

    /// Whether every present made has been finished: none is being filled,
    /// waits to go, or awaits its fate.
    #[must_use]
    pub const fn idle(&self) -> bool {
        self.filling.is_none() && self.filled.is_none() && self.unfinished.is_none()
    }

    /// The image to draw the next frame into, when one may be drawn now:
    /// never while a frame is being filled or waits to be presented, while
    /// the window is hidden or its frames released, or while a present of it
    /// is unanswered or waits for room; under [`PresentMode::Fifo`] only once
    /// the last present has finished.
    pub fn acquire<T: WindowTransport>(&mut self, client: &mut WindowClient<T>) -> Option<&Image> {
        let idle = client.present_state(self.window) == PresentState::Idle;
        (idle && !self.held_back(client)).then_some(&self.target.image)
    }

    /// Whether this side holds the next frame back, whatever the window's
    /// presents: one is being filled or waits to go, the frames are released,
    /// the window is hidden, or `Fifo` awaits the last present's fate.
    fn held_back<T: WindowTransport>(&self, client: &WindowClient<T>) -> bool {
        let paced = self.present_mode == PresentMode::Fifo && self.unfinished.is_some();
        self.filling.is_some()
            || self.filled.is_some()
            || self.released
            || paced
            || client.is_hidden(self.window)
    }

    /// Present `damage` of the image, drawn by work queued on `queue`, a
    /// queue of `context`, before this: the rows the window's next frame
    /// lacks are copied into the staging buffer after that work, and
    /// [`Self::service`] presents the frame once the worker has filled it.
    /// Answers whether the frame was presented here instead: one lacking no
    /// rows, or one filled on this thread, there being no worker to wake the
    /// loop.
    ///
    /// # Errors
    ///
    /// [`Errno::WouldBlock`] where [`Self::acquire`] would hand nothing out,
    /// and the device's refusal of the copy, which leaves every frame to be
    /// drawn again whole.
    pub fn present<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        (context, queue): (&Context, &Queue),
        damage: DamageList,
    ) -> Result<bool, Refused> {
        if self.held_back(client) {
            return Err(Errno::WouldBlock.into());
        }
        match client.present_state(self.window) {
            PresentState::Idle => {}
            PresentState::InFlight => {
                client.owe_frame(self.window);
                return Err(Errno::WouldBlock.into());
            }
            // Posted by the client's next drain, whenever that runs, so a fill
            // of the frame it names would race the session reading it.
            PresentState::Waiting(_) => return Err(Errno::WouldBlock.into()),
        }
        let index = self.ring.back();
        let bounds = Rect::new(0, 0, self.mode.width_px, self.mode.height_px);
        let mut rows = None;
        self.ring
            .bring_up_to_date(index, (damage.rects(), bounds), |refresh| {
                rows = row_span(refresh);
                Ok(())
            })?;
        let Some(rows) = rows else {
            let serial = self.ring.commit(client, self.window, index, damage)?;
            self.unfinished = Some(serial);
            return Ok(true);
        };
        let queued = fill_job(
            (&self.target, &self.mode, self.deadline),
            (index, rows),
            (context, queue),
        )
        .and_then(|job| {
            self.worker
                .submit(job)
                .map_err(|_| Refused::Window(Errno::WouldBlock))
        });
        let filled_here = match queued {
            Ok(filled_here) => filled_here,
            Err(refused) => {
                self.ring.invalidate(&self.mode, None);
                return Err(refused);
            }
        };
        self.filling = Some((index, damage));
        if filled_here {
            self.service(client, false)
        } else {
            Ok(false)
        }
    }

    /// Take what the worker answered and present a frame it filled, answering
    /// whether one was presented: on any turn of the loop, `woken` saying
    /// whether its wait reported the fill wake's token, whose nudge is taken
    /// then and only then. A frame the worker could not fill is drawn again
    /// whole by the next present; one whose present finds both frames the
    /// session's waits for one to return.
    ///
    /// # Errors
    ///
    /// The device's refusal of a fill, and the window channel's of a present.
    pub fn service<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        woken: bool,
    ) -> Result<bool, Refused> {
        // The wake's read blocks on an empty pipe, so it is taken only once
        // the wait reported it readable.
        if woken {
            self.worker.wake().drain();
        }
        let mut failed = None;
        let (filling, discarded, filled) =
            (&mut self.filling, &mut self.discarded, &mut self.filled);
        self.worker.collect_landed(|answer| {
            let Some(result) = answer else {
                return;
            };
            if *discarded > 0 {
                *discarded -= 1;
                return;
            }
            match (filling.take(), result) {
                (Some(frame), Ok(())) => *filled = Some(frame),
                (_, Err(error)) => failed = Some(error),
                (None, Ok(())) => {}
            }
        });
        if let Some(error) = failed {
            self.ring.invalidate(&self.mode, None);
            return Err(error.into());
        }
        let Some((index, damage)) = self.filled.take() else {
            return Ok(false);
        };
        match self.ring.commit(client, self.window, index, damage) {
            Ok(serial) => {
                self.unfinished = Some(serial);
                Ok(true)
            }
            Err(Errno::WouldBlock) => {
                self.filled = Some((index, damage));
                Ok(false)
            }
            Err(errno) => Err(errno.into()),
        }
    }

    /// Take `event`, answering whether it was this swapchain's: a frame's
    /// fate, which advances the pacing.
    pub fn event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::FramePresented {
            window_id,
            serial,
            at_ns,
            shown,
        } = *event
        else {
            return false;
        };
        if window_id != self.window {
            return false;
        }
        if self.fate.is_some_and(|newest| serial <= newest.serial) {
            return true;
        }
        if self.unfinished.is_some_and(|waited| serial >= waited) {
            self.unfinished = None;
        }
        self.fate = Some(FrameFate {
            serial,
            at_ns,
            shown,
        });
        true
    }

    /// Let the frames go, the session having released its side: the window
    /// is drawn again once [`Self::reattach`] makes them anew.
    ///
    /// # Errors
    ///
    /// The worker's refusal of the job for want of room, which leaves the
    /// frames held.
    pub fn release(&mut self) -> Result<(), Refused> {
        self.adopt(None)?;
        self.released = true;
        Ok(())
    }

    /// Make the window's frames anew at its size and re-map the window onto
    /// them, after [`Self::release`].
    ///
    /// # Errors
    ///
    /// As [`Self::resize`].
    pub fn reattach<T: WindowTransport<Region = B>>(
        &mut self,
        client: &mut WindowClient<T>,
        (device, context): (&Device, &Context),
    ) -> Result<(), Refused> {
        let size = (self.mode.width_px, self.mode.height_px);
        self.resize(client, (device, context), size)
    }

    /// Make the image, staging buffer and frames again at `size`, and re-map
    /// the window onto the frames. A present waiting on a fill of the last
    /// geometry is dropped, and the frames lack everything.
    ///
    /// # Errors
    ///
    /// [`Errno::WouldBlock`] while the worker holds as many jobs as it has
    /// room for, the device's refusal of the new image, staging memory or
    /// attachment, and the window channel's of the region or the re-map; the
    /// window keeps the geometry it had.
    pub fn resize<T: WindowTransport<Region = B>>(
        &mut self,
        client: &mut WindowClient<T>,
        (device, context): (&Device, &Context),
        size: (u32, u32),
    ) -> Result<(), Refused> {
        let mode = frames_mode(size, self.mode.format)?;
        // Once the re-map is posted the window has the new geometry, so the
        // worker must have room to take its frames.
        if !self.worker.room() {
            return Err(Errno::WouldBlock.into());
        }
        let (frames, target, staging, grant) = geometry(client, (device, context), &mode)?;
        client.resize(self.window, grant, FRAME_COUNT, &mode, None)?;
        self.adopt(Some((frames, staging)))?;
        if self.filling.take().is_some() {
            self.discarded += 1;
        }
        self.filled = None;
        self.unfinished = None;
        self.released = false;
        self.mode = mode;
        self.target = target;
        self.ring.reset(&mode, None);
        Ok(())
    }

    /// Hand the worker the frames and staging memory to fill from.
    fn adopt(&self, adopted: Option<(Attachment<B>, Allocation)>) -> Result<(), Refused> {
        self.worker
            .submit(Job::Adopt(adopted))
            .map(|_| ())
            .map_err(|_| Refused::Window(Errno::WouldBlock))
    }
}

/// The fill of rows `rows` of frame `index` of a window shaped as `mode`:
/// the rows copied from `target`'s image into its staging buffer on `queue`,
/// a queue of `context`, run at most `deadline`, and the job that reads them
/// into the frame once the copy lands.
fn fill_job<B: Attachable>(
    (target, mode, deadline): (&Target, &DisplayMode, Duration64),
    (index, (top, bottom)): (usize, (u32, u32)),
    (context, queue): (&Context, &Queue),
) -> Result<Job<B>, Refused> {
    let stride = mode.stride_bytes;
    let staged = u64::from(top) * u64::from(stride);
    let mut commands = context.command_buffer();
    commands.copy_image_to_buffer(
        &target.image,
        &target.staging,
        &[BufferImageCopy {
            buffer_offset: staged,
            bytes_per_row: stride,
            rows_per_image: bottom - top,
            image: ImageSubresource {
                aspect: Aspects::COLOR,
                level: 0,
                base_layer: 0,
                layers: 1,
            },
            origin: Origin3d { x: 0, y: top, z: 0 },
            extent: Extent3d {
                width: mode.width_px,
                height: bottom - top,
                depth: 1,
            },
        }],
    )?;
    let fence = queue
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[],
            signals: &[],
            deadline,
        })
        .map_err(|refused| refused.error)?;
    let frame = region_bytes(mode, 1).ok_or(Errno::OutOfRange)?;
    let start = usize::try_from(staged).map_err(|_| Errno::OutOfRange)?;
    let len = usize::try_from(u64::from(bottom - top) * u64::from(stride))
        .map_err(|_| Errno::OutOfRange)?;
    let at = index
        .checked_mul(frame)
        .and_then(|base| base.checked_add(start))
        .ok_or(Errno::OutOfRange)?;
    Ok(Job::Fill {
        fence,
        deadline,
        staged,
        at,
        len,
    })
}

/// Start `worker` on a thread of its own, its answers waking `waits`.
#[cfg(freestanding)]
fn start_filling<B: Attachable + 'static>(worker: &Arc<Filling<B>>, waits: Waits<'_>) {
    crate::app::start_worker(waits.name, worker, waits.set, waits.token, "frame fill");
}

/// A host has no wake pipe, so its worker stops at once and each fill is made
/// where it is asked.
#[cfg(not(freestanding))]
fn start_filling<B: Attachable + 'static>(worker: &Arc<Filling<B>>, _: Waits<'_>) {
    let _ = Worker::start(worker);
}

/// The frames of a window `size` pixels across, encoded as `format`: rows of
/// whole pixels, packed.
fn frames_mode(
    (width_px, height_px): (u32, u32),
    format: DisplayFormat,
) -> Result<DisplayMode, Refused> {
    let stride_bytes = width_px
        .checked_mul(format.bytes_per_pixel())
        .ok_or(Errno::OutOfRange)?;
    if width_px == 0 || height_px == 0 {
        return Err(Errno::OutOfRange.into());
    }
    Ok(DisplayMode {
        width_px,
        height_px,
        stride_bytes,
        format,
    })
}

/// The image format a frame encoded as `format` is drawn in.
fn image_format(format: DisplayFormat) -> Result<Format, Error> {
    match format {
        DisplayFormat::Bgra8888 => Ok(Format::Bgra8Unorm),
        DisplayFormat::Rgba8888 => Ok(Format::Rgba8Unorm),
        _ => Err(Error::Unsupported),
    }
}

/// The frames of `mode`, granted to the session and attached to `device`;
/// the image and staging buffer they are drawn from; the staging memory; and
/// the frames' grant.
fn geometry<B: Attachable, T: WindowTransport<Region = B>>(
    client: &mut WindowClient<T>,
    (device, context): (&Device, &Context),
    mode: &DisplayMode,
) -> Result<(Attachment<B>, Target, Allocation, u64), Refused> {
    let len = region_bytes(mode, FRAME_COUNT).ok_or(Errno::OutOfRange)?;
    let (region, grant) = client.grant_region(len)?;
    let frames = device.attach(region)?;
    let (target, staging) = target(device, context, mode)?;
    Ok((frames, target, staging, grant))
}

/// The image a window shaped as `mode` is drawn in, and a host-visible
/// staging buffer of one frame, with its memory.
fn target(
    device: &Device,
    context: &Context,
    mode: &DisplayMode,
) -> Result<(Target, Allocation), Error> {
    let format = image_format(mode.format)?;
    let mut usage = ImageUsage::COLOR_ATTACHMENT
        .with(ImageUsage::TRANSFER_SRC)
        .with(ImageUsage::TRANSFER_DST)
        .with(ImageUsage::SAMPLED);
    if device
        .info()
        .formats
        .get(format)
        .contains(FormatFeatures::STORAGE)
    {
        usage = usage.with(ImageUsage::STORAGE);
    }
    let desc = ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width: mode.width_px,
            height: mode.height_px,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage,
    };
    let required = context.image_requirements(&desc)?;
    let memory = context.allocate(&AllocationDesc {
        size: required.size,
        memory_type: required.memory_types.trailing_zeros(),
    })?;
    let image = context.create_image(&memory, 0, &desc)?;
    let size = u64::from(mode.stride_bytes) * u64::from(mode.height_px);
    let near = host_visible(device).ok_or(Error::NoSuchMemoryType)?;
    let staging_memory = context.allocate(&AllocationDesc {
        size,
        memory_type: near,
    })?;
    let staging = context.create_buffer(
        &staging_memory,
        &BufferDesc {
            offset: 0,
            size,
            usage: BufferUsage::TRANSFER_DST,
        },
    )?;
    Ok((
        Target {
            image,
            _memory: memory,
            staging,
        },
        staging_memory,
    ))
}

/// The first of `device`'s memory types the CPU reaches coherently.
fn host_visible(device: &Device) -> Option<u32> {
    let wanted = MemoryProperties::HOST_VISIBLE.with(MemoryProperties::HOST_COHERENT);
    let at = device
        .info()
        .memory_types
        .iter()
        .position(|kind| kind.properties.contains(wanted))?;
    u32::try_from(at).ok()
}

/// The rows `region` spans, top inclusive and bottom exclusive, or `None` for
/// an empty region.
fn row_span(region: &Region) -> Option<(u32, u32)> {
    let rects = region.rects();
    let top = rects.iter().map(Rect::top).min()?;
    let bottom = rects.iter().map(Rect::bottom).max()?;
    Some((u32::try_from(top).ok()?, u32::try_from(bottom).ok()?))
        .filter(|(top, bottom)| top < bottom)
}

#[cfg(test)]
#[path = "swapchain_tests.rs"]
mod tests;
