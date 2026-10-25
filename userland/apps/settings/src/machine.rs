//! The machine's boot-time configuration as form rows.
//!
//! The desktop's own settables live in [`crate::form`] over the session's
//! published document; these are the other store — `system.conf`, whose one
//! writer engine is `lib/sysconfig` and whose one command app is
//! `configure`. Settings grows no second writer: a change here is staged,
//! and applying it re-runs `configure` as an account that may.
//!
//! Nothing here performs I/O or holds authority. A row reports the choice
//! the reader made against a working copy of the document; the shell turns
//! the difference into the one elevated command that writes it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::driver::gpio::Polarity;
use tairix_abi::thermal_ipc::{FanMode, FanPolicy, FAN_HYSTERESIS_CELSIUS, FAN_START_CELSIUS};
use tairix_controls::{ComboBox, FieldControl, FieldRow, Slider};
use tairix_sysconfig::{CacheMode, Key, LoginType, NetToggle, Switch, SynCookies, SystemConfig};

use crate::pi::{self, Cooling};

/// What a row states while the machine's configuration has not been read.
const UNREAD: &str = "not read yet";

/// One settable of the machine's boot-time configuration store.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MachineSetting {
    /// Whether the machine starts at a text login or a graphical one.
    LoginType,
    /// The master caching switch, which is a ceiling over every class.
    CacheAll,
    /// The filesystem cache's own switch.
    CacheFilesystem,
    /// The block cache's own switch.
    CacheBlock,
    /// The transform cache's own switch.
    CacheTransform,
    /// The launch cache's own switch.
    CacheSemantic,
    /// Whether the stack uses IPv4 at all.
    NetIpv4Enabled,
    /// Whether the stack uses IPv6 at all.
    NetIpv6Enabled,
    /// Whether the stack also forms temporary IPv6 source addresses.
    NetIpv6Privacy,
    /// How the stack answers a flood of half-open TCP connections.
    NetTcpSynCookies,
    /// Whether an idle TCP connection is probed.
    NetTcpKeepalive,
    /// Whether TCP negotiates explicit congestion notification.
    NetTcpEcn,
    /// How the CPU fan is switched.
    FanMode,
    /// The header line the CPU fan is wired to.
    FanLine,
    /// Which level of the fan's line runs it.
    FanSignal,
    /// The temperature an automatic fan starts at.
    FanStart,
    /// How far a running automatic fan cools the processor before it stops.
    FanHysteresis,
}

impl MachineSetting {
    /// The store key this setting writes.
    #[must_use]
    pub const fn key(self) -> Key {
        match self {
            Self::LoginType => Key::LoginType,
            Self::CacheAll => Key::CacheAll,
            Self::CacheFilesystem => Key::CacheFilesystem,
            Self::CacheBlock => Key::CacheBlock,
            Self::CacheTransform => Key::CacheTransform,
            Self::CacheSemantic => Key::CacheSemantic,
            Self::NetIpv4Enabled => Key::NetIpv4Enabled,
            Self::NetIpv6Enabled => Key::NetIpv6Enabled,
            Self::NetIpv6Privacy => Key::NetIpv6Privacy,
            Self::NetTcpSynCookies => Key::NetTcpSynCookies,
            Self::NetTcpKeepalive => Key::NetTcpKeepalive,
            Self::NetTcpEcn => Key::NetTcpEcn,
            Self::FanMode => Key::CoolingFanMode,
            Self::FanLine => Key::CoolingFanLine,
            Self::FanSignal => Key::CoolingFanSignal,
            Self::FanStart => Key::CoolingFanStart,
            Self::FanHysteresis => Key::CoolingFanHysteresis,
        }
    }

    /// The row's leading label, which is also its search term.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LoginType => "Start up to",
            Self::CacheAll => "Caching",
            Self::CacheFilesystem => "Filesystem cache",
            Self::CacheBlock => "Disk cache",
            Self::CacheTransform => "Decoded-data cache",
            Self::CacheSemantic => "Application-launch cache",
            Self::NetIpv4Enabled => "IPv4",
            Self::NetIpv6Enabled => "IPv6",
            Self::NetIpv6Privacy => "Temporary IPv6 addresses",
            Self::NetTcpSynCookies => "Connection-flood defence",
            Self::NetTcpKeepalive => "Keepalive probes",
            Self::NetTcpEcn => "Congestion notification",
            Self::FanMode => "CPU fan",
            Self::FanLine => "Control line",
            Self::FanSignal => "Control signal",
            Self::FanStart => "Turns on at",
            Self::FanHysteresis => "Turns off after cooling by",
        }
    }

    /// The sentence beneath the label: what choosing this actually does.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::LoginType => {
                "Whether this machine asks for a login at a text prompt or on the desktop. A \
                 machine that cannot draw a desktop asks at the text prompt either way."
            }
            Self::CacheAll => {
                "Whether this machine may keep caches at all. Turning it off disables every \
                 cache below, whatever each of them is set to."
            }
            Self::CacheFilesystem => {
                "Directory listings and file metadata the machine has already read."
            }
            Self::CacheBlock => "Disk blocks the machine has already read.",
            Self::CacheTransform => {
                "Data the machine has already decrypted or decompressed, kept in that form."
            }
            Self::CacheSemantic => "What an application needs to start, kept ready for next time.",
            Self::NetIpv4Enabled => {
                "Whether this machine uses IPv4. With it off no interface takes an IPv4 address \
                 and no program can open an IPv4 connection."
            }
            Self::NetIpv6Enabled => {
                "Whether this machine uses IPv6. With it off no interface forms an IPv6 address \
                 and no program can open an IPv6 connection."
            }
            Self::NetIpv6Privacy => {
                "Whether this machine also forms short-lived IPv6 addresses to start outgoing \
                 connections from, so its traffic is harder to follow between sites."
            }
            Self::NetTcpSynCookies => {
                "How this machine answers a flood of half-finished connections. Automatic keeps a \
                 queue and replies without one once it fills; Always never keeps a queue."
            }
            Self::NetTcpKeepalive => {
                "Whether a connection with nothing to send is probed, so one whose other end has \
                 gone is noticed rather than held open."
            }
            Self::NetTcpEcn => {
                "Whether connections let a congested router say so, instead of it having to drop \
                 packets to signal the same thing."
            }
            Self::FanMode => {
                "Off leaves the fan's line undriven, On runs the fan all the time, and Automatic \
                 runs it by the processor's temperature."
            }
            Self::FanLine => "The GPIO line on the 40-pin header the fan's switch is wired to.",
            Self::FanSignal => {
                "Which level of the line runs the fan: high for a fan switched by an NPN \
                 transistor or an N-channel MOSFET, low for a PNP or P-channel one."
            }
            Self::FanStart => "The temperature an automatic fan starts at.",
            Self::FanHysteresis => {
                "How far the processor cools before a running automatic fan stops, so a fan near \
                 its threshold does not turn on and off."
            }
        }
    }

    /// What a per-class caching row states while the master switch is off.
    const CACHING_OFF: &'static str =
        "Caching is off for the whole machine, so this has no effect.";

    /// What the temporary-address row states while IPv6 itself is off.
    const IPV6_OFF: &'static str = "IPv6 is off for this machine, so this has no effect.";

    /// What a TCP row states while neither address family is on.
    const NO_FAMILY: &'static str =
        "Both IPv4 and IPv6 are off, so this machine makes no connections at all.";

    /// What a fan row states while the fan is off.
    const FAN_OFF: &'static str = "The fan is off, so this has no effect.";

    /// What a temperature row states while the fan runs all the time.
    const FAN_ALWAYS: &'static str = "The fan runs all the time, so this has no effect.";

    /// The sentence this row shows against `config`: its own, plus the
    /// ceiling where something above it has taken its effect away.
    fn stated(self, config: &SystemConfig) -> String {
        let mut text = String::from(self.description());
        let fan = config.cooling_fan;
        let consequence = match self {
            Self::FanStart => Some(alloc::format!("It starts at {} °C.", fan.start_celsius())),
            Self::FanHysteresis => Some(alloc::format!(
                "A running fan stops at {} °C.",
                fan.stop_celsius()
            )),
            _ => None,
        };
        if let Some(consequence) = consequence {
            text.push(' ');
            text.push_str(&consequence);
        }
        if let Some(ceiling) = self.overridden(config) {
            text.push(' ');
            text.push_str(ceiling);
        }
        text
    }

    /// The sentence stating that something above this row has taken its
    /// effect away, or `None` while the row's own value is what applies.
    ///
    /// The row keeps its own value either way, because that is what the
    /// store says and what would take effect if the thing above it came
    /// back on — but a reader who saw `On` and nothing else would
    /// reasonably believe it was running.
    fn overridden(self, config: &SystemConfig) -> Option<&'static str> {
        let (taken, sentence) = match self {
            Self::LoginType | Self::CacheAll | Self::NetIpv4Enabled | Self::NetIpv6Enabled => {
                return None
            }
            Self::CacheFilesystem
            | Self::CacheBlock
            | Self::CacheTransform
            | Self::CacheSemantic => (config.cache_all == Switch::Off, Self::CACHING_OFF),
            Self::NetIpv6Privacy => (
                config.net_ipv6_enabled == NetToggle::Disabled,
                Self::IPV6_OFF,
            ),
            Self::NetTcpSynCookies | Self::NetTcpKeepalive | Self::NetTcpEcn => (
                config.net_ipv4_enabled == NetToggle::Disabled
                    && config.net_ipv6_enabled == NetToggle::Disabled,
                Self::NO_FAMILY,
            ),
            Self::FanMode => return None,
            Self::FanLine | Self::FanSignal => {
                (config.cooling_fan.mode() == FanMode::Off, Self::FAN_OFF)
            }
            Self::FanStart | Self::FanHysteresis => match config.cooling_fan.mode() {
                FanMode::Off => (true, Self::FAN_OFF),
                FanMode::On => (true, Self::FAN_ALWAYS),
                FanMode::Automatic => (false, Self::FAN_OFF),
            },
        };
        taken.then_some(sentence)
    }

    /// The choices this setting offers, in order, and which of them the
    /// store currently holds.
    fn choices(self, config: &SystemConfig) -> (Vec<String>, usize) {
        match self {
            Self::LoginType => pick(
                &[LoginType::Graphical, LoginType::Text],
                config.login_type,
                login_label,
            ),
            Self::CacheAll => pick(&[Switch::On, Switch::Off], config.cache_all, switch_label),
            Self::CacheFilesystem
            | Self::CacheBlock
            | Self::CacheTransform
            | Self::CacheSemantic => pick(
                &[CacheMode::Auto, CacheMode::Off],
                self.mode(config),
                mode_label,
            ),
            Self::NetIpv4Enabled
            | Self::NetIpv6Enabled
            | Self::NetIpv6Privacy
            | Self::NetTcpKeepalive
            | Self::NetTcpEcn => pick(
                &[NetToggle::Enabled, NetToggle::Disabled],
                self.toggle(config),
                toggle_label,
            ),
            Self::NetTcpSynCookies => pick(
                &[SynCookies::Auto, SynCookies::Always],
                config.net_tcp_syncookies,
                syncookies_label,
            ),
            Self::FanMode => pick(&FanMode::ALL, config.cooling_fan.mode(), fan_mode_label),
            Self::FanSignal => pick(
                &Polarity::ALL,
                config.cooling_fan.polarity(),
                polarity_label,
            ),
            Self::FanLine | Self::FanStart | Self::FanHysteresis => (Vec::new(), 0),
        }
    }

    /// The stack-wide switch this setting holds in `config`.
    ///
    /// Its own value, not its effective one, for the same reason a cache
    /// class keeps its own: an effective value would silently rewrite what
    /// the store says the moment the switch above it went off.
    fn toggle(self, config: &SystemConfig) -> NetToggle {
        match self {
            Self::NetIpv4Enabled => config.net_ipv4_enabled,
            Self::NetIpv6Enabled => config.net_ipv6_enabled,
            Self::NetIpv6Privacy => config.net_ipv6_privacy,
            Self::NetTcpKeepalive => config.net_tcp_keepalive,
            Self::NetTcpEcn => config.net_tcp_ecn,
            Self::LoginType
            | Self::CacheAll
            | Self::CacheFilesystem
            | Self::CacheBlock
            | Self::CacheTransform
            | Self::CacheSemantic
            | Self::NetTcpSynCookies
            | Self::FanMode
            | Self::FanLine
            | Self::FanSignal
            | Self::FanStart
            | Self::FanHysteresis => NetToggle::Disabled,
        }
    }

    /// The per-class mode this setting holds in `config`.
    ///
    /// The class's **own** value, not its effective one: the effective mode
    /// folds in the master switch, and a row that showed it would silently
    /// rewrite what the store says the moment the master switch went off.
    fn mode(self, config: &SystemConfig) -> CacheMode {
        match self {
            Self::CacheFilesystem => config.cache_filesystem,
            Self::CacheBlock => config.cache_block,
            Self::CacheTransform => config.cache_transform,
            Self::CacheSemantic => config.cache_semantic,
            Self::LoginType
            | Self::CacheAll
            | Self::NetIpv4Enabled
            | Self::NetIpv6Enabled
            | Self::NetIpv6Privacy
            | Self::NetTcpSynCookies
            | Self::NetTcpKeepalive
            | Self::NetTcpEcn
            | Self::FanMode
            | Self::FanLine
            | Self::FanSignal
            | Self::FanStart
            | Self::FanHysteresis => CacheMode::Auto,
        }
    }

    /// Write the choice at `index` onto `config`, answering whether it
    /// named one this setting offers.
    ///
    /// Fails closed: an index outside the list this very surface built
    /// changes nothing, so a routing defect cannot stage a value the reader
    /// did not choose.
    pub(crate) fn adopt(self, index: usize, config: &mut SystemConfig, cooling: &Cooling) -> bool {
        let fan = config.cooling_fan;
        let refan = |mode, line, polarity, start, hysteresis| {
            FanPolicy::new(mode, line, polarity, start, hysteresis).ok()
        };
        let staged = match self {
            Self::FanMode => FanMode::ALL.get(index).and_then(|&mode| {
                refan(
                    mode,
                    fan.line(),
                    fan.polarity(),
                    fan.start_celsius(),
                    fan.hysteresis_celsius(),
                )
            }),
            Self::FanLine => line_choices(config, cooling).get(index).and_then(|&line| {
                refan(
                    fan.mode(),
                    line,
                    fan.polarity(),
                    fan.start_celsius(),
                    fan.hysteresis_celsius(),
                )
            }),
            Self::FanSignal => Polarity::ALL.get(index).and_then(|&polarity| {
                refan(
                    fan.mode(),
                    fan.line(),
                    polarity,
                    fan.start_celsius(),
                    fan.hysteresis_celsius(),
                )
            }),
            Self::FanStart => nth_in(&FAN_START_CELSIUS, index).and_then(|start| {
                refan(
                    fan.mode(),
                    fan.line(),
                    fan.polarity(),
                    start,
                    fan.hysteresis_celsius(),
                )
            }),
            Self::FanHysteresis => nth_in(&FAN_HYSTERESIS_CELSIUS, index).and_then(|hysteresis| {
                refan(
                    fan.mode(),
                    fan.line(),
                    fan.polarity(),
                    fan.start_celsius(),
                    hysteresis,
                )
            }),
            _ => return self.adopt_choice(index, config),
        };
        match staged {
            Some(policy) => {
                config.cooling_fan = policy;
                true
            }
            None => false,
        }
    }

    /// [`Self::adopt`] for a setting whose choices do not depend on the fan.
    fn adopt_choice(self, index: usize, config: &mut SystemConfig) -> bool {
        match self {
            Self::LoginType => set(
                &[LoginType::Graphical, LoginType::Text],
                index,
                &mut config.login_type,
            ),
            Self::CacheAll => set(&[Switch::On, Switch::Off], index, &mut config.cache_all),
            Self::CacheFilesystem => modes(index, &mut config.cache_filesystem),
            Self::CacheBlock => modes(index, &mut config.cache_block),
            Self::CacheTransform => modes(index, &mut config.cache_transform),
            Self::CacheSemantic => modes(index, &mut config.cache_semantic),
            Self::NetIpv4Enabled => toggles(index, &mut config.net_ipv4_enabled),
            Self::NetIpv6Enabled => toggles(index, &mut config.net_ipv6_enabled),
            Self::NetIpv6Privacy => toggles(index, &mut config.net_ipv6_privacy),
            Self::NetTcpKeepalive => toggles(index, &mut config.net_tcp_keepalive),
            Self::NetTcpEcn => toggles(index, &mut config.net_tcp_ecn),
            Self::NetTcpSynCookies => set(
                &[SynCookies::Auto, SynCookies::Always],
                index,
                &mut config.net_tcp_syncookies,
            ),
            Self::FanMode
            | Self::FanLine
            | Self::FanSignal
            | Self::FanStart
            | Self::FanHysteresis => false,
        }
    }

    /// The value `config` holds for this setting, spelled as the store
    /// spells it — which is what an elevated `configure` is handed.
    #[must_use]
    pub fn value(self, config: &SystemConfig) -> String {
        config.render_value(self.key())
    }

    /// The row this setting draws for `config`, or the row it draws while
    /// the store has not been read.
    ///
    /// An unread store is not a store of defaults: showing one would be a
    /// value the reader could not account for and could not have set. The
    /// row says so instead, and offers nothing to change until the reading
    /// lands.
    pub(crate) fn row(self, config: Option<&SystemConfig>, cooling: &Cooling) -> FieldRow {
        let Some(config) = config else {
            return FieldRow::new(self.label(), FieldControl::Unmeasured(String::from(UNREAD)))
                .with_description(self.description());
        };
        let control = match self {
            Self::FanLine => {
                let mut text = self.stated(config);
                if let Some(in_use) = in_use_sentence(config, cooling) {
                    text.push(' ');
                    text.push_str(&in_use);
                }
                return FieldRow::new(self.label(), line_control(config, cooling))
                    .with_description(text);
            }
            Self::FanStart => scale(&FAN_START_CELSIUS, config.cooling_fan.start_celsius()),
            Self::FanHysteresis => scale(
                &FAN_HYSTERESIS_CELSIUS,
                config.cooling_fan.hysteresis_celsius(),
            ),
            _ => {
                let (choices, current) = self.choices(config);
                let mut combo = ComboBox::new(choices);
                combo.set_selected(current);
                FieldControl::Combo(combo)
            }
        };
        FieldRow::new(self.label(), control).with_description(self.stated(config))
    }
}

/// The lines the fan may be put on, in the order the line row offers them:
/// none, every line the header lends that is free, and the line the store
/// names even where it is not, so the row can show what is set.
fn line_choices(config: &SystemConfig, cooling: &Cooling) -> Vec<Option<u8>> {
    let mut choices = alloc::vec![None];
    if let Some(lines) = cooling.status().and_then(|status| status.lines) {
        choices.extend(lines.free().iter().map(Some));
    }
    if let Some(held) = config.cooling_fan.line() {
        if !choices.contains(&Some(held)) {
            choices.push(Some(held));
        }
    }
    choices
}

/// The line row's control: a choice of the lines the fan may be put on, once
/// the cooling service has said which.
fn line_control(config: &SystemConfig, cooling: &Cooling) -> FieldControl {
    if cooling.status().is_none() {
        return FieldControl::Unmeasured(String::from(UNREAD));
    }
    let choices = line_choices(config, cooling);
    let mut combo = ComboBox::new(
        choices
            .iter()
            .map(|line| line.map_or_else(|| String::from("None"), pi::line_label))
            .collect(),
    );
    let held = config.cooling_fan.line();
    combo.set_selected(choices.iter().position(|line| *line == held).unwrap_or(0));
    FieldControl::Combo(combo)
}

/// Why the header's other lines are not offered, where any are not: those
/// something else holds, but the one the row shows as staged.
fn in_use_sentence(config: &SystemConfig, cooling: &Cooling) -> Option<String> {
    let staged = config.cooling_fan.line();
    let named: Vec<String> = cooling
        .status()?
        .lines?
        .held()
        .iter()
        .filter(|line| Some(*line) != staged)
        .map(pi::line_label)
        .collect();
    if named.is_empty() {
        return None;
    }
    Some(alloc::format!(
        "In use and not offered: {}.",
        named.join(", ")
    ))
}

/// The `index`th whole degree of `range`.
fn nth_in(range: &core::ops::RangeInclusive<u8>, index: usize) -> Option<u8> {
    let offset = u8::try_from(index).ok()?;
    range
        .start()
        .checked_add(offset)
        .filter(|degrees| range.contains(degrees))
}

/// A slider over the whole degrees of `range`, at `current`.
fn scale(range: &core::ops::RangeInclusive<u8>, current: u8) -> FieldControl {
    let stops = u16::from(range.end() - range.start()) + 1;
    let mut slider = Slider::new(0).with_stops(stops).with_ends(
        alloc::format!("{} °C", range.start()),
        alloc::format!("{} °C", range.end()),
    );
    let at = u16::from(current.saturating_sub(*range.start()));
    slider.set_value(slider.stop_value(at).unwrap_or(0));
    FieldControl::Slider(slider)
}

/// `values`' labels, and the index of `current` among them.
fn pick<T: Copy + PartialEq>(
    values: &[T],
    current: T,
    label: fn(T) -> &'static str,
) -> (Vec<String>, usize) {
    (
        values
            .iter()
            .map(|value| label(*value).to_string())
            .collect(),
        values.iter().position(|v| *v == current).unwrap_or(0),
    )
}

/// Set `field` to `values[index]`, answering whether the index named one.
fn set<T: Copy>(values: &[T], index: usize, field: &mut T) -> bool {
    match values.get(index) {
        Some(value) => {
            *field = *value;
            true
        }
        None => false,
    }
}

/// The per-class mode ladder, shared by all four class rows.
fn modes(index: usize, field: &mut CacheMode) -> bool {
    set(&[CacheMode::Auto, CacheMode::Off], index, field)
}

/// The on/off ladder, shared by every stack-wide network switch.
fn toggles(index: usize, field: &mut NetToggle) -> bool {
    set(&[NetToggle::Enabled, NetToggle::Disabled], index, field)
}

/// The display label of a login type. Distinct from the document spelling
/// on purpose: one is what a reader reads, the other what the store holds.
const fn login_label(kind: LoginType) -> &'static str {
    match kind {
        LoginType::Graphical => "The desktop",
        LoginType::Text => "A text prompt",
    }
}

/// The display label of the master caching switch.
const fn switch_label(switch: Switch) -> &'static str {
    match switch {
        Switch::On => "On",
        Switch::Off => "Off",
    }
}

/// The display label of a per-class caching mode.
///
/// There is deliberately no forced-on choice: a class is either governed by
/// the memory-pressure governor or hard-disabled, and offering a third
/// would promise something the store cannot hold.
const fn mode_label(mode: CacheMode) -> &'static str {
    match mode {
        CacheMode::Auto => "Automatic",
        CacheMode::Off => "Off",
    }
}

/// The display label of a stack-wide network switch.
const fn toggle_label(toggle: NetToggle) -> &'static str {
    match toggle {
        NetToggle::Enabled => "On",
        NetToggle::Disabled => "Off",
    }
}

/// The display label of the connection-flood defence policy.
///
/// `Always` is spelled as what it costs rather than as a bare word: a
/// reader choosing it is giving up the half-open queue, and the choice list
/// is the only place that is said.
const fn syncookies_label(mode: SynCookies) -> &'static str {
    match mode {
        SynCookies::Auto => "Automatic",
        SynCookies::Always => "Always, keeping no queue",
    }
}

/// The display label of a fan mode.
const fn fan_mode_label(mode: FanMode) -> &'static str {
    match mode {
        FanMode::Off => "Off",
        FanMode::On => "On",
        FanMode::Automatic => "Automatic",
    }
}

/// The display label of a fan's signal polarity.
const fn polarity_label(polarity: Polarity) -> &'static str {
    match polarity {
        Polarity::ActiveHigh => "On when high",
        Polarity::ActiveLow => "On when low",
    }
}
