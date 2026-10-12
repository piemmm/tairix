//! Which of a sink's timings a transmitter runs: the sink's preferred one
//! when the transmitter, its link and the engine's pipe all carry it, else
//! the largest that they do.

use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
use tairix_abi::driver::encoder::PipeLimits;
use tairix_edid::{hdmi_video_code, video_code, Edid, VideoCode};

/// The fastest TMDS character rate HDMI 1.4 sends unscrambled, in kHz.
pub(crate) const HDMI_14_MAX_KHZ: u32 = 340_000;

/// The fastest single-link DVI sends, in kHz.
const DVI_MAX_KHZ: u32 = 165_000;

/// TMDS bit rates, ten bits a character, inside the 2.4 GHz band's first
/// channel, in kHz of character rate.
const WIFI_CHANNEL_1_KHZ: core::ops::RangeInclusive<u32> = 240_000..=242_200;

/// The character rate a timing inside that channel is pulled down to: out
/// of it, and close enough that no sink notices.
const WIFI_CLEAR_KHZ: u32 = 238_560;

/// What the transmitter is: its fastest character rate, and whether it shares
/// a board with a 2.4 GHz radio its TMDS harmonics must keep clear of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Transmitter {
    /// Its fastest TMDS character rate, in kHz.
    pub max_tmds_khz: u32,
    /// The board's radio shares the 2.4 GHz band.
    pub wifi_coexistence: bool,
}

/// The mode an output runs, and how the link carries it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Chosen {
    /// The timing.
    pub timing: Timing,
    /// The code its `InfoFrames` name it by, when one does.
    pub code: Option<VideoCode>,
    /// The sink speaks HDMI, so it is sent `InfoFrames`; a DVI sink is not.
    pub hdmi: bool,
    /// Above HDMI 1.4's rate: scrambled, the sink told so over SCDC.
    pub scrambled: bool,
}

/// What a sink with no readable description is given: 1024x768 at 60 Hz,
/// VESA DMT's mode every monitor takes.
pub(crate) const FALLBACK: Timing = Timing {
    clock_khz: 65_000,
    horizontal: Axis {
        active: 1024,
        front: 24,
        sync: 136,
        back: 160,
    },
    vertical: Axis {
        active: 768,
        front: 3,
        sync: 6,
        back: 29,
    },
    hsync: Polarity::Negative,
    vsync: Polarity::Negative,
    scan: Scan::Progressive,
    repetition: 1,
};

/// Whether every part of a line is an even number of pixels: the pixel valve
/// feeding the transmitter clocks two at a time.
fn even_line(timing: &Timing) -> bool {
    let line = timing.horizontal;
    [line.active, line.front, line.sync, line.back]
        .iter()
        .all(|part| part % 2 == 0)
}

/// The fastest character rate the sink takes, in kHz.
fn sink_max_khz(edid: &Edid) -> u32 {
    let range = edid.max_clock_khz.unwrap_or(u32::MAX);
    let stated = match &edid.hdmi {
        None => DVI_MAX_KHZ,
        Some(hdmi) => match hdmi.forum.and_then(|forum| forum.max_tmds_khz) {
            Some(forum) => forum,
            None => hdmi
                .max_tmds_khz
                .unwrap_or(HDMI_14_MAX_KHZ)
                .min(HDMI_14_MAX_KHZ),
        },
    };
    range.min(stated)
}

/// `timing` moved out of the radio's channel where the board needs it.
fn clear_of_wifi(transmitter: Transmitter, mut timing: Timing) -> Timing {
    if transmitter.wifi_coexistence && WIFI_CHANNEL_1_KHZ.contains(&timing.clock_khz) {
        timing.clock_khz = WIFI_CLEAR_KHZ;
    }
    timing
}

/// The mode `transmitter` runs for the sink `edid` describes within `limits`,
/// or [`None`] when nothing it lists can be carried. A sink with no readable
/// description is given [`FALLBACK`], as DVI.
pub fn choose(transmitter: Transmitter, edid: Option<&Edid>, limits: PipeLimits) -> Option<Chosen> {
    let Some(edid) = edid else {
        return limits.carries(&FALLBACK).then_some(Chosen {
            timing: FALLBACK,
            code: None,
            hdmi: false,
            scrambled: false,
        });
    };
    let sink_khz = sink_max_khz(edid);
    let scdc = edid
        .hdmi
        .and_then(|hdmi| hdmi.forum)
        .is_some_and(|forum| forum.scdc);
    let carried = |mode: &&tairix_edid::Mode| {
        let timing = clear_of_wifi(transmitter, mode.timing);
        mode.timing.repetition == 1
            && even_line(&timing)
            && timing.clock_khz <= transmitter.max_tmds_khz.min(sink_khz)
            && (timing.clock_khz <= HDMI_14_MAX_KHZ || scdc)
            && limits.carries(&timing)
    };
    let largest = |mode: &&tairix_edid::Mode| {
        let timing = mode.timing;
        (
            u32::from(timing.horizontal.active) * u32::from(timing.vertical.active),
            timing.scan == Scan::Progressive,
            timing.refresh_mhz(),
        )
    };
    let mode = edid
        .modes
        .iter()
        .filter(carried)
        .find(|mode| mode.preferred)
        .or_else(|| edid.modes.iter().filter(carried).max_by_key(largest))?;
    let hdmi = edid.hdmi.is_some();
    // An HDMI sink is sent one of the four 4K timings by its HDMI code, as
    // Linux's `drm_mode_hdmi_vic` has it. Any other keeps the CTA code the
    // sink listed it by, for the aspect that names, else the standard's.
    let listed = |other: &tairix_edid::Mode| {
        other.timing == mode.timing && matches!(other.code, Some(VideoCode::Cta { .. }))
    };
    let code = hdmi
        .then(|| hdmi_video_code(&mode.timing))
        .flatten()
        .or_else(|| {
            core::iter::once(mode)
                .chain(&edid.modes)
                .find(|other| listed(other))
                .and_then(|other| other.code)
        })
        .or_else(|| video_code(&mode.timing));
    let timing = clear_of_wifi(transmitter, mode.timing);
    Some(Chosen {
        timing,
        code,
        hdmi,
        scrambled: timing.clock_khz > HDMI_14_MAX_KHZ,
    })
}
