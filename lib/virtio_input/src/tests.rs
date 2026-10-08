//! virtio-input device-logic unit tests against the in-process
//! [`MockTransport`].

extern crate alloc;

use super::*;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use tairix_virtio::{ChainView, MockHost, MockTransport, MockWait};

/// A queued raw `virtio_input_event` the mock device will deliver:
/// `(type, code, value)`.
type RawEvent = (u16, u16, i32);
type EventQueue = Rc<RefCell<VecDeque<RawEvent>>>;

/// Build a `MockTransport` configured as a virtio-input device with two
/// queues (eventq + statusq). The returned `Rc` shares the queue of
/// events the device will deliver when the driver posts a device-write
/// buffer on the eventq.
fn build_device() -> (MockTransport, EventQueue) {
    build_device_with_queue_max(8)
}

/// [`build_device`] whose queues hold at most `queue_max` descriptors.
fn build_device_with_queue_max(queue_max: u16) -> (MockTransport, EventQueue) {
    build_device_with(queue_max, 0)
}

/// A device of two queues (eventq = 0, statusq = 1) of at most `queue_max`
/// descriptors, no feature bits, and a `config_len`-byte config window.
fn build_device_with(queue_max: u16, config_len: usize) -> (MockTransport, EventQueue) {
    let mut t = MockTransport::new(2, queue_max, 0, config_len);
    let events: EventQueue = Rc::new(RefCell::new(VecDeque::new()));
    // Eventq shim (queue 0): on each posted device-write buffer, pop one
    // queued event and write its 8 little-endian bytes into the buffer.
    // An empty queue completes the buffer with zero bytes written (the
    // "no event pending" case).
    let events_for_shim = Rc::clone(&events);
    t.install_shim(
        wire::EVENT_QUEUE,
        Box::new(move |chain: &mut ChainView<'_>| {
            let dst = chain
                .device_write
                .first_mut()
                .ok_or(VirtioError::DeviceFault)?;
            if dst.len() < wire::EVENT_LEN as usize {
                return Err(VirtioError::DeviceFault);
            }
            let Some((etype, code, value)) = events_for_shim.borrow_mut().pop_front() else {
                return Ok(0);
            };
            dst[0..2].copy_from_slice(&etype.to_le_bytes());
            dst[2..4].copy_from_slice(&code.to_le_bytes());
            dst[4..8].copy_from_slice(&value.to_le_bytes());
            Ok(wire::EVENT_LEN)
        }),
    );
    (t, events)
}

/// [`build_device`] whose configuration answers an `EV_BITS` query for
/// `EV_REL` with `relative`, and every other query with nothing.
fn build_device_reporting(relative: &'static [u16]) -> (MockTransport, EventQueue) {
    let (mut t, events) = build_device_with(8, wire::CFG_ANSWER + wire::CFG_ANSWER_LEN);
    t.install_config_responder(Box::new(move |window: &mut [u8]| {
        let answer = (window[wire::CFG_SELECT] == wire::CFG_EV_BITS
            && u16::from(window[wire::CFG_SELECT + 1]) == wire::EV_REL)
            .then(|| bitmap(relative));
        window[wire::CFG_ANSWER..].fill(0);
        window[wire::CFG_SIZE] = 0;
        if let Some(bits) = answer {
            window[wire::CFG_ANSWER..wire::CFG_ANSWER + bits.len()].copy_from_slice(&bits);
            window[wire::CFG_SIZE] = u8::try_from(bits.len()).unwrap_or(0);
        }
    }));
    (t, events)
}

/// The mock device, shared by the driver under test and the host playing it.
type Device = Rc<RefCell<MockTransport>>;

/// Open a driver on `t`, whose waits `host` answers by playing the device.
fn open_input(t: MockTransport, host: &MockHost) -> (Box<VirtioInput<'_, Device>>, Device) {
    let device = t.into_shared();
    host.attach(&device);
    let dev = Box::new(VirtioInput::open(Rc::clone(&device), host).expect("open"));
    (dev, device)
}

/// Scancode-neutral keycode for the `A` key (Linux `KEY_A`).
const KEY_A: u16 = 30;

#[test]
fn decode_maps_key_press_and_release() {
    let press = Wheels::DETENT
        .decode(wire::EV_KEY, KEY_A, 1)
        .expect("key press decodes");
    assert_eq!(press.kind, InputEventKind::Key);
    assert_eq!(press.code, KEY_A);
    assert_eq!(press.value, 1);
    let release = Wheels::DETENT
        .decode(wire::EV_KEY, KEY_A, 0)
        .expect("key release decodes");
    assert_eq!(release.kind, InputEventKind::Key);
    assert_eq!(release.value, 0);
}

#[test]
fn decode_maps_relative_pointer_and_wheel() {
    let x = Wheels::DETENT
        .decode(wire::EV_REL, wire::REL_X, -3)
        .expect("rel-x decodes");
    assert_eq!(x.kind, InputEventKind::Pointer);
    assert_eq!(x.code, AXIS_X);
    assert_eq!(x.value, -3);
    let y = Wheels::DETENT
        .decode(wire::EV_REL, wire::REL_Y, 7)
        .expect("rel-y decodes");
    assert_eq!(y.kind, InputEventKind::Pointer);
    assert_eq!(y.code, AXIS_Y);
    assert_eq!(y.value, 7);
    let wheel = Wheels::DETENT
        .decode(wire::EV_REL, wire::REL_WHEEL, 1)
        .expect("wheel decodes");
    assert_eq!(wheel.kind, InputEventKind::Scroll);
    assert_eq!(wheel.code, AXIS_Y);
    // `evdev` counts the wheel away from the user; the shared axis counts
    // downward, as the pointer's does, in scroll units.
    assert_eq!(
        wheel.value, -SCROLL_UNITS_PER_DETENT,
        "a detent away scrolls toward the start"
    );
    let extreme = Wheels::DETENT
        .decode(wire::EV_REL, wire::REL_WHEEL, i32::MIN)
        .expect("decodes");
    assert_eq!(
        extreme.value,
        i32::MAX,
        "scaled and negated without overflow"
    );
}

#[test]
fn a_horizontal_wheel_counts_toward_the_logical_end_unnegated() {
    let right = Wheels::DETENT
        .decode(wire::EV_REL, wire::REL_HWHEEL, 2)
        .expect("a horizontal detent decodes");
    assert_eq!(
        (right.kind, right.code, right.value),
        (InputEventKind::Scroll, AXIS_X, 2 * SCROLL_UNITS_PER_DETENT)
    );
    assert_eq!(
        tairix_abi::input::PointerInput::from_device_event(&right),
        Some(tairix_abi::input::PointerInput::Scrolled { dx: 240, dy: 0 })
    );
}

/// The bitmap of a device reporting `codes`, as its `EV_BITS` answer is laid.
fn bitmap(codes: &[u16]) -> [u8; 2] {
    let mut bits = [0u8; 2];
    for &code in codes {
        bits[usize::from(code / 8)] |= 1 << (code % 8);
    }
    bits
}

#[test]
fn a_hi_res_axis_reads_its_fine_code_alone_and_drops_the_detent_twin() {
    let wheels = Wheels::of(&bitmap(&[
        wire::REL_X,
        wire::REL_Y,
        wire::REL_WHEEL,
        wire::REL_WHEEL_HI_RES,
        wire::REL_HWHEEL,
        wire::REL_HWHEEL_HI_RES,
    ]));
    // One detent arrives as both codes; only the fine one becomes a scroll.
    assert!(wheels.decode(wire::EV_REL, wire::REL_WHEEL, 1).is_none());
    let fine = wheels
        .decode(wire::EV_REL, wire::REL_WHEEL_HI_RES, 15)
        .expect("a fine step decodes");
    assert_eq!((fine.code, fine.value), (AXIS_Y, -15));
    assert!(wheels.decode(wire::EV_REL, wire::REL_HWHEEL, -1).is_none());
    let sideways = wheels
        .decode(wire::EV_REL, wire::REL_HWHEEL_HI_RES, -40)
        .expect("a fine sideways step decodes");
    assert_eq!((sideways.code, sideways.value), (AXIS_X, -40));
}

#[test]
fn each_axis_takes_its_own_resolution_and_unoffered_fine_codes_are_dropped() {
    let wheels = Wheels::of(&bitmap(&[
        wire::REL_WHEEL,
        wire::REL_WHEEL_HI_RES,
        wire::REL_HWHEEL,
    ]));
    assert_eq!(
        wheels
            .decode(wire::EV_REL, wire::REL_HWHEEL, 1)
            .map(|e| e.value),
        Some(SCROLL_UNITS_PER_DETENT)
    );
    // A fine code the device never offered is not a second reading of an
    // axis already read at detents.
    assert!(wheels
        .decode(wire::EV_REL, wire::REL_HWHEEL_HI_RES, 30)
        .is_none());
    assert!(Wheels::DETENT
        .decode(wire::EV_REL, wire::REL_WHEEL_HI_RES, 30)
        .is_none());
}

/// D26: QEMU's HID pointers report the wheel as gear-button presses, which
/// the button range never accepted, so a wheel reached nothing at all.
#[test]
fn decode_maps_the_gear_buttons_a_hid_pointer_reports_to_wheel_detents() {
    let down = Wheels::DETENT
        .decode(wire::EV_KEY, wire::BTN_GEAR_DOWN, 1)
        .expect("a detent decodes");
    assert_eq!(
        (down.kind, down.code, down.value),
        (InputEventKind::Scroll, AXIS_Y, SCROLL_UNITS_PER_DETENT),
        "toward the user scrolls toward the end"
    );
    let up = Wheels::DETENT
        .decode(wire::EV_KEY, wire::BTN_GEAR_UP, 1)
        .expect("a detent decodes");
    assert_eq!(
        (up.kind, up.code, up.value),
        (InputEventKind::Scroll, AXIS_Y, -SCROLL_UNITS_PER_DETENT)
    );
    // The release that follows each press is no second detent.
    assert!(Wheels::DETENT
        .decode(wire::EV_KEY, wire::BTN_GEAR_DOWN, 0)
        .is_none());
    assert!(Wheels::DETENT
        .decode(wire::EV_KEY, wire::BTN_GEAR_UP, 0)
        .is_none());
    // And a detent reaches the seat as a scroll through the one pointer
    // mapping every input driver shares.
    assert_eq!(
        tairix_abi::input::PointerInput::from_device_event(&down),
        Some(tairix_abi::input::PointerInput::Scrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT
        })
    );
}

#[test]
fn decode_discards_frame_markers_and_unmodelled_events() {
    // EV_SYN frame separator: no surfaced event.
    assert!(Wheels::DETENT.decode(wire::EV_SYN, 0, 0).is_none());
    // An unmapped EV_REL code (e.g. REL_Z): no surfaced event.
    assert!(Wheels::DETENT.decode(wire::EV_REL, 0x02, 5).is_none());
    // An entirely unmodelled type (EV_ABS = 3): no surfaced event.
    assert!(Wheels::DETENT.decode(0x03, 0, 0).is_none());
}

#[test]
fn poll_returns_queued_key_press() {
    let (t, events) = build_device();
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    events.borrow_mut().push_back((wire::EV_KEY, KEY_A, 1));
    let mut buf = [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; 4];
    assert_eq!(dev.poll(&mut buf), Ok(1));
    assert_eq!(buf[0].kind, InputEventKind::Key);
    assert_eq!(buf[0].code, KEY_A);
    assert_eq!(buf[0].value, 1);
}

#[test]
fn poll_drains_press_then_release_in_order() {
    let (t, events) = build_device();
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    events.borrow_mut().push_back((wire::EV_KEY, KEY_A, 1));
    events.borrow_mut().push_back((wire::EV_KEY, KEY_A, 0));
    let mut buf = [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; 1];
    assert_eq!(dev.poll(&mut buf), Ok(1));
    assert_eq!(buf[0].value, 1);
    assert_eq!(dev.poll(&mut buf), Ok(1));
    assert_eq!(buf[0].value, 0);
}

#[test]
fn poll_skips_frame_marker_as_no_event() {
    let (t, events) = build_device();
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    // An EV_SYN completion is consumed but surfaces no event.
    events.borrow_mut().push_back((wire::EV_SYN, 0, 0));
    let mut buf = [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; 1];
    assert_eq!(dev.poll(&mut buf), Ok(0));
}

#[test]
fn a_device_offering_fine_codes_is_read_from_them_alone() {
    let (t, events) = build_device_reporting(&[
        wire::REL_X,
        wire::REL_Y,
        wire::REL_WHEEL,
        wire::REL_WHEEL_HI_RES,
    ]);
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    // One detent as a hi-res device reports it: the fine code, its detent
    // twin, and the frame separator.
    events.borrow_mut().extend([
        (wire::EV_REL, wire::REL_WHEEL_HI_RES, 120),
        (wire::EV_REL, wire::REL_WHEEL, 1),
        (wire::EV_SYN, 0, 0),
    ]);
    let mut buf = batch::<4>();
    assert_eq!(dev.poll(&mut buf), Ok(1));
    assert_eq!(
        (buf[0].kind, buf[0].code, buf[0].value),
        (InputEventKind::Scroll, AXIS_Y, -SCROLL_UNITS_PER_DETENT)
    );
}

#[test]
fn a_device_stating_no_event_bitmap_is_read_at_detents() {
    let (t, events) = build_device_reporting(&[]);
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    events.borrow_mut().extend([
        (wire::EV_REL, wire::REL_HWHEEL, -1),
        (wire::EV_REL, wire::REL_WHEEL_HI_RES, 60),
    ]);
    let mut buf = batch::<4>();
    assert_eq!(dev.poll(&mut buf), Ok(1));
    assert_eq!(
        (buf[0].code, buf[0].value),
        (AXIS_X, -SCROLL_UNITS_PER_DETENT)
    );
}

#[test]
fn poll_with_no_pending_event_returns_zero() {
    let (t, _events) = build_device();
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    let mut buf = [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; 1];
    assert_eq!(dev.poll(&mut buf), Ok(0));
}

#[test]
fn poll_acknowledges_the_device_interrupt_each_cycle() {
    // Regression: `poll` must acknowledge the device once per wait + drain
    // cycle, or a level-signalled transport (virtio-MMIO) keeps its line
    // asserted and every subsequent wait wakes immediately — the busy loop
    // that pegged a core under the curses login screen.
    let (t, events) = build_device();
    let host = &MockHost::new();
    let (mut dev, device) = open_input(t, host);
    let mut buf = [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; 4];
    // Delivered-event path: one poll, one acknowledge.
    events.borrow_mut().push_back((wire::EV_KEY, KEY_A, 1));
    assert_eq!(dev.poll(&mut buf), Ok(1));
    assert_eq!(device.borrow_mut().ack_interrupts, 1);
    // Empty wait path (a spurious wake): still exactly one acknowledge,
    // so a faulted or empty drain never leaves the line asserted.
    assert_eq!(dev.poll(&mut buf), Ok(0));
    assert_eq!(device.borrow_mut().ack_interrupts, 2);
}

#[test]
fn a_device_capping_its_queue_below_a_power_of_two_comes_up_on_the_next_one_down() {
    // Pre-clamped to the device's 12, the request was no ring size at all.
    let (t, events) = build_device_with_queue_max(12);
    let host = &MockHost::new();
    let (mut dev, device) = open_input(t, host);
    assert_eq!(
        device
            .borrow_mut()
            .drain_queue(wire::EVENT_QUEUE)
            .expect("posted buffers"),
        8
    );
    events.borrow_mut().push_back((wire::EV_KEY, KEY_A, 1));
    let mut buf = [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; 4];
    assert!(dev.poll(&mut buf).is_ok());
}

/// An empty poll batch of `N` events.
fn batch<const N: usize>() -> [InputEvent; N] {
    [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; N]
}

#[test]
fn one_drain_takes_no_more_than_a_ring_of_completions() {
    // Frame separators decode to nothing, and each buffer goes straight back,
    // so a device completing them as fast as they are reposted would hold
    // the drain for ever.
    let (t, _events) = build_device();
    let host = &MockHost::new();
    let (mut dev, device) = open_input(t, host);
    let ring = dev.eventq.size();
    // Head 0 is reposted under head 0 each time it comes back.
    for _ in 0..2 * ring {
        device
            .borrow_mut()
            .publish_raw_used(wire::EVENT_QUEUE, 0, wire::EVENT_LEN)
            .expect("in the ring");
    }
    let mut buf = batch::<4>();
    assert_eq!(
        dev.drain_ready(&mut Events {
            out: &mut buf,
            len: 0
        }),
        Ok(0)
    );
    assert!(
        dev.eventq.poll_used().is_ok(),
        "a ring's worth is still waiting"
    );
}

#[test]
fn an_event_slot_completed_without_a_write_is_not_decoded_again() {
    // Decoded again, a stale key press is a keystroke nobody typed.
    let (t, events) = build_device();
    let host = &MockHost::new();
    let (mut dev, device) = open_input(t, host);
    events.borrow_mut().push_back((wire::EV_KEY, KEY_A, 1));
    let mut buf = batch::<4>();
    assert_eq!(dev.poll(&mut buf), Ok(1));
    // The press landed in head 0's slot, reposted under head 0.
    device
        .borrow_mut()
        .publish_raw_used(wire::EVENT_QUEUE, 0, wire::EVENT_LEN)
        .expect("in the ring");
    assert_eq!(
        dev.drain_ready(&mut Events {
            out: &mut buf,
            len: 0
        }),
        Ok(0)
    );
}

#[test]
fn a_wait_that_cannot_be_made_fails_the_poll_rather_than_spinning() {
    // A revoked binding times every wait out at once: returning no events
    // would have the caller poll again at once, for ever.
    let (t, _events) = build_device();
    let host = &MockHost::new();
    host.script_waits([MockWait::Refused]);
    let device = t.into_shared();
    host.attach(&device);
    let mut dev = VirtioInput::open(Rc::clone(&device), host).expect("open");
    assert_eq!(dev.poll(&mut batch::<4>()), Err(DriverError::DeviceOffline));
    assert_eq!(host.notify_log().len(), 1);
}

#[test]
fn poll_rejects_empty_buffer() {
    let (t, _events) = build_device();
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    let mut empty: [InputEvent; 0] = [];
    assert_eq!(dev.poll(&mut empty), Err(DriverError::BufferTooSmall));
}

#[test]
fn bring_up_declares_the_device_quiesced_once_its_reset_confirms() {
    let (t, _events) = build_device();
    let host = MockHost::new();
    let _dev = VirtioInput::open(t, &host).expect("open");
    assert_eq!(host.quiesced_calls(), 1);
}

#[test]
fn a_device_whose_reset_never_confirms_is_refused_before_it_is_given_memory() {
    let (mut t, _events) = build_device();
    t.refuse_resets_after(0);
    let host = MockHost::new();
    assert_eq!(
        VirtioInput::open(t, &host).err(),
        Some(DriverError::DeviceFault)
    );
    assert_eq!(host.quiesced_calls(), 0);
    assert_eq!(host.bytes_allocated(), 0);
}

#[test]
fn a_dropped_device_that_confirms_its_reset_releases_every_region() {
    let (t, _events) = build_device();
    let host = MockHost::new();
    drop(VirtioInput::open(t, &host).expect("open"));
    assert_eq!(host.slabs_outstanding(), 0);
}

#[test]
fn a_dropped_device_whose_reset_never_confirms_releases_nothing() {
    // The keyboard driver's event loop returns on a device fault with the
    // event pool still posted.
    let (t, _events) = build_device();
    let host = MockHost::new();
    let device = t.into_shared();
    let dev = VirtioInput::open(Rc::clone(&device), &host).expect("open");
    let held = host.slabs_outstanding();
    assert!(held > 0);
    device.borrow_mut().refuse_resets_after(0);
    drop(dev);
    assert_eq!(host.slabs_outstanding(), held);
}

#[test]
fn open_armed_arms_only_after_the_event_queue_is_live() {
    // Regression: the arm step (the driver's `irq_bind` — the readiness
    // witness a harness or supervisor watches for) must run only once the
    // device can already accept an event. Arming first advertised a
    // keyboard whose eventq had no posted buffers, and a keystroke typed
    // in that window was silently dropped — the flaky autoload-input
    // vertical's lost keypress.
    let (t, events) = build_device();
    let host = &MockHost::new();
    let device = t.into_shared();
    device.borrow_mut().reach(host);
    // A keystroke is already pending at the device when the arm step runs.
    events.borrow_mut().push_back((wire::EV_KEY, KEY_A, 1));
    let armed = core::cell::Cell::new(0u32);
    let mut dev = VirtioInput::open_armed(Rc::clone(&device), host, |_| {
        armed.set(armed.get() + 1);
        let mut t = device.borrow_mut();
        // The device is live before the arm step runs...
        assert!(t.status().contains(Status::DRIVER_OK));
        // ...with every event buffer already posted and device-visible:
        // the device can deliver the pending keystroke (and complete the
        // remaining posted buffers) right now. The posted count is the
        // *negotiated* depth — the mock's advertised queue maximum,
        // clamped by the pool ceiling — never a demand the device cannot
        // honour.
        let negotiated = t.queue_max_size().min(wire::EVENT_QUEUE_SIZE);
        assert_eq!(
            t.drain_queue(wire::EVENT_QUEUE).expect("posted buffers"),
            usize::from(negotiated)
        );
        Ok(())
    })
    .expect("open_armed");
    assert_eq!(armed.get(), 1);
    // The keystroke delivered while arming is not lost: the first poll's
    // pre-wait drain collects it.
    let mut buf = [InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    }; 4];
    assert_eq!(dev.poll(&mut buf), Ok(1));
    assert_eq!(buf[0].code, KEY_A);
    assert_eq!(buf[0].value, 1);
}

/// [`Transport`] wrapper that counts device resets while delegating to the
/// in-process mock, so a test can observe the teardown
/// [`VirtioInput::open_armed`] performs after a failed arm step even though
/// the device value is consumed by that teardown.
struct ResetProbe {
    inner: MockTransport,
    resets: Rc<core::cell::Cell<u32>>,
}

impl Transport for ResetProbe {
    fn reset(&mut self) -> Result<(), VirtioError> {
        self.resets.set(self.resets.get() + 1);
        self.inner.reset()
    }
    fn status(&self) -> Status {
        self.inner.status()
    }
    fn set_status(&mut self, status: Status) {
        self.inner.set_status(status);
    }
    fn device_features(&self) -> u64 {
        self.inner.device_features()
    }
    fn set_driver_features(&mut self, features: u64) {
        self.inner.set_driver_features(features);
    }
    fn num_queues(&self) -> u16 {
        self.inner.num_queues()
    }
    fn queue_select(&mut self, queue: u16) -> Result<(), VirtioError> {
        self.inner.queue_select(queue)
    }
    fn queue_max_size(&self) -> u16 {
        self.inner.queue_max_size()
    }
    fn queue_set(
        &mut self,
        size: u16,
        desc: u64,
        avail: u64,
        used: u64,
    ) -> Result<(), VirtioError> {
        self.inner.queue_set(size, desc, avail, used)
    }
    fn notify(&mut self, queue: u16) {
        self.inner.notify(queue);
    }
    fn config_len(&self) -> usize {
        self.inner.config_len()
    }
    fn read_config(&self, offset: usize, buf: &mut [u8]) {
        self.inner.read_config(offset, buf);
    }
    fn write_config(&mut self, offset: usize, data: &[u8]) {
        self.inner.write_config(offset, data);
    }
    fn ack_interrupt(&mut self) {
        self.inner.ack_interrupt();
    }
}

#[test]
fn open_armed_surfaces_the_arm_error_and_resets_the_device() {
    // A failed arm step must tear the device down (a live device is never
    // left DMA-writing into a driver that is about to exit) and surface
    // the arm error unchanged.
    let (t, _events) = build_device();
    let resets = Rc::new(core::cell::Cell::new(0u32));
    let probe = ResetProbe {
        inner: t,
        resets: Rc::clone(&resets),
    };
    let host = &MockHost::new();
    let Err(err) = VirtioInput::open_armed(probe, host, |_| Err(DriverError::PermissionDenied))
    else {
        panic!("arm failure must surface");
    };
    assert_eq!(err, DriverError::PermissionDenied);
    // `open`'s initialisation reset plus the arm-failure teardown.
    assert_eq!(resets.get(), 2);
}

/// A bitmap of `EV_ABS` codes, up to `ABS_MT_TRACKING_ID`.
fn absolute_bitmap(codes: &[u16]) -> [u8; 8] {
    let mut bits = [0u8; 8];
    for &code in codes {
        bits[usize::from(code / 8)] |= 1 << (code % 8);
    }
    bits
}

/// The slotted axes a multi-touch device reports.
const SLOTTED: [u16; 4] = [
    wire::ABS_MT_SLOT,
    wire::ABS_MT_POSITION_X,
    wire::ABS_MT_POSITION_Y,
    wire::ABS_MT_TRACKING_ID,
];

/// A multi-touch device whose two axes run `min..=max` at `resolution` units
/// a millimetre, stating `properties` (none at all for `None`).
fn build_touch_device(
    min: i32,
    max: i32,
    resolution: u32,
    properties: Option<u8>,
) -> (MockTransport, EventQueue) {
    let (mut t, events) = build_device_with(64, wire::CFG_ANSWER + wire::CFG_ANSWER_LEN);
    t.install_config_responder(Box::new(move |window: &mut [u8]| {
        let (select, subsel) = (window[wire::CFG_SELECT], window[wire::CFG_SELECT + 1]);
        let mut answer = [0u8; wire::ABS_INFO_LEN];
        let len = match select {
            wire::CFG_EV_BITS if u16::from(subsel) == wire::EV_ABS => {
                answer[..8].copy_from_slice(&absolute_bitmap(&SLOTTED));
                8
            }
            wire::CFG_ABS_INFO
                if [wire::ABS_MT_POSITION_X, wire::ABS_MT_POSITION_Y]
                    .contains(&u16::from(subsel)) =>
            {
                answer[0..4].copy_from_slice(&min.to_le_bytes());
                answer[4..8].copy_from_slice(&max.to_le_bytes());
                answer[16..20].copy_from_slice(&resolution.to_le_bytes());
                wire::ABS_INFO_LEN
            }
            wire::CFG_PROP_BITS => match properties {
                Some(bits) => {
                    answer[0] = bits;
                    1
                }
                None => 0,
            },
            _ => 0,
        };
        window[wire::CFG_ANSWER..].fill(0);
        window[wire::CFG_ANSWER..wire::CFG_ANSWER + len].copy_from_slice(&answer[..len]);
        window[wire::CFG_SIZE] = u8::try_from(len).unwrap_or(0);
    }));
    (t, events)
}

/// The events placing contact `id` in `slot` at `(x, y)`.
fn touching(slot: i32, id: i32, x: i32, y: i32) -> [RawEvent; 4] {
    [
        (wire::EV_ABS, wire::ABS_MT_SLOT, slot),
        (wire::EV_ABS, wire::ABS_MT_TRACKING_ID, id),
        (wire::EV_ABS, wire::ABS_MT_POSITION_X, x),
        (wire::EV_ABS, wire::ABS_MT_POSITION_Y, y),
    ]
}

const REPORT: RawEvent = (wire::EV_SYN, wire::SYN_REPORT, 0);

/// Every touch frame the device has queued.
fn frames(dev: &mut VirtioInput<'_, Device>) -> alloc::vec::Vec<TouchFrame> {
    let mut reports = [Report::Event(batch::<1>()[0]); 16];
    let mut out = alloc::vec::Vec::new();
    while let Ok(count @ 1..) = dev.poll_reports(&mut reports) {
        out.extend(reports[..count].iter().filter_map(|report| match report {
            Report::Touch(frame) => Some(*frame),
            Report::Event(_) => None,
        }));
    }
    out
}

/// `(id, x, y)` of each contact a frame carries.
fn contacts(frame: &TouchFrame) -> alloc::vec::Vec<(u16, u16, u16)> {
    frame
        .contacts()
        .iter()
        .map(|contact| (contact.id, contact.x, contact.y))
        .collect()
}

#[test]
fn a_slotted_device_frames_every_contact_down_on_each_report() {
    use tairix_abi::touch::TouchSurface;
    let (t, events) = build_touch_device(0, 1_000, 0, None);
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    let mut queue = events.borrow_mut();
    queue.extend(touching(0, 5, 0, 0));
    queue.push_back(REPORT);
    queue.extend(touching(1, 6, 1_000, 500));
    queue.push_back(REPORT);
    // Contact 5 lifts; contact 6 moves without naming its slot again.
    queue.extend([
        (wire::EV_ABS, wire::ABS_MT_SLOT, 0),
        (wire::EV_ABS, wire::ABS_MT_TRACKING_ID, -1),
        (wire::EV_ABS, wire::ABS_MT_SLOT, 1),
        (wire::EV_ABS, wire::ABS_MT_POSITION_X, 250),
        REPORT,
    ]);
    drop(queue);
    let frames = frames(&mut dev);
    assert_eq!(frames.len(), 3);
    assert_eq!(contacts(&frames[0]), [(5, 0, 0)]);
    assert_eq!(contacts(&frames[1]), [(5, 0, 0), (6, u16::MAX, 32_767)]);
    assert_eq!(contacts(&frames[2]), [(6, 16_383, 32_767)]);
    assert_eq!(
        frames[0].surface(),
        TouchSurface::Screen,
        "it names no property"
    );
    assert!(dev
        .touch_lifted()
        .is_some_and(|lifted| lifted.contacts().is_empty()));
}

#[test]
fn a_position_is_normalised_over_the_stated_range_and_held_to_it() {
    let (t, events) = build_touch_device(100, 1_100, 0, None);
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    events.borrow_mut().extend(touching(0, 1, 600, 2_000));
    events.borrow_mut().extend(touching(1, 2, 50, -7));
    events.borrow_mut().push_back(REPORT);
    let frames = frames(&mut dev);
    assert_eq!(
        contacts(&frames[0]),
        [(1, 32_767, u16::MAX), (2, 0, 0)],
        "the middle, and both ends held"
    );
}

#[test]
fn the_properties_say_what_kind_of_surface_it_is() {
    use tairix_abi::touch::TouchSurface;
    let pointer = 1 << wire::INPUT_PROP_POINTER;
    for (properties, surface) in [
        (None, TouchSurface::Screen),
        (Some(1 << wire::INPUT_PROP_DIRECT), TouchSurface::Screen),
        (Some(pointer), TouchSurface::Touchpad),
        (
            Some(pointer | 1 << wire::INPUT_PROP_BUTTONPAD),
            TouchSurface::Clickpad,
        ),
    ] {
        let (t, _events) = build_touch_device(0, 1_000, 0, properties);
        let host = &MockHost::new();
        let (dev, _device) = open_input(t, host);
        assert_eq!(
            dev.touch_lifted().map(|frame| frame.surface()),
            Some(surface),
            "{properties:?}"
        );
    }
}

#[test]
fn a_stated_resolution_gives_the_surface_its_size() {
    use tairix_abi::touch::TouchExtent;
    let (t, _events) = build_touch_device(0, 1_000, 10, None);
    let host = &MockHost::new();
    let (dev, _device) = open_input(t, host);
    assert_eq!(
        dev.touch_lifted().map(|frame| frame.extent()),
        Some(TouchExtent {
            width: 1_000,
            height: 1_000
        }),
        "a thousand units at ten a millimetre are 100 mm"
    );
}

#[test]
fn lost_events_are_discarded_to_the_report_and_every_contact_lifts() {
    let (t, events) = build_touch_device(0, 1_000, 0, None);
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    let mut queue = events.borrow_mut();
    queue.extend(touching(0, 5, 10, 10));
    queue.push_back(REPORT);
    queue.push_back((wire::EV_SYN, wire::SYN_DROPPED, 0));
    queue.extend(touching(1, 6, 20, 20));
    queue.push_back(REPORT);
    queue.extend(touching(1, 7, 30, 30));
    queue.push_back(REPORT);
    drop(queue);
    let frames = frames(&mut dev);
    assert_eq!(frames.len(), 3);
    assert!(frames[1].contacts().is_empty(), "what was down is let go");
    assert_eq!(contacts(&frames[2]).len(), 1, "and a new touch is followed");
}

#[test]
fn a_slot_past_what_a_frame_carries_and_a_repeated_id_are_not_followed() {
    let (t, events) = build_touch_device(0, 1_000, 0, None);
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    let mut queue = events.borrow_mut();
    queue.extend(touching(12, 9, 10, 10));
    queue.extend(touching(0, 4, 20, 20));
    queue.extend(touching(1, 4, 30, 30));
    queue.push_back(REPORT);
    drop(queue);
    let frames = frames(&mut dev);
    assert_eq!(
        contacts(&frames[0]).len(),
        1,
        "slot 12 and the second id 4 dropped"
    );
}

#[test]
fn a_touchpads_buttons_and_palms_ride_the_frame() {
    use tairix_abi::touch::{ContactKind, TouchButtons};
    let (t, events) = build_touch_device(0, 1_000, 0, Some(1 << wire::INPUT_PROP_POINTER));
    let host = &MockHost::new();
    let (mut dev, _device) = open_input(t, host);
    let mut queue = events.borrow_mut();
    queue.extend(touching(0, 1, 10, 10));
    queue.push_back((wire::EV_ABS, wire::ABS_MT_TOOL_TYPE, wire::MT_TOOL_PALM));
    queue.push_back((wire::EV_KEY, wire::BTN_LEFT, 1));
    queue.push_back(REPORT);
    queue.push_back((wire::EV_KEY, wire::BTN_LEFT, 0));
    queue.push_back(REPORT);
    drop(queue);
    let frames = frames(&mut dev);
    assert!(frames[0].buttons().holds(TouchButtons::PRIMARY));
    assert_eq!(frames[0].contacts()[0].kind, ContactKind::Palm);
    assert_eq!(frames[1].buttons(), TouchButtons::NONE);
}

#[test]
fn a_device_with_no_slotted_axes_is_no_touch_surface() {
    let (t, _events) = build_device_reporting(&[wire::REL_X, wire::REL_Y]);
    let host = &MockHost::new();
    let (dev, _device) = open_input(t, host);
    assert!(dev.touch_lifted().is_none());
}
