//! The device engine: one USB audio function as an [`Audio`] device.
//!
//! Each streaming interface is one endpoint of the device. Configuring one
//! selects the alternate setting carrying the asked-for format nearest, sets
//! its clock, and sizes its stream. A playback endpoint serviced before it
//! starts has its stream set up and as many whole slots as the ring fills
//! held, so the device begins on the mixer's first frames rather than a gap;
//! starting it starts the feedback endpoint or the capture endpoint whose
//! packets pace it, then queues them — or one slot of counted silence when
//! none is held, since a device with nothing to finish never asks to be
//! serviced. Every period is one slot the host controller reports finished
//! on the driver's port.
//!
//! # The clock pair
//!
//! A position is the device's own timeline: frames it played or captured,
//! frames it ran short of, and frames of intervals that passed carrying
//! nothing. Each is stamped with the moment the controller finished the slot
//! that carried it, so the mixer's linear fit sees the device's clock rather
//! than the driver's scheduling.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{
    ring_bounds, Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioName,
    AudioServiced, ChannelMap, Frames, JackState, Rate, RateSet, RateSupport, SampleFormats,
    StreamDirection, MAX_DEVICE_ENDPOINTS, MAX_DEVICE_RATES, STANDARD_RATES,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::time::Time64;
use tairix_abi::usb_urb::{
    IsoGrant, IsoLayout, IsoNotify, IsoStartParams, UsbSpeed, ISO_MAX_INTERVALS, ISO_MAX_SLOTS,
    ISO_MIN_SLOTS, ISO_NOTIFY_LEN,
};
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_usb::periodic::{
    EndpointDescriptor, FeedbackDecoder, IsoSync, PacketPacer, PeriodicBudget, ServiceInterval,
    TransferKind,
};

use crate::controls::{read_gain, ClockRoute, ClockWalk, Gain};
use crate::requests::{self, selector, Direction};
use crate::stream::{
    deliver_capture, fill_playback, plan_slot, playback_losses, HwStream, Shortfall, SlotShape,
};
use crate::streaming::{streaming_interfaces, AltFormat, AltRates, StreamingInterface};
use crate::topology::{terminal_name, EntityKind, Topology, Version};

/// What the engine needs of the world: its interface's control requests and
/// stream operations, the regions its streams ride, the notices they send,
/// and a monotonic clock.
pub trait UacTransport {
    /// A control-IN transfer of `data.len()` bytes, answering the bytes the
    /// device delivered.
    ///
    /// # Errors
    ///
    /// The transfer's [`Errno`]: [`Errno::EndpointStalled`] for a request
    /// the device refused.
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno>;

    /// A control-OUT transfer whose data stage carries `data`.
    ///
    /// # Errors
    ///
    /// As [`Self::control_in`].
    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno>;

    /// Govern streaming interface `interface` of the same device.
    ///
    /// # Errors
    ///
    /// The host controller's refusal.
    fn claim_interface(&mut self, interface: u8) -> Result<(), Errno>;

    /// Select `alternate` on `interface`.
    ///
    /// # Errors
    ///
    /// The host controller's refusal: [`Errno::NoBandwidth`] when the bus
    /// cannot carry the setting.
    fn set_interface(&mut self, interface: u8, alternate: u8) -> Result<(), Errno>;

    /// Start a stream and map the region it rides.
    ///
    /// # Errors
    ///
    /// The host controller's refusal, or the mapping's.
    fn iso_start(&mut self, params: IsoStartParams) -> Result<IsoGrant, Errno>;

    /// Hand slot `slot` of the stream on `endpoint` to the controller.
    ///
    /// # Errors
    ///
    /// The host controller's refusal.
    fn iso_queue(&mut self, endpoint: u8, slot: u16) -> Result<(), Errno>;

    /// Stop the stream on `endpoint` and release its region.
    ///
    /// # Errors
    ///
    /// The host controller's refusal; the region is released regardless.
    fn iso_stop(&mut self, endpoint: u8) -> Result<(), Errno>;

    /// The region of the stream on `endpoint`, while it runs.
    fn region(&mut self, endpoint: u8) -> Option<&mut [u8]>;

    /// The next notice waiting on any of the driver's stream ports, with the
    /// process that sent it, or `None` when none waits.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to read a port.
    fn next_notice(&mut self) -> Result<Option<(ProcId, [u8; ISO_NOTIFY_LEN])>, Errno>;

    /// Monotonic nanoseconds.
    fn now_ns(&self) -> u64;
}

/// Microframes of data a stream keeps queued past the slot finishing: the
/// longest isochronous scheduling threshold a controller states (eight
/// frames), plus as long again for the driver's own wake to come round.
const QUEUED_MICROFRAMES: u32 = 128;

/// Most intervals one slot may span while a stream keeps two slots.
const MAX_SLOT_PACKETS: u32 = ISO_MAX_INTERVALS / ISO_MIN_SLOTS as u32;

/// Microframes one explicit feedback slot spans, so the rate is read about
/// every eight milliseconds whatever the endpoint's interval.
const FEEDBACK_SLOT_MICROFRAMES: u32 = 64;

/// Slots an explicit feedback stream keeps.
const FEEDBACK_SLOTS: u16 = 4;

/// The longest period a configuration grants, a bound on the driver's own
/// buffering rather than a property of the device.
const MAX_PERIOD_FRAMES: u32 = 4_096;

/// Microframes in one second.
const MICROFRAMES_PER_SECOND: u64 = 8_000;

/// What the device is called before any endpoint is named.
const DEVICE_NAME: &str = "USB Audio";

/// What the bus has selected on an interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Selected {
    format: usize,
    rate: Rate,
    /// Version 2.0: the route the rate was set through.
    route: Option<usize>,
}

/// The mixer's configuration of an endpoint.
#[derive(Clone, Copy, Debug)]
struct Config {
    format: usize,
    rate: Rate,
    grant: ConfigureGrant,
    layout: IsoLayout,
    interval: u32,
}

/// The gain the mixer last set on an endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GainSetting {
    millibel: i32,
    mute: bool,
}

/// An endpoint's place on the device's timeline, from its first service or
/// start until it is configured again.
#[derive(Debug)]
struct Run {
    position: Frames,
    sampled_at: Time64,
    /// Losses since the configuration, so a restart keeps the tally.
    xrun_frames: u64,
    /// The nominal frames per interval, for intervals that carried nothing.
    timeline: PacketPacer,
    running: bool,
    draining: bool,
    playback: Option<Playback>,
    /// Why the endpoint's streams could not be started again, answered to
    /// every service until the mixer stops or starts it.
    fault: Option<DriverError>,
}

impl Run {
    /// An endpoint at `position` that is not clocking, having lost nothing.
    const fn held(position: Frames, sampled_at: Time64, timeline: PacketPacer) -> Self {
        Self {
            position,
            sampled_at,
            xrun_frames: 0,
            timeline,
            running: false,
            draining: false,
            playback: None,
            fault: None,
        }
    }
}

/// A playback endpoint's pacing.
#[derive(Debug)]
struct Playback {
    pacer: PacketPacer,
    decoder: FeedbackDecoder,
    feedback: Option<HwStream>,
}

/// One streaming interface, as the endpoint of the device it is.
#[derive(Debug)]
struct Interface {
    info: StreamingInterface,
    facts: AudioEndpointFacts,
    gain: Option<Gain>,
    routes: Vec<ClockRoute>,
    /// Playback: the capture interface whose data packets pace it.
    implicit: Option<usize>,
    selected: Option<Selected>,
    config: Option<Config>,
    data: Option<HwStream>,
    run: Option<Run>,
    /// Capture: the playback interface its stream paces.
    feeds: Option<usize>,
    /// What a reset device forgot and a stream start sets again.
    gain_setting: Option<GainSetting>,
}

impl Interface {
    fn format(&self, index: usize) -> Result<&AltFormat, DriverError> {
        self.info.formats.get(index).ok_or(DriverError::DeviceFault)
    }
}

/// One USB audio function.
pub struct UsbAudio<T: UacTransport> {
    transport: T,
    version: Version,
    control: u8,
    speed: UsbSpeed,
    interfaces: Vec<Interface>,
    pending: AudioInterrupt,
}

/// A transport refusal as the class trait reports it.
fn refused(errno: Errno) -> DriverError {
    DriverError::from_errno(errno)
}

impl<T: UacTransport> UsbAudio<T> {
    /// Open the function whose control interface is `control` in
    /// configuration descriptor `config`, its device running at `speed`:
    /// claim its streaming interfaces and read its controls.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for descriptors that do not parse,
    /// [`DriverError::NotFound`] for a function with no endpoint the
    /// vocabulary carries, [`DriverError::Unsupported`] for one with more
    /// than [`MAX_DEVICE_ENDPOINTS`], or the host controller's refusal of a
    /// claim.
    pub fn open(
        config: &[u8],
        control: u8,
        speed: UsbSpeed,
        mut transport: T,
    ) -> Result<Self, DriverError> {
        let topology = Topology::parse(config, control)?;
        let streaming = streaming_interfaces(config, &topology, control)?;
        if streaming.len() > usize::from(MAX_DEVICE_ENDPOINTS) {
            return Err(DriverError::Unsupported);
        }
        let mut interfaces = Vec::new();
        interfaces
            .try_reserve_exact(streaming.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        for info in streaming {
            transport.claim_interface(info.interface).map_err(refused)?;
            let index = u16::try_from(interfaces.len()).map_err(|_| DriverError::Unsupported)?;
            if let Some(interface) =
                describe(&mut transport, &topology, control, speed, info, index)?
            {
                interfaces.push(interface);
            }
        }
        if interfaces.is_empty() {
            return Err(DriverError::NotFound);
        }
        link_implicit_feedback(&mut interfaces);
        Ok(Self {
            transport,
            version: topology.version,
            control,
            speed,
            interfaces,
            pending: AudioInterrupt::NONE,
        })
    }

    /// The transport.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The transport, mutably: where the program records the ports it bound.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Every endpoint a stream of the function may run on: each setting's
    /// data endpoint, and its feedback endpoint where it has one.
    pub fn stream_endpoints(&self) -> impl Iterator<Item = u8> + '_ {
        self.interfaces.iter().flat_map(|interface| {
            interface.info.formats.iter().flat_map(|format| {
                core::iter::once(format.data.address)
                    .chain(format.feedback.map(|feedback| feedback.address))
            })
        })
    }

    fn interface(&self, endpoint: u16) -> Result<&Interface, DriverError> {
        self.interfaces
            .get(usize::from(endpoint))
            .ok_or(DriverError::NotFound)
    }

    /// Choose the setting of `index` that carries `params` nearest: the
    /// asked-for channel count first, then the rate, then the encoding, then
    /// the widest encoding.
    fn choose_format(&self, index: usize, params: &ConfigureParams) -> Option<usize> {
        let interface = self.interfaces.get(index)?;
        interface
            .info
            .formats
            .iter()
            .enumerate()
            .max_by_key(|(_, format)| {
                let rate_admitted = Self::format_rates(interface, format)
                    .is_ok_and(|rates| rates.contains(&params.rate));
                (
                    format.channel_map.channels() == params.channel_map.channels(),
                    rate_admitted,
                    format.format == params.format,
                    format.format.valid_bits(),
                    // Earlier settings win ties: `max_by_key` keeps the last.
                    core::cmp::Reverse(format.alternate),
                )
            })
            .map(|(at, _)| at)
    }

    /// The rates `format` of `interface` runs at.
    fn format_rates(interface: &Interface, format: &AltFormat) -> Result<Vec<Rate>, DriverError> {
        match format.rates {
            AltRates::Listed(support) => standard_rates_of(&support),
            AltRates::Clock => {
                let mut rates: Vec<Rate> = Vec::new();
                for &rate in interface.routes.iter().flat_map(|route| route.rates.iter()) {
                    if !rates.contains(&rate) {
                        rates.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
                        rates.push(rate);
                    }
                }
                Ok(rates)
            }
        }
    }

    /// Set `rate` on setting `format` of interface `index`, answering the
    /// rate the device then runs at and the route it runs through.
    fn apply_rate(
        &mut self,
        index: usize,
        format: usize,
        rate: Rate,
    ) -> Result<(Rate, Option<usize>), DriverError> {
        match self.version {
            Version::One => {
                let alt = *self.interfaces[index].format(format)?;
                if !alt.frequency_control {
                    return Ok((rate, None));
                }
                let address = alt.data.address;
                self.transport
                    .control_out(
                        requests::endpoint(
                            Direction::Out,
                            requests::v1::SET_CUR,
                            selector::SAMPLING_FREQUENCY,
                            address,
                            requests::FREQUENCY_V1_LEN,
                        ),
                        &requests::frequency_v1(rate.hz()),
                    )
                    .map_err(refused)?;
                let mut current = [0u8; 3];
                let read = self.transport.control_in(
                    requests::endpoint(
                        Direction::In,
                        requests::v1::GET_CUR,
                        selector::SAMPLING_FREQUENCY,
                        address,
                        requests::FREQUENCY_V1_LEN,
                    ),
                    &mut current,
                );
                // A device that will not report its rate runs at the one set.
                let actual = match read {
                    Ok(n) => requests::read_frequency_v1(&current[..n])
                        .and_then(|hz| Rate::new(hz).ok())
                        .ok_or(DriverError::DeviceFault)?,
                    Err(Errno::EndpointStalled) => rate,
                    Err(errno) => return Err(refused(errno)),
                };
                Ok((actual, None))
            }
            Version::Two => {
                for route in self.free_routes(index, rate)? {
                    if let Ok(actual) = self.set_route_rate(index, route, rate) {
                        return Ok((actual, Some(route)));
                    }
                }
                Err(DriverError::DeviceFault)
            }
        }
    }

    /// The clock routes of interface `index` that run its terminal at `rate`
    /// and that no other endpoint holds otherwise, the one it runs through
    /// now first.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] when only routes another endpoint holds run
    /// `rate`, [`DriverError::DeviceFault`] when none does.
    fn free_routes(&self, index: usize, rate: Rate) -> Result<Vec<usize>, DriverError> {
        let interface = &self.interfaces[index];
        let current = interface.selected.and_then(|selected| selected.route);
        let mut free = Vec::new();
        free.try_reserve_exact(interface.routes.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        let mut contended = false;
        let order = current
            .into_iter()
            .chain((0..interface.routes.len()).filter(|&at| Some(at) != current));
        for route in order {
            if !interface.routes[route].rates.contains(&rate) {
                continue;
            }
            if self.clock_contended(index, route, rate) {
                contended = true;
                continue;
            }
            free.push(route);
        }
        if free.is_empty() {
            return Err(if contended {
                DriverError::Busy
            } else {
                DriverError::DeviceFault
            });
        }
        Ok(free)
    }

    /// Steer every selector on `route` of interface `index`, set its source
    /// to run the terminal at `rate`, and answer the rate it reports.
    fn set_route_rate(
        &mut self,
        index: usize,
        route: usize,
        rate: Rate,
    ) -> Result<Rate, DriverError> {
        let (pins, source, programmable, validity, ratio, source_hz) = {
            let route = self.interfaces[index]
                .routes
                .get(route)
                .ok_or(DriverError::DeviceFault)?;
            if !route.rates.contains(&rate) {
                return Err(DriverError::Unsupported);
            }
            (
                route.pins.len(),
                route.source,
                route.programmable,
                route.validity,
                (route.numerator, route.denominator),
                route.source_hz(rate),
            )
        };
        let control = self.control;
        for at in 0..pins {
            let (selector_id, pin) = self.interfaces[index].routes[route].pins[at];
            self.transport
                .control_out(
                    requests::entity(
                        Direction::Out,
                        requests::v2::CUR,
                        selector::CLOCK_SELECTOR,
                        0,
                        selector_id,
                        control,
                        1,
                    ),
                    &[pin],
                )
                .map_err(refused)?;
        }
        let source_hz = u32::try_from(source_hz).map_err(|_| DriverError::OutOfRange)?;
        if programmable {
            self.transport
                .control_out(
                    frequency_request(Direction::Out, source, control),
                    &source_hz.to_le_bytes(),
                )
                .map_err(refused)?;
        }
        let mut current = [0u8; 4];
        let read = self
            .transport
            .control_in(
                frequency_request(Direction::In, source, control),
                &mut current,
            )
            .map_err(refused)?;
        let actual_source = requests::read_u32(&current[..read]).ok_or(DriverError::DeviceFault)?;
        if validity {
            let mut valid = [0u8; 1];
            let read = self
                .transport
                .control_in(
                    requests::entity(
                        Direction::In,
                        requests::v2::CUR,
                        selector::CLOCK_VALID,
                        0,
                        source,
                        control,
                        1,
                    ),
                    &mut valid,
                )
                .map_err(refused)?;
            if read != 1 || valid[0] == 0 {
                return Err(DriverError::DeviceFault);
            }
        }
        let terminal_hz = u64::from(actual_source) * u64::from(ratio.0) / u64::from(ratio.1);
        Rate::new(u32::try_from(terminal_hz).map_err(|_| DriverError::DeviceFault)?)
            .map_err(|_| DriverError::DeviceFault)
    }

    /// Whether running route `route` of interface `index` at `rate` would
    /// retune another endpoint's clock: a source the two share run at another
    /// frequency, or a selector they share steered to another pin.
    fn clock_contended(&self, index: usize, route: usize, rate: Rate) -> bool {
        let Some(mine) = self.interfaces[index].routes.get(route) else {
            return false;
        };
        let mine_hz = mine.source_hz(rate);
        self.interfaces
            .iter()
            .enumerate()
            .any(|(other, interface)| {
                let Some(selected) = interface.selected.filter(|_| other != index) else {
                    return false;
                };
                let Some(theirs) = selected.route.and_then(|at| interface.routes.get(at)) else {
                    return false;
                };
                (theirs.source == mine.source && theirs.source_hz(selected.rate) != mine_hz)
                    || theirs.pins.iter().any(|&(selector_id, pin)| {
                        mine.pins
                            .iter()
                            .any(|&(own, own_pin)| own == selector_id && own_pin != pin)
                    })
            })
    }

    /// Start a stream of `layout` on `endpoint`, its intervals `interval`
    /// microframes apart.
    fn start_stream(
        &mut self,
        endpoint: u8,
        layout: IsoLayout,
        interval: u32,
    ) -> Result<HwStream, DriverError> {
        let grant = self
            .transport
            .iso_start(IsoStartParams { endpoint, layout })
            .map_err(refused)?;
        // The host controller's reading of the endpoint must be this
        // driver's, or every position it accounts would be wrong.
        if grant.interval_microframes != interval || grant.speed != self.speed {
            let _ = self.transport.iso_stop(endpoint);
            return Err(DriverError::DeviceFault);
        }
        match HwStream::new(endpoint, layout, grant.grantor, grant.stream) {
            Ok(stream) => Ok(stream),
            Err(err) => {
                let _ = self.transport.iso_stop(endpoint);
                Err(err)
            }
        }
    }

    /// Start a receiving stream as [`Self::start_stream`] does, with every
    /// slot set out; one that cannot be set out whole is stopped again.
    fn start_receiving(
        &mut self,
        endpoint: u8,
        layout: IsoLayout,
        interval: u32,
    ) -> Result<HwStream, DriverError> {
        let mut stream = self.start_stream(endpoint, layout, interval)?;
        while let Some(slot) = stream.next_free() {
            if let Err(errno) = self.transport.iso_queue(endpoint, slot) {
                let _ = self.transport.iso_stop(endpoint);
                return Err(refused(errno));
            }
            stream.mark_queued(slot, 0);
        }
        Ok(stream)
    }

    /// The stream layout and interval for `format` at `rate`, slots sized to
    /// carry about `period` frames.
    fn layout_for(
        &self,
        format: &AltFormat,
        rate: Rate,
        period: u32,
    ) -> Result<(IsoLayout, u32, u32), DriverError> {
        let interval =
            ServiceInterval::isochronous(self.speed, format.data.interval)?.microframes();
        let budget = PeriodicBudget::isochronous(&format.data, self.speed)?;
        // Frames per interval, scaled by microframes per second.
        let per_interval = u64::from(rate.hz()) * u64::from(interval);
        let packets = (u64::from(period) * MICROFRAMES_PER_SECOND)
            .div_ceil(per_interval.max(1))
            .clamp(1, u64::from(MAX_SLOT_PACKETS));
        let packets = u32::try_from(packets).unwrap_or(MAX_SLOT_PACKETS);
        let slot_microframes = packets * interval;
        let slots = (QUEUED_MICROFRAMES.div_ceil(slot_microframes) + 1)
            .clamp(u32::from(ISO_MIN_SLOTS), u32::from(ISO_MAX_SLOTS))
            .min(ISO_MAX_INTERVALS / packets)
            .max(u32::from(ISO_MIN_SLOTS));
        let layout = IsoLayout::new(
            u16::try_from(slots).map_err(|_| DriverError::OutOfRange)?,
            u16::try_from(packets).map_err(|_| DriverError::OutOfRange)?,
            budget.max_esit_payload,
        )
        .map_err(|_| DriverError::OutOfRange)?;
        Ok((layout, interval, slot_frames(layout, rate, interval)?))
    }

    /// Bring interface `index`'s device side to setting `format` at `rate`
    /// from the engine's own record: claimed, the setting selected, the rate
    /// set, and the mixer's gain applied. A controller reset forgets every
    /// one of them, so no stream starts on what the device may have dropped.
    fn establish(&mut self, index: usize, format: usize, rate: Rate) -> Result<(), DriverError> {
        let number = self.interfaces[index].info.interface;
        let alternate = self.interfaces[index].format(format)?.alternate;
        self.transport.claim_interface(number).map_err(refused)?;
        self.transport
            .set_interface(number, alternate)
            .map_err(refused)?;
        let route = self.interfaces[index]
            .selected
            .and_then(|selected| selected.route);
        self.interfaces[index].selected = Some(Selected {
            format,
            rate,
            route,
        });
        let (actual, route) = self.apply_rate(index, format, rate)?;
        if actual != rate {
            return Err(DriverError::DeviceFault);
        }
        self.interfaces[index].selected = Some(Selected {
            format,
            rate,
            route,
        });
        match self.interfaces[index].gain_setting {
            Some(setting) => self.apply_gain(index, setting),
            None => Ok(()),
        }
    }

    /// Select setting `format` on interface `index` if the bus holds another,
    /// set `rate`, and record what the bus then holds: the rate the device
    /// runs at.
    fn select(&mut self, index: usize, format: usize, rate: Rate) -> Result<Rate, DriverError> {
        if self.interfaces[index]
            .selected
            .map(|selected| selected.format)
            != Some(format)
        {
            let number = self.interfaces[index].info.interface;
            let alternate = self.interfaces[index].format(format)?.alternate;
            self.transport
                .set_interface(number, alternate)
                .map_err(refused)?;
            // The bus holds this setting whatever the rate does, so a later
            // restore or release frees it.
            self.interfaces[index].selected = Some(Selected {
                format,
                rate,
                route: None,
            });
        }
        let (actual, route) = self.apply_rate(index, format, rate)?;
        self.interfaces[index].selected = Some(Selected {
            format,
            rate: actual,
            route,
        });
        Ok(actual)
    }

    /// Put interface `index` back to `previous` after a configuration that
    /// failed part-way. Where that fails too the endpoint is left
    /// unconfigured, so nothing streams in a setting its grant does not
    /// describe.
    fn restore(&mut self, index: usize, previous: Option<Selected>) {
        let restored = if let Some(previous) = previous {
            self.select(index, previous.format, previous.rate)
                .is_ok_and(|actual| actual == previous.rate)
        } else {
            let number = self.interfaces[index].info.interface;
            self.interfaces[index].selected = None;
            self.transport.set_interface(number, 0).is_ok()
        };
        if !restored {
            let interface = &mut self.interfaces[index];
            interface.config = None;
            interface.run = None;
        }
    }

    /// Set playback interface `index` up to clock: its data stream started
    /// with nothing queued, and its pacing begun afresh from the nominal
    /// rate. A stream already set up is left as it is.
    fn arm_playback(&mut self, index: usize, config: Config) -> Result<(), DriverError> {
        if self.interfaces[index].data.is_some() {
            return Ok(());
        }
        self.establish(index, config.format, config.rate)?;
        let address = self.interfaces[index].format(config.format)?.data.address;
        let stream = self.start_stream(address, config.layout, config.interval)?;
        let sampled_at = Time64::from_nanos(self.transport.now_ns());
        let pacer = PacketPacer::nominal(config.rate.hz(), config.interval);
        let decoder = FeedbackDecoder::new(self.speed, config.rate.hz());
        let interface = &mut self.interfaces[index];
        interface.data = Some(stream);
        let run = interface
            .run
            .get_or_insert_with(|| Run::held(Frames::ZERO, sampled_at, pacer));
        match run.playback.as_mut() {
            Some(playback) => {
                playback.pacer = pacer;
                playback.decoder = decoder;
            }
            None => {
                run.playback = Some(Playback {
                    pacer,
                    decoder,
                    feedback: None,
                });
            }
        }
        Ok(())
    }

    /// Clock playback interface `index`: the stream pacing it first, then
    /// the slots a service filled ahead — or, with none, one of silence, so
    /// the device has a slot to finish and the stream a period to report.
    fn clock_playback(&mut self, index: usize, config: Config) -> Result<(), DriverError> {
        self.arm_playback(index, config)?;
        let alt = *self.interfaces[index].format(config.format)?;
        match (alt.feedback, self.interfaces[index].implicit) {
            (Some(descriptor), _) => {
                let stream = self.start_feedback(&descriptor)?;
                self.install_feedback(index, stream)?;
            }
            (None, Some(source)) => self.ensure_source(source, index, config.rate)?,
            (None, None) => {}
        }
        let shape = SlotShape {
            layout: config.layout,
            frame_bytes: alt.frame_bytes(),
            silence: alt.format.silence_byte(),
        };
        let Self {
            transport,
            interfaces,
            ..
        } = self;
        let interface = &mut interfaces[index];
        let (Some(stream), Some(run)) = (interface.data.as_mut(), interface.run.as_mut()) else {
            return Err(DriverError::DeviceFault);
        };
        if stream.staged() == 0 {
            let playback = run.playback.as_mut().ok_or(DriverError::DeviceFault)?;
            let slot = stream.next_free().ok_or(DriverError::DeviceFault)?;
            let (counts, _) = plan_slot(shape, &mut playback.pacer);
            let region = transport
                .region(stream.endpoint)
                .ok_or(DriverError::DeviceFault)?;
            let filled = fill_playback(shape, region, slot, &counts, None, Shortfall::Pad)?;
            stream.mark_staged(slot, filled.carried());
            run.xrun_frames = run.xrun_frames.saturating_add(u64::from(filled.padded));
        }
        while let Some(slot) = stream.oldest_staged() {
            if let Err(errno) = transport.iso_queue(stream.endpoint, slot) {
                stream.halt(errno);
                return Err(refused(errno));
            }
            stream.queue_staged(slot);
        }
        Ok(())
    }

    /// Bring capture interface `source` to `rate` and its stream running, to
    /// pace playback interface `index`.
    fn ensure_source(
        &mut self,
        source: usize,
        index: usize,
        rate: Rate,
    ) -> Result<(), DriverError> {
        if self.interfaces[source].data.is_some() {
            if self.interfaces[source].selected.map(|s| s.rate) != Some(rate) {
                return Err(DriverError::Busy);
            }
            self.interfaces[source].feeds = Some(index);
            return Ok(());
        }
        let period = self.interfaces[index]
            .config
            .map_or(ring_bounds::MIN_FRAMES, |config| config.grant.period_frames);
        let (format, config) = match self.interfaces[source].config {
            Some(config) if config.rate == rate => (config.format, Some(config)),
            Some(_) => return Err(DriverError::Busy),
            None => {
                let interface = &self.interfaces[source];
                let format = interface
                    .info
                    .formats
                    .iter()
                    .position(|alt| {
                        Self::format_rates(interface, alt).is_ok_and(|rates| rates.contains(&rate))
                    })
                    .ok_or(DriverError::Unsupported)?;
                (format, None)
            }
        };
        let alt = *self.interfaces[source].format(format)?;
        let (layout, interval) = if let Some(config) = config {
            (config.layout, config.interval)
        } else {
            let (layout, interval, _) = self.layout_for(&alt, rate, period)?;
            (layout, interval)
        };
        let started = self
            .establish(source, format, rate)
            .and_then(|()| self.start_receiving(alt.data.address, layout, interval));
        match started {
            Ok(stream) => {
                self.interfaces[source].data = Some(stream);
                self.interfaces[source].feeds = Some(index);
                Ok(())
            }
            Err(err) => {
                // A setting selected only to pace another endpoint is given
                // back, or its bandwidth stays reserved for nothing.
                if self.interfaces[source].config.is_none() {
                    let number = self.interfaces[source].info.interface;
                    self.interfaces[source].selected = None;
                    let _ = self.transport.set_interface(number, 0);
                }
                Err(err)
            }
        }
    }

    /// Stop interface `index`'s streams. A capture stream pacing another
    /// endpoint keeps running for it; a source this one alone kept running
    /// stops with it.
    fn stop_streams(&mut self, index: usize) {
        let interface = &mut self.interfaces[index];
        if let Some(feedback) = interface
            .run
            .as_mut()
            .and_then(|run| run.playback.as_mut())
            .and_then(|playback| playback.feedback.take())
        {
            let _ = self.transport.iso_stop(feedback.endpoint);
        }
        if interface.feeds.is_none() {
            if let Some(data) = interface.data.take() {
                let _ = self.transport.iso_stop(data.endpoint);
            }
        }
        if let Some(source) = self.interfaces[index].implicit {
            if self.interfaces[source].feeds == Some(index) {
                self.release_source(source);
            }
        }
    }

    /// Capture interface `source` no longer paces anything.
    fn release_source(&mut self, source: usize) {
        let interface = &mut self.interfaces[source];
        interface.feeds = None;
        if interface.run.as_ref().is_none_or(|run| !run.running) {
            if let Some(data) = interface.data.take() {
                let _ = self.transport.iso_stop(data.endpoint);
            }
            if interface.config.is_none() {
                let number = interface.info.interface;
                interface.selected = None;
                let _ = self.transport.set_interface(number, 0);
            }
        }
    }

    /// Read every finished slot of explicit feedback stream of interface
    /// `index` into its pacer, and queue each again.
    fn take_feedback(&mut self, index: usize) {
        let Some(config) = self.interfaces[index].config else {
            return;
        };
        let Some(playback) = self.interfaces[index]
            .run
            .as_mut()
            .and_then(|run| run.playback.as_mut())
        else {
            return;
        };
        let Some(stream) = playback.feedback.as_mut() else {
            return;
        };
        while let Some(done) = stream.take_done() {
            if let Some(region) = self.transport.region(stream.endpoint) {
                for packet in 0..stream.layout.packets {
                    let Ok(record) = stream.layout.record(region, done.slot, packet) else {
                        continue;
                    };
                    let Ok(data) = stream.layout.data(region, done.slot, packet) else {
                        continue;
                    };
                    if record.status != tairix_abi::usb_urb::IsoPacketStatus::Moved {
                        continue;
                    }
                    let report = data.get(..record.length as usize).unwrap_or_default();
                    if let Some(rate) = playback.decoder.decode(report) {
                        playback.pacer.follow(rate, config.interval);
                    }
                }
            }
            if let Err(errno) = self.transport.iso_queue(stream.endpoint, done.slot) {
                stream.halt(errno);
                return;
            }
            stream.mark_queued(done.slot, 0);
        }
    }

    /// Read the rate capture interface `source`'s latest finished slot
    /// carried into the pacer of the playback interface it feeds; with no
    /// capture running there, the slot is queued again at once.
    fn take_implicit(&mut self, source: usize) {
        let Some(index) = self.interfaces[source].feeds else {
            return;
        };
        let (Some(selected), Some(stream)) = (
            self.interfaces[source].selected,
            self.interfaces[source].data.as_ref(),
        ) else {
            return;
        };
        let Ok(alt) = self.interfaces[source].format(selected.format).copied() else {
            return;
        };
        let Ok(interval) = ServiceInterval::isochronous(self.speed, alt.data.interval) else {
            return;
        };
        let shape = SlotShape {
            layout: stream.layout,
            frame_bytes: alt.frame_bytes(),
            silence: alt.format.silence_byte(),
        };
        let endpoint = stream.endpoint;
        let delivering = self.interfaces[source]
            .run
            .as_ref()
            .is_some_and(|run| run.running);
        let Some(latest) = self.interfaces[source]
            .data
            .as_ref()
            .and_then(HwStream::latest_done)
        else {
            return;
        };
        if let Some(region) = self.transport.region(endpoint) {
            let mut timeline = PacketPacer::nominal(selected.rate.hz(), interval.microframes());
            if let Ok(delivered) = deliver_capture(
                shape,
                region,
                latest,
                None,
                &mut timeline,
                interval.microframes(),
            ) {
                let consumer = self.interfaces[index].config.map(|config| config.interval);
                if let (Some(playback), Some(data_interval)) = (
                    self.interfaces[index]
                        .run
                        .as_mut()
                        .and_then(|run| run.playback.as_mut()),
                    consumer,
                ) {
                    if let Some(rate) = playback
                        .decoder
                        .implicit(u64::from(delivered.received), delivered.moved_microframes)
                    {
                        playback.pacer.follow(rate, data_interval);
                    }
                }
            }
        }
        if delivering {
            return;
        }
        let Some(stream) = self.interfaces[source].data.as_mut() else {
            return;
        };
        while let Some(done) = stream.take_done() {
            if let Err(errno) = self.transport.iso_queue(endpoint, done.slot) {
                stream.halt(errno);
                return;
            }
            stream.mark_queued(done.slot, 0);
        }
    }

    /// Restart interface `index`'s streams after a halt that left its device
    /// in place: select its setting again, set its rate again, and start
    /// fresh streams — its feedback stream with its data stream. Frames the
    /// halted stream held are lost.
    fn restart(&mut self, index: usize) -> Result<(), DriverError> {
        let config = self.interfaces[index]
            .config
            .ok_or(DriverError::DeviceFault)?;
        let alt = *self.interfaces[index].format(config.format)?;
        if let Some(stream) = self.interfaces[index].data.take() {
            let _ = self.transport.iso_stop(stream.endpoint);
        }
        let feedback = self.interfaces[index]
            .run
            .as_mut()
            .and_then(|run| run.playback.as_mut())
            .and_then(|playback| playback.feedback.take());
        if let Some(stream) = feedback {
            let _ = self.transport.iso_stop(stream.endpoint);
        }
        self.establish(index, config.format, config.rate)?;
        let stream = if alt.data.is_in() {
            self.start_receiving(alt.data.address, config.layout, config.interval)?
        } else {
            self.start_stream(alt.data.address, config.layout, config.interval)?
        };
        self.interfaces[index].data = Some(stream);
        if let Some(descriptor) = alt.feedback {
            let stream = self.start_feedback(&descriptor)?;
            self.install_feedback(index, stream)?;
        }
        Ok(())
    }

    /// Start playback interface `index`'s explicit feedback stream afresh
    /// after the host controller ended it under a device that is still there.
    fn restart_feedback(&mut self, index: usize, config: Config) -> Result<(), DriverError> {
        let descriptor = self.interfaces[index]
            .format(config.format)?
            .feedback
            .ok_or(DriverError::DeviceFault)?;
        let halted = self.interfaces[index]
            .run
            .as_mut()
            .and_then(|run| run.playback.as_mut())
            .and_then(|playback| playback.feedback.take());
        if let Some(stream) = halted {
            let _ = self.transport.iso_stop(stream.endpoint);
        }
        let stream = self.start_feedback(&descriptor)?;
        self.install_feedback(index, stream)
    }

    /// Hand feedback stream `stream` to playback interface `index`'s pacing,
    /// stopping it again if the interface has none to hand it to.
    fn install_feedback(&mut self, index: usize, stream: HwStream) -> Result<(), DriverError> {
        let Some(playback) = self.interfaces[index]
            .run
            .as_mut()
            .and_then(|run| run.playback.as_mut())
        else {
            let _ = self.transport.iso_stop(stream.endpoint);
            return Err(DriverError::DeviceFault);
        };
        playback.feedback = Some(stream);
        Ok(())
    }

    /// Restart whatever of running interface `index`'s streams the host
    /// controller ended under a device that is still there, counting what
    /// the ended data stream held as lost; one whose device went faults the
    /// endpoint.
    fn recover(&mut self, index: usize, config: Config) -> Result<(), DriverError> {
        let interface = &self.interfaces[index];
        let playback = interface.info.direction == StreamDirection::Playback;
        let data = interface.data.as_ref().and_then(HwStream::halted);
        let feedback = interface
            .run
            .as_ref()
            .and_then(|run| run.playback.as_ref())
            .and_then(|playback| playback.feedback.as_ref())
            .and_then(HwStream::halted);
        if data == Some(Errno::NotFound) || feedback == Some(Errno::NotFound) {
            return Err(DriverError::DeviceFault);
        }
        if data.is_some() {
            let lost = match interface.data.as_ref() {
                Some(stream) if playback => stream.queued_frames(),
                Some(stream) => {
                    u32::from(stream.queued())
                        * slot_frames(stream.layout, config.rate, config.interval)?
                }
                None => 0,
            };
            self.restart(index)?;
            if let Some(run) = self.interfaces[index].run.as_mut() {
                run.xrun_frames = run.xrun_frames.saturating_add(u64::from(lost));
            }
        } else if feedback.is_some() {
            self.restart_feedback(index, config)?;
        }
        if playback {
            self.restart_source(index, config.rate)?;
        }
        Ok(())
    }

    /// Start again the implicit-feedback source pacing playback interface
    /// `index`, if its stream halted under a device that is still there.
    fn restart_source(&mut self, index: usize, rate: Rate) -> Result<(), DriverError> {
        let Some(source) = self.interfaces[index].implicit else {
            return Ok(());
        };
        if self.interfaces[source].feeds != Some(index) {
            return Ok(());
        }
        match self.interfaces[source]
            .data
            .as_ref()
            .and_then(HwStream::halted)
        {
            None => Ok(()),
            Some(Errno::NotFound) => Err(DriverError::DeviceFault),
            Some(_) => {
                if let Some(stream) = self.interfaces[source].data.take() {
                    let _ = self.transport.iso_stop(stream.endpoint);
                }
                // Its setting may have been lost with whatever halted it.
                self.interfaces[source].selected = None;
                self.ensure_source(source, index, rate)
            }
        }
    }

    /// Account every finished slot of playback interface `index` and fill
    /// every free one from `ring` — queued while it clocks, held for its
    /// start while it does not — answering the frames taken and whether a
    /// drain just played out.
    fn service_playback(
        &mut self,
        index: usize,
        ring: &mut PcmRing<'_>,
    ) -> Result<(u32, bool), DriverError> {
        let config = self.interfaces[index]
            .config
            .ok_or(DriverError::DeviceFault)?;
        let alt = *self.interfaces[index].format(config.format)?;
        let shape = SlotShape {
            layout: config.layout,
            frame_bytes: alt.frame_bytes(),
            silence: alt.format.silence_byte(),
        };
        let Self {
            transport,
            interfaces,
            ..
        } = self;
        let interface = &mut interfaces[index];
        let (Some(stream), Some(run)) = (interface.data.as_mut(), interface.run.as_mut()) else {
            return Ok((0, false));
        };
        let Some(playback) = run.playback.as_mut() else {
            return Err(DriverError::DeviceFault);
        };
        while let Some(done) = stream.take_done() {
            let lost = transport
                .region(stream.endpoint)
                .map_or(Ok(done.frames), |region| {
                    playback_losses(shape, region, done.slot, done.frames)
                })?;
            let gap = run.timeline.advance(done.skipped);
            let _ = run.timeline.advance(u32::from(shape.layout.packets));
            run.position = Frames::new(
                run.position
                    .get()
                    .saturating_add(u64::from(done.frames))
                    .saturating_add(gap),
            );
            run.xrun_frames = run
                .xrun_frames
                .saturating_add(u64::from(lost))
                .saturating_add(gap);
            run.sampled_at = Time64::from_nanos(done.completed_at);
        }
        let clocking = run.running;
        let mut taken = 0u32;
        while let Some(slot) = stream.next_free() {
            let mut preview = playback.pacer;
            let (counts, needed) = plan_slot(shape, &mut preview);
            let readable = ring.readable_frames().map_err(|_| DriverError::BadMagic)?;
            // Silence goes only where a clocking device would otherwise run
            // dry: padding a slot it has not yet asked for would turn frames
            // that were merely late into a glitch.
            let shortfall = if readable >= needed {
                Shortfall::Wait
            } else if run.draining {
                if readable == 0 {
                    break;
                }
                Shortfall::Short
            } else if clocking && stream.queued() == 0 {
                Shortfall::Pad
            } else {
                break;
            };
            let region = transport
                .region(stream.endpoint)
                .ok_or(DriverError::DeviceFault)?;
            let filled = fill_playback(shape, region, slot, &counts, Some(&mut *ring), shortfall)?;
            if clocking {
                if let Err(errno) = transport.iso_queue(stream.endpoint, slot) {
                    stream.halt(errno);
                    break;
                }
                stream.mark_queued(slot, filled.carried());
            } else {
                stream.mark_staged(slot, filled.carried());
            }
            playback.pacer = preview;
            run.xrun_frames = run.xrun_frames.saturating_add(u64::from(filled.padded));
            taken += filled.taken;
        }
        let readable = ring.readable_frames().map_err(|_| DriverError::BadMagic)?;
        let drained = clocking && run.draining && stream.queued() == 0 && readable == 0;
        if drained {
            run.running = false;
            run.draining = false;
        }
        Ok((taken, drained))
    }

    /// Deliver every finished slot of capture interface `index` into `ring`
    /// and queue each again, answering the frames written.
    fn service_capture(
        &mut self,
        index: usize,
        ring: &mut PcmRing<'_>,
    ) -> Result<u32, DriverError> {
        let config = self.interfaces[index]
            .config
            .ok_or(DriverError::DeviceFault)?;
        let alt = *self.interfaces[index].format(config.format)?;
        let Self {
            transport,
            interfaces,
            ..
        } = self;
        let interface = &mut interfaces[index];
        let paces = interface.feeds.is_some();
        let (Some(stream), Some(run)) = (interface.data.as_mut(), interface.run.as_mut()) else {
            return Ok(0);
        };
        // A capture pacing a playback runs on whatever its own mixer does.
        let requeue = paces || run.running;
        // The stream's own slots: a stream started to pace a playback was
        // sized before this endpoint was configured.
        let shape = SlotShape {
            layout: stream.layout,
            frame_bytes: alt.frame_bytes(),
            silence: alt.format.silence_byte(),
        };
        let mut written = 0u32;
        let mut failure = None;
        while let Some(done) = stream.take_done() {
            let delivered = transport
                .region(stream.endpoint)
                .ok_or(DriverError::DeviceFault)
                .and_then(|region| {
                    deliver_capture(
                        shape,
                        region,
                        done.slot,
                        Some(&mut *ring),
                        &mut run.timeline,
                        config.interval,
                    )
                });
            // A slot left out of the controller's hands would stall the
            // stream for good, so it goes back whatever its delivery did.
            if requeue && stream.halted().is_none() {
                match transport.iso_queue(stream.endpoint, done.slot) {
                    Ok(()) => stream.mark_queued(done.slot, 0),
                    Err(errno) => stream.halt(errno),
                }
            }
            let delivered = match delivered {
                Ok(delivered) => delivered,
                Err(err) => {
                    failure.get_or_insert(err);
                    continue;
                }
            };
            let gap = run.timeline.advance(done.skipped);
            run.position = Frames::new(
                run.position
                    .get()
                    .saturating_add(u64::from(delivered.received))
                    .saturating_add(u64::from(delivered.missed))
                    .saturating_add(gap),
            );
            run.xrun_frames = run
                .xrun_frames
                .saturating_add(u64::from(delivered.overrun))
                .saturating_add(u64::from(delivered.missed))
                .saturating_add(gap);
            run.sampled_at = Time64::from_nanos(done.completed_at);
            written += delivered.written;
        }
        failure.map_or(Ok(written), Err)
    }

    /// Which running stream `notice`, sent by `origin`, is about: the
    /// interface, and whether it is a feedback stream.
    fn locate(&self, notice: &IsoNotify, origin: ProcId) -> Option<(usize, bool)> {
        self.interfaces
            .iter()
            .enumerate()
            .find_map(|(index, interface)| {
                if interface
                    .data
                    .as_ref()
                    .is_some_and(|stream| stream.hears(notice, origin))
                {
                    return Some((index, false));
                }
                interface
                    .run
                    .as_ref()
                    .and_then(|run| run.playback.as_ref())
                    .and_then(|playback| playback.feedback.as_ref())
                    .filter(|stream| stream.hears(notice, origin))
                    .map(|_| (index, true))
            })
    }

    fn stream_mut(&mut self, index: usize, feedback: bool) -> Option<&mut HwStream> {
        let interface = self.interfaces.get_mut(index)?;
        if feedback {
            interface.run.as_mut()?.playback.as_mut()?.feedback.as_mut()
        } else {
            interface.data.as_mut()
        }
    }

    /// Mark endpoint `index` as having something to report.
    fn signal(&mut self, index: usize) {
        if let Ok(bit) = u32::try_from(index) {
            if bit < tairix_abi::driver::audio::MAX_SIGNALLED_ENDPOINTS {
                self.pending.period_elapsed |= 1 << bit;
            }
        }
    }
}

/// A clock source's frequency request through control interface `control`.
const fn frequency_request(direction: Direction, source: u8, control: u8) -> [u8; 8] {
    requests::entity(
        direction,
        requests::v2::CUR,
        selector::CLOCK_FREQUENCY,
        0,
        source,
        control,
        requests::FREQUENCY_V2_LEN,
    )
}

/// Frames one slot of `layout` carries at `rate`, its intervals `interval`
/// microframes apart, rounded up: the period a grant states.
fn slot_frames(layout: IsoLayout, rate: Rate, interval: u32) -> Result<u32, DriverError> {
    let frames = (u64::from(layout.packets) * u64::from(rate.hz()) * u64::from(interval))
        .div_ceil(MICROFRAMES_PER_SECOND);
    u32::try_from(frames).map_err(|_| DriverError::OutOfRange)
}

/// The standard rates `support` admits, and any rate it lists beyond them,
/// lowest first, at most [`MAX_DEVICE_RATES`].
fn standard_rates_of(support: &RateSupport) -> Result<Vec<Rate>, DriverError> {
    let mut rates: Vec<Rate> = Vec::new();
    rates
        .try_reserve_exact(MAX_DEVICE_RATES)
        .map_err(|_| DriverError::OutOfMemory)?;
    match support {
        RateSupport::Discrete(set) => rates.extend_from_slice(set.rates()),
        RateSupport::Continuous { .. } => {
            rates.extend(
                STANDARD_RATES
                    .into_iter()
                    .filter(|&rate| support.admits(rate)),
            );
        }
    }
    Ok(rates)
}

/// The rate set over `rates`: sorted, each once, the lowest
/// [`MAX_DEVICE_RATES`] kept.
fn rate_set(rates: &mut Vec<Rate>) -> Option<RateSet> {
    rates.sort_unstable();
    rates.dedup();
    rates.truncate(MAX_DEVICE_RATES);
    RateSet::new(rates).ok()
}

/// The gain of the first feature unit on `path` with a volume control.
fn path_gain<T: UacTransport>(
    transport: &mut T,
    topology: &Topology,
    control: u8,
    path: &[u8],
) -> Option<Gain> {
    let (unit, master, channels) =
        path.iter()
            .find_map(|&id| match &topology.entity(id)?.kind {
                EntityKind::Feature {
                    master, channels, ..
                } if master.volume || channels.iter().any(|c| c.volume) => {
                    Some((id, *master, channels.as_slice()))
                }
                _ => None,
            })?;
    read_gain(transport, topology.version, control, unit, master, channels)
}

/// Every route from terminal `terminal`'s clock to a source; version 1.0
/// has no clock entities, so none.
fn terminal_routes<T: UacTransport>(
    transport: &mut T,
    topology: &Topology,
    control: u8,
    terminal: u8,
) -> Result<Vec<ClockRoute>, DriverError> {
    if topology.version == Version::One {
        return Ok(Vec::new());
    }
    let clock = match topology.entity(terminal).map(|entity| &entity.kind) {
        Some(
            EntityKind::InputTerminal { clock, .. } | EntityKind::OutputTerminal { clock, .. },
        ) => *clock,
        _ => 0,
    };
    ClockWalk {
        transport,
        topology,
        control,
        routes: Vec::new(),
    }
    .routes_from(clock)
}

/// The endpoint streaming interface `info` is, read from its device: its
/// gain, its clock routes, and the facts it reports. `None` for one whose
/// rates, once read, include no rate the vocabulary carries.
fn describe<T: UacTransport>(
    transport: &mut T,
    topology: &Topology,
    control: u8,
    speed: UsbSpeed,
    info: StreamingInterface,
    index: u16,
) -> Result<Option<Interface>, DriverError> {
    let path = topology.path_from(info.terminal).unwrap_or_default();
    let far_type = path
        .last()
        .and_then(|&id| topology.entity(id))
        .map_or(0, |entity| match entity.kind {
            EntityKind::InputTerminal { terminal_type, .. }
            | EntityKind::OutputTerminal { terminal_type, .. } => terminal_type,
            _ => 0,
        });
    let gain = path_gain(transport, topology, control, &path);
    let routes = terminal_routes(transport, topology, control, info.terminal)?;
    let mut rates: Vec<Rate> = Vec::new();
    let mut formats = SampleFormats::EMPTY;
    let mut widest: Option<ChannelMap> = None;
    for format in &info.formats {
        formats = formats.with(format.format);
        if widest.is_none_or(|map| format.channel_map.channels() > map.channels()) {
            widest = Some(format.channel_map);
        }
        match format.rates {
            AltRates::Listed(support) => {
                let listed = standard_rates_of(&support)?;
                rates
                    .try_reserve(listed.len())
                    .map_err(|_| DriverError::OutOfMemory)?;
                rates.extend(listed);
            }
            AltRates::Clock => {
                for route in &routes {
                    rates
                        .try_reserve(route.rates.len())
                        .map_err(|_| DriverError::OutOfMemory)?;
                    rates.extend_from_slice(&route.rates);
                }
            }
        }
    }
    // Every setting's interval must be one the bus speed defines.
    for format in &info.formats {
        ServiceInterval::isochronous(speed, format.data.interval)?;
        PeriodicBudget::isochronous(&format.data, speed)?;
    }
    let (Some(rates), Some(channel_map)) = (rate_set(&mut rates), widest) else {
        return Ok(None);
    };
    let facts = AudioEndpointFacts {
        index,
        direction: info.direction,
        jack: JackState::Unknown,
        formats,
        channel_map,
        rates: RateSupport::Discrete(rates),
        min_period_frames: ring_bounds::MIN_FRAMES,
        max_period_frames: MAX_PERIOD_FRAMES,
        max_ring_frames: ring_bounds::MAX_FRAMES,
        gain: gain.as_ref().map(|gain| gain.range),
        name: AudioName::new(terminal_name(far_type)).map_err(|_| DriverError::DeviceFault)?,
    };
    facts.validate().map_err(|_| DriverError::DeviceFault)?;
    Ok(Some(Interface {
        info,
        facts,
        gain,
        routes,
        implicit: None,
        selected: None,
        config: None,
        data: None,
        run: None,
        feeds: None,
        gain_setting: None,
    }))
}

/// Join each asynchronous playback interface with no feedback endpoint of
/// its own to the capture interface whose data packets pace it: one whose
/// data endpoint states implicit-feedback usage, or the one its data
/// endpoint names as its synchronisation endpoint.
fn link_implicit_feedback(interfaces: &mut [Interface]) {
    let paced = |format: &AltFormat| {
        format.feedback.is_none()
            && matches!(
                format.data.kind,
                TransferKind::Isochronous {
                    sync: IsoSync::Asynchronous,
                    ..
                }
            )
    };
    for index in 0..interfaces.len() {
        let interface = &interfaces[index];
        if interface.info.direction != StreamDirection::Playback
            || !interface.info.formats.iter().any(paced)
        {
            continue;
        }
        let names = |address: u8| {
            interface.info.formats.iter().any(|format| {
                format.data.synch_address != 0 && format.data.synch_address == address
            })
        };
        let source = interfaces.iter().position(|candidate| {
            candidate.info.direction == StreamDirection::Capture
                && candidate
                    .info
                    .formats
                    .iter()
                    .any(|format| implicit_source(&format.data) || names(format.data.address))
        });
        interfaces[index].implicit = source;
    }
}

/// Whether `endpoint` states implicit-feedback data usage.
fn implicit_source(endpoint: &EndpointDescriptor) -> bool {
    matches!(
        endpoint.kind,
        TransferKind::Isochronous {
            usage: tairix_usb::periodic::IsoUsage::ImplicitFeedbackData,
            ..
        }
    )
}

impl<T: UacTransport> Audio for UsbAudio<T> {
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        Ok(AudioDeviceFacts {
            endpoints: u16::try_from(self.interfaces.len())
                .map_err(|_| DriverError::DeviceFault)?,
            name: AudioName::new(DEVICE_NAME).map_err(|_| DriverError::DeviceFault)?,
        })
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        Ok(self.interface(endpoint)?.facts)
    }

    fn configure(
        &mut self,
        endpoint: u16,
        params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        let index = usize::from(endpoint);
        let interface = self.interface(endpoint)?;
        if interface.run.as_ref().is_some_and(|run| run.running) {
            return Err(DriverError::Busy);
        }
        let format = self
            .choose_format(index, params)
            .ok_or(DriverError::Unsupported)?;
        let alt = *self.interfaces[index].format(format)?;
        let mut rates = Self::format_rates(&self.interfaces[index], &alt)?;
        let set = rate_set(&mut rates).ok_or(DriverError::Unsupported)?;
        let rate = RateSupport::Discrete(set).nearest(params.rate);
        let (actual, layout, interval, period_frames) = if self.interfaces[index].feeds.is_some() {
            // The stream pacing a playback runs on: its setting, rate and
            // slots stand, and the grant states the slots it actually has.
            if self.interfaces[index].selected.map(|s| (s.format, s.rate)) != Some((format, rate)) {
                return Err(DriverError::Busy);
            }
            let layout = self.interfaces[index]
                .data
                .as_ref()
                .map(|stream| stream.layout)
                .ok_or(DriverError::DeviceFault)?;
            let interval =
                ServiceInterval::isochronous(self.speed, alt.data.interval)?.microframes();
            (rate, layout, interval, slot_frames(layout, rate, interval)?)
        } else {
            // A clock another endpoint holds is refused before the bus
            // changes at all.
            if self.version == Version::Two {
                self.free_routes(index, rate)?;
            }
            // Slots filled ahead of a start were sized for the old
            // configuration.
            self.stop_streams(index);
            let previous = self.interfaces[index].selected;
            let actual = match self.select(index, format, rate) {
                Ok(actual) if set.contains(actual) => actual,
                Ok(_) => {
                    self.restore(index, previous);
                    return Err(DriverError::DeviceFault);
                }
                Err(err) => {
                    self.restore(index, previous);
                    return Err(err);
                }
            };
            let period = params
                .period_frames
                .clamp(ring_bounds::MIN_FRAMES, MAX_PERIOD_FRAMES);
            let (layout, interval, period_frames) = self.layout_for(&alt, actual, period)?;
            (actual, layout, interval, period_frames)
        };
        let grant = ConfigureGrant {
            rate: actual,
            format: alt.format,
            channel_map: alt.channel_map,
            period_frames,
            max_ring_frames: ring_bounds::MAX_FRAMES,
        };
        grant.validate().map_err(|_| DriverError::OutOfRange)?;
        self.interfaces[index].config = Some(Config {
            format,
            rate: actual,
            grant,
            layout,
            interval,
        });
        self.interfaces[index].run = None;
        Ok(grant)
    }

    fn start(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        let index = usize::from(endpoint);
        let config = self
            .interface(endpoint)?
            .config
            .ok_or(DriverError::DeviceFault)?;
        if self.interfaces[index]
            .run
            .as_ref()
            .is_some_and(|run| run.running)
        {
            self.stop_streams(index);
        }
        match self.interfaces[index].info.direction {
            StreamDirection::Playback => {
                if let Err(err) = self.clock_playback(index, config) {
                    self.stop_streams(index);
                    return Err(err);
                }
            }
            StreamDirection::Capture => {
                if self.interfaces[index].data.is_none() {
                    self.establish(index, config.format, config.rate)?;
                    let address = self.interfaces[index].format(config.format)?.data.address;
                    let data = self.start_receiving(address, config.layout, config.interval)?;
                    self.interfaces[index].data = Some(data);
                }
            }
        }
        let sampled_at = Time64::from_nanos(self.transport.now_ns());
        let timeline = PacketPacer::nominal(config.rate.hz(), config.interval);
        let run = self.interfaces[index]
            .run
            .get_or_insert_with(|| Run::held(at, sampled_at, timeline));
        run.position = at;
        run.sampled_at = sampled_at;
        run.timeline = timeline;
        run.running = true;
        run.draining = false;
        run.fault = None;
        Ok(())
    }

    fn stop(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        let index = usize::from(endpoint);
        self.interface(endpoint)?
            .config
            .ok_or(DriverError::DeviceFault)?;
        self.stop_streams(index);
        if let Some(run) = self.interfaces[index].run.as_mut() {
            run.position = at;
            run.running = false;
            run.draining = false;
            run.fault = None;
        }
        Ok(())
    }

    fn drain(&mut self, endpoint: u16) -> Result<(), DriverError> {
        let index = usize::from(endpoint);
        self.interface(endpoint)?
            .config
            .ok_or(DriverError::DeviceFault)?;
        match self.interfaces[index].info.direction {
            StreamDirection::Playback => {
                if let Some(run) = self.interfaces[index].run.as_mut() {
                    run.draining = run.running;
                }
            }
            StreamDirection::Capture => {
                self.stop_streams(index);
                if let Some(run) = self.interfaces[index].run.as_mut() {
                    run.running = false;
                }
            }
        }
        Ok(())
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        let index = usize::from(endpoint);
        let config = self
            .interface(endpoint)?
            .config
            .ok_or(DriverError::DeviceFault)?;
        if ring.geometry().format() != config.grant.format
            || ring.geometry().channels() != config.grant.channel_map.channels()
        {
            return Err(DriverError::BadMagic);
        }
        if let Some(fault) = self.interfaces[index]
            .run
            .as_ref()
            .and_then(|run| run.fault)
        {
            return Err(fault);
        }
        let running = self.interfaces[index]
            .run
            .as_ref()
            .is_some_and(|run| run.running);
        if running {
            if let Err(err) = self.recover(index, config) {
                if let Some(run) = self.interfaces[index].run.as_mut() {
                    run.fault = Some(err);
                }
                return Err(err);
            }
        }
        let playback = self.interfaces[index].info.direction == StreamDirection::Playback;
        // A service ahead of the start fills the slots the start queues; with
        // nothing to hold, nothing is set up.
        if !running
            && playback
            && self.interfaces[index].data.is_none()
            && ring.readable_frames().map_err(|_| DriverError::BadMagic)? != 0
        {
            self.arm_playback(index, config)?;
        }
        let (transferred, drained) = if playback {
            self.service_playback(index, ring)?
        } else {
            (self.service_capture(index, ring)?, false)
        };
        if drained {
            self.stop_streams(index);
        }
        let now = Time64::from_nanos(self.transport.now_ns());
        let report = self.interfaces[index].run.as_ref().map_or(
            AudioServiced {
                transferred,
                running: false,
                position: Frames::ZERO,
                xrun_frames: 0,
                sampled_at: now,
            },
            |run| AudioServiced {
                transferred,
                running: run.running,
                position: run.position,
                xrun_frames: run.xrun_frames,
                sampled_at: run.sampled_at,
            },
        );
        Ok(report)
    }

    fn set_gain(&mut self, endpoint: u16, millibel: i32, mute: bool) -> Result<(), DriverError> {
        self.interface(endpoint)?;
        let index = usize::from(endpoint);
        let setting = GainSetting { millibel, mute };
        self.apply_gain(index, setting)?;
        self.interfaces[index].gain_setting = Some(setting);
        Ok(())
    }

    fn release(&mut self, endpoint: u16) -> Result<(), DriverError> {
        let index = usize::from(endpoint);
        self.interface(endpoint)?;
        self.stop_streams(index);
        let interface = &mut self.interfaces[index];
        interface.run = None;
        interface.config = None;
        if interface.feeds.is_none() && interface.selected.take().is_some() {
            let number = interface.info.interface;
            self.transport.set_interface(number, 0).map_err(refused)?;
        }
        Ok(())
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        while let Some((origin, frame)) = self.transport.next_notice().map_err(refused)? {
            let Ok(notice) = IsoNotify::decode(&frame) else {
                continue;
            };
            let Some((index, feedback)) = self.locate(&notice, origin) else {
                continue;
            };
            match notice {
                IsoNotify::SlotDone {
                    slot,
                    skipped,
                    completed_at,
                    ..
                } => {
                    if let Some(stream) = self.stream_mut(index, feedback) {
                        stream.complete(slot, skipped, completed_at);
                    }
                    if feedback {
                        self.take_feedback(index);
                        continue;
                    }
                    if self.interfaces[index].feeds.is_some() {
                        self.take_implicit(index);
                    }
                }
                IsoNotify::Halted { reason, .. } => {
                    if let Some(stream) = self.stream_mut(index, feedback) {
                        stream.halt(reason);
                    }
                }
            }
            if self.interfaces[index]
                .run
                .as_ref()
                .is_some_and(|run| run.running)
            {
                self.signal(index);
            }
            if let Some(consumer) = self.interfaces[index].feeds {
                if matches!(notice, IsoNotify::Halted { .. }) {
                    self.signal(consumer);
                }
            }
        }
        Ok(core::mem::replace(&mut self.pending, AudioInterrupt::NONE))
    }

    fn set_event_interrupts(&mut self, _enabled: bool) -> Result<(), DriverError> {
        // A stream's notices are bounded by the slots it has queued, so
        // nothing here can storm the driver; there is nothing to mask.
        Ok(())
    }
}

impl<T: UacTransport> UsbAudio<T> {
    /// Set interface `index`'s volume to the device step at or above
    /// `setting`, and its mute — on every channel its feature unit carries
    /// them on.
    fn apply_gain(&mut self, index: usize, setting: GainSetting) -> Result<(), DriverError> {
        let Self {
            transport,
            interfaces,
            version,
            control,
            ..
        } = self;
        let gain = interfaces[index]
            .gain
            .as_ref()
            .ok_or(DriverError::NotImplemented)?;
        let volume = if setting.mute && gain.mute.is_empty() {
            requests::VOLUME_SILENCE
        } else {
            requests::volume_at_least(setting.millibel, gain.min, gain.max, gain.res)
        };
        let set_request = match version {
            Version::One => requests::v1::SET_CUR,
            Version::Two => requests::v2::CUR,
        };
        for &channel in &gain.volume {
            transport
                .control_out(
                    requests::entity(
                        Direction::Out,
                        set_request,
                        selector::VOLUME,
                        channel,
                        gain.unit,
                        *control,
                        requests::VOLUME_LEN,
                    ),
                    &volume.to_le_bytes(),
                )
                .map_err(refused)?;
        }
        for &channel in &gain.mute {
            transport
                .control_out(
                    requests::entity(
                        Direction::Out,
                        set_request,
                        selector::MUTE,
                        channel,
                        gain.unit,
                        *control,
                        1,
                    ),
                    &[u8::from(setting.mute)],
                )
                .map_err(refused)?;
        }
        Ok(())
    }

    /// Start an explicit feedback stream on `descriptor`'s endpoint and queue
    /// every slot of it.
    fn start_feedback(&mut self, descriptor: &EndpointDescriptor) -> Result<HwStream, DriverError> {
        let interval = ServiceInterval::isochronous(self.speed, descriptor.interval)?.microframes();
        let budget = PeriodicBudget::isochronous(descriptor, self.speed)?;
        let packets = (FEEDBACK_SLOT_MICROFRAMES / interval)
            .clamp(1, ISO_MAX_INTERVALS / u32::from(FEEDBACK_SLOTS));
        let layout = IsoLayout::new(
            FEEDBACK_SLOTS,
            u16::try_from(packets).map_err(|_| DriverError::OutOfRange)?,
            budget.max_esit_payload,
        )
        .map_err(|_| DriverError::OutOfRange)?;
        self.start_receiving(descriptor.address, layout, interval)
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
