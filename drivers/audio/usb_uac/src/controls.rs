//! The controls a function's endpoints are read through when the driver
//! opens it: the volume range a feature unit offers, and every route from a
//! terminal's clock entity to a clock source it can run from (USB Audio 1.0
//! §5.2.2.4, 2.0 §5.2.5).
//!
//! A control the device will not describe is one the driver does not use:
//! an endpoint with no readable volume range applies its gain in software,
//! and a clock route whose rates cannot be read is no route.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{GainRange, Rate, MAX_DEVICE_RATES, STANDARD_RATES};
use tairix_abi::DriverError;
use tairix_usb::device::CTRL_DATA_LEN;

use crate::engine::UacTransport;
use crate::requests::{self, selector, Direction};
use crate::topology::{EntityKind, FeatureControls, Topology, Version};

/// The hardware gain one endpoint's feature unit offers.
#[derive(Clone, Debug)]
pub(crate) struct Gain {
    pub(crate) unit: u8,
    /// Channels carrying volume: `0` alone for the master channel.
    pub(crate) volume: Vec<u8>,
    /// Channels carrying mute; empty where mute is volume's silence.
    pub(crate) mute: Vec<u8>,
    pub(crate) min: i16,
    pub(crate) max: i16,
    pub(crate) res: i16,
    pub(crate) range: GainRange,
}

/// One way from a terminal to the clock source it runs from.
#[derive(Clone, Debug)]
pub(crate) struct ClockRoute {
    /// Each selector on the way and the pin taken.
    pub(crate) pins: Vec<(u8, u8)>,
    pub(crate) source: u8,
    pub(crate) programmable: bool,
    pub(crate) validity: bool,
    /// The terminal runs at the source's rate times this ratio.
    pub(crate) numerator: u32,
    pub(crate) denominator: u32,
    /// Standard rates the terminal can run at through this route.
    pub(crate) rates: Vec<Rate>,
}

impl ClockRoute {
    /// The frequency the route's source runs at to run the terminal at
    /// `rate`.
    pub(crate) fn source_hz(&self, rate: Rate) -> u64 {
        u64::from(rate.hz()) * u64::from(self.denominator) / u64::from(self.numerator.max(1))
    }
}

/// The volume range of feature unit `unit`, read through control interface
/// `control`; `None` where it offers no volume or will not state its range.
pub(crate) fn read_gain<T: UacTransport>(
    transport: &mut T,
    version: Version,
    control: u8,
    unit: u8,
    master: FeatureControls,
    channels: &[FeatureControls],
) -> Option<Gain> {
    let pick = |wanted: fn(&FeatureControls) -> bool| -> Vec<u8> {
        let mut out = Vec::new();
        if wanted(&master) {
            if out.try_reserve(1).is_ok() {
                out.push(0);
            }
            return out;
        }
        for (number, controls) in (1u8..).zip(channels) {
            if wanted(controls) && out.try_reserve(1).is_ok() {
                out.push(number);
            }
        }
        out
    };
    let volume = pick(|controls| controls.volume);
    let mute = pick(|controls| controls.mute);
    let &first = volume.first()?;
    let (min, max, res) = match version {
        Version::One => {
            let mut read = |request| {
                let mut value = [0u8; 2];
                let n = transport
                    .control_in(
                        requests::entity(
                            Direction::In,
                            request,
                            selector::VOLUME,
                            first,
                            unit,
                            control,
                            requests::VOLUME_LEN,
                        ),
                        &mut value,
                    )
                    .ok()?;
                requests::read_i16(&value[..n])
            };
            (
                read(requests::v1::GET_MIN)?,
                read(requests::v1::GET_MAX)?,
                read(requests::v1::GET_RES)?,
            )
        }
        Version::Two => {
            let block = read_range(transport, control, unit, selector::VOLUME, first, 2)?;
            let mut subranges = (0..).map_while(|at| requests::subrange_i16(&block, at));
            let first_range = subranges.next()?;
            subranges.fold(
                (first_range.min, first_range.max, first_range.res),
                |(min, max, res), range| {
                    let res = match (res, range.res) {
                        (0, other) | (other, 0) => other,
                        (a, b) => a.min(b),
                    };
                    (min.min(range.min), max.max(range.max), res)
                },
            )
        }
    };
    let step = (u32::try_from(i32::from(res.max(1))).ok()? * 100)
        .div_ceil(256)
        .max(1);
    let range = GainRange::new(requests::millibel(min), requests::millibel(max), step).ok()?;
    Some(Gain {
        unit,
        volume,
        mute,
        min,
        max,
        res,
        range,
    })
}

/// A version 2.0 `RANGE` block for control `control_selector` on `channel`
/// of `entity`, of `width`-byte parameters: as many subranges as one data
/// stage carries.
fn read_range<T: UacTransport>(
    transport: &mut T,
    control: u8,
    entity: u8,
    control_selector: u8,
    channel: u8,
    width: u16,
) -> Option<Vec<u8>> {
    let request = |length| {
        requests::entity(
            Direction::In,
            requests::v2::RANGE,
            control_selector,
            channel,
            entity,
            control,
            length,
        )
    };
    let mut count = [0u8; 2];
    let read = transport.control_in(request(2), &mut count).ok()?;
    if read != 2 {
        return None;
    }
    let stated = u16::from_le_bytes(count);
    let fits = u16::try_from((CTRL_DATA_LEN - 2) / (3 * usize::from(width))).ok()?;
    let length = requests::range_len(stated.min(fits), width);
    let mut block = Vec::new();
    block.try_reserve_exact(usize::from(length)).ok()?;
    block.resize(usize::from(length), 0);
    let read = transport.control_in(request(length), &mut block).ok()?;
    block.truncate(read);
    // The count the block carries is the device's; read no further than the
    // subranges actually fetched.
    let held = u16::try_from(block.len().saturating_sub(2) / (3 * usize::from(width))).ok()?;
    let first_two = block.get_mut(..2)?;
    first_two.copy_from_slice(&stated.min(held).to_le_bytes());
    Some(block)
}

/// The walk from a terminal's clock entity to every clock source it can run
/// from: a programmable selector's every pin, a fixed one's current pin, a
/// multiplier's ratio applied on the way.
pub(crate) struct ClockWalk<'a, T: UacTransport> {
    pub(crate) transport: &'a mut T,
    pub(crate) topology: &'a Topology,
    pub(crate) control: u8,
    pub(crate) routes: Vec<ClockRoute>,
}

/// Entities one walk may step through. Each selector pin explores from what
/// its own path has visited, so a lattice of selectors a device describes
/// could otherwise branch without limit.
const CLOCK_WALK_STEPS: u32 = 1_024;

/// Routes one walk collects: past the standard rate family's size, another
/// route adds no rate a mixer can be asked for.
pub(crate) const MAX_CLOCK_ROUTES: usize = MAX_DEVICE_RATES;

impl<T: UacTransport> ClockWalk<'_, T> {
    /// Every route from clock entity `clock`.
    pub(crate) fn routes_from(mut self, clock: u8) -> Result<Vec<ClockRoute>, DriverError> {
        let mut pins = Vec::new();
        let mut steps = CLOCK_WALK_STEPS;
        self.walk(clock, &mut pins, (1, 1), [false; 256], &mut steps)?;
        Ok(self.routes)
    }

    fn walk(
        &mut self,
        at: u8,
        pins: &mut Vec<(u8, u8)>,
        ratio: (u32, u32),
        mut visited: [bool; 256],
        steps: &mut u32,
    ) -> Result<(), DriverError> {
        if *steps == 0 || self.routes.len() >= MAX_CLOCK_ROUTES {
            return Ok(());
        }
        *steps -= 1;
        let Some(entity) = self.topology.entity(at) else {
            return Ok(());
        };
        if core::mem::replace(&mut visited[usize::from(at)], true) {
            return Ok(());
        }
        match &entity.kind {
            EntityKind::ClockSource {
                programmable,
                validity,
            } => {
                let rates = self.source_rates(at, ratio)?;
                if rates.is_empty() {
                    return Ok(());
                }
                let mut taken = Vec::new();
                taken
                    .try_reserve_exact(pins.len())
                    .map_err(|_| DriverError::OutOfMemory)?;
                taken.extend_from_slice(pins);
                self.routes
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
                self.routes.push(ClockRoute {
                    pins: taken,
                    source: at,
                    programmable: *programmable,
                    validity: *validity,
                    numerator: ratio.0,
                    denominator: ratio.1,
                    rates,
                });
            }
            EntityKind::ClockMultiplier { source } => {
                let (Some(numerator), Some(denominator)) = (
                    self.read_cur_u16(selector::NUMERATOR, at),
                    self.read_cur_u16(selector::DENOMINATOR, at),
                ) else {
                    return Ok(());
                };
                if numerator == 0 || denominator == 0 {
                    return Ok(());
                }
                // A ratio past 32 bits cannot be carried exactly, and a
                // rounded one would state rates the clock does not make.
                let (Some(numerator), Some(denominator)) = (
                    ratio.0.checked_mul(numerator),
                    ratio.1.checked_mul(denominator),
                ) else {
                    return Ok(());
                };
                self.walk(*source, pins, (numerator, denominator), visited, steps)?;
            }
            EntityKind::ClockSelector {
                sources,
                programmable,
            } => {
                let sources = sources.clone();
                let pin_count = u8::try_from(sources.len()).unwrap_or(u8::MAX);
                let (first, last) = if *programmable {
                    (1, pin_count)
                } else {
                    match self.read_selector(at) {
                        Some(pin) => (pin, pin),
                        None => return Ok(()),
                    }
                };
                for pin in first..=last {
                    let Some(&next) = pin
                        .checked_sub(1)
                        .and_then(|slot| sources.get(usize::from(slot)))
                    else {
                        continue;
                    };
                    pins.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
                    if *programmable {
                        pins.push((at, pin));
                    }
                    // Each pin explores from what this path has visited, so
                    // two pins may reach the same source.
                    self.walk(next, pins, ratio, visited, steps)?;
                    if *programmable {
                        pins.pop();
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The `CUR` of 2-byte control `control_selector` on clock entity `at`.
    fn read_cur_u16(&mut self, control_selector: u8, at: u8) -> Option<u32> {
        let mut value = [0u8; 2];
        let read = self
            .transport
            .control_in(
                requests::entity(
                    Direction::In,
                    requests::v2::CUR,
                    control_selector,
                    0,
                    at,
                    self.control,
                    2,
                ),
                &mut value,
            )
            .ok()?;
        (read == 2).then(|| u32::from(u16::from_le_bytes(value)))
    }

    /// The pin clock selector `at` has chosen.
    fn read_selector(&mut self, at: u8) -> Option<u8> {
        let mut pin = [0u8; 1];
        let read = self
            .transport
            .control_in(
                requests::entity(
                    Direction::In,
                    requests::v2::CUR,
                    selector::CLOCK_SELECTOR,
                    0,
                    at,
                    self.control,
                    1,
                ),
                &mut pin,
            )
            .ok()?;
        (read == 1).then_some(pin[0])
    }

    /// The standard rates a terminal runs at from clock source `source`, its
    /// rate scaled by `ratio` on the way.
    fn source_rates(
        &mut self,
        source: u8,
        (numerator, denominator): (u32, u32),
    ) -> Result<Vec<Rate>, DriverError> {
        let Some(block) = read_range(
            self.transport,
            self.control,
            source,
            selector::CLOCK_FREQUENCY,
            0,
            4,
        ) else {
            return Ok(Vec::new());
        };
        let mut rates = Vec::new();
        rates
            .try_reserve_exact(MAX_DEVICE_RATES)
            .map_err(|_| DriverError::OutOfMemory)?;
        for rate in STANDARD_RATES {
            let scaled = u64::from(rate.hz()) * u64::from(denominator);
            if scaled % u64::from(numerator) != 0 {
                continue;
            }
            let Ok(at_source) = u32::try_from(scaled / u64::from(numerator)) else {
                continue;
            };
            let admitted = (0..)
                .map_while(|at| requests::subrange_u32(&block, at))
                .any(|range| {
                    at_source >= range.min
                        && at_source <= range.max
                        && (range.res == 0 || (at_source - range.min) % range.res == 0)
                });
            if admitted {
                rates.push(rate);
            }
        }
        Ok(rates)
    }
}
