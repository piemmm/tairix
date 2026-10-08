//! TAIRiX virtio-input device logic (keyboard / pointer).
//!
//! The arch-neutral, transport-agnostic open/poll/decode engine for a
//! virtio-input device, implementing [`tairix_abi::driver::input::Input`]
//! on top of the cross-arch virtio transport from `lib/virtio`. As with
//! `virtio_blk` / `virtio_net`, the logic is bus-agnostic: the same
//! source drives the PCI and MMIO transports (the
//! queue protocol lives once, in the transport crate).
//!
//! It lives in `lib/*` so both the in-kernel `-M virt` input verticals
//! and the user-space input-driver process compose it without a
//! `drivers/*`→`drivers/*` dependency (the
//! virtio analogue of `lib/hid` ↔ `drivers/input/usb_hid`). The thin
//! `drivers/input/virtio_input` crate keeps only the `register` entry
//! and the bind table built from [`VIRTIO_INPUT_DEVICE_ID`].
//!
//! # Wire protocol
//!
//! Virtio 1.1. A virtio-input device exposes two virtqueues —
//! the **eventq** (index 0), on which the device delivers
//! `struct virtio_input_event` records to the driver, and the
//! **statusq** (index 1), on which the driver returns status (LEDs,
//! force-feedback) to the device. This logic consumes the eventq
//! only; the statusq is optional and left unprogrammed (`abi-v1`
//! input reports events, it does not drive device feedback).
//!
//! Each event is the 8-byte little-endian record
//! `{ __le16 type; __le16 code; __le32 value; }` (virtio 1.1 §5.8.6).
//! The `type`/`code` namespaces are the Linux `evdev` ones, so the
//! decode below maps `EV_KEY` to [`InputEventKind::Key`] and `EV_REL`
//! pointer axes to [`InputEventKind::Pointer`], discarding the `EV_SYN`
//! frame markers and any namespace this `abi-v1` surface does not model.
//!
//! A wheel reaches [`InputEventKind::Scroll`] in scroll units on both axes.
//! Which code an axis is read from is the device's own answer, read once at
//! open from its `EV_REL` event bitmap (virtio 1.1 §5.8.4): a device that
//! offers `REL_WHEEL_HI_RES` or `REL_HWHEEL_HI_RES` reports every turn twice,
//! at fine and at detent resolution, so the fine code alone is read and its
//! detent twin dropped; a device that does not, or answers no bitmap, is read
//! at 120 units a detent. QEMU's HID pointers send the `BTN_GEAR_*` presses
//! instead, one detent each.
//!
//! A device that reports slotted contacts (`ABS_MT_*`, the Linux
//! multi-touch protocol B) on two axes it states a range for is a touch
//! surface: its contacts are framed by `SYN_REPORT` into the seat's
//! [`TouchFrame`]s, read through [`VirtioInput::poll_reports`], and its input
//! properties say whether it is a touchscreen, a touchpad or a clickpad.
//!
//! No feature bits are negotiated: virtio-input defines none, so it
//! accepts the empty feature subset.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

pub mod console;
mod touch;

pub use console::VirtioKeyboardConsole;

use tairix_abi::driver::input::{
    Input, InputEvent, InputEventKind, AXIS_X, AXIS_Y, SCROLL_UNITS_PER_DETENT,
};
use tairix_abi::driver::BufferClass;
use tairix_abi::touch::TouchFrame;
use tairix_abi::DriverError;
use tairix_virtio::{
    BounceBuffer, ChainSegment, CompletionSignal, Direction, SplitQueue, Status, Transport,
    VirtioError, VirtioHost, TRANSPORT_FEATURES,
};

/// The virtio device id of an input device (virtio 1.1 §5.8 —
/// `virtio-input` is device type 18). The `drivers/input/virtio_input`
/// bind table's match key is built from it, so a discovered virtio node
/// whose probed device id is 18 binds that driver and nothing else; it
/// lives here as the single source of truth the device logic and the
/// driver crate's `BIND_KEYS` both depend on.
pub const VIRTIO_INPUT_DEVICE_ID: u32 = 18;

/// Virtio-input wire-protocol constants (virtio 1.1 §5.8 + the Linux
/// `evdev` `type`/`code` namespaces the device reports in).
mod wire {
    /// Event virtqueue index (device → driver), virtio 1.1 §5.8.2.
    pub const EVENT_QUEUE: u16 = 0;
    /// Event-queue depth ceiling (descriptors), power-of-two per virtio
    /// §2.6; the programmed depth is the device's advertised
    /// `queue_max_size` clamped to this (QEMU's virtio-input advertises
    /// 64). The whole pool stays posted, and its depth is the loss
    /// bound: the device **silently drops** events when no posted buffer
    /// is free (virtio 1.1 §5.8.6.2), and the driver's drain can lag
    /// whole bursts behind a saturated CPU (a busy desktop re-rendering
    /// while a click arrives), so a shallow pool loses real input — a
    /// click's press/release vanishing mid-burst, observed end to end
    /// before this depth was raised from eight. Sixty-four single-event
    /// buffers (512 bytes of bounce memory) absorb every realistic input
    /// burst between two driver wakes.
    pub const EVENT_QUEUE_SIZE: u16 = 64;
    /// Byte length of one `struct virtio_input_event`
    /// (`__le16 type`, `__le16 code`, `__le32 value`), virtio 1.1 §5.8.6.
    /// A `u32` so it feeds a descriptor `len` directly; widen to `usize`
    /// (lint-free) for slice/allocation sizes.
    pub const EVENT_LEN: u32 = 8;

    /// `EV_SYN` — event-frame separator (Linux `evdev`). Carries no
    /// surfaced event.
    pub const EV_SYN: u16 = 0x00;
    /// `EV_KEY` — key / button press or release.
    pub const EV_KEY: u16 = 0x01;
    /// `EV_REL` — relative pointer / wheel motion.
    pub const EV_REL: u16 = 0x02;

    /// `REL_X` — relative motion along the X axis.
    pub const REL_X: u16 = 0x00;
    /// `REL_Y` — relative motion along the Y axis.
    pub const REL_Y: u16 = 0x01;
    /// `REL_HWHEEL` — horizontal scroll-wheel motion in detents, counted
    /// toward the right.
    pub const REL_HWHEEL: u16 = 0x06;
    /// `REL_WHEEL` — vertical scroll-wheel motion in detents, counted away
    /// from the user.
    pub const REL_WHEEL: u16 = 0x08;
    /// `REL_WHEEL_HI_RES` — vertical wheel motion in 1/120 of a detent,
    /// counted away from the user.
    pub const REL_WHEEL_HI_RES: u16 = 0x0B;
    /// `REL_HWHEEL_HI_RES` — horizontal wheel motion in 1/120 of a detent,
    /// counted toward the right.
    pub const REL_HWHEEL_HI_RES: u16 = 0x0C;

    /// `BTN_GEAR_DOWN` — one wheel detent toward the user, reported as a
    /// press and a release rather than as `REL_WHEEL`: QEMU's HID pointer
    /// devices encode the wheel this way.
    pub const BTN_GEAR_DOWN: u16 = 0x150;
    /// `BTN_GEAR_UP` — one wheel detent away from the user (see
    /// [`BTN_GEAR_DOWN`]).
    pub const BTN_GEAR_UP: u16 = 0x151;

    /// `select` byte of the device-configuration query (virtio 1.1 §5.8.4).
    pub const CFG_SELECT: usize = 0;
    /// `size` byte: how many bytes of the answer are valid, zero for none.
    pub const CFG_SIZE: usize = 2;
    /// Start of the answer union.
    pub const CFG_ANSWER: usize = 8;
    /// Longest answer the union holds.
    pub const CFG_ANSWER_LEN: usize = 128;
    /// `VIRTIO_INPUT_CFG_EV_BITS`: the codes the device reports for the event
    /// type `subsel` names, one bit each.
    pub const CFG_EV_BITS: u8 = 0x11;
    /// `VIRTIO_INPUT_CFG_PROP_BITS`: the device's input properties, one bit
    /// each.
    pub const CFG_PROP_BITS: u8 = 0x10;
    /// `VIRTIO_INPUT_CFG_ABS_INFO`: the range of the absolute axis `subsel`
    /// names.
    pub const CFG_ABS_INFO: u8 = 0x12;
    /// Byte length of `struct virtio_input_absinfo`: `min`, `max`, `fuzz`,
    /// `flat`, `res`, each a 32-bit little-endian word.
    pub const ABS_INFO_LEN: usize = 20;

    /// `EV_ABS` — an absolute axis.
    pub const EV_ABS: u16 = 0x03;
    /// `SYN_REPORT` — the end of one frame of events.
    pub const SYN_REPORT: u16 = 0x00;
    /// `SYN_DROPPED` — the device lost events; the state is unknown until
    /// the frame ends.
    pub const SYN_DROPPED: u16 = 0x03;
    /// `ABS_MT_SLOT` — which slot the contact events after it address.
    pub const ABS_MT_SLOT: u16 = 0x2F;
    /// `ABS_MT_POSITION_X` — a slotted contact's position across.
    pub const ABS_MT_POSITION_X: u16 = 0x35;
    /// `ABS_MT_POSITION_Y` — a slotted contact's position down.
    pub const ABS_MT_POSITION_Y: u16 = 0x36;
    /// `ABS_MT_TOOL_TYPE` — what a slotted contact is.
    pub const ABS_MT_TOOL_TYPE: u16 = 0x37;
    /// `ABS_MT_TRACKING_ID` — the contact a slot holds; negative when it
    /// lifts.
    pub const ABS_MT_TRACKING_ID: u16 = 0x39;
    /// `MT_TOOL_PALM` — a contact the device judges a palm.
    pub const MT_TOOL_PALM: i32 = 2;
    /// `BTN_LEFT` — a touchpad's primary button, a clickpad's surface.
    pub const BTN_LEFT: u16 = 0x110;
    /// `BTN_RIGHT` — a touchpad's secondary button.
    pub const BTN_RIGHT: u16 = 0x111;
    /// `BTN_MIDDLE` — a touchpad's middle button.
    pub const BTN_MIDDLE: u16 = 0x112;
    /// `INPUT_PROP_POINTER` — the device moves a pointer: a touchpad.
    pub const INPUT_PROP_POINTER: u8 = 0;
    /// `INPUT_PROP_DIRECT` — the device's contacts name places on a display.
    pub const INPUT_PROP_DIRECT: u8 = 1;
    /// `INPUT_PROP_BUTTONPAD` — the whole surface is the button.
    pub const INPUT_PROP_BUTTONPAD: u8 = 2;
}

/// What a drain surfaces: a key, pointer or wheel event, or a touch frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Report {
    /// A key, pointer or wheel event.
    Event(InputEvent),
    /// A frame of a touch surface's contacts.
    Touch(TouchFrame),
}

/// Which of an axis's two wheel codes the device's turns are read from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WheelCode {
    /// The detent code, at [`SCROLL_UNITS_PER_DETENT`] a detent.
    Detent,
    /// The `_HI_RES` code, already in scroll units; the detent twin the
    /// device also sends is dropped.
    HiRes,
}

/// How a device's wheel axes are read, decided once from its event bitmap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Wheels {
    vertical: WheelCode,
    horizontal: WheelCode,
}

impl Wheels {
    /// Every axis read at detent resolution: the reading for a device that
    /// states no event bitmap.
    const DETENT: Self = Self {
        vertical: WheelCode::Detent,
        horizontal: WheelCode::Detent,
    };

    /// The wheel codes the device's `EV_REL` bitmap offers.
    fn of(relative: &[u8]) -> Self {
        let code = |hi_res: u16| {
            if reports(relative, hi_res) {
                WheelCode::HiRes
            } else {
                WheelCode::Detent
            }
        };
        Self {
            vertical: code(wire::REL_WHEEL_HI_RES),
            horizontal: code(wire::REL_HWHEEL_HI_RES),
        }
    }

    /// Ask the device which relative codes it reports.
    fn reported_by<T: Transport>(transport: &mut T) -> Self {
        let mut bitmap = [0u8; wire::CFG_ANSWER_LEN];
        let answered = event_bits(transport, wire::EV_REL, &mut bitmap);
        if answered == 0 {
            Self::DETENT
        } else {
            Self::of(&bitmap[..answered])
        }
    }

    /// Decode one raw `virtio_input_event` triple into the platform-neutral
    /// [`InputEvent`].
    ///
    /// Returns `None` for frame markers (`EV_SYN`), for the wheel code an
    /// axis is not read from, and for any `type`/`code` this `abi-v1` input
    /// surface does not model, so a consumed-but-unmapped event is "no
    /// event" rather than a fabricated one.
    fn decode(self, etype: u16, code: u16, value: i32) -> Option<InputEvent> {
        // `EV_SYN` is kept as its own arm: a frame separator is a distinct,
        // expected protocol case (virtio 1.1 §5.8.6) that we deliberately
        // drop, documented apart from the catch-all for unknown types even
        // though both yield `None`.
        #[allow(clippy::match_same_arms)]
        match etype {
            // A gear press is a detent; its release carries nothing further.
            wire::EV_KEY if code == wire::BTN_GEAR_DOWN => {
                (value == 1).then(|| scroll(AXIS_Y, SCROLL_UNITS_PER_DETENT))
            }
            wire::EV_KEY if code == wire::BTN_GEAR_UP => {
                (value == 1).then(|| scroll(AXIS_Y, -SCROLL_UNITS_PER_DETENT))
            }
            wire::EV_KEY => Some(InputEvent {
                kind: InputEventKind::Key,
                reserved0: 0,
                code,
                value,
            }),
            wire::EV_REL => self.relative(code, value),
            // Frame separator: end of an event group, no surfaced event.
            wire::EV_SYN => None,
            // Every other evdev namespace carries no surfaced event here.
            _ => None,
        }
    }

    /// Decode one `EV_REL` code. evdev counts the vertical wheel away from
    /// the user, so it is negated onto the shared axis, which counts toward
    /// the end; the horizontal wheel already counts toward it.
    fn relative(self, code: u16, value: i32) -> Option<InputEvent> {
        let detents = |units: i32| value.saturating_mul(units);
        match (code, self.vertical, self.horizontal) {
            (wire::REL_X, ..) => Some(pointer(AXIS_X, value)),
            (wire::REL_Y, ..) => Some(pointer(AXIS_Y, value)),
            (wire::REL_WHEEL, WheelCode::Detent, _) => Some(scroll(
                AXIS_Y,
                detents(SCROLL_UNITS_PER_DETENT).saturating_neg(),
            )),
            (wire::REL_WHEEL_HI_RES, WheelCode::HiRes, _) => {
                Some(scroll(AXIS_Y, value.saturating_neg()))
            }
            (wire::REL_HWHEEL, _, WheelCode::Detent) => {
                Some(scroll(AXIS_X, detents(SCROLL_UNITS_PER_DETENT)))
            }
            (wire::REL_HWHEEL_HI_RES, _, WheelCode::HiRes) => Some(scroll(AXIS_X, value)),
            _ => None,
        }
    }
}

/// Whether the event bitmap `bits` sets the bit for `code`.
fn reports(bits: &[u8], code: u16) -> bool {
    bits.get(usize::from(code / 8))
        .is_some_and(|byte| byte >> (code % 8) & 1 == 1)
}

/// Ask the device for its bitmap of `event_type` codes, writing what it
/// answers into `bitmap` and returning how many bytes are valid (zero when it
/// states none).
fn event_bits<T: Transport>(
    transport: &mut T,
    event_type: u16,
    bitmap: &mut [u8; wire::CFG_ANSWER_LEN],
) -> usize {
    let Ok(subsel) = u8::try_from(event_type) else {
        return 0;
    };
    transport.write_config(wire::CFG_SELECT, &[wire::CFG_EV_BITS, subsel]);
    let mut size = [0u8];
    transport.read_config(wire::CFG_SIZE, &mut size);
    let answered = usize::from(size[0]).min(wire::CFG_ANSWER_LEN);
    transport.read_config(wire::CFG_ANSWER, &mut bitmap[..answered]);
    answered
}

/// A relative pointer displacement on `axis`.
const fn pointer(axis: u16, value: i32) -> InputEvent {
    InputEvent {
        kind: InputEventKind::Pointer,
        reserved0: 0,
        code: axis,
        value,
    }
}

/// A wheel turn of `units` scroll units on `axis`.
const fn scroll(axis: u16, units: i32) -> InputEvent {
    InputEvent {
        kind: InputEventKind::Scroll,
        reserved0: 0,
        code: axis,
        value: units,
    }
}

/// Input device backed by a cross-arch virtio transport.
///
/// `'h` bounds the borrow of the [`VirtioHost`] the driver allocates
/// its DMA regions through; the host is minted per driver load and
/// lives only for the duration of that load, so the driver borrows it
/// for `'h` rather than demanding a `'static` host (per-process pools are reclaimed when the driver unloads). This
/// mirrors [`VirtioNet`](../tairix_drv_network_virtio_net/struct.VirtioNet.html).
pub struct VirtioInput<'h, T: Transport> {
    transport: T,
    eventq: SplitQueue,
    host: &'h dyn VirtioHost,
    /// One shared device-writable region holding every event slot back
    /// to back (`negotiated depth × EVENT_LEN` bytes — a fraction of a
    /// page, never a page per 8-byte event). The device fills one slot
    /// per `virtio_input_event` it delivers; the driver keeps every
    /// slot posted so several events (e.g. an `EV_KEY` plus its
    /// `EV_SYN` frame separator) can be in flight at once — a single
    /// posted buffer is not enough, because the device needs a free
    /// buffer for *every* event of a report, including the `EV_SYN`
    /// (virtio 1.1 §5.8.6).
    event_pool: BounceBuffer,
    /// Descriptor head → pool slot index for every in-flight slot. The
    /// queue assigns heads from its own free list, so the map is
    /// re-recorded on every repost.
    event_slots: [Option<u16>; wire::EVENT_QUEUE_SIZE as usize],
    /// Which code each wheel axis is read from.
    wheels: Wheels,
    /// The contact decoder, for a device that is a touch surface.
    touch: Option<touch::MultiTouch>,
}

impl<'h, T: Transport> VirtioInput<'h, T> {
    /// Bring the device online and post the event-buffer pool.
    ///
    /// Implements the virtio-1.1 §3.1 initialisation sequence: reset,
    /// ACKNOWLEDGE, DRIVER, feature negotiation (the transport features
    /// only, no device-specific ones), `FEATURES_OK`, the wheel-code query,
    /// set up the event queue, `DRIVER_OK`,
    /// then fill the eventq with one device-write slot per negotiated
    /// descriptor (all carved from one shared DMA region) and notify
    /// the device. Once the reset confirms, the device is declared
    /// quiesced to `host`, so memory an earlier instance left with it can
    /// be released; a failure once the device is live resets it again
    /// before its memory is released.
    ///
    /// # Errors
    ///
    /// Propagates the transport / queue-setup [`VirtioError`] (mapped to
    /// [`DriverError`]), [`DriverError::DeviceFault`] if the device
    /// never confirms its reset or clears [`Status::FEATURES_OK`] after
    /// negotiation, and any [`DriverError`] from the DMA-buffer allocation.
    pub fn open(mut transport: T, host: &'h dyn VirtioHost) -> Result<Self, DriverError> {
        // QEMU's non-transitional virtio-input makes the posted eventq
        // buffers visible only once the modern interface is acked. No
        // device-specific feature is negotiated.
        let negotiated = tairix_virtio::negotiate::<_, DriverError>(
            &mut transport,
            || host.device_quiesced(),
            |offered| Ok(offered & TRANSPORT_FEATURES),
        )?;
        let mut status = negotiated.status;
        let wheels = Wheels::reported_by(&mut transport);
        let touch = touch::MultiTouch::reported_by(&mut transport);
        // Program the deepest event queue the device supports, up to the
        // pool ceiling: depth is the input-loss bound (the device drops
        // events with no posted buffer), so take everything offered. A
        // device advertising a zero-sized queue is broken; refuse it
        // rather than run a driver that can never receive an event.
        let eventq = SplitQueue::new(
            &mut transport,
            host,
            wire::EVENT_QUEUE,
            wire::EVENT_QUEUE_SIZE,
            1,
        )
        .map_err(VirtioError::as_driver_error)?;
        let queue_size = eventq.size();
        // One region carries every slot: the depth is bounded by the
        // 64-entry ceiling, so the whole pool is 512 bytes — never a DMA
        // page per 8-byte event.
        let region = host.alloc_dma_zeroed(usize::from(queue_size) * wire::EVENT_LEN as usize)?;
        let event_pool = BounceBuffer::new(region, BufferClass::NonSensitive);
        status = status.with(Status::DRIVER_OK);
        transport.set_status(status);

        let mut input = Self {
            transport,
            eventq,
            host,
            event_pool,
            event_slots: core::array::from_fn(|_| None),
            wheels,
            touch,
        };
        input.post_pool(queue_size)?;
        Ok(input)
    }

    /// Post every event slot and notify the device.
    fn post_pool(&mut self, queue_size: u16) -> Result<(), DriverError> {
        for slot in 0..queue_size {
            Self::post_slot(
                &mut self.eventq,
                &mut self.event_pool,
                slot,
                &mut self.event_slots,
            )?;
        }
        self.eventq.kick(&mut self.transport);
        Ok(())
    }

    /// Bring the device online ([`Self::open`]) and only then run the
    /// caller's `arm` step — the driver's externally observable
    /// readiness action, e.g. binding the granted device interrupt
    /// (the audited `irq_bind` syscall a test harness or supervisor
    /// watches for).
    ///
    /// The ordering is the point of this constructor. A virtio-input
    /// device silently discards events while its eventq has no posted
    /// buffers, so an `arm` step performed *before* [`Self::open`]
    /// advertises readiness while a keystroke can still be dropped —
    /// the lost-first-keypress race observed on the autoload input
    /// vertical. Running `arm` strictly after the eventq is live
    /// (`DRIVER_OK` set, every buffer posted, the device kicked) makes
    /// the arm step a truthful readiness witness; an event that
    /// arrives between the kick and the `arm` return sits in the used
    /// ring and is collected by [`Input::poll`]'s pre-wait drain, so
    /// nothing is lost in that window either.
    ///
    /// `arm` must not wait for input (it runs before the event pump
    /// exists); it performs its one readiness action and returns.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::open`]'s errors unchanged. If `arm` fails,
    /// the device is torn down — reset before its memory goes, so a live
    /// device is never left DMA-writing into a driver that is about to
    /// exit — and the `arm` error is returned.
    pub fn open_armed<F>(
        transport: T,
        host: &'h dyn VirtioHost,
        arm: F,
    ) -> Result<Self, DriverError>
    where
        F: FnOnce(&mut Self) -> Result<(), DriverError>,
    {
        let mut input = Self::open(transport, host)?;
        arm(&mut input)?;
        Ok(input)
    }

    /// Zero event slot `slot`, post it to the eventq, and record it in
    /// `event_slots` under the descriptor head the queue assigned. The caller
    /// is responsible for the single `kick` once a batch has been posted.
    ///
    /// A zeroed slot decodes as a frame separator, so a completion that wrote
    /// nothing surfaces no event rather than the slot's last one again.
    fn post_slot(
        eventq: &mut SplitQueue,
        event_pool: &mut BounceBuffer,
        slot: u16,
        event_slots: &mut [Option<u16>],
    ) -> Result<(), DriverError> {
        let start = usize::from(slot) * wire::EVENT_LEN as usize;
        event_pool
            .full_region_mut()
            .get_mut(start..start + wire::EVENT_LEN as usize)
            .ok_or(DriverError::DeviceFault)?
            .fill(0);
        let segments = [ChainSegment {
            device_addr: event_pool
                .device_addr_at(start, wire::EVENT_LEN as usize)
                .ok_or(DriverError::DeviceFault)?,
            len: wire::EVENT_LEN,
            direction: Direction::DeviceWrite,
        }];
        let head = eventq
            .add_chain(&segments)
            .map_err(VirtioError::as_driver_error)?;
        // `head` is queue-assigned (the driver's own free list), so it is
        // always in range; guard anyway and fail closed.
        *event_slots
            .get_mut(head as usize)
            .ok_or(DriverError::DeviceFault)? = Some(slot);
        Ok(())
    }
}

impl<T: Transport> Drop for VirtioInput<'_, T> {
    /// Reset the device before its memory goes: a device that will not confirm
    /// may still master its ring and event pool, which are then held for the
    /// kernel to quarantine when the driver exits.
    fn drop(&mut self) {
        if self.transport.reset().is_err() {
            self.eventq.withhold();
            self.event_pool.withhold();
        }
    }
}

/// Where a drain puts what it decodes.
trait Sink {
    /// Whether there is room for no more.
    fn full(&self) -> bool;
    /// Keep `report`.
    fn put(&mut self, report: Report);
    /// How many are kept.
    fn kept(&self) -> usize;
}

/// The key, pointer and wheel events [`Input::poll`] answers; a touch frame
/// has no form there, and a touch device is read through
/// [`VirtioInput::poll_reports`].
struct Events<'a> {
    out: &'a mut [InputEvent],
    len: usize,
}

impl Sink for Events<'_> {
    fn full(&self) -> bool {
        self.len == self.out.len()
    }

    fn put(&mut self, report: Report) {
        if let Report::Event(event) = report {
            self.out[self.len] = event;
            self.len += 1;
        }
    }

    fn kept(&self) -> usize {
        self.len
    }
}

/// Everything a drain surfaces.
struct Reports<'a> {
    out: &'a mut [Report],
    len: usize,
}

impl Sink for Reports<'_> {
    fn full(&self) -> bool {
        self.len == self.out.len()
    }

    fn put(&mut self, report: Report) {
        self.out[self.len] = report;
        self.len += 1;
    }

    fn kept(&self) -> usize {
        self.len
    }
}

impl<T: Transport> Input for VirtioInput<'_, T> {
    fn poll(&mut self, events: &mut [InputEvent]) -> Result<usize, DriverError> {
        if events.is_empty() {
            return Err(DriverError::BufferTooSmall);
        }
        self.pump(&mut Events {
            out: events,
            len: 0,
        })
    }
}

impl<T: Transport> VirtioInput<'_, T> {
    /// Drain the device into `reports`: its key, pointer and wheel events, and
    /// a touch surface's frames. Parks on the device interrupt while nothing
    /// is pending, as [`Input::poll`] does.
    ///
    /// # Errors
    ///
    /// [`DriverError::BufferTooSmall`] for an empty `reports`, and as
    /// [`Input::poll`] otherwise.
    pub fn poll_reports(&mut self, reports: &mut [Report]) -> Result<usize, DriverError> {
        if reports.is_empty() {
            return Err(DriverError::BufferTooSmall);
        }
        self.pump(&mut Reports {
            out: reports,
            len: 0,
        })
    }

    /// A frame with every contact lifted, for a touch surface: what its
    /// driver tells the seat when the device stops reporting, so nothing it
    /// held stays held.
    #[must_use]
    pub fn touch_lifted(&self) -> Option<TouchFrame> {
        self.touch.as_ref().map(touch::MultiTouch::lifted)
    }

    /// Drain whatever the device has completed into `sink`; only when nothing
    /// is pending, park the CPU on the device interrupt (never a busy-spin)
    /// and drain once more. Nothing outstanding bounds the wait — the next
    /// event may legitimately never come — so it parks indefinitely rather
    /// than manufacture a deadline.
    fn pump(&mut self, sink: &mut impl Sink) -> Result<usize, DriverError> {
        let mut count = self.drain_ready(sink);
        if matches!(count, Ok(0)) {
            let signal = self.host.notify_wait(self.eventq.index(), u64::MAX);
            count = self.drain_ready(sink);
            // A wait with no deadline that still timed out could not be made
            // at all — the binding was revoked or refused — so no interrupt
            // will end the next one either, and polling again would spin.
            if signal == CompletionSignal::TimedOut && matches!(count, Ok(0)) {
                count = Err(DriverError::DeviceOffline);
            }
        }
        // Acknowledge the device's interrupt now its completions have been
        // observed, faulted drain or not, so it de-asserts its line before
        // the next wait re-arms the kernel IRQ; otherwise every later wait
        // returns at once and the park is a busy loop through the kernel. A
        // no-op on transports with no device-side ack (MSI-X PCI, the mock).
        self.transport.ack_interrupt();
        count
    }

    /// Drain every completed event the device has posted, up to what `sink`
    /// has room for, decoding each and handing its buffer straight back so the
    /// pool stays full; answers how many reports it kept.
    ///
    /// Frame separators, unmodelled events and short completions consume and
    /// replenish a buffer without a report (fail closed — never decode stale
    /// bytes), so a keypress's `EV_KEY`+`EV_SYN` pair surfaces one event and a
    /// touch frame's many events one frame. At most a ring's worth of
    /// completions per call, decoding or not: a device completing buffers as
    /// fast as they are reposted would otherwise hold the drain for ever.
    fn drain_ready(&mut self, sink: &mut impl Sink) -> Result<usize, DriverError> {
        let before = sink.kept();
        let mut reposted = false;
        for _ in 0..self.eventq.size() {
            if sink.full() {
                break;
            }
            let token = match self.eventq.poll_used() {
                Ok(t) => t,
                Err(VirtioError::NoCompletion) => break,
                Err(e) => return Err(e.as_driver_error()),
            };
            let slot = self
                .event_slots
                .get_mut(token.head as usize)
                .and_then(Option::take)
                .ok_or(DriverError::DeviceFault)?;
            if token.written >= wire::EVENT_LEN {
                let offset = usize::from(slot) * wire::EVENT_LEN as usize;
                let bytes = self
                    .event_pool
                    .full_region_mut()
                    .get(offset..offset + wire::EVENT_LEN as usize)
                    .ok_or(DriverError::DeviceFault)?;
                let etype = u16::from_le_bytes([bytes[0], bytes[1]]);
                let code = u16::from_le_bytes([bytes[2], bytes[3]]);
                let value = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                let report = match self.touch.as_mut() {
                    Some(touch) => touch.decode(etype, code, value).map(Report::Touch),
                    None => self.wheels.decode(etype, code, value).map(Report::Event),
                };
                if let Some(report) = report {
                    sink.put(report);
                }
            }
            Self::post_slot(
                &mut self.eventq,
                &mut self.event_pool,
                slot,
                &mut self.event_slots,
            )?;
            reposted = true;
        }
        if reposted {
            self.eventq.kick(&mut self.transport);
        }
        Ok(sink.kept() - before)
    }
}

#[cfg(test)]
mod tests;
