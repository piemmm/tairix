//! The endpoints a codec's graph presents.
//!
//! Each output connector is routed back to a converter of its own; the
//! analogue pins of one association, ordered by sequence, become one
//! multichannel output whose converters carry successive channel pairs. A
//! pin that finds no converter left to itself shares the front pair of an
//! output it can reach, as a headphone jack beside a speaker does. Each input
//! connector is routed to a converter that can capture it; inputs that share
//! a converter cannot run together, which the engine refuses rather than
//! letting one silently steal the other's.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::driver::audio::{AudioName, ChannelMap, ChannelPosition};
use tairix_abi::DriverError;

use crate::codec::{Function, Widget};
use crate::verb::{Device, WidgetKind};

/// Most widgets a route passes through, converter and pin included. A
/// containment bound on the search a deep or cyclic graph could make.
pub const MAX_HOPS: usize = 8;

/// Most converters one output carries: eight channels, a pair each.
pub const MAX_LANES: usize = 4;

/// One widget of a route, and the entry of its connection list the route
/// takes next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hop {
    /// The widget.
    pub nid: u8,
    /// Where the next hop sits in this widget's connection list; none at the
    /// route's far end.
    pub select: Option<u8>,
}

/// A path through the graph against the flow of samples: from a pin back to
/// the converter feeding it for playback, from a converter back to the pin
/// feeding it for capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    hops: Vec<Hop>,
}

impl Route {
    /// The hops, downstream end first.
    #[must_use]
    pub fn hops(&self) -> &[Hop] {
        &self.hops
    }

    /// The node where samples leave the route: a playback route's pin, a
    /// capture route's converter.
    #[must_use]
    pub fn downstream(&self) -> u8 {
        self.hops.first().map_or(0, |hop| hop.nid)
    }

    /// The node where samples enter the route: a playback route's converter,
    /// a capture route's pin.
    #[must_use]
    pub fn upstream(&self) -> u8 {
        self.hops.last().map_or(0, |hop| hop.nid)
    }
}

/// One playback endpoint of a codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    /// One route per converter, in channel order: the front pair first.
    pub lanes: Vec<Route>,
    /// Further pins that play the front pair.
    pub mirrors: Vec<Route>,
    /// What to call it.
    pub name: AudioName,
    /// Its channels, pair by pair as the lanes carry them.
    pub channel_map: ChannelMap,
    /// It carries digital samples.
    pub digital: bool,
    /// Its pin is an HDMI or `DisplayPort` connector.
    pub display: bool,
}

/// One capture endpoint of a codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Input {
    /// From the converter back to the pin.
    pub route: Route,
    /// What to call it.
    pub name: AudioName,
    /// Its channels.
    pub channel_map: ChannelMap,
}

/// The endpoints one audio function presents.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    /// Its playback endpoints.
    pub outputs: Vec<Output>,
    /// Its capture endpoints.
    pub inputs: Vec<Input>,
}

impl Plan {
    /// The endpoints of `function`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`].
    pub fn of(function: &Function) -> Result<Self, DriverError> {
        Ok(Self {
            outputs: outputs(function)?,
            inputs: inputs(function)?,
        })
    }
}

fn is_output_device(device: Device) -> bool {
    matches!(
        device,
        Device::LineOut
            | Device::Speaker
            | Device::Headphones
            | Device::SpdifOut
            | Device::DigitalOut
    )
}

fn is_input_device(device: Device) -> bool {
    matches!(
        device,
        Device::LineIn
            | Device::Aux
            | Device::Microphone
            | Device::Cd
            | Device::SpdifIn
            | Device::DigitalIn
    )
}

/// Whether `pin` carries digital samples.
fn digital(pin: &Widget) -> bool {
    pin.caps.digital()
        || pin.pin.hdmi()
        || pin.pin.display_port()
        || matches!(
            pin.config.device(),
            Device::SpdifOut | Device::DigitalOut | Device::SpdifIn | Device::DigitalIn
        )
}

/// The pins of `function` that connect to something.
fn connected_pins(function: &Function) -> impl Iterator<Item = &Widget> {
    function
        .widgets()
        .iter()
        .filter(|widget| widget.kind() == WidgetKind::Pin && !widget.config.unconnected())
}

/// The shortest route from `from` back through connection lists to each
/// widget `reached` accepts, nearest first. Breadth-first and bounded by
/// [`MAX_HOPS`], so a cycle or a deep graph costs one visit per widget.
fn routes_from(
    function: &Function,
    from: u8,
    reached: impl Fn(&Widget) -> bool,
    passable: impl Fn(&Widget) -> bool,
) -> Result<Vec<Route>, DriverError> {
    let widgets = function.widgets();
    let index_of = |nid: u8| widgets.binary_search_by_key(&nid, |widget| widget.nid).ok();
    let Some(start) = index_of(from) else {
        return Ok(Vec::new());
    };
    // Each widget's predecessor toward `from`, and its depth.
    let mut previous: Vec<Option<(usize, u8)>> = Vec::new();
    previous
        .try_reserve(widgets.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    previous.resize(widgets.len(), None);
    let mut depth = Vec::new();
    depth
        .try_reserve(widgets.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    depth.resize(widgets.len(), usize::MAX);
    depth[start] = 1;
    let mut queue = VecDeque::new();
    queue
        .try_reserve(widgets.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    queue.push_back(start);
    let mut found = Vec::new();
    while let Some(at) = queue.pop_front() {
        let widget = &widgets[at];
        if at != start && reached(widget) {
            found.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
            found.push(at);
            continue;
        }
        if depth[at] >= MAX_HOPS || (at != start && !passable(widget)) {
            continue;
        }
        for (slot, &source) in widget.sources.iter().enumerate() {
            let Some(next) = index_of(source) else {
                continue;
            };
            if depth[next] != usize::MAX {
                continue;
            }
            depth[next] = depth[at] + 1;
            previous[next] = u8::try_from(slot).ok().map(|slot| (at, slot));
            queue.push_back(next);
        }
    }
    found
        .into_iter()
        .map(|end| {
            let mut hops = Vec::new();
            hops.try_reserve(depth[end])
                .map_err(|_| DriverError::OutOfMemory)?;
            let mut at = end;
            let mut select = None;
            loop {
                hops.push(Hop {
                    nid: widgets[at].nid,
                    select,
                });
                match previous[at] {
                    Some((before, slot)) => {
                        select = Some(slot);
                        at = before;
                    }
                    None => break,
                }
            }
            hops.reverse();
            Ok(Route { hops })
        })
        .collect()
}

fn is_relay(widget: &Widget) -> bool {
    matches!(widget.kind(), WidgetKind::Mixer | WidgetKind::Selector)
}

/// Routes from output pin `pin` back to a converter of the same kind of
/// samples, nearest first.
fn playback_routes(function: &Function, pin: &Widget) -> Result<Vec<Route>, DriverError> {
    let wants_digital = digital(pin);
    routes_from(
        function,
        pin.nid,
        |widget| widget.kind() == WidgetKind::Output && widget.caps.digital() == wants_digital,
        is_relay,
    )
}

/// Pins in the order outputs are built: each association's pins by sequence,
/// associations ascending, then the pins of no association. A digital pin
/// carries its channels in one converter, so it is never one lane of
/// several.
fn output_groups(function: &Function) -> Vec<Vec<&Widget>> {
    let mut pins: Vec<&Widget> = connected_pins(function)
        .filter(|pin| pin.pin.output() && is_output_device(pin.config.device()))
        .collect();
    let key = |pin: &Widget| -> (u8, u8, u8, u8) {
        let association = pin.config.association();
        if (1..15).contains(&association) && !digital(pin) {
            (0, association, pin.config.sequence(), pin.nid)
        } else {
            (1, 0, 0, pin.nid)
        }
    };
    pins.sort_by_key(|pin| key(pin));
    let mut groups: Vec<Vec<&Widget>> = Vec::new();
    let mut open: Option<u8> = None;
    for pin in pins {
        let (ungrouped, association, ..) = key(pin);
        let association = (ungrouped == 0).then_some(association);
        match groups.last_mut() {
            Some(group) if association.is_some() && association == open => group.push(pin),
            _ => groups.push(alloc::vec![pin]),
        }
        open = association;
    }
    groups
}

fn outputs(function: &Function) -> Result<Vec<Output>, DriverError> {
    let mut claimed: Vec<u8> = Vec::new();
    let mut outputs: Vec<Output> = Vec::new();
    let mut strays: Vec<&Widget> = Vec::new();
    for group in output_groups(function) {
        let mut lanes: Vec<Route> = Vec::new();
        let mut mirrors: Vec<Route> = Vec::new();
        for &pin in &group {
            let routes = playback_routes(function, pin)?;
            let free = routes.iter().find(|route| {
                let converter = route.upstream();
                !claimed.contains(&converter) && (lanes.is_empty() || stereo(function, converter))
            });
            match free {
                Some(route) if lanes.len() < MAX_LANES => {
                    claimed.push(route.upstream());
                    lanes.push(route.clone());
                }
                _ => match lanes.first().and_then(|front| {
                    routes
                        .iter()
                        .find(|route| route.upstream() == front.upstream())
                }) {
                    Some(route) => mirrors.push(route.clone()),
                    None => strays.push(pin),
                },
            }
        }
        let Some(front) = lanes.first() else {
            continue;
        };
        let Some(pin) = function.widget(front.downstream()) else {
            continue;
        };
        let Some(channel_map) = lane_map(function, &lanes) else {
            continue;
        };
        outputs.push(Output {
            name: name(pin)?,
            channel_map,
            digital: digital(pin),
            display: pin.pin.hdmi() || pin.pin.display_port(),
            lanes,
            mirrors,
        });
    }
    for pin in strays {
        let routes = playback_routes(function, pin)?;
        if let Some((output, route)) = outputs.iter_mut().find_map(|output| {
            let front = output.lanes.first()?.upstream();
            routes
                .iter()
                .find(|route| route.upstream() == front)
                .map(|route| (output, route.clone()))
        }) {
            output.mirrors.push(route);
        }
    }
    Ok(outputs)
}

/// Whether converter `nid` carries a stereo pair or more.
fn stereo(function: &Function, nid: u8) -> bool {
    function
        .widget(nid)
        .is_some_and(|widget| widget.caps.channels() >= 2)
}

/// The channel map `lanes` carry: a pair per converter, in HDA's sequence
/// order — front, centre and low-frequency, rear, side — or the one channel
/// of a lone mono converter.
fn lane_map(function: &Function, lanes: &[Route]) -> Option<ChannelMap> {
    use ChannelPosition::{
        FrontCentre, FrontLeft, FrontRight, LowFrequency, RearLeft, RearRight, SideLeft, SideRight,
    };
    let front = lanes.first()?.upstream();
    if lanes.len() == 1 && !stereo(function, front) {
        return Some(ChannelMap::MONO);
    }
    let positions: &[ChannelPosition] = match lanes.len() {
        1 => &[FrontLeft, FrontRight],
        2 => &[FrontLeft, FrontRight, RearLeft, RearRight],
        3 => &[
            FrontLeft,
            FrontRight,
            FrontCentre,
            LowFrequency,
            RearLeft,
            RearRight,
        ],
        4 => &[
            FrontLeft,
            FrontRight,
            FrontCentre,
            LowFrequency,
            RearLeft,
            RearRight,
            SideLeft,
            SideRight,
        ],
        _ => return None,
    };
    ChannelMap::new(positions).ok()
}

fn inputs(function: &Function) -> Result<Vec<Input>, DriverError> {
    let pins: Vec<&Widget> = connected_pins(function)
        .filter(|pin| pin.pin.input() && is_input_device(pin.config.device()))
        .collect();
    let mut routes: Vec<Route> = Vec::new();
    for converter in function
        .widgets()
        .iter()
        .filter(|widget| widget.kind() == WidgetKind::Input)
    {
        let wanted = |widget: &Widget| {
            widget.kind() == WidgetKind::Pin
                && pins.iter().any(|pin| pin.nid == widget.nid)
                && digital(widget) == converter.caps.digital()
        };
        routes.extend(routes_from(function, converter.nid, wanted, is_relay)?);
    }
    let mut primaries: Vec<u8> = Vec::new();
    let mut inputs = Vec::new();
    for pin in pins {
        let mut candidates = routes.iter().filter(|route| route.upstream() == pin.nid);
        let chosen = candidates
            .clone()
            .find(|route| !primaries.contains(&route.downstream()))
            .or_else(|| candidates.next());
        let Some(route) = chosen else { continue };
        primaries.push(route.downstream());
        let channel_map = if stereo(function, route.downstream()) {
            ChannelMap::STEREO
        } else {
            ChannelMap::MONO
        };
        inputs.push(Input {
            route: route.clone(),
            name: name(pin)?,
            channel_map,
        });
    }
    Ok(inputs)
}

/// What to call the endpoint `pin` ends: what it is for, and where it is,
/// as much of that as a name holds.
///
/// # Errors
///
/// [`DriverError::Unsupported`] should not even the bare kind fit.
pub fn name(pin: &Widget) -> Result<AudioName, DriverError> {
    let what = match pin.config.device() {
        _ if pin.pin.display_port() => "DisplayPort",
        _ if pin.pin.hdmi() => "HDMI",
        Device::LineOut => "Line Out",
        Device::Speaker => "Speaker",
        Device::Headphones => "Headphones",
        Device::Cd => "CD",
        Device::SpdifOut => "S/PDIF Out",
        Device::DigitalOut => "Digital Out",
        Device::LineIn => "Line In",
        Device::Aux => "Aux",
        Device::Microphone => "Microphone",
        Device::SpdifIn => "S/PDIF In",
        Device::DigitalIn => "Digital In",
        Device::Other => "Audio",
    };
    let display = pin.pin.hdmi() || pin.pin.display_port();
    let place = if display {
        None
    } else if pin.config.fixed() || pin.config.internal() {
        Some("Internal")
    } else {
        pin.config.place()
    };
    let colour = pin
        .config
        .colour()
        .filter(|_| !pin.config.fixed() && !display);
    for details in [[place, colour], [place, None]] {
        let mut text = String::from(what);
        let mut parts = details.into_iter().flatten();
        if let Some(first) = parts.next() {
            let _ = write!(text, " ({first}");
            for part in parts {
                let _ = write!(text, ", {part}");
            }
            text.push(')');
        }
        if let Ok(name) = AudioName::new(&text) {
            return Ok(name);
        }
    }
    AudioName::new(what).map_err(|_| DriverError::Unsupported)
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
