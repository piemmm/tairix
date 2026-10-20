//! TAIRiX virtio-gpu display driver.
//!
//! The 2D device of virtio 1.2 §5.7 (device type 16), driven as the seat's
//! display over the bus-agnostic split-virtqueue transport in `lib/virtio`.
//! The engine ([`VirtioGpu`]), [`BIND_KEYS`] and [`register`] are this
//! crate's `lib` target, co-located in the driver because a display adapter
//! has no charter-legal non-driver consumer; `src/main.rs` is the `Run`
//! binary the signed bundle installs.
//!
//! # What the device is given
//!
//! The output is the first scanout the device reports enabled, at the size
//! its display information states, or, where that names no size, the
//! preferred timing of the EDID it offers; its rate is the EDID's for a timing
//! of the size it shows. A presenter's frame is the backing of a 2D resource
//! the driver creates for it, carved in memory the device reads and granted to
//! the presenter, so a present copies no pixel:
//! it has the device transfer the damaged rectangles from the backing into
//! its copy and flush them to the scanout. The device answers every command,
//! and a present returns once it has, so the frame is the presenter's again
//! the moment the call does. The pointer is the device's cursor; the kernel
//! console the node hosts is a resource of its own backed by the memory the
//! kernel paints it in.
//!
//! # Ordering
//!
//! A present's commands go to the device in one batch: the transfers, the
//! scan-out switch, the flushes. The device processes its control queue in
//! order, as every virtio-gpu device does and Linux's driver relies on.
//!
//! # Capabilities
//!
//! Loading requires [`CapabilityId::DRV_LOAD`]. The process reaches its
//! hardware only through the grants its matched node requested.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(test)]
extern crate alloc;

pub mod wire;

use tairix_abi::display_ipc::DISPLAY_MAX_BUFFERS;
use tairix_abi::driver::display::{
    AllocatedBuffer, BufferId, CursorCaps, DamageRect, Display, DisplayFormat, DisplayMode,
    DisplayPower, HardwareCursor, HostedConsole, Presented, RefreshRate, Serviced,
    MAX_DAMAGE_RECTS,
};
use tairix_abi::driver::dma::DmaSlab;
use tairix_abi::driver::virtio::{VirtioHost, VIRTIO_GPU_DEVICE_ID};
use tairix_abi::seat::ReleaseSurface;
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost, HwMatchKey};
use tairix_display::{
    copy_rows, frames_budget, BufferIds, BufferMemory, DeviceBuffer, SharedBuffer,
};
use tairix_drvrt::Recorder;
use tairix_inline::ArrayVec;
use tairix_virtio::{
    ChainSegment, Direction, RequestQueue, SplitQueue, Status, Transport, VirtioError,
    TRANSPORT_FEATURES,
};

use wire::{Format, Request};

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x5647_5055_0000_0001; // "VGPU"

/// The bind priority [`BIND_KEYS`] carries: a virtio device id is an exact
/// match.
const BIND_PRIORITY: u16 = 10;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the transport's windows, the buffers and console its device reads, the
/// device interrupt, granting the buffers, the seat's display endpoint, and
/// the decisions it records.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::MEM_DMA,
    CapabilityId::IRQ_BIND,
    CapabilityId::SHM,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::LOG_EMIT,
];

/// This driver's hardware bind table: a virtio GPU on either transport.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    HwMatchKey::virtio(VIRTIO_GPU_DEVICE_ID),
)];

/// The widest and tallest output the driver drives, as Linux's virtio-gpu
/// bounds its modes (`XRES_MAX`, `YRES_MAX`): a bound on what a device
/// states, which keeps every backing addressable by one memory entry.
pub const SIDE_MAX: u32 = 8192;

/// The pointer image's side: the cursor resource a device shows is exactly
/// this size, as Linux's driver and QEMU's device both fix it.
pub const CURSOR_SIDE: u32 = 64;

const CURSOR_STRIDE: usize = CURSOR_SIDE as usize * 4;

const CURSOR_BYTES: u32 = CURSOR_SIDE * CURSOR_SIDE * 4;

/// How long one batch of commands may take before it is failed closed: far
/// past any 2D command, and short of a user noticing a hang.
const COMMAND_TIMEOUT_NS: u64 = 1_000_000_000;

/// Commands one batch carries at most: a fresh frame's creation and backing,
/// a transfer per damage rectangle, the scan-out switch, the unreference of
/// a frame it replaces, and a flush per damage rectangle.
const BATCH_MAX: usize = 2 * MAX_DAMAGE_RECTS + 4;

/// Bytes one command's request slot holds.
const REQUEST_SLOT: usize = 64;

/// Bytes one command's response slot holds: a header.
const RESPONSE_SLOT: usize = 32;

const _: () = assert!(wire::REQUEST_MAX <= REQUEST_SLOT && wire::HEADER_LEN <= RESPONSE_SLOT);

/// Where the command area's response slots start.
const RESPONSES_AT: usize = BATCH_MAX * REQUEST_SLOT;

/// Where the one long response — display information or an EDID — lands.
const LONG_AT: usize = RESPONSES_AT + BATCH_MAX * RESPONSE_SLOT;

/// The longest response the driver reads.
const LONG_LEN: usize = if wire::EDID_LEN > wire::DISPLAY_INFO_LEN {
    wire::EDID_LEN
} else {
    wire::DISPLAY_INFO_LEN
};

/// Where the cursor queue's one request lies.
const CURSOR_AT: usize = LONG_AT + LONG_LEN;

/// Bytes of the command area.
const COMMAND_AREA: usize = CURSOR_AT + REQUEST_SLOT;

/// Descriptors one batch occupies on the control queue: a request and its
/// response per command.
#[allow(clippy::cast_possible_truncation)] // A few dozen, asserted to fit below.
const CONTROL_DESCRIPTORS: u16 = (2 * BATCH_MAX) as u16;

const _: () = assert!(2 * BATCH_MAX <= u16::MAX as usize);

/// Descriptors the control queue is programmed with: a batch's, in the power
/// of two the split ring wants.
const CONTROL_QUEUE_SIZE: u16 = CONTROL_DESCRIPTORS.next_power_of_two();

/// Times one service reads the output again for changes raised while it read
/// it: past that, a device raising them without end waits for the next.
const REREADS_PER_SERVICE: usize = 4;

/// Descriptors the cursor queue is programmed with: one command at a time.
const CURSOR_QUEUE_SIZE: u16 = 4;

/// What the driver logs once the console is on the device's screen, which a
/// harness keys a look at that screen on.
pub const CONSOLE_SHOWN_MESSAGE: &str = "display: the console is shown on the virtio GPU";

/// A decision the engine records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// The kernel console is on the device's screen, hosted at this mode.
    ConsoleShown {
        /// Its width in pixels.
        width: u32,
        /// Its height in pixels.
        height: u32,
    },
    /// The kernel console could not be hosted at the output's mode.
    ConsoleUnhosted {
        /// Why.
        reason: DriverError,
    },
    /// What the seat shows — the console, nothing, or the pointer gone —
    /// could not be shown.
    ShowRefused {
        /// Why.
        reason: DriverError,
    },
    /// The console's newly painted rows could not be shown.
    ConsoleUnshown {
        /// Why.
        reason: DriverError,
    },
}

/// The kernel console a node hosts, behind a seam.
pub trait ConsoleSource {
    /// Host the console at `width` × `height`, answering where the device
    /// reads it.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn adopt(&mut self, width: u32, height: u32) -> Result<HostedConsole, DriverError>;

    /// Give the console's memory back: the device no longer reads it.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn relinquish(&mut self) -> Result<(), DriverError>;

    /// The rows the console painted since last asked, or `None`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn damage(&mut self) -> Result<Option<DamageRect>, DriverError>;
}

/// The output the driver drives.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Output {
    scanout: u32,
    mode: DisplayMode,
    /// The rate its sink states for a timing of the size it shows.
    refresh: Option<RefreshRate>,
}

/// The resource memory the device reads backs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Backing {
    /// None yet.
    Unmade,
    /// This resource.
    Made(u32),
    /// This resource may exist: the batch that made it went unanswered. It
    /// is unreferenced before the memory is used again, and detached before
    /// the memory is given back.
    Doubtful(u32),
}

impl Backing {
    /// The resource the device may hold the buffer as a backing of.
    const fn resource(self) -> Option<u32> {
        match self {
            Self::Unmade => None,
            Self::Made(resource) | Self::Doubtful(resource) => Some(resource),
        }
    }
}

/// One buffer a presenter fills.
struct Buffer<R> {
    id: BufferId,
    memory: R,
    width: u32,
    height: u32,
    backing: Backing,
    /// The presenter gave it back, and the device has not yet let go of its
    /// backing: held, and counted, until it does.
    given_back: bool,
}

impl<R> Buffer<R> {
    fn len(&self) -> usize {
        self.width as usize * 4 * self.height as usize
    }
}

/// What the scanout shows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Shown {
    /// Nothing.
    Nothing,
    /// The hosted console.
    Console,
    /// A presenter's frame, by its resource and size; `buffer` is `None` once
    /// the presenter gave it back while it was still shown.
    Frame {
        resource: u32,
        size: (u32, u32),
        buffer: Option<BufferId>,
    },
}

/// The kernel console hosted on the device: its resource, and the mode it
/// is painted at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Hosted {
    resource: u32,
    mode: DisplayMode,
}

/// The kernel console as the driver holds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Hosting {
    /// Not held.
    Unheld,
    /// Held, and made a resource of.
    Hosted(Hosted),
    /// Held, but the batch that would have made this resource of it went
    /// unanswered: its memory goes back only once the resource is
    /// unreferenced.
    Doubtful(u32),
}

impl Hosting {
    /// The console, where it can be shown.
    const fn hosted(self) -> Option<Hosted> {
        match self {
            Self::Hosted(hosted) => Some(hosted),
            Self::Unheld | Self::Doubtful(_) => None,
        }
    }

    /// The resource the device may hold the console as the backing of.
    const fn resource(self) -> Option<u32> {
        match self {
            Self::Unheld => None,
            Self::Hosted(Hosted { resource, .. }) | Self::Doubtful(resource) => Some(resource),
        }
    }
}

/// The pointer: the driver's own image of it, the resource that image backs,
/// and where it is.
struct Pointer {
    image: DmaSlab,
    resource: Backing,
    at: (i32, i32),
    hot: (u32, u32),
    shown: bool,
}

/// The device's answers to one batch, read once it is back.
struct Answers<'a> {
    area: &'a [u8],
    count: usize,
}

impl Answers<'_> {
    /// The answer to the command at `at`.
    fn answer(&self, at: usize) -> Result<(), DriverError> {
        let start = RESPONSES_AT + at * RESPONSE_SLOT;
        wire::ok_nodata(
            self.area
                .get(start..start + wire::HEADER_LEN)
                .ok_or(DriverError::DeviceFault)?,
        )
    }

    /// Whether the command at `at` was done.
    fn done(&self, at: usize) -> bool {
        self.answer(at).is_ok()
    }

    /// The first refusal among the answers. The unreference at `retire` of a
    /// resource already gone — a batch that answered late took it — counts
    /// as done.
    fn verdict(&self, retire: Option<usize>) -> Result<(), DriverError> {
        (0..self.count).try_for_each(|at| match self.answer(at) {
            Err(DriverError::NotFound) if retire == Some(at) => Ok(()),
            answer => answer,
        })
    }
}

/// The virtio-gpu engine.
pub struct VirtioGpu<'h, T: Transport, M: BufferMemory, C, L> {
    transport: T,
    host: &'h dyn VirtioHost,
    control: RequestQueue,
    cursor_queue: RequestQueue,
    commands: DmaSlab,
    memory: M,
    console: Option<C>,
    log: L,
    edid: bool,
    output: Option<Output>,
    /// The device said its displays changed, and the output has not been
    /// read since.
    reread_owed: bool,
    buffers: ArrayVec<Buffer<M::Region>, DISPLAY_MAX_BUFFERS>,
    held_bytes: usize,
    ids: BufferIds,
    next_resource: u32,
    shown: Shown,
    /// Whether the seat shows text, so the console is shown once hosted.
    text: bool,
    power: DisplayPower,
    hosting: Hosting,
    pointer: Pointer,
}

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// A rectangle covering `width` × `height` from the origin.
const fn whole(width: u32, height: u32) -> DamageRect {
    DamageRect {
        x: 0,
        y: 0,
        width_px: width,
        height_px: height,
    }
}

/// Where `rect`'s first pixel lies in a backing `width` pixels wide.
fn offset_of(rect: &DamageRect, width: u32) -> u64 {
    (u64::from(rect.y) * u64::from(width) + u64::from(rect.x)) * 4
}

/// The output a display-information response and the first enabled
/// scanout's `sink` timings name: that scanout, at the size the response
/// states or, failing one, the sink's preferred timing's, at the rate the sink
/// states for a timing of that size — its preferred timing's first.
fn output_of(
    scanouts: &[wire::Scanout; wire::SCANOUTS_MAX],
    sink: &[tairix_edid::Mode],
) -> Option<Output> {
    let (index, scanout) = scanouts
        .iter()
        .enumerate()
        .find(|(_, scanout)| scanout.enabled)?;
    let scanout_id = u32::try_from(index).ok()?;
    let preferred = sink
        .iter()
        .find(|mode| mode.preferred)
        .or_else(|| sink.first());
    let (width, height) = if scanout.width != 0 && scanout.height != 0 {
        (scanout.width, scanout.height)
    } else {
        preferred.map(|mode| mode.timing.active_size())?
    };
    if width == 0 || height == 0 || width > SIDE_MAX || height > SIDE_MAX {
        return None;
    }
    let refresh = preferred.into_iter().chain(sink).find_map(|mode| {
        (mode.timing.active_size() == (width, height))
            .then(|| RefreshRate::of(&mode.timing))
            .flatten()
    });
    Some(Output {
        scanout: scanout_id,
        mode: DisplayMode {
            width_px: width,
            height_px: height,
            stride_bytes: width * 4,
            format: DisplayFormat::Bgra8888,
        },
        refresh,
    })
}

impl<'h, T, M, C, L> VirtioGpu<'h, T, M, C, L>
where
    T: Transport,
    M: BufferMemory,
    M::Region: DeviceBuffer,
    C: ConsoleSource,
    L: Recorder<Record>,
{
    /// Bring the device up: features negotiated, both queues programmed, the
    /// output read, and the console, where the node hosts one, shown on it.
    ///
    /// # Errors
    ///
    /// The transport's refusal, a device missing a queue, no memory for the
    /// commands, or a device that refused to describe its output.
    pub fn open(
        mut transport: T,
        host: &'h dyn VirtioHost,
        memory: M,
        console: Option<C>,
        log: L,
    ) -> Result<Self, DriverError> {
        let negotiated = tairix_virtio::negotiate::<_, DriverError>(
            &mut transport,
            || host.device_quiesced(),
            |offered| Ok(offered & (TRANSPORT_FEATURES | wire::FEATURE_EDID)),
        )?;
        if transport.num_queues() <= wire::CURSOR_QUEUE {
            return Err(DriverError::DeviceFault);
        }
        let program = |transport: &mut T, index, size, needed| {
            SplitQueue::new(transport, host, index, size, needed)
                .map(RequestQueue::new)
                .map_err(VirtioError::as_driver_error)
        };
        let control = program(
            &mut transport,
            wire::CONTROL_QUEUE,
            CONTROL_QUEUE_SIZE,
            CONTROL_DESCRIPTORS,
        )?;
        let cursor_queue = program(&mut transport, wire::CURSOR_QUEUE, CURSOR_QUEUE_SIZE, 1)?;
        let commands = host.alloc_dma_zeroed(COMMAND_AREA)?;
        let image = host.alloc_dma_zeroed(CURSOR_BYTES as usize)?;
        transport.set_status(negotiated.status.with(Status::DRIVER_OK));
        let mut gpu = Self {
            transport,
            host,
            control,
            cursor_queue,
            commands,
            memory,
            console,
            log,
            edid: negotiated.features & wire::FEATURE_EDID != 0,
            output: None,
            reread_owed: false,
            buffers: ArrayVec::new(),
            held_bytes: 0,
            ids: BufferIds::new(),
            next_resource: 0,
            shown: Shown::Nothing,
            text: true,
            power: DisplayPower::On,
            hosting: Hosting::Unheld,
            pointer: Pointer {
                image,
                resource: Backing::Unmade,
                at: (0, 0),
                hot: (0, 0),
                shown: false,
            },
        };
        gpu.take_display_events();
        gpu.output = gpu.read_output()?;
        gpu.host_console();
        if gpu.hosting.hosted().is_some() {
            gpu.show_or_record(Shown::Console);
        }
        Ok(gpu)
    }

    /// Show `what`, recording a refusal, and that the console is shown.
    fn show_or_record(&mut self, what: Shown) {
        match self.show(what) {
            Ok(()) => self.note_console_shown(),
            Err(reason) => self.log.record(Record::ShowRefused { reason }),
        }
    }

    /// Record that the console is on the screen, where it is.
    fn note_console_shown(&mut self) {
        if let (Shown::Console, Some(hosted), DisplayPower::On) =
            (self.shown, self.hosting.hosted(), self.power)
        {
            self.log.record(Record::ConsoleShown {
                width: hosted.mode.width_px,
                height: hosted.mode.height_px,
            });
        }
    }

    /// Clear the display events the device has raised, answering whether it
    /// had.
    fn take_display_events(&mut self) -> bool {
        let raised = self.transport.read_config_u32(wire::CONFIG_EVENTS_READ) & wire::EVENT_DISPLAY;
        if raised != 0 {
            self.transport
                .write_config(wire::CONFIG_EVENTS_CLEAR, &raised.to_le_bytes());
        }
        raised != 0
    }

    /// Publish `batch`, each command in its own slot, and wait for every
    /// answer.
    ///
    /// # Errors
    ///
    /// The queue's refusal, or a device that did not answer in time: the
    /// commands' memory may then still be read.
    fn submit(&mut self, batch: &[Request]) -> Result<Answers<'_>, DriverError> {
        if batch.len() > BATCH_MAX {
            return Err(DriverError::LengthOutOfRange);
        }
        self.control.settle(&mut self.transport, self.host)?;
        let area = self.commands.as_bytes_mut();
        for (at, request) in batch.iter().enumerate() {
            let bytes = request.as_bytes();
            area[at * REQUEST_SLOT..at * REQUEST_SLOT + bytes.len()].copy_from_slice(bytes);
            let response = RESPONSES_AT + at * RESPONSE_SLOT;
            area[response..response + RESPONSE_SLOT].fill(0);
        }
        let mut chains: ArrayVec<[ChainSegment; 2], BATCH_MAX> = ArrayVec::new();
        for (at, request) in batch.iter().enumerate() {
            let len = request.as_bytes().len();
            let chain = [
                self.segment(at * REQUEST_SLOT, len, Direction::DeviceRead)?,
                self.segment(
                    RESPONSES_AT + at * RESPONSE_SLOT,
                    wire::HEADER_LEN,
                    Direction::DeviceWrite,
                )?,
            ];
            chains
                .try_push(chain)
                .map_err(|_| DriverError::LengthOutOfRange)?;
        }
        let mut requests: ArrayVec<&[ChainSegment], BATCH_MAX> = ArrayVec::new();
        for chain in &chains {
            requests
                .try_push(chain)
                .map_err(|_| DriverError::LengthOutOfRange)?;
        }
        self.control.submit_all_and_wait(
            &mut self.transport,
            self.host,
            &requests,
            COMMAND_TIMEOUT_NS,
        )?;
        Ok(Answers {
            area: self.commands.as_bytes(),
            count: batch.len(),
        })
    }

    /// Run `batch` and read the device's verdict on it.
    fn run(&mut self, batch: &[Request]) -> Result<(), DriverError> {
        self.submit(batch)?.verdict(None)
    }

    /// `backing` with any doubt settled: a resource an unanswered batch may
    /// have made is unreferenced, and an answer of any kind means it no
    /// longer exists.
    ///
    /// # Errors
    ///
    /// A device that does not answer: the doubt stands.
    fn settled(&mut self, backing: Backing) -> Result<Backing, DriverError> {
        match backing {
            Backing::Doubtful(resource) => {
                self.submit(&[wire::resource_unref(resource)])?;
                Ok(Backing::Unmade)
            }
            known => Ok(known),
        }
    }

    /// The segment over `len` bytes of the command area at `offset`.
    fn segment(
        &self,
        offset: usize,
        len: usize,
        direction: Direction,
    ) -> Result<ChainSegment, DriverError> {
        Ok(ChainSegment {
            device_addr: self
                .commands
                .device_addr_at(offset, len)
                .ok_or(DriverError::LengthOutOfRange)?,
            len: u32::try_from(len).map_err(|_| DriverError::LengthOutOfRange)?,
            direction,
        })
    }

    /// Send `request`, whose answer is up to `len` bytes, and answer those
    /// bytes.
    fn ask(&mut self, request: &Request, len: usize) -> Result<&[u8], DriverError> {
        self.control.settle(&mut self.transport, self.host)?;
        let bytes = request.as_bytes();
        let area = self.commands.as_bytes_mut();
        area[..bytes.len()].copy_from_slice(bytes);
        area[LONG_AT..LONG_AT + len].fill(0);
        let chain = [
            self.segment(0, bytes.len(), Direction::DeviceRead)?,
            self.segment(LONG_AT, len, Direction::DeviceWrite)?,
        ];
        self.control
            .submit_and_wait(&mut self.transport, self.host, &chain, COMMAND_TIMEOUT_NS)?;
        Ok(&self.commands.as_bytes()[LONG_AT..LONG_AT + len])
    }

    /// The output the device describes now.
    fn read_output(&mut self) -> Result<Option<Output>, DriverError> {
        let scanouts =
            wire::display_info(self.ask(&wire::get_display_info(), wire::DISPLAY_INFO_LEN)?)?;
        let sink = match scanouts.iter().position(|scanout| scanout.enabled) {
            Some(index) if self.edid => {
                self.sink(u32::try_from(index).map_err(|_| DriverError::DeviceFault)?)
            }
            _ => None,
        };
        Ok(output_of(
            &scanouts,
            sink.as_ref().map_or(&[], |edid| edid.modes.as_slice()),
        ))
    }

    /// Scanout `scanout`'s sink as its EDID describes it, or `None` where it
    /// offers none the driver can read.
    fn sink(&mut self, scanout: u32) -> Option<tairix_edid::Edid> {
        let response = self.ask(&wire::get_edid(scanout), wire::EDID_LEN).ok()?;
        tairix_edid::Edid::parse(wire::edid(response).ok()?).ok()
    }

    /// A resource id no live resource carries.
    fn fresh_resource(&mut self) -> u32 {
        loop {
            self.next_resource = self.next_resource.wrapping_add(1).max(1);
            let id = self.next_resource;
            let in_use = self
                .buffers
                .iter()
                .any(|buffer| buffer.backing.resource() == Some(id))
                || self.hosting.resource() == Some(id)
                || self.pointer.resource.resource() == Some(id)
                || matches!(self.shown, Shown::Frame { resource, .. } if resource == id);
            if !in_use {
                return id;
            }
        }
    }

    /// Host the console at the output's mode where it is not, recording a
    /// refusal.
    fn host_console(&mut self) {
        if self.hosting.hosted().is_some() {
            return;
        }
        // A resource an unanswered batch may have made of it goes first.
        if let Err(reason) = self.unhost_console() {
            self.log.record(Record::ConsoleUnhosted { reason });
            return;
        }
        let (Some(output), Some(console)) = (self.output, self.console.as_mut()) else {
            return;
        };
        let (width, height) = (output.mode.width_px, output.mode.height_px);
        let adopted = match console.adopt(width, height) {
            Ok(adopted) => adopted,
            Err(reason) => {
                self.log.record(Record::ConsoleUnhosted { reason });
                return;
            }
        };
        // The device reads a backing's rows a resource's width apart.
        let len = u32::try_from(u64::from(adopted.stride_bytes) * u64::from(height))
            .ok()
            .filter(|_| adopted.stride_bytes == width * 4);
        let Some(len) = len else {
            // The device was never told of it, so it goes straight back.
            let _ = console.relinquish();
            self.log.record(Record::ConsoleUnhosted {
                reason: DriverError::LengthOutOfRange,
            });
            return;
        };
        let resource = self.fresh_resource();
        if let Err(reason) = self.run(&[
            wire::resource_create_2d(resource, Format::B8G8R8X8, width, height),
            wire::attach_backing(resource, adopted.device_addr, len),
        ]) {
            self.hosting = Hosting::Doubtful(resource);
            // Settled now if the device answers, else before it is next hosted.
            let _ = self.unhost_console();
            self.log.record(Record::ConsoleUnhosted { reason });
            return;
        }
        self.hosting = Hosting::Hosted(Hosted {
            resource,
            mode: DisplayMode {
                stride_bytes: adopted.stride_bytes,
                ..output.mode
            },
        });
    }

    /// Take the console off the device and give its memory back, once the
    /// device has let it go.
    fn unhost_console(&mut self) -> Result<(), DriverError> {
        let resource = match self.hosting {
            Hosting::Unheld => return Ok(()),
            Hosting::Hosted(hosted) => {
                if self.shown == Shown::Console {
                    self.show(Shown::Nothing)?;
                }
                hosted.resource
            }
            Hosting::Doubtful(resource) => resource,
        };
        // An answer of any kind means the device has let the backing go.
        self.submit(&[
            wire::detach_backing(resource),
            wire::resource_unref(resource),
        ])?;
        self.hosting = Hosting::Unheld;
        self.console
            .as_mut()
            .map_or(Ok(()), ConsoleSource::relinquish)
    }

    /// Have the scanout show `what`: the console brought up to date first, a
    /// frame as its resource last held it, and a frame given back and no
    /// longer shown unreferenced.
    fn show(&mut self, what: Shown) -> Result<(), DriverError> {
        let mut batch: ArrayVec<Request, BATCH_MAX> = ArrayVec::new();
        if let Some(output) = self.output.filter(|_| self.power == DisplayPower::On) {
            self.scan_out(&mut batch, output, what)?;
        }
        let retire = self.retire_shown(&mut batch, what)?;
        if !batch.is_empty() {
            self.submit(&batch)?.verdict(retire)?;
        }
        self.shown = what;
        Ok(())
    }

    /// Add to `batch` what has `output`'s scanout show `what`, each at its
    /// own size: a frame of an output that has since moved stays shown until
    /// its presenter presents one of the new size.
    fn scan_out(
        &self,
        batch: &mut ArrayVec<Request, BATCH_MAX>,
        output: Output,
        what: Shown,
    ) -> Result<(), DriverError> {
        match what {
            Shown::Nothing => push(batch, wire::set_scanout(output.scanout, 0, &whole(0, 0))),
            Shown::Console => {
                let hosted = self.hosting.hosted().ok_or(DriverError::NotFound)?;
                let area = whole(hosted.mode.width_px, hosted.mode.height_px);
                push(batch, wire::transfer_to_host_2d(hosted.resource, &area, 0))?;
                push(
                    batch,
                    wire::set_scanout(output.scanout, hosted.resource, &area),
                )?;
                push(batch, wire::resource_flush(hosted.resource, &area))
            }
            Shown::Frame {
                resource,
                size: (width, height),
                ..
            } => {
                let area = whole(width, height);
                push(batch, wire::set_scanout(output.scanout, resource, &area))?;
                push(batch, wire::resource_flush(resource, &area))
            }
        }
    }

    /// Add to `batch` the unreference of a given-back frame `next` replaces,
    /// answering where in the batch it lies.
    fn retire_shown(
        &self,
        batch: &mut ArrayVec<Request, BATCH_MAX>,
        next: Shown,
    ) -> Result<Option<usize>, DriverError> {
        if let Shown::Frame {
            resource,
            buffer: None,
            ..
        } = self.shown
        {
            if next != self.shown {
                push(batch, wire::resource_unref(resource))?;
                return Ok(Some(batch.len() - 1));
            }
        }
        Ok(None)
    }

    /// Read the output again after the device said its displays changed,
    /// and follow it.
    fn reread_output(&mut self) -> Result<(), DriverError> {
        let before = self.output;
        match self.read_output() {
            Ok(output) => {
                self.output = output;
                // A rate alone is the display service's to read; the device
                // is told nothing new.
                let shown = |output: Option<Output>| output.map(|o| (o.scanout, o.mode));
                if shown(output) == shown(before) {
                    return Ok(());
                }
                self.follow(before)
            }
            Err(err) => {
                self.output = None;
                Err(err)
            }
        }
    }

    /// Take the scanout the output left dark, and bring the console to the
    /// output's new mode.
    fn follow(&mut self, before: Option<Output>) -> Result<(), DriverError> {
        let lit = self.power == DisplayPower::On;
        let left =
            before.filter(|old| lit && self.output.is_none_or(|new| new.scanout != old.scanout));
        if let Some(old) = left {
            self.run(&[wire::set_scanout(old.scanout, 0, &whole(0, 0))])?;
        }
        let Some(output) = self.output else {
            return Ok(());
        };
        let moved = self.hosting.hosted().is_some_and(|hosted| {
            (hosted.mode.width_px, hosted.mode.height_px)
                != (output.mode.width_px, output.mode.height_px)
        });
        if moved {
            self.unhost_console()?;
        }
        self.host_console();
        let hosted = self.hosting.hosted().is_some();
        let next = match self.shown {
            Shown::Nothing if self.text && hosted => Shown::Console,
            Shown::Console if !hosted => Shown::Nothing,
            shown => shown,
        };
        self.show(next)?;
        self.note_console_shown();
        Ok(())
    }

    /// Show the console's newly painted rows, if it is shown.
    fn show_console_rows(&mut self) {
        // Only a console this driver holds owes it rows. One held at all is
        // drained whether or not it can be shown: its rows wake the driver
        // for as long as they are owed.
        if self.hosting == Hosting::Unheld {
            return;
        }
        let Some(console) = self.console.as_mut() else {
            return;
        };
        let rows = match console.damage() {
            Ok(Some(rows)) => rows,
            Ok(None) => return,
            Err(reason) => {
                self.log.record(Record::ConsoleUnshown { reason });
                return;
            }
        };
        // Rows painted while the console is hidden are brought up when it is
        // shown again, whole.
        let (Some(hosted), Shown::Console, DisplayPower::On) =
            (self.hosting.hosted(), self.shown, self.power)
        else {
            return;
        };
        if let Err(reason) = DamageRect::validate_list(core::slice::from_ref(&rows), &hosted.mode) {
            self.log.record(Record::ConsoleUnshown { reason });
            return;
        }
        let offset = offset_of(&rows, hosted.mode.width_px);
        if let Err(reason) = self.run(&[
            wire::transfer_to_host_2d(hosted.resource, &rows, offset),
            wire::resource_flush(hosted.resource, &rows),
        ]) {
            self.log.record(Record::ConsoleUnshown { reason });
        }
    }

    fn buffer_at(&self, id: BufferId) -> Result<usize, DriverError> {
        self.buffers
            .iter()
            .position(|buffer| buffer.id == id && !buffer.given_back)
            .ok_or(DriverError::NotFound)
    }

    /// Let go of every buffer given back whose backing the device has let go
    /// of; one that does not answer stays held, since the device may read it.
    fn release_given_back(&mut self) {
        let mut at = 0;
        while let Some((given_back, backing)) = self
            .buffers
            .get(at)
            .map(|buffer| (buffer.given_back, buffer.backing))
        {
            if given_back && self.release_backing(backing) {
                if let Some(buffer) = self.buffers.swap_remove(at) {
                    self.held_bytes = self.held_bytes.saturating_sub(buffer.len());
                }
            } else {
                at += 1;
            }
        }
    }

    /// Have the device let go of the resource `backing` names, answering
    /// whether it has: an answer of any kind means it no longer reads the
    /// memory. A shown frame keeps its resource, detached.
    fn release_backing(&mut self, backing: Backing) -> bool {
        let Some(resource) = backing.resource() else {
            return true;
        };
        let shown = matches!(self.shown, Shown::Frame { resource: on, .. } if on == resource);
        let mut batch: ArrayVec<Request, 2> = ArrayVec::new();
        let _ = batch.try_push(wire::detach_backing(resource));
        if !shown {
            let _ = batch.try_push(wire::resource_unref(resource));
        }
        self.submit(&batch).is_ok()
    }

    /// Send `request` on the cursor queue.
    fn cursor_command(&mut self, request: &Request) -> Result<(), DriverError> {
        self.cursor_queue.settle(&mut self.transport, self.host)?;
        let bytes = request.as_bytes();
        self.commands.as_bytes_mut()[CURSOR_AT..CURSOR_AT + bytes.len()].copy_from_slice(bytes);
        let chain = [self.segment(CURSOR_AT, bytes.len(), Direction::DeviceRead)?];
        self.cursor_queue
            .submit_and_wait(&mut self.transport, self.host, &chain, COMMAND_TIMEOUT_NS)
            .map(|_| ())
    }

    /// The pointer's position as the device takes it: the hot spot's, its
    /// coordinates two's-complement words, as Linux sends an off-screen one.
    const fn position(&self) -> (u32, u32) {
        (
            self.pointer.at.0.cast_unsigned(),
            self.pointer.at.1.cast_unsigned(),
        )
    }

    /// The scanout the pointer is drawn on.
    fn pointer_scanout(&self) -> u32 {
        self.output.map_or(0, |output| output.scanout)
    }

    /// The resource the pointer image backs, made the first time it is
    /// needed.
    fn pointer_resource(&mut self) -> Result<u32, DriverError> {
        self.pointer.resource = self.settled(self.pointer.resource)?;
        if let Backing::Made(resource) = self.pointer.resource {
            return Ok(resource);
        }
        let resource = self.fresh_resource();
        let image = self.pointer.image.device_addr();
        if let Err(reason) = self.run(&[
            wire::resource_create_2d(resource, Format::B8G8R8A8, CURSOR_SIDE, CURSOR_SIDE),
            wire::attach_backing(resource, image, CURSOR_BYTES),
        ]) {
            let doubt = Backing::Doubtful(resource);
            self.pointer.resource = self.settled(doubt).unwrap_or(doubt);
            return Err(reason);
        }
        self.pointer.resource = Backing::Made(resource);
        Ok(resource)
    }
}

/// Add `request` to `batch`.
fn push(batch: &mut ArrayVec<Request, BATCH_MAX>, request: Request) -> Result<(), DriverError> {
    batch
        .try_push(request)
        .map_err(|_| DriverError::LengthOutOfRange)
}

impl<T, M, C, L> Display for VirtioGpu<'_, T, M, C, L>
where
    T: Transport,
    M: BufferMemory,
    M::Region: DeviceBuffer,
    C: ConsoleSource,
    L: Recorder<Record>,
{
    fn output(&self) -> Option<DisplayMode> {
        self.output.map(|output| output.mode)
    }

    fn refresh(&self) -> Option<RefreshRate> {
        self.output.and_then(|output| output.refresh)
    }

    fn service_events(&mut self) -> Result<Serviced, DriverError> {
        self.transport.ack_interrupt();
        // A batch abandoned to a slow device is taken back once it answers.
        let _ = self.control.settle(&mut self.transport, self.host);
        let _ = self.cursor_queue.settle(&mut self.transport, self.host);
        // A change raised while the output is read is read again, its
        // interrupt taken by the read's own wait; a read that failed is owed
        // to the next service.
        let mut moved = Ok(());
        for _ in 0..REREADS_PER_SERVICE {
            self.reread_owed |= self.take_display_events();
            if !self.reread_owed {
                break;
            }
            moved = self.reread_output();
            if moved.is_err() {
                break;
            }
            self.reread_owed = false;
        }
        self.release_given_back();
        self.show_console_rows();
        moved.map(|()| Serviced::default())
    }

    fn requests_take_wakes(&self) -> bool {
        // Every command waits on the one interrupt a display event raises too.
        true
    }

    fn frame_count(&self) -> u32 {
        // The device copies each present out of the frame before the call
        // returns.
        1
    }

    fn alloc_buffer(
        &mut self,
        width_px: u32,
        height_px: u32,
    ) -> Result<AllocatedBuffer, DriverError> {
        let mode = self.output.ok_or(DriverError::Stale)?.mode;
        if width_px == 0 || height_px == 0 || width_px > mode.width_px || height_px > mode.height_px
        {
            return Err(DriverError::LengthOutOfRange);
        }
        let stride_bytes = width_px * 4;
        let len = stride_bytes as usize * height_px as usize;
        let held = self
            .held_bytes
            .checked_add(len)
            .ok_or(DriverError::NoSpace)?;
        if self.buffers.is_full() || held > frames_budget(&mode).unwrap_or(0) {
            return Err(DriverError::NoSpace);
        }
        let taken = &self.buffers;
        let id = self
            .ids
            .mint(|id| taken.iter().any(|buffer| buffer.id == id))
            .ok_or(DriverError::NoSpace)?;
        let memory = self.memory.create(len)?;
        let region = memory.id();
        self.buffers
            .try_push(Buffer {
                id,
                memory,
                width: width_px,
                height: height_px,
                backing: Backing::Unmade,
                given_back: false,
            })
            .map_err(|_| DriverError::NoSpace)?;
        self.held_bytes = held;
        Ok(AllocatedBuffer {
            id,
            region,
            stride_bytes,
        })
    }

    fn free_buffer(&mut self, id: BufferId) -> Result<(), DriverError> {
        let at = self.buffer_at(id)?;
        // A shown frame keeps its resource, and the device its copy, until
        // something else is shown.
        if let Shown::Frame { buffer, .. } = &mut self.shown {
            if *buffer == Some(id) {
                *buffer = None;
            }
        }
        self.buffers[at].given_back = true;
        self.release_given_back();
        Ok(())
    }

    fn present(&mut self, id: BufferId, damage: &[DamageRect]) -> Result<Presented, DriverError> {
        let output = self.output.ok_or(DriverError::Stale)?;
        DamageRect::validate_list(damage, &output.mode)?;
        let at = self.buffer_at(id)?;
        let buffer = &self.buffers[at];
        if (buffer.width, buffer.height) != (output.mode.width_px, output.mode.height_px) {
            return Err(DriverError::Stale);
        }
        let (width, height, held) = (buffer.width, buffer.height, buffer.backing);
        let backing = self.settled(held)?;
        self.buffers[at].backing = backing;
        let full = whole(width, height);
        let (fresh, resource) = match backing {
            Backing::Made(resource) => (false, resource),
            Backing::Unmade | Backing::Doubtful(_) => (true, self.fresh_resource()),
        };
        let buffer = &self.buffers[at];
        let next = Shown::Frame {
            resource,
            size: (width, height),
            buffer: Some(id),
        };
        let switching = self.shown != next;
        let lit = self.power == DisplayPower::On;
        let mut batch: ArrayVec<Request, BATCH_MAX> = ArrayVec::new();
        let whole_frame = [full];
        let rects = if fresh { &whole_frame[..] } else { damage };
        let made_at = fresh.then_some(batch.len());
        if fresh {
            let len = u32::try_from(buffer.len()).map_err(|_| DriverError::LengthOutOfRange)?;
            push(
                &mut batch,
                wire::resource_create_2d(resource, Format::B8G8R8X8, width, height),
            )?;
            push(
                &mut batch,
                wire::attach_backing(resource, buffer.memory.device_addr(), len),
            )?;
        }
        for rect in rects {
            push(
                &mut batch,
                wire::transfer_to_host_2d(resource, rect, offset_of(rect, width)),
            )?;
        }
        let scanned_at = (lit && switching).then_some(batch.len());
        if lit && switching {
            push(
                &mut batch,
                wire::set_scanout(output.scanout, resource, &full),
            )?;
        }
        let retire = if switching {
            self.retire_shown(&mut batch, next)?
        } else {
            None
        };
        if lit {
            for rect in if switching { &whole_frame[..] } else { rects } {
                push(&mut batch, wire::resource_flush(resource, rect))?;
            }
        }
        // Each command is taken at its own answer, so what the device did is
        // what is kept: a resource made is kept, and a frame put on the scanout
        // is the one shown, whatever else the batch refused.
        let (made, scanned, verdict) = match self.submit(&batch) {
            Ok(answers) => (
                made_at.is_none_or(|at| answers.done(at) && answers.done(at + 1)),
                scanned_at.is_none_or(|at| answers.done(at)),
                answers.verdict(retire),
            ),
            Err(unanswered) => (false, false, Err(unanswered)),
        };
        if made {
            self.buffers[at].backing = Backing::Made(resource);
        } else if fresh {
            // The batch may have made the resource and attached the backing.
            let doubt = Backing::Doubtful(resource);
            self.buffers[at].backing = self.settled(doubt).unwrap_or(doubt);
        }
        if switching && scanned && (made || !fresh) {
            self.shown = next;
            self.text = false;
        }
        verdict.map(|()| Presented::Shown)
    }

    fn released(&mut self, surface: ReleaseSurface) {
        if self.pointer.shown {
            self.pointer.shown = false;
            let hide = wire::update_cursor(self.pointer_scanout(), self.position(), 0, (0, 0));
            if let Err(reason) = self.cursor_command(&hide) {
                self.log.record(Record::ShowRefused { reason });
            }
        }
        self.text = surface == ReleaseSurface::Text;
        let next = if self.text && self.hosting.hosted().is_some() {
            Shown::Console
        } else {
            Shown::Nothing
        };
        self.show_or_record(next);
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if power == self.power {
            return Ok(());
        }
        let Some(output) = self.output else {
            self.power = power;
            return Ok(());
        };
        match power {
            DisplayPower::Off => {
                self.run(&[wire::set_scanout(output.scanout, 0, &whole(0, 0))])?;
                self.power = power;
                Ok(())
            }
            DisplayPower::On => {
                self.power = power;
                let shown = self.shown;
                self.show(shown)
                    .inspect_err(|_| self.power = DisplayPower::Off)
            }
        }
    }

    fn hardware_cursor(&mut self) -> Option<&mut dyn HardwareCursor> {
        Some(self)
    }
}

impl<T, M, C, L> HardwareCursor for VirtioGpu<'_, T, M, C, L>
where
    T: Transport,
    M: BufferMemory,
    M::Region: DeviceBuffer,
    C: ConsoleSource,
    L: Recorder<Record>,
{
    fn cursor_caps(&self) -> CursorCaps {
        CursorCaps {
            max_width_px: CURSOR_SIDE,
            max_height_px: CURSOR_SIDE,
        }
    }

    fn set_cursor(&mut self, id: BufferId, hot_x: u32, hot_y: u32) -> Result<(), DriverError> {
        let at = self.buffer_at(id)?;
        let buffer = &self.buffers[at];
        let (width, height) = (buffer.width, buffer.height);
        if width > CURSOR_SIDE || height > CURSOR_SIDE || hot_x >= width || hot_y >= height {
            return Err(DriverError::LengthOutOfRange);
        }
        // A transfer the device has not answered may still be reading the
        // image.
        self.control.settle(&mut self.transport, self.host)?;
        // The image is the driver's own once copied, so the presenter may
        // draw the next into its buffer at once.
        let image = self.pointer.image.as_bytes_mut();
        image.fill(0);
        buffer.memory.with_bytes(|source| {
            copy_rows(
                source,
                width as usize * 4,
                image,
                CURSOR_STRIDE,
                width as usize * 4,
                height as usize,
            )
        })?;
        let resource = self.pointer_resource()?;
        self.run(&[wire::transfer_to_host_2d(
            resource,
            &whole(CURSOR_SIDE, CURSOR_SIDE),
            0,
        )])?;
        let show = wire::update_cursor(
            self.pointer_scanout(),
            self.position(),
            resource,
            (hot_x, hot_y),
        );
        self.cursor_command(&show)?;
        self.pointer.hot = (hot_x, hot_y);
        self.pointer.shown = true;
        Ok(())
    }

    fn move_cursor(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
        self.pointer.at = (x, y);
        if !self.pointer.shown {
            return Ok(());
        }
        let moved = wire::move_cursor(
            self.pointer_scanout(),
            self.position(),
            self.pointer.resource.resource().unwrap_or(0),
            self.pointer.hot,
        );
        self.cursor_command(&moved)
    }

    fn hide_cursor(&mut self) -> Result<(), DriverError> {
        if !self.pointer.shown {
            return Ok(());
        }
        let hide =
            wire::update_cursor(self.pointer_scanout(), self.position(), 0, self.pointer.hot);
        self.cursor_command(&hide)?;
        self.pointer.shown = false;
        Ok(())
    }
}

impl<T: Transport, M: BufferMemory, C, L> Drop for VirtioGpu<'_, T, M, C, L> {
    /// Reset the device before its memory goes: a device that will not
    /// confirm may still read its rings, its commands and every backing,
    /// which are then held for the kernel to reclaim when the driver exits.
    fn drop(&mut self) {
        if self.transport.reset().is_err() {
            self.control.withhold();
            self.cursor_queue.withhold();
            self.commands.withhold();
            self.pointer.image.withhold();
            while let Some(buffer) = self.buffers.pop() {
                core::mem::forget(buffer.memory);
            }
        }
    }
}

#[cfg(test)]
mod tests;
