//! The CTA-861 extension: data blocks naming the sink's video codes, audio,
//! speakers and HDMI capabilities, then detailed timings.

use tairix_abi::driver::display::Timing;

use crate::audio::Speakers;
use crate::edid::{Edid, Hdmi, HdmiForum, Mode, VideoCode, BLOCK_LEN};
use crate::tables::{Vic, HDMI_VICS, VICS};
use crate::timing::{detailed, Aspect, DETAILED_LEN};

/// The extension's tag.
pub(crate) const TAG: u8 = 0x02;

const BLOCK_AUDIO: u8 = 1;
const BLOCK_VIDEO: u8 = 2;
const BLOCK_VENDOR: u8 = 3;
const BLOCK_SPEAKERS: u8 = 4;
const BLOCK_EXTENDED: u8 = 7;

const EXTENDED_VIDEO_CAPABILITY: u8 = 0x00;
const EXTENDED_HF_EEODB: u8 = 0x78;
const EXTENDED_HF_SCDB: u8 = 0x79;

/// The IEEE OUIs vendor blocks open with, least significant byte first.
const OUI_HDMI: [u8; 3] = [0x03, 0x0C, 0x00];
const OUI_HDMI_FORUM: [u8; 3] = [0xD8, 0x5D, 0xC4];

/// Bytes an HDMI Forum block holds at the least, beside its header: its
/// rates and flags all present.
const FORUM_LEN: usize = 7;

/// Where a CTA block's data block collection ends and its detailed timings
/// begin, when the block states a usable place.
fn detailed_start(block: &[u8; BLOCK_LEN]) -> Option<usize> {
    let start = usize::from(block[2]);
    (4..BLOCK_LEN).contains(&start).then_some(start)
}

/// Each data block of `block`'s collection: its tag and payload. A block
/// running past the collection ends it.
fn data_blocks(block: &[u8; BLOCK_LEN]) -> impl Iterator<Item = (u8, &[u8])> {
    let end = if block[1] >= 3 {
        detailed_start(block).unwrap_or(4)
    } else {
        4
    };
    let mut at = 4;
    core::iter::from_fn(move || {
        let header = *block.get(at).filter(|_| at < end)?;
        let payload = block
            .get(at + 1..at + 1 + usize::from(header & 0x1F))
            .filter(|payload| at + 1 + payload.len() <= end)?;
        at += 1 + payload.len();
        Some((header >> 5, payload))
    })
}

/// The extension count an HDMI Forum override states, when `first`, the
/// first extension, opens its collection with one.
pub(crate) fn override_count(first: &[u8; BLOCK_LEN]) -> Option<usize> {
    if first[0] != TAG {
        return None;
    }
    match data_blocks(first).next()? {
        (BLOCK_EXTENDED, [EXTENDED_HF_EEODB, count, ..]) => Some(usize::from(*count)),
        _ => None,
    }
}

/// Read `block`, a CTA-861 extension, into `edid`.
pub(crate) fn read(block: &[u8; BLOCK_LEN], edid: &mut Edid) {
    if block[1] >= 2 {
        edid.audio.basic |= block[3] & 0x40 != 0;
    }
    for (tag, payload) in data_blocks(block) {
        match tag {
            BLOCK_AUDIO => edid.audio.add_descriptors(payload),
            BLOCK_VIDEO => add_video_codes(payload, edid),
            BLOCK_SPEAKERS => {
                if let [first, second, third, ..] = *payload {
                    edid.audio.speakers = Some(Speakers::decode([first, second, third]));
                }
            }
            BLOCK_VENDOR => read_vendor(payload, edid),
            BLOCK_EXTENDED => read_extended(payload, edid),
            _ => {}
        }
    }
    let Some(start) = detailed_start(block) else {
        return;
    };
    let (descriptors, _) = block[start..BLOCK_LEN - 1].as_chunks::<DETAILED_LEN>();
    for descriptor in descriptors {
        if descriptor[0..2] == [0, 0] {
            break;
        }
        if let Some((timing, _)) = detailed(descriptor) {
            edid.add(Mode {
                timing,
                code: None,
                preferred: false,
            });
        }
    }
}

/// How far, in kHz, a timing's clock may lie from a code's, or from the
/// thousand-and-first slower variant every 60 Hz code also names.
const CLOCK_TOLERANCE_KHZ: u32 = 5;

/// Whether `entry` is the timing `timing` is, at either of its rates.
fn names(entry: &Vic, timing: &Timing) -> bool {
    let near = |clock: u32| timing.clock_khz.abs_diff(clock) <= CLOCK_TOLERANCE_KHZ;
    let clock = entry.timing.clock_khz;
    Timing {
        clock_khz: timing.clock_khz,
        ..entry.timing
    } == *timing
        && (near(clock) || near(clock * 1000 / 1001))
}

/// The CTA-861 code naming `timing`, a 59.94 Hz variant's included: what a
/// source names a timing by in its AVI `InfoFrame` whatever list the sink
/// gave it in. Of the codes two aspect ratios share, the lower.
#[must_use]
pub fn video_code(timing: &Timing) -> Option<VideoCode> {
    VICS.iter()
        .find(|entry| names(entry, timing))
        .map(|entry| VideoCode::Cta {
            vic: entry.code,
            aspect: entry.aspect,
        })
}

/// The HDMI 1.4 code naming `timing`: what a source names one of those four
/// 4K timings by, in its vendor-specific `InfoFrame`, to any HDMI sink —
/// every one takes it, where a sink older than CTA-861-F knows no CTA code
/// for them.
#[must_use]
pub fn hdmi_video_code(timing: &Timing) -> Option<VideoCode> {
    HDMI_VICS
        .iter()
        .find(|entry| names(entry, timing))
        .map(|entry| VideoCode::Hdmi(entry.code))
}

/// The picture aspect HDMI 1.4 code `vic`'s timing carries.
pub(crate) fn hdmi_aspect(vic: u8) -> Option<Aspect> {
    HDMI_VICS
        .iter()
        .find(|entry| entry.code == vic)
        .map(|entry| entry.aspect)
}

/// The timing CTA-861 video code `vic` names, and the aspect it carries.
fn cta_code(vic: u8) -> Option<Mode> {
    let index = match vic {
        1..=127 => usize::from(vic) - 1,
        193..=219 => usize::from(vic) - 193 + 127,
        _ => return None,
    };
    VICS.get(index)
        .filter(|entry| entry.code == vic)
        .map(|entry| Mode {
            timing: entry.timing,
            code: Some(VideoCode::Cta {
                vic,
                aspect: entry.aspect,
            }),
            preferred: false,
        })
}

fn add_video_codes(descriptors: &[u8], edid: &mut Edid) {
    for &descriptor in descriptors {
        // Codes 1 to 64 carry a native flag in their top bit.
        let vic = match descriptor {
            1..=64 | 129..=192 => descriptor & 0x7F,
            other => other,
        };
        if let Some(mode) = cta_code(vic) {
            edid.add(mode);
        }
    }
}

fn read_vendor(payload: &[u8], edid: &mut Edid) {
    match payload.get(..3) {
        Some(oui) if oui == OUI_HDMI => read_hdmi(payload, edid),
        Some(oui) if oui == OUI_HDMI_FORUM => read_forum(payload, edid),
        _ => {}
    }
}

/// The HDMI 1.4 vendor block: an HDMI sink's physical address, its fastest
/// TMDS clock, and the HDMI video codes it takes.
fn read_hdmi(payload: &[u8], edid: &mut Edid) {
    let Some(&[high, low]) = payload.get(3..5) else {
        return;
    };
    let max_tmds_khz = payload
        .get(6)
        .filter(|&&rate| rate != 0)
        .map(|&rate| u32::from(rate) * 5_000);
    let hdmi = edid.hdmi.get_or_insert(Hdmi {
        physical_address: 0,
        max_tmds_khz: None,
        forum: None,
    });
    hdmi.physical_address = u16::from_be_bytes([high, low]);
    hdmi.max_tmds_khz = max_tmds_khz;
    let Some(&flags) = payload.get(7) else {
        return;
    };
    if flags & 0x20 == 0 {
        return;
    }
    let latency = match (flags & 0x80 != 0, flags & 0x40 != 0) {
        (true, true) => 4,
        (true, false) => 2,
        (false, _) => 0,
    };
    let Some(&lengths) = payload.get(9 + latency) else {
        return;
    };
    let codes = payload
        .get(10 + latency..)
        .unwrap_or_default()
        .iter()
        .take(usize::from(lengths >> 5));
    for &code in codes {
        if let Some(entry) = usize::from(code)
            .checked_sub(1)
            .and_then(|index| HDMI_VICS.get(index))
        {
            edid.add(Mode {
                timing: entry.timing,
                code: Some(VideoCode::Hdmi(code)),
                preferred: false,
            });
        }
    }
}

/// An HDMI Forum block, vendor-specific or the HDMI 2.1 extended one: the
/// two lay out their rates and flags at the same offsets.
fn read_forum(payload: &[u8], edid: &mut Edid) {
    if payload.len() < FORUM_LEN {
        return;
    }
    let flags = payload[5];
    let scdc = flags & 0x80 != 0;
    let forum = HdmiForum {
        max_tmds_khz: Some(u32::from(payload[4]) * 5_000).filter(|&rate| rate > 340_000),
        scdc,
        read_request: scdc && flags & 0x40 != 0,
        scrambles_below_340: scdc && payload[4] != 0 && flags & 0x08 != 0,
    };
    edid.hdmi
        .get_or_insert(Hdmi {
            physical_address: 0,
            max_tmds_khz: None,
            forum: None,
        })
        .forum = Some(forum);
}

fn read_extended(payload: &[u8], edid: &mut Edid) {
    match *payload {
        [EXTENDED_VIDEO_CAPABILITY, capabilities] => {
            edid.rgb_quantization_selectable = capabilities & 0x40 != 0;
        }
        [EXTENDED_HF_SCDB, ..] => read_forum(payload, edid),
        _ => {}
    }
}
