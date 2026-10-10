//! The engine: the controller and every codec on its link, served through
//! the [`Audio`] class trait.
//!
//! Each endpoint's stream runs over a cyclic buffer of [`PERIODS`] periods,
//! one buffer descriptor each, interrupting at every period's end. The
//! period after the one playing is always written: as silence counted lost
//! when the mixer has not supplied it, because a period left alone would
//! replay a lap-old sound. Positions come from the DMA position buffer the
//! controller writes, so a period's service reads memory rather than a
//! register.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{
    ring_bounds, Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioName,
    AudioServiced, ChannelMap, Frames, GainRange, JackState, RateSupport, SampleFormat,
    SampleFormats, StreamDirection, MAX_DEVICE_ENDPOINTS,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::driver::dma::{DmaHost, DmaSlab};
use tairix_abi::time::{MonotonicClock, Time64};
use tairix_abi::{Delay, DriverError};

use crate::codec::{Function, Verbs, Widget};
use crate::controller::{Controller, Flow, StreamSetup, Wait, DMA_ALIGN};
use crate::format::{frame_bytes, stream_format, PcmSupport};
use crate::plan::{Plan, Route};
use crate::regs::{sd, Registers};
use crate::verb::{
    amp, pin_control, pin_sense, AmpCaps, Device, Verb, WidgetKind, EXECUTE_PIN_SENSE,
    GET_DIP_SIZE, GET_ELD_DATA, GET_PIN_SENSE, POWER_D0, SET_AMP, SET_CHANNEL_COUNT,
    SET_CONNECTION_SELECT, SET_DIGITAL_CONTROL, SET_DIP_DATA, SET_DIP_INDEX, SET_DIP_TRANSMIT,
    SET_EAPD, SET_FORMAT, SET_PIN_CONTROL, SET_POWER_STATE, SET_STREAM_CHANNEL, SET_UNSOLICITED,
};

/// Periods a stream's buffer holds.
pub const PERIODS: u32 = 4;

/// The step a period's frames are rounded to: with every frame an even
/// number of bytes, a period is then a whole number of the 128-byte pieces
/// the controller's buffers are made of.
pub const PERIOD_STEP: u32 = 64;

/// Most frames a period may hold: four periods of eight 32-bit channels then
/// take a mebibyte.
pub const MAX_PERIOD_FRAMES: u32 = 8_192;

/// Bytes per buffer descriptor list entry.
const BDL_ENTRY: usize = 16;

/// A buffer descriptor's interrupt-on-completion flag.
const BDL_IOC: u32 = 1;

/// The highest stream number a descriptor carries.
const MAX_TAG: u8 = 15;

/// `SET_UNSOLICITED`: enabled, with the tag in the low six bits.
const UNSOLICITED_ENABLE: u8 = 1 << 7;

/// `SET_EAPD`: the external amplifier is powered.
const EAPD_ON: u8 = 1 << 1;

/// `SET_DIGITAL_CONTROL`: the converter's digital output is on.
const DIGITAL_ENABLE: u8 = 1;

/// `GET_DIP_SIZE`'s request for the ELD buffer's size.
const ELD_SIZE: u8 = 0x08;

/// Bytes of an ELD read: the header, the baseline block's fixed part, and
/// the longest monitor name.
const ELD_READ: u8 = 36;

/// `SET_DIP_TRANSMIT`: send the infoframe best effort, always.
const DIP_ALWAYS: u8 = 0xC0;

/// An amplifier on a route, at its place.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Amp {
    nid: u8,
    output: bool,
    index: u8,
    caps: AmpCaps,
}

impl Amp {
    /// The `SET_AMP` verb setting both channels to `step`, muted or not.
    fn verb(self, step: u8, muted: bool) -> Verb {
        let side = if self.output { amp::OUTPUT } else { amp::INPUT };
        let mut payload =
            side | amp::LEFT | amp::RIGHT | (u16::from(self.index) << amp::INDEX_SHIFT);
        payload |= u16::from(step & 0x7F);
        if muted {
            payload |= amp::MUTE;
        }
        Verb::long(self.nid, SET_AMP, payload)
    }
}

/// What an endpoint is.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Kind {
    /// Playback: its converters' routes in channel order, and further pins
    /// sharing the front pair.
    Output {
        lanes: Vec<Route>,
        mirrors: Vec<Route>,
        digital: bool,
        display: bool,
    },
    /// Capture: from its converter back to its pin.
    Input { route: Route },
}

/// A configured stream.
struct Stream {
    descriptor: u8,
    tag: u8,
    format: SampleFormat,
    rate_hz: u32,
    frame_bytes: u32,
    period_frames: u32,
    buffer: DmaSlab,
    bdl: DmaSlab,
    running: bool,
    draining: bool,
    /// Periods written into the buffer, or taken out of it, since it was
    /// last set up.
    periods: u64,
    /// Bytes the DMA has moved since the buffer was last set up.
    moved: u64,
    /// The position buffer's last reading.
    last_position: u32,
    /// Where the last frame the mixer supplied lies: its period, and the
    /// frames of that period that are the mixer's.
    supplied_through: Option<(u64, u32)>,
    /// The stream position of the first frame moved since the start.
    base: u64,
    xrun_frames: u64,
    /// A drain's end: where it fell silent, and when.
    drained_at: Option<(u64, Time64)>,
}

impl Stream {
    const fn period_bytes(&self) -> u64 {
        self.period_frames as u64 * self.frame_bytes as u64
    }

    const fn buffer_bytes(&self) -> u64 {
        self.period_bytes() * PERIODS as u64
    }

    /// Account the DMA's progress to `position` within the buffer.
    fn advance(&mut self, position: u32) {
        let length = self.buffer_bytes();
        let position = u64::from(position).min(length);
        let last = u64::from(self.last_position);
        self.moved += if position >= last {
            position - last
        } else {
            length - last + position
        };
        self.last_position = u32::try_from(position).unwrap_or(0);
    }

    /// Periods the DMA has finished.
    const fn finished(&self) -> u64 {
        self.moved / self.period_bytes()
    }

    /// Forget the progress through the buffer, as the stream stops.
    fn rewind(&mut self, at: u64) {
        self.running = false;
        self.draining = false;
        self.periods = 0;
        self.moved = 0;
        self.last_position = 0;
        self.supplied_through = None;
        self.base = at;
        self.buffer.as_bytes_mut().fill(0);
        self.buffer.sync_range(0, self.buffer.len());
    }
}

/// One sink or source.
struct Endpoint {
    function: usize,
    kind: Kind,
    name: AudioName,
    channel_map: ChannelMap,
    pcm: PcmSupport,
    /// One gain amplifier per lane, at the same place on each.
    gain: Vec<Amp>,
    /// The amplifiers that mute it; none means its pins are switched off.
    mute: Vec<Amp>,
    jack: JackState,
    tag: u8,
    stream: Option<Stream>,
}

impl Endpoint {
    const fn flow(&self) -> Flow {
        match self.kind {
            Kind::Output { .. } => Flow::Out,
            Kind::Input { .. } => Flow::In,
        }
    }

    /// Every route the endpoint takes.
    fn routes(&self) -> impl Iterator<Item = &Route> {
        let (lanes, mirrors): (&[Route], &[Route]) = match &self.kind {
            Kind::Output { lanes, mirrors, .. } => (lanes, mirrors),
            Kind::Input { route } => (core::slice::from_ref(route), &[]),
        };
        lanes.iter().chain(mirrors)
    }

    /// Its pins.
    fn pins(&self) -> impl Iterator<Item = u8> + '_ {
        self.routes().map(|route| match self.kind {
            Kind::Output { .. } => route.downstream(),
            Kind::Input { .. } => route.upstream(),
        })
    }

    /// Its converters: one per lane, or the one it captures through.
    fn converters(&self) -> impl Iterator<Item = u8> + '_ {
        let routes: &[Route] = match &self.kind {
            Kind::Output { lanes, .. } => lanes,
            Kind::Input { route } => core::slice::from_ref(route),
        };
        routes.iter().map(|route| match self.kind {
            Kind::Output { .. } => route.upstream(),
            Kind::Input { .. } => route.downstream(),
        })
    }

    /// The pin whose connector is the endpoint's jack.
    fn front_pin(&self) -> u8 {
        self.pins().next().unwrap_or(0)
    }

    fn formats(&self) -> SampleFormats {
        self.pcm.formats()
    }
}

/// An HDA controller and its codecs.
pub struct Hda<'h, R: Registers, W: Wait, D: Delay> {
    controller: Controller<R, W, D>,
    dma: &'h dyn DmaHost,
    clock: &'h dyn MonotonicClock,
    functions: Vec<Function>,
    endpoints: Vec<Endpoint>,
    name: AudioName,
    events: bool,
}

impl<'h, R: Registers, W: Wait, D: Delay> Hda<'h, R, W, D> {
    /// Reset the controller behind `regs`, walk every codec on its link, and
    /// set each endpoint's routes up.
    ///
    /// A codec that does not answer is passed over; the others serve.
    ///
    /// # Errors
    ///
    /// The controller's failure to come up, [`DriverError::NotFound`] for a
    /// link with no endpoint at all, or [`DriverError::OutOfMemory`].
    pub fn open(
        regs: R,
        wait: W,
        delay: D,
        dma: &'h dyn DmaHost,
        clock: &'h dyn MonotonicClock,
    ) -> Result<Self, DriverError> {
        let mut controller = Controller::open(regs, wait, delay, dma)?;
        let mut functions = Vec::new();
        let mut endpoints: Vec<Endpoint> = Vec::new();
        let codecs: Vec<u8> = controller.codecs().collect();
        for address in codecs {
            let Ok(found) = Function::read_all(&mut controller, address) else {
                continue;
            };
            for function in found {
                let plan = Plan::of(&function)?;
                let index = functions.len();
                for endpoint in endpoints_of(&function, index, &plan) {
                    if endpoints.len() < usize::from(MAX_DEVICE_ENDPOINTS) {
                        endpoints
                            .try_reserve(1)
                            .map_err(|_| DriverError::OutOfMemory)?;
                        endpoints.push(endpoint);
                    }
                }
                functions
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
                functions.push(function);
            }
        }
        if endpoints.is_empty() {
            return Err(DriverError::NotFound);
        }
        // One tag per endpoint across every codec and function: a response
        // names its codec, but two functions of one codec share its tags.
        for (endpoint, tag) in endpoints.iter_mut().zip(1u8..) {
            endpoint.tag = tag;
        }
        let name = device_name(functions.first().map_or(0, |function| function.vendor))?;
        let mut hda = Self {
            controller,
            dma,
            clock,
            functions,
            endpoints,
            name,
            events: false,
        };
        for function in 0..hda.functions.len() {
            hda.power_up(function)?;
        }
        for index in 0..hda.endpoints.len() {
            hda.prepare(index)?;
        }
        Ok(hda)
    }

    /// Send `verb` to the codec of function `function`.
    fn send(&mut self, function: usize, verb: Verb) -> Result<u32, DriverError> {
        let address = self
            .functions
            .get(function)
            .ok_or(DriverError::DeviceFault)?
            .address;
        self.controller.exchange(address, verb)
    }

    fn widget(&self, function: usize, nid: u8) -> Result<&Widget, DriverError> {
        self.functions
            .get(function)
            .and_then(|graph| graph.widget(nid))
            .ok_or(DriverError::DeviceFault)
    }

    /// Bring the function and each widget that has power control to D0.
    fn power_up(&mut self, function: usize) -> Result<(), DriverError> {
        let group = self.functions[function].nid;
        self.send(function, Verb::short(group, SET_POWER_STATE, POWER_D0))?;
        let powered: Vec<u8> = self.functions[function]
            .widgets()
            .iter()
            .filter(|widget| widget.caps.power_control())
            .map(|widget| widget.nid)
            .collect();
        for nid in powered {
            self.send(function, Verb::short(nid, SET_POWER_STATE, POWER_D0))?;
        }
        Ok(())
    }

    /// Set endpoint `index`'s routes up: each selector on the route, every
    /// amplifier at 0 dB and open, its pins driven, its jack watched.
    fn prepare(&mut self, index: usize) -> Result<(), DriverError> {
        let function = self.endpoints[index].function;
        let input = matches!(self.endpoints[index].kind, Kind::Input { .. });
        let routes: Vec<Route> = self.endpoints[index].routes().cloned().collect();
        for route in &routes {
            self.open_route(function, route, !input)?;
        }
        let tag = self.endpoints[index].tag;
        let pins: Vec<u8> = self.endpoints[index].pins().collect();
        for pin in pins {
            let widget = self.widget(function, pin)?.clone();
            let control = if input {
                let bias = widget.config.device() == Device::Microphone && widget.pin.vref_80();
                pin_control::IN | if bias { pin_control::VREF_80 } else { 0 }
            } else {
                let headphones =
                    widget.config.device() == Device::Headphones && widget.pin.headphone_drive();
                pin_control::OUT
                    | if headphones {
                        pin_control::HEADPHONE
                    } else {
                        0
                    }
            };
            self.send(function, Verb::short(pin, SET_PIN_CONTROL, control))?;
            if !input && widget.pin.eapd() {
                self.send(function, Verb::short(pin, SET_EAPD, EAPD_ON))?;
            }
            if watched(&widget) {
                self.send(
                    function,
                    Verb::short(pin, SET_UNSOLICITED, UNSOLICITED_ENABLE | tag),
                )?;
            }
        }
        self.sense(index)?;
        Ok(())
    }

    /// Choose each next hop along `route` and open every amplifier on it at
    /// 0 dB; a mixer's other inputs are shut, so nothing else is summed in.
    fn open_route(
        &mut self,
        function: usize,
        route: &Route,
        output: bool,
    ) -> Result<(), DriverError> {
        for hop in route.hops() {
            let widget = self.widget(function, hop.nid)?.clone();
            let Some(select) = hop.select else { continue };
            let chooses = matches!(
                widget.kind(),
                WidgetKind::Selector | WidgetKind::Pin | WidgetKind::Input
            );
            if chooses && widget.sources.len() > 1 {
                self.send(
                    function,
                    Verb::short(hop.nid, SET_CONNECTION_SELECT, select),
                )?;
            }
            if widget.kind() == WidgetKind::Mixer && widget.caps.input_amp() {
                for input in 0..u8::try_from(widget.sources.len()).unwrap_or(u8::MAX) {
                    if input != select {
                        let shut = Amp {
                            nid: hop.nid,
                            output: false,
                            index: input,
                            caps: widget.input_amp,
                        };
                        self.send(function, shut.verb(0, true))?;
                    }
                }
            }
        }
        let amps = route_amps(&self.functions[function], route, output);
        for amp in amps {
            self.send(function, amp.verb(amp.caps.unity(), false))?;
        }
        Ok(())
    }

    /// Read endpoint `index`'s jack, rename a display after its monitor, and
    /// switch off speakers its own headphones replace. Answers whether the
    /// jack changed.
    fn sense(&mut self, index: usize) -> Result<bool, DriverError> {
        let function = self.endpoints[index].function;
        let front = self.endpoints[index].front_pin();
        let widget = self.widget(function, front)?.clone();
        let display = matches!(
            self.endpoints[index].kind,
            Kind::Output { display: true, .. }
        );
        let jack = if widget.config.fixed() {
            JackState::Present
        } else if watched(&widget) {
            let sense = self.pin_sense(function, &widget)?;
            let present =
                sense & pin_sense::PRESENT != 0 && (!display || sense & pin_sense::ELD_VALID != 0);
            if present {
                JackState::Present
            } else {
                JackState::Absent
            }
        } else {
            JackState::Unknown
        };
        if display {
            // A monitor's name is a courtesy: a codec that will not read its
            // ELD leaves the connector's own name.
            let name = match jack {
                JackState::Present => self.monitor_name(function, &widget).ok().flatten(),
                _ => None,
            };
            self.endpoints[index].name = match name {
                Some(name) => name,
                None => crate::plan::name(&widget)?,
            };
        }
        let changed = self.endpoints[index].jack != jack;
        self.endpoints[index].jack = jack;
        if !matches!(self.endpoints[index].kind, Kind::Input { .. }) {
            self.mute_replaced_speakers(index)?;
        }
        Ok(changed)
    }

    fn pin_sense(&mut self, function: usize, pin: &Widget) -> Result<u32, DriverError> {
        if pin.pin.trigger_required() {
            self.send(function, Verb::short(pin.nid, EXECUTE_PIN_SENSE, 0))?;
        }
        self.send(function, Verb::short(pin.nid, GET_PIN_SENSE, 0))
    }

    /// Plugged headphones take over from the speakers that share their
    /// converter, as every desktop does.
    fn mute_replaced_speakers(&mut self, index: usize) -> Result<(), DriverError> {
        let function = self.endpoints[index].function;
        let pins: Vec<u8> = self.endpoints[index].pins().collect();
        let mut headphones = false;
        for &pin in &pins {
            let widget = self.widget(function, pin)?.clone();
            if widget.config.device() == Device::Headphones
                && watched(&widget)
                && self.pin_sense(function, &widget)? & pin_sense::PRESENT != 0
            {
                headphones = true;
            }
        }
        for pin in pins {
            if self.widget(function, pin)?.config.device() == Device::Speaker {
                let control = if headphones { 0 } else { pin_control::OUT };
                self.send(function, Verb::short(pin, SET_PIN_CONTROL, control))?;
            }
        }
        Ok(())
    }

    /// The monitor's name from the display pin's ELD, where it states one.
    fn monitor_name(
        &mut self,
        function: usize,
        pin: &Widget,
    ) -> Result<Option<AudioName>, DriverError> {
        let size = self.send(function, Verb::short(pin.nid, GET_DIP_SIZE, ELD_SIZE))? & 0xFF;
        let length = u8::try_from(size).unwrap_or(0).min(ELD_READ);
        let mut eld = [0u8; ELD_READ as usize];
        for at in 0..length {
            let answer = self.send(function, Verb::short(pin.nid, GET_ELD_DATA, at))?;
            if answer & (1 << 31) == 0 {
                return Ok(None);
            }
            eld[usize::from(at)] = (answer & 0xFF) as u8;
        }
        Ok(monitor(&eld[..usize::from(length)], pin.pin.display_port()))
    }

    fn endpoint(&self, endpoint: u16) -> Result<&Endpoint, DriverError> {
        self.endpoints
            .get(usize::from(endpoint))
            .ok_or(DriverError::NotFound)
    }
}

impl<R: Registers, W: Wait, D: Delay> Hda<'_, R, W, D> {
    fn stream_mut(&mut self, endpoint: u16) -> Result<&mut Stream, DriverError> {
        self.endpoints
            .get_mut(usize::from(endpoint))
            .ok_or(DriverError::NotFound)?
            .stream
            .as_mut()
            .ok_or(DriverError::DeviceFault)
    }

    /// A descriptor carrying `flow` and a stream number no configured stream
    /// of that flow uses.
    fn free_stream(&self, flow: Flow) -> Result<(u8, u8), DriverError> {
        let streams = || {
            self.endpoints.iter().filter_map(|endpoint| {
                endpoint
                    .stream
                    .as_ref()
                    .map(|stream| (endpoint.flow(), stream))
            })
        };
        let descriptor = self
            .controller
            .descriptors(flow)
            .find(|&index| streams().all(|(_, stream)| stream.descriptor != index))
            .ok_or(DriverError::Busy)?;
        let tag = (1..=MAX_TAG)
            .find(|&tag| streams().all(|(other, stream)| other != flow || stream.tag != tag))
            .ok_or(DriverError::Busy)?;
        Ok((descriptor, tag))
    }

    /// A buffer the controller can reach, of `len` bytes.
    fn controller_memory(&self, len: usize) -> Result<DmaSlab, DriverError> {
        let slab = self.dma.alloc_dma_zeroed(len)?;
        let wide = self.controller.capabilities().wide;
        let end = slab
            .device_addr()
            .checked_add(len as u64)
            .ok_or(DriverError::Unsupported)?;
        if slab.device_addr() % DMA_ALIGN != 0 || (!wide && end > 1 << 32) {
            return Err(DriverError::Unsupported);
        }
        Ok(slab)
    }

    /// Point each of endpoint `index`'s converters at stream `tag`, its
    /// pair of channels within it, in `format`; or, with `tag` zero, stop
    /// them listening.
    fn tune_converters(&mut self, index: usize, tag: u8, format: u16) -> Result<(), DriverError> {
        let endpoint = &self.endpoints[index];
        let function = endpoint.function;
        let converters: Vec<u8> = endpoint.converters().collect();
        let (digital, display) = match endpoint.kind {
            Kind::Output {
                digital, display, ..
            } => (digital, display),
            Kind::Input { .. } => (false, false),
        };
        let channels = endpoint.channel_map.channels();
        let pin = endpoint.front_pin();
        for (lane, nid) in (0u8..).zip(converters) {
            if tag != 0 {
                self.send(function, Verb::long(nid, SET_FORMAT, format))?;
                if display {
                    self.send(function, Verb::short(nid, SET_CHANNEL_COUNT, channels - 1))?;
                }
            }
            self.send(
                function,
                Verb::short(nid, SET_STREAM_CHANNEL, (tag << 4) | (lane * 2)),
            )?;
            if digital {
                let control = if tag == 0 { 0 } else { DIGITAL_ENABLE };
                self.send(function, Verb::short(nid, SET_DIGITAL_CONTROL, control))?;
            }
        }
        if display && tag != 0 {
            self.send_infoframe(function, pin, channels)?;
        }
        Ok(())
    }

    /// Describe the audio to a display: its channel count, and no speaker
    /// allocation beyond the front pair it carries.
    fn send_infoframe(
        &mut self,
        function: usize,
        pin: u8,
        channels: u8,
    ) -> Result<(), DriverError> {
        let display_port = self.widget(function, pin)?.pin.display_port();
        let count = channels - 1;
        let frame: &[u8] = if display_port {
            &[0x84, 0x1B, 0x11 << 2, count, 0, 0, 0, 0]
        } else {
            let sum = 0x84u8
                .wrapping_add(0x01)
                .wrapping_add(0x0A)
                .wrapping_add(count);
            &[0x84, 0x01, 0x0A, 0u8.wrapping_sub(sum), count, 0, 0, 0, 0]
        };
        let frame = frame.to_vec();
        self.send(function, Verb::short(pin, SET_DIP_INDEX, 0))?;
        for byte in frame {
            self.send(function, Verb::short(pin, SET_DIP_DATA, byte))?;
        }
        self.send(function, Verb::short(pin, SET_DIP_TRANSMIT, DIP_ALWAYS))?;
        Ok(())
    }

    /// Reset endpoint `index`'s descriptor and program it afresh over its
    /// buffer.
    fn program(&mut self, index: usize, format: u16) -> Result<(), DriverError> {
        let flow = self.endpoints[index].flow();
        let stream = self.endpoints[index]
            .stream
            .as_ref()
            .ok_or(DriverError::DeviceFault)?;
        let setup = StreamSetup {
            flow,
            tag: stream.tag,
            format,
            bdl: stream.bdl.device_addr(),
            length: u32::try_from(stream.buffer_bytes()).map_err(|_| DriverError::OutOfRange)?,
            entries: u8::try_from(PERIODS).map_err(|_| DriverError::OutOfRange)?,
        };
        let descriptor = stream.descriptor;
        self.controller.reset_stream(descriptor)?;
        self.controller.program_stream(descriptor, &setup)?;
        self.controller.stream_interrupt(descriptor, self.events)
    }

    /// Stop endpoint `index`'s descriptor where it is, rewound for a fresh
    /// start from `at`.
    fn halt(&mut self, index: usize, at: u64) -> Result<(), DriverError> {
        let Some(stream) = self.endpoints[index].stream.as_mut() else {
            return Ok(());
        };
        let descriptor = stream.descriptor;
        stream.rewind(at);
        self.controller.run_stream(descriptor, false)?;
        let format = self.format_word(index)?;
        self.program(index, format)
    }

    /// The format word endpoint `index`'s stream runs in.
    fn format_word(&self, index: usize) -> Result<u16, DriverError> {
        let endpoint = &self.endpoints[index];
        let stream = endpoint.stream.as_ref().ok_or(DriverError::DeviceFault)?;
        let rate = stream.rate_hz;
        stream_format(
            endpoint.pcm,
            rate,
            stream.format,
            endpoint.channel_map.channels(),
        )
    }

    /// Take the DMA's progress since the last service.
    fn track(&mut self, index: usize) {
        let Some(descriptor) = self.endpoints[index]
            .stream
            .as_ref()
            .map(|stream| stream.descriptor)
        else {
            return;
        };
        let position = self.controller.position(descriptor);
        if let Some(stream) = self.endpoints[index].stream.as_mut() {
            if stream.running {
                stream.advance(position);
            }
        }
    }

    fn now(&self) -> Time64 {
        Time64::from_nanos(self.clock.now_ns())
    }
}

/// Write the stream's next period: `take` frames from `ring`, silence after
/// them.
fn write_period(
    stream: &mut Stream,
    ring: Option<&mut PcmRing<'_>>,
    take: u32,
) -> Result<u32, DriverError> {
    let period_bytes =
        usize::try_from(stream.period_bytes()).map_err(|_| DriverError::OutOfRange)?;
    let slot = usize::try_from(stream.periods % u64::from(PERIODS))
        .map_err(|_| DriverError::OutOfRange)?;
    let start = slot * period_bytes;
    let frame_bytes = stream.frame_bytes as usize;
    let period = stream
        .buffer
        .as_bytes_mut()
        .get_mut(start..start + period_bytes)
        .ok_or(DriverError::DeviceFault)?;
    let (supplied, silent) = period.split_at_mut((take as usize * frame_bytes).min(period_bytes));
    let taken = match ring {
        Some(ring) if take > 0 => ring.read(supplied).map_err(|_| DriverError::BadMagic)?,
        _ => 0,
    };
    if taken != take {
        return Err(DriverError::BadMagic);
    }
    silent.fill(0);
    stream.buffer.sync_range(start, period_bytes);
    if take > 0 {
        stream.supplied_through = Some((stream.periods, take));
    }
    stream.periods += 1;
    Ok(take)
}

fn readable(ring: &PcmRing<'_>) -> Result<u32, DriverError> {
    ring.readable_frames().map_err(|_| DriverError::BadMagic)
}

/// Before the start: stage whole periods, as many as the buffer holds.
fn stage(stream: &mut Stream, ring: &mut PcmRing<'_>) -> Result<u32, DriverError> {
    let mut transferred = 0;
    while stream.periods < u64::from(PERIODS) && readable(ring)? >= stream.period_frames {
        let period = stream.period_frames;
        transferred += write_period(stream, Some(ring), period)?;
    }
    Ok(transferred)
}

/// While playing: keep the period after the one playing written, and write
/// ahead as far as the ring's whole periods reach.
fn refill(stream: &mut Stream, ring: &mut PcmRing<'_>) -> Result<u32, DriverError> {
    let finished = stream.finished();
    if stream.periods < finished + 1 {
        // The DMA overtook what was written: it has replayed old periods.
        let behind = finished + 1 - stream.periods;
        stream.xrun_frames += behind * u64::from(stream.period_frames);
        stream.periods = finished + 1;
    }
    let mut transferred = 0;
    while stream.periods < finished + u64::from(PERIODS) {
        let available = readable(ring)?;
        let period = stream.period_frames;
        let due = stream.periods < finished + 2;
        if available >= period {
            transferred += write_period(stream, Some(ring), period)?;
        } else if stream.draining {
            // A drain's tail is short, not lost: it is the end.
            transferred += write_period(stream, Some(ring), available)?;
        } else if due {
            transferred += write_period(stream, Some(ring), available)?;
            stream.xrun_frames += u64::from(period - available);
        } else {
            break;
        }
    }
    Ok(transferred)
}

/// While capturing: hand every finished period to `ring`, counting what it
/// had no room for.
fn deliver(stream: &mut Stream, ring: &mut PcmRing<'_>) -> Result<u32, DriverError> {
    let finished = stream.finished();
    if finished > stream.periods + u64::from(PERIODS) - 1 {
        // The DMA lapped what was not yet taken: those periods are gone.
        let lapped = finished - (stream.periods + u64::from(PERIODS) - 1);
        stream.xrun_frames += lapped * u64::from(stream.period_frames);
        stream.periods += lapped;
    }
    let period_bytes =
        usize::try_from(stream.period_bytes()).map_err(|_| DriverError::OutOfRange)?;
    let mut transferred = 0;
    while stream.periods < finished {
        let slot = usize::try_from(stream.periods % u64::from(PERIODS))
            .map_err(|_| DriverError::OutOfRange)?;
        let start = slot * period_bytes;
        stream.buffer.sync_range(start, period_bytes);
        let period = stream
            .buffer
            .as_bytes()
            .get(start..start + period_bytes)
            .ok_or(DriverError::DeviceFault)?;
        let room = ring.writable_frames().map_err(|_| DriverError::BadMagic)?;
        let fits = room.min(stream.period_frames);
        let bytes = fits as usize * stream.frame_bytes as usize;
        let written = ring
            .write(&period[..bytes])
            .map_err(|_| DriverError::BadMagic)?;
        stream.xrun_frames += u64::from(stream.period_frames - written);
        transferred += written;
        stream.periods += 1;
    }
    Ok(transferred)
}

impl<R: Registers, W: Wait, D: Delay> Audio for Hda<'_, R, W, D> {
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        Ok(AudioDeviceFacts {
            endpoints: u16::try_from(self.endpoints.len()).map_err(|_| DriverError::OutOfRange)?,
            name: self.name,
        })
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        let found = self.endpoint(endpoint)?;
        let gain = match found.gain.first() {
            Some(amp) => Some(
                GainRange::new(
                    amp.caps.millibel_at(0),
                    amp.caps.millibel_at(amp.caps.top()),
                    amp.caps.step_millibel(),
                )
                .map_err(|_| DriverError::DeviceFault)?,
            ),
            None => None,
        };
        Ok(AudioEndpointFacts {
            index: endpoint,
            direction: match found.flow() {
                Flow::Out => StreamDirection::Playback,
                Flow::In => StreamDirection::Capture,
            },
            jack: found.jack,
            formats: found.formats(),
            channel_map: found.channel_map,
            rates: RateSupport::Discrete(found.pcm.rates()?),
            min_period_frames: PERIOD_STEP,
            max_period_frames: MAX_PERIOD_FRAMES,
            max_ring_frames: ring_bounds::MAX_FRAMES,
            gain,
            name: found.name,
        })
    }

    fn configure(
        &mut self,
        endpoint: u16,
        params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        let index = usize::from(endpoint);
        let found = self.endpoint(endpoint)?;
        if found.stream.as_ref().is_some_and(|stream| stream.running) {
            return Err(DriverError::Busy);
        }
        // Inputs sharing a converter cannot run together.
        let converters: Vec<u8> = found.converters().collect();
        let function = found.function;
        let taken = self.endpoints.iter().enumerate().any(|(other, peer)| {
            other != index
                && peer.function == function
                && peer.stream.is_some()
                && peer.converters().any(|nid| converters.contains(&nid))
        });
        if taken {
            return Err(DriverError::Busy);
        }
        let formats = found.formats();
        let format = if formats.contains(params.format) {
            params.format
        } else if formats.contains(SampleFormat::S16) {
            SampleFormat::S16
        } else if formats.contains(SampleFormat::S32) {
            SampleFormat::S32
        } else {
            return Err(DriverError::Unsupported);
        };
        let rate = if found.pcm.runs_at(params.rate.hz()) {
            params.rate
        } else {
            found.pcm.rates()?.nearest(params.rate)
        };
        let channel_map = found.channel_map;
        let channels = channel_map.channels();
        let word = stream_format(found.pcm, rate.hz(), format, channels)?;
        let period_frames = params
            .period_frames
            .clamp(PERIOD_STEP, MAX_PERIOD_FRAMES)
            .div_ceil(PERIOD_STEP)
            * PERIOD_STEP;
        let flow = found.flow();
        self.release(endpoint)?;
        let (descriptor, tag) = self.free_stream(flow)?;
        let frame = frame_bytes(format, channels);
        let period_bytes = period_frames as usize * frame as usize;
        let buffer = self.controller_memory(period_bytes * PERIODS as usize)?;
        let mut bdl = self.controller_memory(BDL_ENTRY * PERIODS as usize)?;
        for (slot, entry) in bdl
            .as_bytes_mut()
            .as_chunks_mut::<BDL_ENTRY>()
            .0
            .iter_mut()
            .enumerate()
        {
            let address = buffer
                .device_addr_at(slot * period_bytes, period_bytes)
                .ok_or(DriverError::DeviceFault)?;
            entry[0..8].copy_from_slice(&address.to_le_bytes());
            entry[8..12].copy_from_slice(
                &u32::try_from(period_bytes)
                    .map_err(|_| DriverError::OutOfRange)?
                    .to_le_bytes(),
            );
            entry[12..16].copy_from_slice(&BDL_IOC.to_le_bytes());
        }
        bdl.sync_range(0, bdl.len());
        self.endpoints[index].stream = Some(Stream {
            descriptor,
            tag,
            format,
            rate_hz: rate.hz(),
            frame_bytes: frame,
            period_frames,
            buffer,
            bdl,
            running: false,
            draining: false,
            periods: 0,
            moved: 0,
            last_position: 0,
            supplied_through: None,
            base: 0,
            xrun_frames: 0,
            drained_at: None,
        });
        let programmed = self
            .program(index, word)
            .and_then(|()| self.tune_converters(index, tag, word));
        if let Err(err) = programmed {
            let _ = self.release(endpoint);
            return Err(err);
        }
        Ok(ConfigureGrant {
            rate,
            format,
            channel_map,
            period_frames,
            max_ring_frames: ring_bounds::MAX_FRAMES,
        })
    }

    fn start(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        let flow = self.endpoint(endpoint)?.flow();
        let stream = self.stream_mut(endpoint)?;
        if stream.running {
            return Ok(());
        }
        stream.base = at.get();
        stream.draining = false;
        stream.drained_at = None;
        if flow == Flow::Out {
            // The controller plays two periods before the first boundary can
            // be answered, so both are written, silence counted lost where
            // nothing was staged.
            while stream.periods < 2 {
                write_period(stream, None, 0)?;
                stream.xrun_frames += u64::from(stream.period_frames);
            }
        }
        stream.running = true;
        let descriptor = stream.descriptor;
        self.controller.run_stream(descriptor, true)
    }

    fn stop(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        self.endpoint(endpoint)?;
        self.halt(usize::from(endpoint), at.get())
    }

    fn drain(&mut self, endpoint: u16) -> Result<(), DriverError> {
        let index = usize::from(endpoint);
        let flow = self.endpoint(endpoint)?.flow();
        let now = self.now();
        let stream = self.stream_mut(endpoint)?;
        if !stream.running {
            return Ok(());
        }
        if flow == Flow::In || stream.supplied_through.is_none() {
            // Nothing the mixer supplied is queued, so the drain is over.
            let at = stream.base + stream.moved / u64::from(stream.frame_bytes);
            stream.drained_at = Some((at, now));
            return self.halt(index, at);
        }
        stream.draining = true;
        Ok(())
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        let index = usize::from(endpoint);
        let found = self.endpoint(endpoint)?;
        let (flow, channels) = (found.flow(), found.channel_map.channels());
        let geometry = ring.geometry();
        {
            let stream = found.stream.as_ref().ok_or(DriverError::DeviceFault)?;
            if geometry.format() != stream.format || geometry.channels() != channels {
                return Err(DriverError::BadMagic);
            }
        }
        self.track(index);
        let now = self.now();
        let stream = self.stream_mut(endpoint)?;
        // A drain ends once the DMA has passed the last frame the mixer
        // supplied.
        let ended = match (stream.draining, stream.supplied_through) {
            (true, Some((period, frames))) if stream.finished() > period => {
                Some(stream.base + period * u64::from(stream.period_frames) + u64::from(frames))
            }
            _ => None,
        };
        if let Some(at) = ended {
            stream.drained_at = Some((at, now));
            self.halt(index, at)?;
        }
        let stream = self.stream_mut(endpoint)?;
        let transferred = match (flow, stream.running) {
            (Flow::Out, true) => refill(stream, ring)?,
            (Flow::Out, false) => stage(stream, ring)?,
            (Flow::In, true) => deliver(stream, ring)?,
            (Flow::In, false) => 0,
        };
        let (position, sampled_at) = match (stream.running, stream.drained_at) {
            (false, Some(drained)) => drained,
            _ => (
                stream.base + stream.moved / u64::from(stream.frame_bytes),
                now,
            ),
        };
        Ok(AudioServiced {
            transferred,
            running: stream.running,
            position: Frames::new(position),
            xrun_frames: stream.xrun_frames,
            sampled_at,
        })
    }

    fn set_gain(&mut self, endpoint: u16, millibel: i32, mute: bool) -> Result<(), DriverError> {
        let found = self.endpoint(endpoint)?;
        if found.gain.is_empty() {
            return Err(DriverError::NotImplemented);
        }
        let function = found.function;
        let gain = found.gain.clone();
        let muters = found.mute.clone();
        let pins: Vec<u8> = found.pins().collect();
        let input = found.flow() == Flow::In;
        for amp in &gain {
            let muted = mute && muters.contains(amp);
            self.send(
                function,
                amp.verb(amp.caps.step_at_or_above(millibel), muted),
            )?;
        }
        for amp in muters.iter().filter(|amp| !gain.contains(amp)) {
            self.send(function, amp.verb(amp.caps.unity(), mute))?;
        }
        if muters.is_empty() {
            // Nothing on the route can mute, so its pins stop driving.
            for pin in pins {
                let on = if input {
                    pin_control::IN
                } else {
                    pin_control::OUT
                };
                self.send(
                    function,
                    Verb::short(pin, SET_PIN_CONTROL, if mute { 0 } else { on }),
                )?;
            }
        }
        Ok(())
    }

    fn release(&mut self, endpoint: u16) -> Result<(), DriverError> {
        let index = usize::from(endpoint);
        self.endpoint(endpoint)?;
        let Some(descriptor) = self.endpoints[index]
            .stream
            .as_ref()
            .map(|stream| stream.descriptor)
        else {
            return Ok(());
        };
        let stopped = self
            .controller
            .run_stream(descriptor, false)
            .and_then(|()| self.controller.reset_stream(descriptor))
            .and_then(|()| self.controller.stream_interrupt(descriptor, false));
        let quiet = self.tune_converters(index, 0, 0);
        if let Some(mut stream) = self.endpoints[index].stream.take() {
            if stopped.is_err() {
                // A descriptor that will not stop may still read or write
                // its buffer, which then is never handed to anyone else.
                stream.buffer.withhold();
                stream.bdl.withhold();
            }
        }
        stopped.and(quiet)
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        self.controller.gather()?;
        let mut causes = self.stream_causes()?;
        let raised = self.controller.take_unsolicited();
        for index in 0..self.endpoints.len() {
            let function = self.endpoints[index].function;
            let address = usize::from(self.functions[function].address);
            let tag = self.endpoints[index].tag;
            if raised
                .get(address)
                .is_some_and(|tags| tags & (1 << tag) != 0)
                && self.sense(index)?
            {
                causes.jack_changed |= 1 << index;
            }
        }
        // Reading the jacks waited on the controller; a period that ended
        // meanwhile is reported now rather than lost.
        let late = self.stream_causes()?;
        causes.period_elapsed |= late.period_elapsed;
        causes.xrun |= late.xrun;
        Ok(causes)
    }

    fn set_event_interrupts(&mut self, enabled: bool) -> Result<(), DriverError> {
        self.events = enabled;
        let descriptors: Vec<u8> = self
            .endpoints
            .iter()
            .filter_map(|endpoint| endpoint.stream.as_ref().map(|stream| stream.descriptor))
            .collect();
        for descriptor in descriptors {
            self.controller.stream_interrupt(descriptor, enabled)?;
        }
        Ok(())
    }
}

impl<R: Registers, W: Wait, D: Delay> Hda<'_, R, W, D> {
    /// Each stream's gathered status, as the endpoints it names.
    fn stream_causes(&mut self) -> Result<AudioInterrupt, DriverError> {
        let status = self.controller.take_status();
        let mut causes = AudioInterrupt::NONE;
        for (index, endpoint) in self.endpoints.iter().enumerate() {
            let Some(stream) = &endpoint.stream else {
                continue;
            };
            let raised = status[usize::from(stream.descriptor)];
            if raised & sd::DESE != 0 {
                // The controller could not fetch the stream's descriptors:
                // its view of memory is not this driver's.
                return Err(DriverError::DeviceFault);
            }
            if raised & sd::BCIS != 0 {
                causes.period_elapsed |= 1 << index;
            }
            if raised & sd::FIFOE != 0 {
                causes.xrun |= 1 << index;
            }
        }
        Ok(causes)
    }
}

/// Whether `pin`'s connector is watched for presence: it can tell, the board
/// did not say otherwise, and it is a connector rather than a fixed device.
fn watched(pin: &Widget) -> bool {
    pin.pin.presence_detect()
        && pin.caps.unsolicited()
        && !pin.config.no_presence_detect()
        && !pin.config.fixed()
}

/// The monitor an ELD names, as the endpoint's name.
fn monitor(eld: &[u8], display_port: bool) -> Option<AudioName> {
    let length = usize::from(*eld.get(4)? & 0x1F);
    let name = core::str::from_utf8(eld.get(20..20 + length)?).ok()?.trim();
    if name.is_empty() {
        return None;
    }
    let connector = if display_port { "DisplayPort" } else { "HDMI" };
    let mut text = alloc::string::String::from(connector);
    text.push_str(": ");
    text.push_str(name);
    AudioName::new(&text).ok()
}

/// What to call a controller whose first codec is `vendor`.
fn device_name(vendor: u32) -> Result<AudioName, DriverError> {
    let mut text = alloc::string::String::from("HD Audio");
    if vendor != 0 {
        let _ = core::fmt::Write::write_fmt(
            &mut text,
            format_args!(" {:04X}:{:04X}", vendor >> 16, vendor & 0xFFFF),
        );
    }
    AudioName::new(&text).map_err(|_| DriverError::Unsupported)
}

/// The endpoints `plan` presents for function number `function`.
fn endpoints_of(graph: &Function, function: usize, plan: &Plan) -> Vec<Endpoint> {
    let outputs = plan.outputs.iter().map(|output| {
        let pcm = common_pcm(graph, output.lanes.iter().map(Route::upstream));
        let (gain, mute) = controls(graph, &output.lanes, true);
        Endpoint {
            function,
            kind: Kind::Output {
                lanes: output.lanes.clone(),
                mirrors: output.mirrors.clone(),
                digital: output.digital,
                display: output.display,
            },
            name: output.name,
            channel_map: output.channel_map,
            pcm,
            gain,
            mute,
            jack: JackState::Unknown,
            tag: 0,
            stream: None,
        }
    });
    let inputs = plan.inputs.iter().map(|input| {
        let pcm = common_pcm(graph, core::iter::once(input.route.downstream()));
        let (gain, mute) = controls(graph, core::slice::from_ref(&input.route), false);
        Endpoint {
            function,
            kind: Kind::Input {
                route: input.route.clone(),
            },
            name: input.name,
            channel_map: input.channel_map,
            pcm,
            gain,
            mute,
            jack: JackState::Unknown,
            tag: 0,
            stream: None,
        }
    });
    outputs.chain(inputs).collect()
}

/// The PCM support every one of `converters` shares.
fn common_pcm(graph: &Function, converters: impl Iterator<Item = u8>) -> PcmSupport {
    converters
        .filter_map(|nid| graph.widget(nid))
        .map(|widget| widget.pcm)
        .reduce(PcmSupport::and)
        .unwrap_or_default()
}

/// Each lane's gain amplifier and the amplifiers that mute the endpoint:
/// the first adjustable one, and the first that can mute, counting from the
/// converter outwards.
fn controls(graph: &Function, lanes: &[Route], output: bool) -> (Vec<Amp>, Vec<Amp>) {
    let mut gain = Vec::new();
    let mut mute = Vec::new();
    for lane in lanes {
        let amps = route_amps(graph, lane, output);
        if let Some(amp) = amps.iter().find(|amp| amp.caps.adjustable()) {
            gain.push(*amp);
        }
        if let Some(amp) = amps.iter().find(|amp| amp.caps.mute()) {
            mute.push(*amp);
        }
    }
    if gain.len() != lanes.len() {
        gain.clear();
    }
    if mute.len() != lanes.len() {
        mute.clear();
    }
    (gain, mute)
}

/// The amplifiers samples pass on `route`, counting from the converter
/// outwards. A pin's input amplifier boosts what comes in at its jack and
/// its output amplifier drives what goes out, so each counts only on the
/// route that runs that way.
fn route_amps(graph: &Function, route: &Route, output: bool) -> Vec<Amp> {
    let mut hops: Vec<_> = route.hops().to_vec();
    if output {
        hops.reverse();
    }
    let mut amps = Vec::new();
    for hop in hops {
        let Some(widget) = graph.widget(hop.nid) else {
            continue;
        };
        let pin = widget.kind() == WidgetKind::Pin;
        let input_index = if pin {
            (!output).then_some(0)
        } else {
            hop.select
        };
        let input = input_index
            .filter(|_| widget.caps.input_amp())
            .map(|index| Amp {
                nid: hop.nid,
                output: false,
                index,
                caps: widget.input_amp,
            });
        let outward = (widget.caps.output_amp() && (output || !pin)).then_some(Amp {
            nid: hop.nid,
            output: true,
            index: 0,
            caps: widget.output_amp,
        });
        // Playback meets a widget's input amplifier first, so from the
        // converter outwards it comes first; capture runs the other way.
        let ordered = if output {
            [input, outward]
        } else {
            [outward, input]
        };
        amps.extend(ordered.into_iter().flatten());
    }
    amps
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
