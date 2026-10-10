//! A codec's audio function, read from what the codec itself states: each
//! widget's capabilities, its connections, and every pin's configuration
//! default (Intel High Definition Audio Specification 1.0a, section 7).
//!
//! There is no quirk table. A board wired contrary to its codec's own
//! defaults gets the answer its codec gave; the alternative is per-machine
//! special cases, which have no place in a driver that names no board.

use alloc::vec::Vec;

use tairix_abi::DriverError;

use crate::format::PcmSupport;
use crate::verb::{
    connection_entries, connection_length, param, subordinates, AmpCaps, PinCaps, PinConfig, Verb,
    WidgetCaps, WidgetKind, AUDIO_FUNCTION_GROUP, GET_CONFIG_DEFAULT, GET_CONNECTION_LIST,
};

/// The verbs a walk issues: answered by the controller's command rings on a
/// machine, or by a modelled codec in a test.
pub trait Verbs {
    /// Send `verb` to the codec at `address` and answer its response.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a codec that does not answer.
    fn exchange(&mut self, address: u8, verb: Verb) -> Result<u32, DriverError>;
}

/// Most connections one widget's list may expand to. A containment bound: a
/// codec stating a range across every node would otherwise make each widget
/// a list of hundreds.
pub const MAX_CONNECTIONS: usize = 32;

/// One widget of an audio function.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Widget {
    /// Its node.
    pub nid: u8,
    /// What it is and what it has.
    pub caps: WidgetCaps,
    /// The nodes it takes input from, in list order.
    pub sources: Vec<u8>,
    /// A pin's capabilities.
    pub pin: PinCaps,
    /// A pin's configuration default.
    pub config: PinConfig,
    /// Its input amplifiers' capabilities.
    pub input_amp: AmpCaps,
    /// Its output amplifier's capabilities.
    pub output_amp: AmpCaps,
    /// A converter's PCM support.
    pub pcm: PcmSupport,
}

impl Widget {
    /// What the widget is.
    #[must_use]
    pub const fn kind(&self) -> WidgetKind {
        self.caps.kind()
    }

    /// Where `source` sits in this widget's connection list.
    #[cfg(test)]
    #[must_use]
    pub fn source_index(&self, source: u8) -> Option<u8> {
        self.sources
            .iter()
            .position(|&nid| nid == source)
            .and_then(|index| u8::try_from(index).ok())
    }
}

/// One audio function group of one codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// The codec's address on the link.
    pub address: u8,
    /// The function group's node.
    pub nid: u8,
    /// The codec's vendor and device.
    pub vendor: u32,
    /// Its widgets, ascending by node.
    widgets: Vec<Widget>,
}

impl Function {
    /// Every audio function of the codec at `address`.
    ///
    /// # Errors
    ///
    /// The codec's failure to answer, or [`DriverError::OutOfMemory`].
    pub fn read_all(verbs: &mut dyn Verbs, address: u8) -> Result<Vec<Self>, DriverError> {
        let vendor = verbs.exchange(address, Verb::parameter(0, param::VENDOR_ID))?;
        let (first, count) =
            subordinates(verbs.exchange(address, Verb::parameter(0, param::NODE_COUNT))?);
        let mut functions = Vec::new();
        for nid in node_range(first, count) {
            let group = verbs.exchange(address, Verb::parameter(nid, param::FUNCTION_GROUP))?;
            if group & 0xFF == AUDIO_FUNCTION_GROUP {
                functions
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
                functions.push(Self::read(verbs, address, nid, vendor)?);
            }
        }
        Ok(functions)
    }

    /// The audio function at node `nid` of the codec at `address`.
    fn read(verbs: &mut dyn Verbs, address: u8, nid: u8, vendor: u32) -> Result<Self, DriverError> {
        let mut ask =
            |node: u8, parameter: u8| verbs.exchange(address, Verb::parameter(node, parameter));
        let defaults = Defaults {
            pcm: PcmSupport::new(ask(nid, param::PCM)?, ask(nid, param::STREAM_FORMATS)?),
            input_amp: AmpCaps(ask(nid, param::INPUT_AMP)?),
            output_amp: AmpCaps(ask(nid, param::OUTPUT_AMP)?),
        };
        let (first, count) = subordinates(ask(nid, param::NODE_COUNT)?);
        let mut widgets = Vec::new();
        widgets
            .try_reserve(usize::from(count))
            .map_err(|_| DriverError::OutOfMemory)?;
        for node in node_range(first, count) {
            widgets.push(read_widget(verbs, address, node, &defaults)?);
        }
        Ok(Self {
            address,
            nid,
            vendor,
            widgets,
        })
    }

    /// A function made of `widgets`, in any order.
    #[cfg(test)]
    #[must_use]
    pub fn of(address: u8, nid: u8, vendor: u32, mut widgets: Vec<Widget>) -> Self {
        widgets.sort_by_key(|widget| widget.nid);
        Self {
            address,
            nid,
            vendor,
            widgets,
        }
    }

    /// The widget at `nid`.
    #[must_use]
    pub fn widget(&self, nid: u8) -> Option<&Widget> {
        self.widgets
            .binary_search_by_key(&nid, |widget| widget.nid)
            .ok()
            .and_then(|at| self.widgets.get(at))
    }

    /// Every widget, ascending by node.
    #[must_use]
    pub fn widgets(&self) -> &[Widget] {
        &self.widgets
    }
}

/// What a function states for widgets that do not override it.
struct Defaults {
    pcm: PcmSupport,
    input_amp: AmpCaps,
    output_amp: AmpCaps,
}

/// The nodes `first` and the `count - 1` after it, cut at the last node an
/// eight-bit id names.
fn node_range(first: u8, count: u8) -> impl Iterator<Item = u8> {
    (u16::from(first)..u16::from(first) + u16::from(count)).filter_map(|nid| u8::try_from(nid).ok())
}

fn read_widget(
    verbs: &mut dyn Verbs,
    address: u8,
    nid: u8,
    defaults: &Defaults,
) -> Result<Widget, DriverError> {
    let mut ask = |parameter: u8| verbs.exchange(address, Verb::parameter(nid, parameter));
    let caps = WidgetCaps(ask(param::WIDGET_CAPS)?);
    let mut widget = Widget {
        nid,
        caps,
        ..Widget::default()
    };
    if matches!(caps.kind(), WidgetKind::Output | WidgetKind::Input) {
        widget.pcm = if caps.format_override() {
            PcmSupport::new(ask(param::PCM)?, ask(param::STREAM_FORMATS)?)
        } else {
            defaults.pcm
        };
    }
    if caps.amp_override() {
        if caps.input_amp() {
            widget.input_amp = AmpCaps(ask(param::INPUT_AMP)?);
        }
        if caps.output_amp() {
            widget.output_amp = AmpCaps(ask(param::OUTPUT_AMP)?);
        }
    } else {
        if caps.input_amp() {
            widget.input_amp = defaults.input_amp;
        }
        if caps.output_amp() {
            widget.output_amp = defaults.output_amp;
        }
    }
    if caps.kind() == WidgetKind::Pin {
        widget.pin = PinCaps(ask(param::PIN_CAPS)?);
        widget.config =
            PinConfig(verbs.exchange(address, Verb::short(nid, GET_CONFIG_DEFAULT, 0))?);
    }
    if caps.connections() {
        widget.sources = read_sources(verbs, address, nid)?;
    }
    Ok(widget)
}

/// A widget's connection list, ranges expanded, at most
/// [`MAX_CONNECTIONS`] long.
fn read_sources(verbs: &mut dyn Verbs, address: u8, nid: u8) -> Result<Vec<u8>, DriverError> {
    let (length, long) =
        connection_length(verbs.exchange(address, Verb::parameter(nid, param::CONNECTION_LENGTH))?);
    let per_answer: u8 = if long { 2 } else { 4 };
    let mut sources = Vec::new();
    let mut previous: Option<u16> = None;
    let mut read = 0u8;
    while read < length {
        let answer = verbs.exchange(address, Verb::short(nid, GET_CONNECTION_LIST, read))?;
        for (entry, range) in connection_entries(answer, long).take(usize::from(length - read)) {
            match (range, previous) {
                (true, Some(start)) if start < entry => {
                    for node in start + 1..=entry {
                        push_source(&mut sources, node)?;
                    }
                }
                _ => push_source(&mut sources, entry)?,
            }
            previous = Some(entry);
        }
        read = read.saturating_add(per_answer);
    }
    Ok(sources)
}

/// Add `node` to `sources`. A node past eight bits names no widget, and an
/// entry past [`MAX_CONNECTIONS`] is one no selection could reach, so both
/// are dropped; the indices of those kept are unchanged.
fn push_source(sources: &mut Vec<u8>, node: u16) -> Result<(), DriverError> {
    let Ok(node) = u8::try_from(node) else {
        return Ok(());
    };
    if sources.len() >= MAX_CONNECTIONS {
        return Ok(());
    }
    sources
        .try_reserve(1)
        .map_err(|_| DriverError::OutOfMemory)?;
    sources.push(node);
    Ok(())
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
