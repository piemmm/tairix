//! The published tables, checked against themselves.

use crate::tables::{DMT, ESTABLISHED, ESTABLISHED_III, HDMI_VICS, VICS};
use crate::{Aspect, Timing, VideoCode};

#[test]
fn each_dmt_timing_refreshes_at_its_stated_rate() {
    for dmt in DMT {
        let refresh = dmt.timing.refresh_mhz();
        assert!(
            refresh.abs_diff(u32::from(dmt.refresh) * 1_000) <= 1_000,
            "{}x{} at {} Hz runs at {refresh} mHz",
            dmt.timing.horizontal.active,
            dmt.timing.vertical.active,
            dmt.refresh
        );
    }
}

#[test]
fn every_established_timing_iii_is_a_dmt_timing() {
    for &(width, height, refresh, reduced) in &ESTABLISHED_III {
        assert!(
            DMT.iter().any(|dmt| dmt.timing.horizontal.active == width
                && dmt.timing.vertical.active == height
                && dmt.refresh == refresh
                && dmt.reduced == reduced),
            "{width}x{height} at {refresh} Hz"
        );
    }
}

#[test]
fn the_codes_ascend_without_a_gap_in_either_range() {
    assert!(VICS
        .iter()
        .map(|entry| entry.code)
        .eq((1..=127).chain(193..=219)));
    for timing in ESTABLISHED
        .iter()
        .chain(HDMI_VICS.iter().map(|vic| &vic.timing))
    {
        assert!(
            (23_000..=89_000).contains(&timing.refresh_mhz()),
            "{timing:?}"
        );
    }
}

#[test]
fn a_timing_is_named_by_its_cta_code_at_either_rate() {
    let full_hd = VICS
        .iter()
        .find(|entry| entry.code == 16)
        .expect("VIC 16")
        .timing;
    assert_eq!(
        crate::video_code(&full_hd),
        Some(VideoCode::Cta {
            vic: 16,
            aspect: Aspect::SixteenNine
        })
    );
    let ntsc_rate = Timing {
        clock_khz: 148_350,
        ..full_hd
    };
    assert_eq!(
        crate::video_code(&ntsc_rate).map(|code| matches!(code, VideoCode::Cta { vic: 16, .. })),
        Some(true),
        "59.94 Hz"
    );
    let sd = VICS
        .iter()
        .find(|entry| entry.code == 3)
        .expect("VIC 3")
        .timing;
    assert!(
        matches!(crate::video_code(&sd), Some(VideoCode::Cta { vic: 2, .. })),
        "the lower of two aspects"
    );
    let xga = DMT
        .iter()
        .find(|dmt| dmt.timing.horizontal.active == 1024 && dmt.refresh == 60)
        .expect("1024x768")
        .timing;
    assert_eq!(crate::video_code(&xga), None);
    let off = Timing {
        clock_khz: 140_000,
        ..full_hd
    };
    assert_eq!(crate::video_code(&off), None, "another clock");
}

#[test]
fn a_4k_timing_is_named_by_its_hdmi_code_and_no_other_timing_is() {
    for entry in &HDMI_VICS {
        assert_eq!(
            crate::hdmi_video_code(&entry.timing),
            Some(VideoCode::Hdmi(entry.code))
        );
    }
    let cinema_ntsc = Timing {
        clock_khz: 296_703,
        ..HDMI_VICS[2].timing
    };
    assert_eq!(
        crate::hdmi_video_code(&cinema_ntsc),
        Some(VideoCode::Hdmi(3)),
        "23.98 Hz"
    );
    let full_hd = VICS
        .iter()
        .find(|entry| entry.code == 16)
        .expect("VIC 16")
        .timing;
    assert_eq!(crate::hdmi_video_code(&full_hd), None);
}
