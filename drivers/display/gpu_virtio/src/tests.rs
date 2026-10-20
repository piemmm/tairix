//! The engine against a model of the 2D device: a mock transport whose
//! control and cursor queues answer as the specification's device does,
//! keeping the resources, scanouts and cursor a real one would.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_display::testkit::HeapMemory;
use tairix_drvrt::testkit::Records;
use tairix_virtio::{ChainView, MockHost, MockTransport, VirtioError};

use super::*;

const OK_NODATA: u32 = 0x1100;
const OK_DISPLAY_INFO: u32 = 0x1101;
const OK_EDID: u32 = 0x1104;
const ERR_UNSPEC: u32 = 0x1200;
const ERR_OUT_OF_MEMORY: u32 = 0x1201;
const ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
const ERR_INVALID_PARAMETER: u32 = 0x1205;

const GET_DISPLAY_INFO: u32 = 0x0100;
const CREATE_2D: u32 = 0x0101;
const SET_SCANOUT: u32 = 0x0103;
const UPDATE_CURSOR: u32 = 0x0300;
const MOVE_CURSOR: u32 = 0x0301;

/// Where the model console's memory lies on the device.
const CONSOLE_AT: u64 = 0x7000_0000;

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("a word"))
}

fn rect_at(bytes: &[u8], at: usize) -> DamageRect {
    DamageRect {
        x: word(bytes, at),
        y: word(bytes, at + 4),
        width_px: word(bytes, at + 8),
        height_px: word(bytes, at + 12),
    }
}

fn inside(rect: &DamageRect, width: u32, height: u32) -> bool {
    rect.x + rect.width_px <= width && rect.y + rect.height_px <= height
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Resource {
    format: u32,
    width: u32,
    height: u32,
    backing: Option<(u64, u32)>,
}

/// One cursor-queue command: its type, scanout, position, resource and hot
/// spot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Cursor {
    kind: u32,
    scanout: u32,
    at: (u32, u32),
    resource: u32,
    hot: (u32, u32),
}

/// The device's state, and what it was told.
#[derive(Default)]
struct Model {
    scanouts: Vec<(u32, u32, bool)>,
    edid: Option<Vec<u8>>,
    resources: BTreeMap<u32, Resource>,
    scanned: BTreeMap<u32, (u32, DamageRect)>,
    transfers: Vec<(u32, DamageRect, u64)>,
    flushes: Vec<(u32, DamageRect)>,
    cursor: Vec<Cursor>,
    commands: Vec<u32>,
    /// The error the next command of this type is answered with.
    refuse: Option<(u32, u32)>,
    /// The device stops answering at the next command of this type.
    fall_silent_at: Option<u32>,
    /// Whether the device has stopped answering.
    mute: bool,
}

impl Model {
    fn apply(&mut self, request: &[u8], response: &mut [u8]) -> u32 {
        let kind = word(request, 0);
        self.commands.push(kind);
        if let Some((refused, error)) = self.refuse {
            if refused == kind {
                self.refuse = None;
                return error;
            }
        }
        match kind {
            GET_DISPLAY_INFO => self.display_info(response),
            0x010A => self.edid(response),
            CREATE_2D => self.create(request),
            0x0102 => self.unref(word(request, 24)),
            SET_SCANOUT => {
                self.set_scanout(rect_at(request, 24), word(request, 40), word(request, 44))
            }
            0x0104 => self.flush(rect_at(request, 24), word(request, 40)),
            0x0105 => self.transfer(request),
            0x0106 => self.attach(request),
            0x0107 => self.detach(word(request, 24)),
            _ => ERR_UNSPEC,
        }
    }

    fn display_info(&self, response: &mut [u8]) -> u32 {
        for (index, (width, height, enabled)) in self.scanouts.iter().enumerate() {
            let at = wire::HEADER_LEN + index * 24;
            response[at + 8..at + 12].copy_from_slice(&width.to_le_bytes());
            response[at + 12..at + 16].copy_from_slice(&height.to_le_bytes());
            response[at + 16..at + 20].copy_from_slice(&u32::from(*enabled).to_le_bytes());
        }
        OK_DISPLAY_INFO
    }

    fn edid(&self, response: &mut [u8]) -> u32 {
        let Some(edid) = &self.edid else {
            return ERR_UNSPEC;
        };
        let len = u32::try_from(edid.len()).expect("small");
        response[wire::HEADER_LEN..wire::HEADER_LEN + 4].copy_from_slice(&len.to_le_bytes());
        response[wire::HEADER_LEN + 8..wire::HEADER_LEN + 8 + edid.len()].copy_from_slice(edid);
        OK_EDID
    }

    fn create(&mut self, request: &[u8]) -> u32 {
        let id = word(request, 24);
        if id == 0 || self.resources.contains_key(&id) {
            return ERR_INVALID_RESOURCE_ID;
        }
        let resource = Resource {
            format: word(request, 28),
            width: word(request, 32),
            height: word(request, 36),
            backing: None,
        };
        self.resources.insert(id, resource);
        OK_NODATA
    }

    fn unref(&mut self, id: u32) -> u32 {
        if self.resources.remove(&id).is_none() {
            return ERR_INVALID_RESOURCE_ID;
        }
        self.scanned.retain(|_, (shown, _)| *shown != id);
        OK_NODATA
    }

    fn set_scanout(&mut self, area: DamageRect, scanout: u32, id: u32) -> u32 {
        if id == 0 {
            self.scanned.remove(&scanout);
            return OK_NODATA;
        }
        match self.resources.get(&id) {
            Some(resource) if inside(&area, resource.width, resource.height) => {
                self.scanned.insert(scanout, (id, area));
                OK_NODATA
            }
            Some(_) => ERR_INVALID_PARAMETER,
            None => ERR_INVALID_RESOURCE_ID,
        }
    }

    fn flush(&mut self, area: DamageRect, id: u32) -> u32 {
        if !self.resources.contains_key(&id) {
            return ERR_INVALID_RESOURCE_ID;
        }
        self.flushes.push((id, area));
        OK_NODATA
    }

    fn transfer(&mut self, request: &[u8]) -> u32 {
        let area = rect_at(request, 24);
        let offset = u64::from_le_bytes(request[40..48].try_into().expect("an offset"));
        let id = word(request, 48);
        match self.resources.get(&id) {
            Some(resource)
                if resource.backing.is_some() && inside(&area, resource.width, resource.height) =>
            {
                self.transfers.push((id, area, offset));
                OK_NODATA
            }
            Some(_) => ERR_INVALID_PARAMETER,
            None => ERR_INVALID_RESOURCE_ID,
        }
    }

    fn attach(&mut self, request: &[u8]) -> u32 {
        let addr = u64::from_le_bytes(request[32..40].try_into().expect("an address"));
        match self.resources.get_mut(&word(request, 24)) {
            Some(resource) if word(request, 28) == 1 => {
                resource.backing = Some((addr, word(request, 40)));
                OK_NODATA
            }
            Some(_) => ERR_INVALID_PARAMETER,
            None => ERR_INVALID_RESOURCE_ID,
        }
    }

    fn detach(&mut self, id: u32) -> u32 {
        match self.resources.get_mut(&id) {
            Some(resource) => {
                resource.backing = None;
                OK_NODATA
            }
            None => ERR_INVALID_RESOURCE_ID,
        }
    }
}

type Shared = Rc<RefCell<Model>>;

/// A transport over the model, answering at each notify.
fn transport(model: &Shared, host: &MockHost, features: u64) -> Rc<RefCell<MockTransport>> {
    let mut transport = MockTransport::new(2, 64, features, 16);
    transport.reach(host);
    transport.set_synchronous_notify(true);
    let control = Rc::clone(model);
    transport.install_shim(
        wire::CONTROL_QUEUE,
        Box::new(move |chain: &mut ChainView<'_>| {
            let mut model = control.borrow_mut();
            let request = chain
                .device_read
                .first()
                .copied()
                .ok_or(VirtioError::DeviceFault)?;
            let kind = word(request, 0);
            if model.fall_silent_at.take_if(|at| *at == kind).is_some() {
                model.mute = true;
            }
            if model.mute {
                return Err(VirtioError::NoCompletion);
            }
            let response = chain
                .device_write
                .first_mut()
                .ok_or(VirtioError::DeviceFault)?;
            response.fill(0);
            let kind = model.apply(request, response);
            response[..4].copy_from_slice(&kind.to_le_bytes());
            u32::try_from(response.len()).map_err(|_| VirtioError::DeviceFault)
        }),
    );
    let cursor = Rc::clone(model);
    transport.install_shim(
        wire::CURSOR_QUEUE,
        Box::new(move |chain: &mut ChainView<'_>| {
            let request = chain
                .device_read
                .first()
                .copied()
                .ok_or(VirtioError::DeviceFault)?;
            cursor.borrow_mut().cursor.push(Cursor {
                kind: word(request, 0),
                scanout: word(request, 24),
                at: (word(request, 28), word(request, 32)),
                resource: word(request, 40),
                hot: (word(request, 44), word(request, 48)),
            });
            Ok(0)
        }),
    );
    transport.install_config_responder(Box::new(|config: &mut [u8]| {
        let clear = word(config, wire::CONFIG_EVENTS_CLEAR);
        let raised = word(config, wire::CONFIG_EVENTS_READ) & !clear;
        config[wire::CONFIG_EVENTS_READ..wire::CONFIG_EVENTS_READ + 4]
            .copy_from_slice(&raised.to_le_bytes());
        config[wire::CONFIG_EVENTS_CLEAR..wire::CONFIG_EVENTS_CLEAR + 4].fill(0);
    }));
    transport.into_shared()
}

/// The kernel console the model node hosts, held as the kernel holds it: at
/// one mode until relinquished.
#[derive(Default)]
struct ConsoleState {
    adopted: Vec<(u32, u32)>,
    held: Option<(u32, u32)>,
    relinquished: usize,
    damage: Option<DamageRect>,
    refuse: Option<DriverError>,
}

#[derive(Clone, Default)]
struct Console(Rc<RefCell<ConsoleState>>);

impl ConsoleSource for Console {
    fn adopt(&mut self, width: u32, height: u32) -> Result<HostedConsole, DriverError> {
        let mut state = self.0.borrow_mut();
        if let Some(refused) = state.refuse {
            return Err(refused);
        }
        if state.held.is_some_and(|held| held != (width, height)) {
            return Err(DriverError::Busy);
        }
        state.adopted.push((width, height));
        state.held = Some((width, height));
        Ok(HostedConsole {
            device_addr: CONSOLE_AT,
            stride_bytes: width * 4,
        })
    }

    fn relinquish(&mut self) -> Result<(), DriverError> {
        let mut state = self.0.borrow_mut();
        state.held.take().ok_or(DriverError::NotFound)?;
        state.relinquished += 1;
        Ok(())
    }

    fn damage(&mut self) -> Result<Option<DamageRect>, DriverError> {
        let mut state = self.0.borrow_mut();
        if state.held.is_none() {
            return Err(DriverError::PermissionDenied);
        }
        Ok(state.damage.take())
    }
}

type Gpu<'h> = VirtioGpu<'h, Rc<RefCell<MockTransport>>, HeapMemory, Console, Records<Record>>;

/// A device, its transport, and what the driver holds and records.
struct Rig {
    model: Shared,
    host: MockHost,
    memory: HeapMemory,
    console: Console,
    records: Records<Record>,
    features: u64,
}

impl Rig {
    /// A device with one 1280 × 800 scanout and no EDID.
    fn new() -> Self {
        let model = Model {
            scanouts: vec![(1280, 800, true)],
            ..Model::default()
        };
        Self {
            model: Rc::new(RefCell::new(model)),
            host: MockHost::new(),
            memory: HeapMemory::default(),
            console: Console::default(),
            records: Records::default(),
            features: TRANSPORT_FEATURES,
        }
    }

    fn open(&self) -> (Gpu<'_>, Rc<RefCell<MockTransport>>) {
        let transport = transport(&self.model, &self.host, self.features);
        let gpu = VirtioGpu::open(
            Rc::clone(&transport),
            &self.host,
            self.memory.clone(),
            Some(self.console.clone()),
            self.records.clone(),
        )
        .expect("opened");
        (gpu, transport)
    }

    fn model(&self) -> core::cell::RefMut<'_, Model> {
        self.model.borrow_mut()
    }

    /// What scanout 0 shows.
    fn scanned(&self) -> Option<u32> {
        self.model
            .borrow()
            .scanned
            .get(&0)
            .map(|(resource, _)| *resource)
    }
}

/// Allocate and present a whole frame, answering it and its resource.
fn present_frame(gpu: &mut Gpu<'_>, rig: &Rig) -> (AllocatedBuffer, u32) {
    let frame = gpu.alloc_buffer(1280, 800).expect("a frame");
    assert_eq!(
        gpu.present(frame.id, &[whole(1280, 800)]),
        Ok(Presented::Shown)
    );
    let resource = rig.scanned().expect("a frame shown");
    (frame, resource)
}

/// Have the device say its displays changed.
fn raise_display_event(transport: &RefCell<MockTransport>) {
    transport
        .borrow_mut()
        .set_config(wire::CONFIG_EVENTS_READ, &wire::EVENT_DISPLAY.to_le_bytes());
}

fn rect(x: u32, y: u32, width_px: u32, height_px: u32) -> DamageRect {
    DamageRect {
        x,
        y,
        width_px,
        height_px,
    }
}

#[test]
fn the_output_is_the_first_enabled_scanout_and_the_console_is_shown_on_it() {
    let rig = Rig::new();
    rig.model().scanouts = vec![(640, 480, false), (1280, 800, true)];
    let (gpu, _) = rig.open();
    assert_eq!(
        gpu.output(),
        Some(DisplayMode {
            width_px: 1280,
            height_px: 800,
            stride_bytes: 5120,
            format: DisplayFormat::Bgra8888,
        })
    );
    assert_eq!(rig.console.0.borrow().adopted, vec![(1280, 800)]);
    assert_eq!(
        rig.records.all(),
        vec![Record::ConsoleShown {
            width: 1280,
            height: 800
        }]
    );
    let model = rig.model.borrow();
    let (&console, resource) = model
        .resources
        .iter()
        .next()
        .expect("the console's resource");
    assert_eq!(
        *resource,
        Resource {
            format: 2,
            width: 1280,
            height: 800,
            backing: Some((CONSOLE_AT, 5120 * 800)),
        }
    );
    assert_eq!(model.scanned.get(&1), Some(&(console, whole(1280, 800))));
    assert_eq!(model.transfers, vec![(console, whole(1280, 800), 0)]);
    assert_eq!(model.flushes, vec![(console, whole(1280, 800))]);
}

#[test]
fn a_scanout_naming_no_size_takes_its_sink_s_preferred_timing() {
    let mut rig = Rig::new();
    rig.features = TRANSPORT_FEATURES | wire::FEATURE_EDID;
    let edid = tairix_edid::synth::Base::new()
        .descriptor(0, tairix_edid::synth::dtd_1080p())
        .build();
    rig.model().scanouts = vec![(0, 0, true)];
    rig.model().edid = Some(edid.to_vec());
    let (gpu, _) = rig.open();
    assert_eq!(
        gpu.output().map(|mode| (mode.width_px, mode.height_px)),
        Some((1920, 1080))
    );
}

#[test]
fn the_output_runs_at_the_rate_its_sink_states_for_the_size_it_shows() {
    let sink = tairix_edid::synth::Base::new()
        .descriptor(0, tairix_edid::synth::dtd_1080p())
        .descriptor(1, tairix_edid::synth::dtd_720p())
        .build();
    for (scanout, rate) in [
        ((0, 0), RefreshRate::from_mhz(60_000)),
        ((1920, 1080), RefreshRate::from_mhz(60_000)),
        ((1280, 720), RefreshRate::from_mhz(60_000)),
        ((1280, 800), None),
    ] {
        let mut rig = Rig::new();
        rig.features = TRANSPORT_FEATURES | wire::FEATURE_EDID;
        rig.model().scanouts = vec![(scanout.0, scanout.1, true)];
        rig.model().edid = Some(sink.to_vec());
        let (gpu, _) = rig.open();
        assert_eq!(gpu.refresh(), rate, "{scanout:?}");
    }
}

/// A sink whose same-sized timing states no usable rate is passed over for
/// one that does, rather than ending the search.
#[test]
fn the_rate_is_the_first_same_sized_timing_that_states_one() {
    // A ten-kilohertz clock refreshes 1080 lines far below a hertz.
    let unclocked = tairix_edid::synth::dtd(
        10,
        [1920, 88, 44, 148],
        [1080, 4, 5, 36],
        tairix_edid::synth::SYNC_POSITIVE,
        (600, 340),
    );
    let mut rig = Rig::new();
    rig.features = TRANSPORT_FEATURES | wire::FEATURE_EDID;
    rig.model().scanouts = vec![(1920, 1080, true)];
    rig.model().edid = Some(
        tairix_edid::synth::Base::new()
            .descriptor(0, unclocked)
            .descriptor(1, tairix_edid::synth::dtd_1080p())
            .build()
            .to_vec(),
    );
    let (gpu, _) = rig.open();
    assert_eq!(gpu.refresh(), RefreshRate::from_mhz(60_000));
}

/// A display that keeps its mode and changes only its rate shows exactly what
/// it did: the rate is the display service's to read, and the scanout stands.
#[test]
fn a_rate_alone_changing_re_shows_nothing() {
    let fifty = tairix_edid::synth::dtd(
        148_500,
        [1920, 528, 44, 148],
        [1080, 4, 5, 36],
        tairix_edid::synth::SYNC_POSITIVE,
        (600, 340),
    );
    let sink = |descriptor: [u8; 18]| {
        tairix_edid::synth::Base::new()
            .descriptor(0, descriptor)
            .build()
            .to_vec()
    };
    let mut rig = Rig::new();
    rig.features = TRANSPORT_FEATURES | wire::FEATURE_EDID;
    rig.model().scanouts = vec![(1920, 1080, true)];
    rig.model().edid = Some(sink(tairix_edid::synth::dtd_1080p()));
    let (mut gpu, transport) = rig.open();
    assert_eq!(gpu.refresh(), RefreshRate::from_mhz(60_000));
    let flushes = rig.model.borrow().flushes.len();
    rig.model().edid = Some(sink(fifty));
    rig.model().commands.clear();
    raise_display_event(&transport);
    assert_eq!(gpu.service_events(), Ok(Serviced::default()));
    assert_eq!(gpu.refresh(), RefreshRate::from_mhz(50_000));
    let model = rig.model.borrow();
    assert!(!model.commands.contains(&SET_SCANOUT), "the scanout stands");
    assert_eq!(model.flushes.len(), flushes, "nothing is shown again");
}

#[test]
fn an_output_with_no_sink_states_no_rate() {
    let rig = Rig::new();
    let (gpu, _) = rig.open();
    assert!(gpu.output().is_some());
    assert_eq!(gpu.refresh(), None);
}

#[test]
fn a_scanout_naming_no_size_and_no_sink_is_no_output() {
    let rig = Rig::new();
    rig.model().scanouts = vec![(0, 0, true), (800, 600, false)];
    let (mut gpu, _) = rig.open();
    assert_eq!(gpu.output(), None);
    assert!(
        rig.console.0.borrow().adopted.is_empty(),
        "nothing to host it at"
    );
    assert_eq!(gpu.alloc_buffer(64, 64).err(), Some(DriverError::Stale));
}

#[test]
fn a_frame_s_first_present_makes_its_resource_and_transfers_it_whole() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let console = rig.scanned().expect("the console");
    let frame = gpu.alloc_buffer(1280, 800).expect("a frame");
    assert_eq!(frame.stride_bytes, 5120);
    let before = rig.model.borrow().transfers.len();
    assert_eq!(
        gpu.present(frame.id, &[rect(10, 20, 30, 40)]),
        Ok(Presented::Shown)
    );
    let shown = rig.scanned().expect("the frame");
    assert_ne!(shown, console);
    let model = rig.model.borrow();
    assert_eq!(
        model.resources.get(&shown),
        Some(&Resource {
            format: 2,
            width: 1280,
            height: 800,
            backing: Some((HeapMemory::device_addr_of(frame.region), 5120 * 800)),
        })
    );
    assert_eq!(
        model.transfers[before..],
        [(shown, whole(1280, 800), 0)],
        "its resource holds nothing yet, whatever the damage"
    );
    assert_eq!(model.flushes.last(), Some(&(shown, whole(1280, 800))));
    assert!(
        model.resources.contains_key(&console),
        "the console stays hosted"
    );
}

#[test]
fn a_later_present_moves_only_its_damage_in_one_batch() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    let (frame, shown) = present_frame(&mut gpu, &rig);
    let (transfers, flushes) = {
        let model = rig.model.borrow();
        (model.transfers.len(), model.flushes.len())
    };
    rig.model().commands.clear();
    let kicks = transport.borrow().notify_log.borrow().len();
    let damage = [rect(8, 2, 16, 4), rect(100, 700, 50, 100)];
    assert_eq!(gpu.present(frame.id, &damage), Ok(Presented::Shown));
    let model = rig.model.borrow();
    assert_eq!(
        model.transfers[transfers..],
        [
            (shown, damage[0], (2 * 1280 + 8) * 4),
            (shown, damage[1], (700 * 1280 + 100) * 4),
        ]
    );
    assert_eq!(
        model.flushes[flushes..],
        [(shown, damage[0]), (shown, damage[1])]
    );
    assert!(
        !model.commands.contains(&SET_SCANOUT),
        "it is already shown"
    );
    assert_eq!(
        transport.borrow().notify_log.borrow().len(),
        kicks + 1,
        "one notify for the whole present"
    );
}

#[test]
fn damage_the_output_does_not_hold_is_refused_before_the_device_hears_of_it() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let frame = gpu.alloc_buffer(1280, 800).expect("a frame");
    rig.model().commands.clear();
    assert_eq!(
        gpu.present(frame.id, &[rect(1200, 0, 100, 1)]),
        Err(DriverError::LengthOutOfRange)
    );
    assert!(rig.model.borrow().commands.is_empty());
    let pointer = gpu.alloc_buffer(64, 64).expect("a pointer image");
    assert_eq!(
        gpu.present(pointer.id, &[rect(0, 0, 1, 1)]),
        Err(DriverError::Stale),
        "not the output's size"
    );
}

#[test]
fn a_refused_first_present_unmakes_its_resource_and_the_next_makes_it_again() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let frame = gpu.alloc_buffer(1280, 800).expect("a frame");
    let held = rig.model.borrow().resources.len();
    rig.model().refuse = Some((CREATE_2D, ERR_OUT_OF_MEMORY));
    assert_eq!(
        gpu.present(frame.id, &[whole(1280, 800)]),
        Err(DriverError::OutOfMemory)
    );
    assert_eq!(rig.model.borrow().resources.len(), held);
    assert_eq!(
        gpu.present(frame.id, &[whole(1280, 800)]),
        Ok(Presented::Shown)
    );
    assert_eq!(rig.model.borrow().resources.len(), held + 1);
}

#[test]
fn a_shown_frame_given_back_is_shown_until_something_replaces_it() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let (frame, shown) = present_frame(&mut gpu, &rig);
    gpu.free_buffer(frame.id).expect("freed");
    {
        let model = rig.model.borrow();
        assert_eq!(
            model.resources.get(&shown).map(|resource| resource.backing),
            Some(None)
        );
    }
    assert_eq!(rig.scanned(), Some(shown), "still on screen");
    assert_eq!(
        rig.memory.fill(frame.region, &[1]),
        Err(DriverError::NotFound),
        "its memory is given back"
    );
    gpu.released(ReleaseSurface::Text);
    assert_ne!(rig.scanned(), Some(shown), "the console instead");
    assert!(
        !rig.model.borrow().resources.contains_key(&shown),
        "and the frame's resource is gone"
    );
    assert_eq!(gpu.free_buffer(frame.id), Err(DriverError::NotFound));
}

#[test]
fn a_frame_not_shown_is_unmade_when_given_back() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let (frame, shown) = present_frame(&mut gpu, &rig);
    gpu.released(ReleaseSurface::Handover);
    assert_eq!(rig.scanned(), None, "nothing shown");
    gpu.free_buffer(frame.id).expect("freed");
    assert!(!rig.model.borrow().resources.contains_key(&shown));
}

#[test]
fn a_release_hides_the_presenter_s_pointer() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let image = gpu.alloc_buffer(16, 16).expect("a pointer image");
    gpu.set_cursor(image.id, 1, 1).expect("shown");
    gpu.released(ReleaseSurface::Handover);
    let last = rig
        .model
        .borrow()
        .cursor
        .last()
        .copied()
        .expect("a cursor command");
    assert_eq!((last.kind, last.resource), (UPDATE_CURSOR, 0));
}

#[test]
fn switching_off_shows_nothing_and_switching_on_shows_the_last_frame() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let (frame, shown) = present_frame(&mut gpu, &rig);
    gpu.set_power(DisplayPower::Off).expect("off");
    assert_eq!(rig.scanned(), None);
    rig.model().commands.clear();
    let transfers = rig.model.borrow().transfers.len();
    assert_eq!(
        gpu.present(frame.id, &[rect(0, 0, 4, 4)]),
        Ok(Presented::Shown),
        "a present is still taken"
    );
    {
        let model = rig.model.borrow();
        assert_eq!(model.transfers.len(), transfers + 1);
        assert!(!model.commands.contains(&SET_SCANOUT));
    }
    gpu.set_power(DisplayPower::On).expect("on");
    assert_eq!(rig.scanned(), Some(shown));
    assert_eq!(
        rig.model.borrow().flushes.last(),
        Some(&(shown, whole(1280, 800)))
    );
    assert_eq!(gpu.set_power(DisplayPower::On), Ok(()), "already on");
}

#[test]
fn a_display_event_moves_the_output_and_the_console_follows_it() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    let console = rig.scanned().expect("the console");
    rig.model().scanouts = vec![(1920, 1080, true)];
    raise_display_event(&transport);
    assert_eq!(gpu.service_events(), Ok(Serviced::default()));
    assert_eq!(
        gpu.output().map(|mode| (mode.width_px, mode.height_px)),
        Some((1920, 1080))
    );
    {
        let state = rig.console.0.borrow();
        assert_eq!(state.adopted, vec![(1280, 800), (1920, 1080)]);
        assert_eq!(state.relinquished, 1);
    }
    let shown = rig.scanned().expect("the console again");
    assert_eq!(
        rig.records.all().last(),
        Some(&Record::ConsoleShown {
            width: 1920,
            height: 1080
        })
    );
    let model = rig.model.borrow();
    assert!(
        !model.resources.contains_key(&console),
        "the old console is unmade"
    );
    assert_eq!(
        model
            .resources
            .get(&shown)
            .map(|resource| (resource.width, resource.height)),
        Some((1920, 1080))
    );
    let mut events = [0u8; 4];
    transport
        .borrow()
        .read_config(wire::CONFIG_EVENTS_READ, &mut events);
    assert_eq!(events, [0; 4], "the event is cleared");
}

/// A display event whose output read goes unanswered is not lost with it:
/// the read is owed, and the next service makes it once the device answers.
#[test]
fn a_display_event_whose_read_failed_is_read_at_the_next_service() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    rig.model().scanouts = vec![(1920, 1080, true)];
    raise_display_event(&transport);
    rig.model().fall_silent_at = Some(GET_DISPLAY_INFO);
    assert!(gpu.service_events().is_err(), "the read went unanswered");
    rig.model().mute = false;
    transport
        .borrow_mut()
        .drain_queue(wire::CONTROL_QUEUE)
        .expect("answered late");
    assert_eq!(gpu.service_events(), Ok(Serviced::default()));
    assert_eq!(
        gpu.output().map(|mode| (mode.width_px, mode.height_px)),
        Some((1920, 1080)),
        "read once the device answered, with no second event"
    );
}

/// Every command waits on the interrupt a display event shares, so the
/// hosting loop owes the driver a service after each request it serves.
#[test]
fn a_request_can_take_a_display_event_s_wake() {
    let rig = Rig::new();
    let (gpu, _) = rig.open();
    assert!(gpu.requests_take_wakes());
}

#[test]
fn an_output_that_leaves_takes_its_scanout_dark() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    rig.model().scanouts = vec![(1280, 800, false)];
    raise_display_event(&transport);
    gpu.service_events().expect("serviced");
    assert_eq!(gpu.output(), None);
    assert_eq!(rig.scanned(), None);
}

#[test]
fn rows_the_console_paints_are_moved_only_while_it_is_shown() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let console = rig.scanned().expect("the console");
    let rows = rect(0, 16, 1280, 32);
    rig.console.0.borrow_mut().damage = Some(rows);
    gpu.service_events().expect("serviced");
    assert_eq!(
        rig.model.borrow().transfers.last(),
        Some(&(console, rows, 16 * 1280 * 4))
    );
    assert_eq!(rig.model.borrow().flushes.last(), Some(&(console, rows)));

    present_frame(&mut gpu, &rig);
    let transfers = rig.model.borrow().transfers.len();
    rig.console.0.borrow_mut().damage = Some(rows);
    gpu.service_events().expect("serviced");
    assert_eq!(
        rig.model.borrow().transfers.len(),
        transfers,
        "the console is hidden"
    );
    assert!(
        rig.console.0.borrow().damage.is_none(),
        "but its rows are taken"
    );
}

#[test]
fn a_console_that_cannot_be_hosted_leaves_the_screen_dark_and_is_recorded() {
    let rig = Rig::new();
    rig.console.0.borrow_mut().refuse = Some(DriverError::LengthOutOfRange);
    let (mut gpu, _) = rig.open();
    assert_eq!(rig.scanned(), None);
    assert_eq!(
        rig.records.all(),
        vec![Record::ConsoleUnhosted {
            reason: DriverError::LengthOutOfRange
        }]
    );
    gpu.released(ReleaseSurface::Text);
    assert_eq!(rig.scanned(), None, "no console to show");
}

/// A console this driver does not hold owes it no rows, so serving requests
/// asks the kernel for none and records nothing.
#[test]
fn a_console_not_held_is_never_asked_for_rows() {
    let rig = Rig::new();
    rig.model().refuse = Some((CREATE_2D, ERR_OUT_OF_MEMORY));
    let (mut gpu, _) = rig.open();
    let records = rig.records.all().len();
    for _ in 0..3 {
        assert_eq!(gpu.service_events(), Ok(Serviced::default()));
    }
    assert_eq!(rig.records.all().len(), records, "nothing recorded");
}

#[test]
fn a_console_the_device_refused_goes_back_and_is_hosted_once_the_output_moves() {
    let rig = Rig::new();
    rig.model().refuse = Some((CREATE_2D, ERR_OUT_OF_MEMORY));
    let (mut gpu, transport) = rig.open();
    assert_eq!(rig.scanned(), None);
    assert_eq!(
        rig.records.all(),
        vec![Record::ConsoleUnhosted {
            reason: DriverError::OutOfMemory
        }]
    );
    assert_eq!(
        rig.console.0.borrow().relinquished,
        1,
        "the device holds none of it"
    );
    rig.model().scanouts = vec![(1920, 1080, true)];
    raise_display_event(&transport);
    assert_eq!(gpu.service_events(), Ok(Serviced::default()));
    assert_eq!(
        rig.console.0.borrow().adopted,
        vec![(1280, 800), (1920, 1080)]
    );
    assert!(rig.scanned().is_some(), "the seat still shows text");
    assert_eq!(
        rig.records.all().last(),
        Some(&Record::ConsoleShown {
            width: 1920,
            height: 1080
        })
    );
}

#[test]
fn a_console_whose_making_went_unanswered_keeps_its_memory_until_the_device_answers() {
    let rig = Rig::new();
    rig.model().fall_silent_at = Some(CREATE_2D);
    let (mut gpu, transport) = rig.open();
    assert!(matches!(
        rig.records.all()[..],
        [Record::ConsoleUnhosted { .. }]
    ));
    assert_eq!(
        rig.console.0.borrow().relinquished,
        0,
        "the device may hold it"
    );
    rig.model().mute = false;
    transport
        .borrow_mut()
        .drain_queue(wire::CONTROL_QUEUE)
        .expect("answered late");
    let made = *rig
        .model
        .borrow()
        .resources
        .keys()
        .next()
        .expect("the late batch made it");
    rig.model().scanouts = vec![(1920, 1080, true)];
    raise_display_event(&transport);
    assert_eq!(gpu.service_events(), Ok(Serviced::default()));
    {
        let state = rig.console.0.borrow();
        assert_eq!(state.relinquished, 1);
        assert_eq!(state.adopted, vec![(1280, 800), (1920, 1080)]);
    }
    assert!(
        !rig.model.borrow().resources.contains_key(&made),
        "unmade before its memory went"
    );
    assert!(rig.scanned().is_some_and(|shown| shown != made));
}

#[test]
fn the_pointer_is_copied_into_the_driver_s_own_image_and_shown_on_the_cursor_queue() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let image = gpu.alloc_buffer(2, 2).expect("a pointer image");
    let pixels: Vec<u8> = (1..=16).collect();
    rig.memory.fill(image.region, &pixels).expect("drawn");
    gpu.move_cursor(-5, 7).expect("moved");
    assert!(
        rig.model.borrow().cursor.is_empty(),
        "nothing shown to move"
    );
    gpu.set_cursor(image.id, 1, 0).expect("shown");
    let own = gpu.pointer.image.as_bytes();
    assert_eq!(&own[..8], &pixels[..8]);
    assert_eq!(&own[CURSOR_STRIDE..CURSOR_STRIDE + 8], &pixels[8..]);
    assert!(own[8..CURSOR_STRIDE].iter().all(|&byte| byte == 0));
    let resource = gpu.pointer.resource.resource().expect("its resource");
    let model = rig.model.borrow();
    assert_eq!(
        model.resources.get(&resource),
        Some(&Resource {
            format: 1,
            width: CURSOR_SIDE,
            height: CURSOR_SIDE,
            backing: Some((gpu.pointer.image.device_addr(), CURSOR_BYTES)),
        })
    );
    assert_eq!(
        model.transfers.last(),
        Some(&(resource, whole(CURSOR_SIDE, CURSOR_SIDE), 0))
    );
    assert_eq!(
        model.cursor.last(),
        Some(&Cursor {
            kind: UPDATE_CURSOR,
            scanout: 0,
            at: ((-5i32).cast_unsigned(), 7),
            resource,
            hot: (1, 0),
        })
    );
}

#[test]
fn a_shown_pointer_moves_without_a_present_and_hides_naming_no_image() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let image = gpu.alloc_buffer(8, 8).expect("a pointer image");
    gpu.set_cursor(image.id, 2, 2).expect("shown");
    let flushes = rig.model.borrow().flushes.len();
    gpu.move_cursor(300, 200).expect("moved");
    {
        let model = rig.model.borrow();
        let last = model.cursor.last().copied().expect("a move");
        assert_eq!((last.kind, last.at), (MOVE_CURSOR, (300, 200)));
        assert_eq!(
            (last.resource, last.hot),
            (gpu.pointer.resource.resource().expect("shown"), (2, 2)),
            "a move names the pointer it moves, which a device reads as shown"
        );
        assert_eq!(model.flushes.len(), flushes, "nothing presented");
    }
    gpu.hide_cursor().expect("hidden");
    let last = rig.model.borrow().cursor.last().copied().expect("a hide");
    assert_eq!((last.kind, last.resource), (UPDATE_CURSOR, 0));
    assert_eq!(
        gpu.set_cursor(image.id, 8, 0),
        Err(DriverError::LengthOutOfRange),
        "a hot spot outside the image"
    );
    let large = gpu.alloc_buffer(65, 1).expect("an image too wide");
    assert_eq!(
        gpu.set_cursor(large.id, 0, 0),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn a_buffer_given_back_to_a_device_that_never_answers_is_held_until_it_does() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    let (frame, shown) = present_frame(&mut gpu, &rig);
    gpu.released(ReleaseSurface::Handover);
    rig.model().mute = true;
    assert_eq!(gpu.free_buffer(frame.id), Ok(()), "the presenter's no more");
    assert_eq!(
        rig.memory.fill(frame.region, &[1]),
        Ok(()),
        "but the device may still read it, so it is held"
    );
    assert_eq!(gpu.free_buffer(frame.id), Err(DriverError::NotFound));
    assert_eq!(
        gpu.present(frame.id, &[rect(0, 0, 1, 1)]),
        Err(DriverError::NotFound),
        "nor presented"
    );
    rig.model().mute = false;
    transport
        .borrow_mut()
        .drain_queue(wire::CONTROL_QUEUE)
        .expect("answered late");
    assert_eq!(gpu.service_events(), Ok(Serviced::default()));
    assert_eq!(
        rig.memory.fill(frame.region, &[1]),
        Err(DriverError::NotFound),
        "let go once the device answers, with no second free"
    );
    assert!(!rig.model.borrow().resources.contains_key(&shown));
}

#[test]
fn an_unanswered_first_present_leaves_a_resource_detached_before_its_buffer_goes() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    let frame = gpu.alloc_buffer(1280, 800).expect("a frame");
    let held = rig.model.borrow().resources.len();
    rig.model().mute = true;
    assert!(gpu.present(frame.id, &[whole(1280, 800)]).is_err());
    rig.model().mute = false;
    transport
        .borrow_mut()
        .drain_queue(wire::CONTROL_QUEUE)
        .expect("answered late");
    let made = {
        let model = rig.model.borrow();
        assert_eq!(model.resources.len(), held + 1, "the late batch made it");
        *model.resources.keys().last().expect("a resource")
    };
    gpu.free_buffer(frame.id).expect("given back");
    let model = rig.model.borrow();
    assert!(
        !model.resources.contains_key(&made),
        "detached and unreferenced before its memory went"
    );
    assert_eq!(
        rig.memory.fill(frame.region, &[1]),
        Err(DriverError::NotFound)
    );
}

/// A first present the device answers late may have unreferenced the frame
/// it replaced; the next present finds that frame gone and carries on, rather
/// than refusing every present after it for the unreference it repeats.
#[test]
fn a_frame_a_late_batch_already_retired_does_not_fail_the_presents_after_it() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    let (old, retired) = present_frame(&mut gpu, &rig);
    gpu.free_buffer(old.id).expect("given back while shown");
    let frame = gpu.alloc_buffer(1280, 800).expect("a frame");
    rig.model().fall_silent_at = Some(CREATE_2D);
    assert!(gpu.present(frame.id, &[whole(1280, 800)]).is_err());
    rig.model().mute = false;
    transport
        .borrow_mut()
        .drain_queue(wire::CONTROL_QUEUE)
        .expect("answered late");
    assert!(
        !rig.model.borrow().resources.contains_key(&retired),
        "the late batch retired the old frame"
    );

    assert_eq!(
        gpu.present(frame.id, &[whole(1280, 800)]),
        Ok(Presented::Shown)
    );
    let shown = rig.scanned().expect("the new frame");
    assert!(rig.model.borrow().resources.contains_key(&shown));
    assert_eq!(
        gpu.present(frame.id, &[rect(0, 0, 8, 8)]),
        Ok(Presented::Shown),
        "and the next"
    );
}

#[test]
fn a_doubtful_resource_is_unreferenced_before_its_buffer_is_presented_again() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    let frame = gpu.alloc_buffer(1280, 800).expect("a frame");
    rig.model().mute = true;
    assert!(gpu.present(frame.id, &[whole(1280, 800)]).is_err());
    rig.model().mute = false;
    transport
        .borrow_mut()
        .drain_queue(wire::CONTROL_QUEUE)
        .expect("answered late");
    let doubtful = *rig
        .model
        .borrow()
        .resources
        .keys()
        .last()
        .expect("a resource");
    assert_eq!(
        gpu.present(frame.id, &[rect(0, 0, 1, 1)]),
        Ok(Presented::Shown)
    );
    let shown = rig.scanned().expect("the frame");
    assert_ne!(shown, doubtful, "made afresh");
    assert!(!rig.model.borrow().resources.contains_key(&doubtful));
}

#[test]
fn a_pointer_image_the_device_may_still_read_is_not_drawn_over() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    let first = gpu.alloc_buffer(1, 1).expect("an image");
    let second = gpu.alloc_buffer(1, 1).expect("another");
    rig.memory.fill(first.region, &[1, 2, 3, 4]).expect("drawn");
    rig.memory
        .fill(second.region, &[9, 9, 9, 9])
        .expect("drawn");
    gpu.set_cursor(first.id, 0, 0).expect("shown");
    rig.model().mute = true;
    assert!(
        gpu.set_cursor(first.id, 0, 0).is_err(),
        "its transfer unanswered"
    );
    assert_eq!(
        gpu.set_cursor(second.id, 0, 0),
        Err(DriverError::DeviceOffline)
    );
    assert_eq!(&gpu.pointer.image.as_bytes()[..4], &[1, 2, 3, 4]);
}

#[test]
fn a_pointer_resource_an_unanswered_batch_may_have_made_is_unmade_before_another() {
    let rig = Rig::new();
    let (mut gpu, transport) = rig.open();
    let image = gpu.alloc_buffer(8, 8).expect("a pointer image");
    rig.model().fall_silent_at = Some(CREATE_2D);
    assert!(gpu.set_cursor(image.id, 0, 0).is_err());
    rig.model().mute = false;
    transport
        .borrow_mut()
        .drain_queue(wire::CONTROL_QUEUE)
        .expect("answered late");
    let doubtful = *rig
        .model
        .borrow()
        .resources
        .keys()
        .last()
        .expect("the late batch made it");
    gpu.set_cursor(image.id, 0, 0).expect("shown");
    let shown = rig
        .model
        .borrow()
        .cursor
        .last()
        .copied()
        .expect("a cursor command");
    assert_eq!(shown.kind, UPDATE_CURSOR);
    assert_ne!(shown.resource, doubtful, "made afresh");
    assert!(!rig.model.borrow().resources.contains_key(&doubtful));
}

#[test]
fn one_presenter_holds_no_more_than_the_frames_it_could_rotate_through() {
    let rig = Rig::new();
    let (mut gpu, _) = rig.open();
    for _ in 0..tairix_abi::display_ipc::DISPLAY_MAX_FRAMES {
        gpu.alloc_buffer(1280, 800).expect("within the budget");
    }
    assert_eq!(
        gpu.alloc_buffer(1280, 800).err(),
        Some(DriverError::NoSpace)
    );
    assert_eq!(
        gpu.alloc_buffer(1281, 800).err(),
        Some(DriverError::LengthOutOfRange)
    );
}

#[test]
fn dropping_the_engine_resets_the_device_before_its_memory_goes() {
    let rig = Rig::new();
    let (gpu, transport) = rig.open();
    drop(gpu);
    assert_eq!(transport.borrow().status().bits(), 0, "reset");
}
