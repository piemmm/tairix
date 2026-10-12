//! Passive device-class and OS inference: what a host most likely is, from
//! what the map already knows of it, with the evidence each guess rests on.
//!
//! Nothing here sends a packet. Each rule reads one signal — the routing role
//! the host plays, the organisation its hardware address was assigned to, a
//! service type it advertises, a model its TXT data names — and suggests a
//! device class, an OS family, or both, each with a weight out of 100.
//!
//! Weights for one guess combine as independent hints: each removes its share
//! of the doubt the others left. The strongest guess wins, less half the
//! runner-up's score, so conflicting evidence lowers the confidence it is
//! reported with; an exact tie at the top is no guess at all. A host no rule
//! speaks for stays unknown — nothing is guessed from nothing.

use alloc::string::String;
use alloc::vec::Vec;

use crate::discover::AdvertisedService;
use crate::model::{Classification, DeviceClass, OsFamily};
use DeviceClass::{AccessPoint, Desktop, Iot, Laptop, Nas, Phone, Printer, Router, Server, Switch};
use OsFamily::{Android, Ios, Linux, MacOs, NetworkOs, Windows};

/// The routing role a host plays for this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The default route's next hop.
    Gateway,
    /// A named route's next hop.
    Router,
}

/// What inference reads of one host.
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals<'a> {
    /// The routing role it plays, if any.
    pub role: Option<Role>,
    /// The organisation its hardware address was assigned to.
    pub vendor: Option<&'a str>,
    /// The services it advertises.
    pub services: &'a [&'a AdvertisedService],
}

/// One signal and what it suggested, each suggestion with its weight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    /// What was seen.
    pub seen: Seen,
    /// The device class it suggests.
    pub device: Option<(DeviceClass, u8)>,
    /// The OS family it suggests.
    pub os: Option<(OsFamily, u8)>,
}

/// A signal a rule read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// It is this host's default gateway.
    Gateway,
    /// It routes to a network.
    Router,
    /// Its hardware address was assigned to this organisation.
    Vendor(String),
    /// It advertises this service type.
    Service(String),
    /// Its TXT data names this model.
    Model(String),
    /// Its TXT data states a macOS version.
    MacOsVersion,
}

/// What one rule suggests.
#[derive(Clone, Copy, Debug)]
struct Hint {
    device: Option<(DeviceClass, u8)>,
    os: Option<(OsFamily, u8)>,
}

const fn device(class: DeviceClass, weight: u8) -> Hint {
    Hint {
        device: Some((class, weight)),
        os: None,
    }
}

const fn os(family: OsFamily, weight: u8) -> Hint {
    Hint {
        device: None,
        os: Some((family, weight)),
    }
}

const fn both(class: DeviceClass, class_weight: u8, family: OsFamily, family_weight: u8) -> Hint {
    Hint {
        device: Some((class, class_weight)),
        os: Some((family, family_weight)),
    }
}

/// How strongly being the default gateway says "router": it is the machine
/// this host forwards everything off-link through.
const GATEWAY_WEIGHT: u8 = 95;
/// How strongly being a named route's next hop says "router".
const ROUTER_WEIGHT: u8 = 85;

/// What the organisation an address was assigned to says, by the leading
/// words of its registered name (any case). The longest matching entry
/// speaks; a maker of many kinds of thing has no entry.
const VENDORS: &[(&str, Hint)] = &[
    ("amazon technologies", device(Iot, 45)),
    ("arris", device(Router, 45)),
    ("askey computer", device(Router, 40)),
    ("avm audiovisuelles", both(Router, 60, NetworkOs, 40)),
    ("avm gmbh", both(Router, 60, NetworkOs, 40)),
    ("axis communications", device(Iot, 65)),
    ("brother industries", device(Printer, 70)),
    ("canon", device(Printer, 50)),
    ("cisco", both(Router, 40, NetworkOs, 40)),
    ("cisco meraki", both(AccessPoint, 40, NetworkOs, 50)),
    ("cisco-linksys", device(Router, 50)),
    ("commscope", device(Router, 45)),
    ("compal broadband", device(Router, 50)),
    ("d-link", device(Router, 40)),
    ("dell", device(Desktop, 35)),
    ("draytek", both(Router, 60, NetworkOs, 50)),
    ("ecobee", device(Iot, 75)),
    ("eero", both(Router, 60, NetworkOs, 40)),
    ("espressif", device(Iot, 70)),
    ("extreme networks", both(Switch, 50, NetworkOs, 50)),
    ("fortinet", both(Router, 60, NetworkOs, 60)),
    ("freebox", device(Router, 60)),
    ("giga-byte", device(Desktop, 40)),
    ("google", device(Iot, 35)),
    ("guangdong oppo", both(Phone, 60, Android, 60)),
    ("hangzhou hikvision", device(Iot, 65)),
    ("hitron", device(Router, 50)),
    ("hp inc", device(Printer, 35)),
    ("htc", both(Phone, 50, Android, 50)),
    ("intel corporate", device(Desktop, 25)),
    ("intel corporation", device(Desktop, 25)),
    ("juniper networks", both(Router, 50, NetworkOs, 60)),
    ("konica minolta", device(Printer, 65)),
    ("lenovo", device(Laptop, 30)),
    ("lenovo mobile", both(Phone, 50, Android, 40)),
    ("lexmark", device(Printer, 75)),
    ("linksys", device(Router, 50)),
    ("micro-star", device(Desktop, 40)),
    ("microsoft", os(Windows, 35)),
    ("motorola mobility", both(Phone, 60, Android, 60)),
    ("nest labs", device(Iot, 70)),
    ("netapp", device(Nas, 70)),
    ("netgear", device(Router, 45)),
    ("oneplus", both(Phone, 60, Android, 60)),
    ("palo alto networks", both(Router, 60, NetworkOs, 60)),
    ("parallels", device(Desktop, 30)),
    ("pcs systemtechnik", device(Desktop, 30)),
    ("philips lighting", device(Iot, 70)),
    ("qnap", both(Nas, 80, Linux, 50)),
    ("raspberry pi", os(Linux, 70)),
    ("ricoh", device(Printer, 65)),
    ("ring llc", device(Iot, 70)),
    ("roku", device(Iot, 75)),
    ("routerboard.com", both(Router, 60, NetworkOs, 70)),
    ("ruckus wireless", both(AccessPoint, 60, NetworkOs, 50)),
    ("sagemcom", device(Router, 55)),
    ("seiko epson", device(Printer, 65)),
    ("sercomm", device(Router, 40)),
    ("signify", device(Iot, 70)),
    ("sonicwall", both(Router, 60, NetworkOs, 60)),
    ("sonos", device(Iot, 85)),
    ("super micro", device(Server, 60)),
    ("synology", both(Nas, 80, Linux, 50)),
    ("technicolor", device(Router, 50)),
    ("tp-link", device(Router, 25)),
    ("tuya smart", device(Iot, 75)),
    ("ubiquiti", both(AccessPoint, 35, NetworkOs, 40)),
    ("vivo mobile", both(Phone, 60, Android, 60)),
    ("vmware", device(Server, 40)),
    ("western digital", device(Nas, 45)),
    ("xensource", device(Server, 40)),
    ("xerox", device(Printer, 65)),
    ("zebra technologies", device(Printer, 60)),
    ("zhejiang dahua", device(Iot, 65)),
    ("zyxel", device(Router, 45)),
];

/// What advertising a service type says (DNS-SD types match in any case).
const SERVICES: &[(&str, Hint)] = &[
    ("_adisk._tcp", device(Nas, 50)),
    ("_afpovertcp._tcp", device(Nas, 30)),
    ("_airplay._tcp", device(Iot, 30)),
    ("_airport._tcp", device(AccessPoint, 85)),
    ("_amzn-wplay._tcp", device(Iot, 60)),
    ("_androidtvremote2._tcp", both(Iot, 70, Android, 70)),
    ("_apple-mobdev2._tcp", both(Phone, 60, Ios, 80)),
    ("_companion-link._tcp", both(Phone, 30, Ios, 35)),
    ("_daap._tcp", device(Desktop, 25)),
    ("_esphomelib._tcp", device(Iot, 85)),
    ("_googlecast._tcp", device(Iot, 70)),
    ("_hap._tcp", device(Iot, 70)),
    ("_hap._udp", device(Iot, 70)),
    ("_home-assistant._tcp", both(Server, 50, Linux, 60)),
    ("_home-sharing._tcp", device(Desktop, 30)),
    ("_homekit._tcp", device(Iot, 40)),
    ("_hue._tcp", device(Iot, 85)),
    ("_ipp._tcp", device(Printer, 80)),
    ("_ipps._tcp", device(Printer, 80)),
    ("_matter._tcp", device(Iot, 40)),
    ("_matterc._udp", device(Iot, 70)),
    ("_nfs._tcp", device(Nas, 40)),
    ("_nvstream._tcp", both(Desktop, 40, Windows, 30)),
    ("_octoprint._tcp", both(Iot, 70, Linux, 50)),
    ("_pdl-datastream._tcp", device(Printer, 80)),
    ("_printer._tcp", device(Printer, 75)),
    ("_raop._tcp", device(Iot, 30)),
    ("_rfb._tcp", device(Desktop, 35)),
    ("_scanner._tcp", device(Printer, 50)),
    ("_sftp-ssh._tcp", device(Server, 25)),
    ("_sleep-proxy._udp", device(Iot, 40)),
    ("_smb._tcp", device(Nas, 25)),
    ("_sonos._tcp", device(Iot, 85)),
    ("_spotify-connect._tcp", device(Iot, 30)),
    ("_ssh._tcp", device(Server, 25)),
    ("_uscan._tcp", device(Printer, 60)),
    ("_uscans._tcp", device(Printer, 60)),
    ("_workstation._tcp", both(Desktop, 30, Linux, 60)),
];

/// The TXT keys a model is named under: `model` (`_device-info`, `_airplay`),
/// `am` (`_raop`), `md` (`_googlecast`).
const MODEL_KEYS: [&str; 3] = ["model", "am", "md"];

/// The TXT key a Mac states its macOS version under (`_device-info`).
const MACOS_VERSION_KEY: &str = "osxvers";

/// How strongly a stated macOS version says "macOS".
const MACOS_VERSION_WEIGHT: u8 = 95;

/// What a named model says, by its exact leading spelling: the family name
/// before the version digits (`MacBookPro18,3`, `AppleTV11,1`, `Google Nest
/// Mini`). `Xserve`, `RackMac` and `TimeCapsule` say nothing of the OS: file
/// servers of every kind name them to be drawn as one.
const MODELS: &[(&str, Hint)] = &[
    ("AirPort", device(AccessPoint, 85)),
    ("AppleTV", both(Iot, 90, Ios, 80)),
    ("AudioAccessory", both(Iot, 90, Ios, 80)),
    ("Chromecast", device(Iot, 90)),
    ("Google Home", device(Iot, 90)),
    ("Google Nest", device(Iot, 90)),
    ("Mac", both(Desktop, 40, MacOs, 95)),
    ("MacBook", both(Laptop, 95, MacOs, 95)),
    ("MacBookAir", both(Laptop, 95, MacOs, 95)),
    ("MacBookPro", both(Laptop, 95, MacOs, 95)),
    ("MacPro", both(Desktop, 95, MacOs, 95)),
    ("Macmini", both(Desktop, 95, MacOs, 95)),
    ("RackMac", device(Server, 70)),
    ("SHIELD Android TV", both(Iot, 85, Android, 85)),
    ("TimeCapsule", device(Nas, 85)),
    ("Xserve", device(Server, 70)),
    ("iMac", both(Desktop, 95, MacOs, 95)),
    ("iMacPro", both(Desktop, 95, MacOs, 95)),
    ("iPad", both(Phone, 85, Ios, 95)),
    ("iPhone", both(Phone, 95, Ios, 95)),
    ("iPod", both(Phone, 85, Ios, 95)),
];

/// What `signals` suggest the host is, and the evidence that says so.
#[must_use]
pub fn infer(signals: &Signals<'_>) -> (Classification, Vec<Evidence>) {
    let mut evidence = Vec::new();
    match signals.role {
        Some(Role::Gateway) => note(&mut evidence, Seen::Gateway, device(Router, GATEWAY_WEIGHT)),
        Some(Role::Router) => note(&mut evidence, Seen::Router, device(Router, ROUTER_WEIGHT)),
        None => {}
    }
    if let Some(vendor) = signals.vendor {
        if let Some(hint) = longest(VENDORS, vendor, true, |next| !next.is_ascii_alphanumeric()) {
            note(&mut evidence, Seen::Vendor(String::from(vendor)), hint);
        }
    }
    for service in signals.services {
        if let Some((name, hint)) = SERVICES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&service.service))
        {
            note(&mut evidence, Seen::Service(String::from(*name)), *hint);
        }
        for text in &service.text {
            let (key, value) = text.split_once('=').unwrap_or((text, ""));
            if key.eq_ignore_ascii_case(MACOS_VERSION_KEY) {
                note(
                    &mut evidence,
                    Seen::MacOsVersion,
                    os(MacOs, MACOS_VERSION_WEIGHT),
                );
            } else if MODEL_KEYS
                .iter()
                .any(|model| key.eq_ignore_ascii_case(model))
            {
                if let Some(hint) =
                    longest(MODELS, value, false, |next| !next.is_ascii_alphabetic())
                {
                    note(&mut evidence, Seen::Model(String::from(value)), hint);
                }
            }
        }
    }
    let (device, device_confidence) = decide(evidence.iter().filter_map(|e| e.device));
    let (os, os_confidence) = decide(evidence.iter().filter_map(|e| e.os));
    let classification = Classification {
        device,
        device_confidence,
        os,
        os_confidence,
    };
    (classification, evidence)
}

/// Record what `seen` suggests, once however often it is seen.
fn note(evidence: &mut Vec<Evidence>, seen: Seen, hint: Hint) {
    if evidence.iter().all(|known| known.seen != seen) {
        evidence.push(Evidence {
            seen,
            device: hint.device,
            os: hint.os,
        });
    }
}

/// The hint of the longest entry of `rules` that `text` begins with — in any
/// case when `fold_case` — and that ends where `text` does or before a byte
/// `boundary` accepts, so `cisco` speaks for `Cisco Systems` but not
/// `Ciscoe`.
fn longest(
    rules: &[(&str, Hint)],
    text: &str,
    fold_case: bool,
    boundary: fn(&u8) -> bool,
) -> Option<Hint> {
    let text = text.as_bytes();
    rules
        .iter()
        .filter(|(prefix, _)| {
            let prefix = prefix.as_bytes();
            text.get(..prefix.len()).is_some_and(|head| {
                if fold_case {
                    head.eq_ignore_ascii_case(prefix)
                } else {
                    head == prefix
                }
            }) && text.get(prefix.len()).is_none_or(boundary)
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, hint)| *hint)
}

/// The guess `votes` make, with its confidence: each guess's weights combined
/// as independent hints, less half the runner-up's score. An exact tie at the
/// top, or no vote at all, is no guess.
fn decide<T: Copy + PartialEq + Default>(votes: impl Iterator<Item = (T, u8)>) -> (T, u8) {
    // Doubt in hundredths of a percent, so the products stay integers.
    let mut doubts: Vec<(T, u32)> = Vec::new();
    for (guess, weight) in votes {
        let left = u32::from(100 - weight.min(100));
        match doubts.iter_mut().find(|(known, _)| *known == guess) {
            Some((_, doubt)) => *doubt = *doubt * left / 100,
            None => doubts.push((guess, 100 * left)),
        }
    }
    let (mut best, mut best_score, mut runner_up) = (T::default(), 0u32, 0u32);
    for (guess, doubt) in doubts {
        let score = (10_000 - doubt) / 100;
        if score > best_score {
            runner_up = best_score;
            (best, best_score) = (guess, score);
        } else {
            runner_up = runner_up.max(score);
        }
    }
    if best_score == runner_up {
        return (T::default(), 0);
    }
    let confidence = best_score - runner_up / 2;
    (best, u8::try_from(confidence).unwrap_or(100))
}

#[cfg(test)]
#[path = "infer_tests.rs"]
mod tests;
