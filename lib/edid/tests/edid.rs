//! What a sink's description states, read from synthetic descriptions laid
//! out as the standards place each field.

use tairix_edid::synth::{self, dtd, dtd_1080p, dtd_720p, Base, Cta, SYNC_POSITIVE};
use tairix_edid::{
    block_count, Aspect, AudioFormat, Axis, Connection, Edid, EdidError, Eld, EldError, Polarity,
    Scan, Size, Speakers, Timing, VideoCode, MAX_BLOCKS,
};

const AUDIO: u8 = 1;
const VIDEO: u8 = 2;
const VENDOR: u8 = 3;
const SPEAKERS: u8 = 4;
const EXTENDED: u8 = 7;

/// A standard timing's first byte for `width` pixels.
fn standard_width(width: u16) -> u8 {
    u8::try_from(width / 8 - 31).expect("a width a code spells")
}

fn parse(blocks: &[[u8; 128]]) -> Edid {
    Edid::parse(&blocks.concat()).expect("a valid description")
}

fn has(edid: &Edid, width: u16, height: u16) -> bool {
    edid.modes
        .iter()
        .any(|mode| mode.timing.horizontal.active == width && mode.timing.vertical.active == height)
}

/// A base block naming its sink and stating the usual monitor timings.
fn monitor() -> Base {
    Base::new()
        .descriptor(0, dtd_1080p())
        .display(1, 0xFC, b"TAIRIX TEST\n ")
        .display(2, 0xFD, &[50, 75, 30, 83, 17])
        .established([0x21, 0x00, 0x00])
        .standard(&[[standard_width(1280), 0x80]])
}

#[test]
fn a_base_block_states_the_sink_its_timings_and_its_limits() {
    let edid = parse(&[monitor().build()]);
    assert_eq!(edid.product.manufacturer, *b"TXS");
    assert_eq!(edid.product.code, 0x1234);
    assert_eq!(edid.product.serial, 42);
    assert_eq!((edid.product.week, edid.product.year), (10, 2024));
    assert_eq!(edid.revision, 4);
    assert!(edid.digital);
    assert_eq!(
        edid.name.as_ref().map(tairix_inline::ArrayString::as_str),
        Some("TAIRIX TEST")
    );
    assert_eq!(edid.max_clock_khz, Some(170_000));
    assert_eq!(
        edid.size,
        Some(Size {
            width_mm: 600,
            height_mm: 340,
        }),
        "the preferred timing's size, finer than the block's centimetres"
    );
    let first = edid.modes[0];
    assert!(
        first.preferred,
        "revision 4's first detailed timing is preferred"
    );
    assert_eq!(
        first.timing,
        Timing {
            clock_khz: 148_500,
            horizontal: Axis {
                active: 1920,
                front: 88,
                sync: 44,
                back: 148,
            },
            vertical: Axis {
                active: 1080,
                front: 4,
                sync: 5,
                back: 36,
            },
            hsync: Polarity::Positive,
            vsync: Polarity::Positive,
            scan: Scan::Progressive,
            repetition: 1,
        }
    );
    assert_eq!(first.timing.refresh_mhz(), 60_000);
    assert!(
        has(&edid, 640, 480),
        "established 640x480 at 60 Hz, byte 0x23 bit 5"
    );
    assert!(
        has(&edid, 800, 600),
        "established 800x600 at 60 Hz, byte 0x23 bit 0"
    );
    assert!(has(&edid, 1280, 1024), "the standard timing, a DMT one");
    assert_eq!(edid.modes.iter().filter(|mode| mode.preferred).count(), 1);
}

#[test]
fn before_revision_4_a_feature_bit_says_whether_the_first_timing_is_preferred() {
    let unmarked = parse(&[monitor().revision(3).build()]);
    assert!(unmarked.modes.iter().all(|mode| !mode.preferred));
    let marked = parse(&[monitor().revision(3).features(0x02).build()]);
    assert!(marked.modes[0].preferred);
}

#[test]
fn a_block_that_is_not_an_edid_or_fails_its_checksum_is_refused() {
    let mut block = monitor().build();
    assert_eq!(Edid::parse(&block[..100]), Err(EdidError::Length));
    assert_eq!(Edid::parse(&[]), Err(EdidError::Length));
    let mut longer = block.to_vec();
    longer.push(0);
    assert_eq!(Edid::parse(&longer), Err(EdidError::Length));
    block[60] ^= 1;
    assert_eq!(Edid::parse(&block), Err(EdidError::Checksum));
    let mut header = monitor().build();
    header[0] = 1;
    assert_eq!(Edid::parse(&header), Err(EdidError::NotEdid));
    assert_eq!(block_count(&header), Err(EdidError::NotEdid));
}

#[test]
fn a_name_that_is_not_printable_is_no_name() {
    let edid = parse(&[Base::new().display(0, 0xFC, b"BAD\x01NAME\n").build()]);
    assert_eq!(edid.name, None);
}

#[test]
fn an_interlaced_timing_is_doubled_into_its_frame() {
    let field = dtd(
        74_250,
        [1920, 88, 44, 148],
        [540, 2, 5, 15],
        SYNC_POSITIVE | 0x80,
        (0, 0),
    );
    let edid = parse(&[Base::new().descriptor(0, field).build()]);
    let timing = edid.modes[0].timing;
    assert_eq!(timing.scan, Scan::Interlaced);
    assert_eq!(
        timing.vertical,
        Axis {
            active: 1080,
            front: 4,
            sync: 10,
            back: 31,
        }
    );
    assert_eq!(timing.vertical.total(), 1125);
}

#[test]
fn an_impossible_detailed_timing_is_skipped() {
    let porches_past_blanking = {
        let mut d = dtd_720p();
        d[3] = 10;
        d[4] &= 0xF0;
        d
    };
    let edid = parse(&[Base::new()
        .descriptor(0, porches_past_blanking)
        .descriptor(1, dtd_1080p())
        .build()]);
    assert_eq!(edid.modes.len(), 1);
    assert_eq!(edid.modes[0].timing.horizontal.active, 1920);
}

#[test]
fn a_panel_of_1366_columns_is_found_from_the_code_nearest_it() {
    let code = [standard_width(1368), 0xC0];
    let edid = parse(&[Base::new().standard(&[code]).build()]);
    assert!(has(&edid, 1366, 768));
}

/// A television's CTA extension: video codes, audio, speakers, the HDMI and
/// HDMI Forum blocks, the video capability block, and a detailed timing.
fn television() -> Cta {
    let hdmi = [0x03, 0x0C, 0x00, 0x10, 0x00, 0x00, 60, 0x20, 0x00, 0x20, 1];
    let forum = [0xD8, 0x5D, 0xC4, 1, 120, 0x80 | 0x40 | 0x08, 0];
    Cta::new()
        .flags(0x40)
        .block(VIDEO, &[16 | 0x80, 4, 97, 0, 128, 255])
        .block(AUDIO, &[0x09, 0x07, 0x07, 0x15, 0x07, 0x50])
        .block(SPEAKERS, &[0x0F, 0, 0])
        .block(VENDOR, &hdmi)
        .block(VENDOR, &forum)
        .block(EXTENDED, &[0x00, 0x40])
        .detailed(dtd_720p())
}

#[test]
fn a_cta_extension_states_the_sink_s_codes_audio_and_hdmi_capabilities() {
    let edid = parse(&[monitor().extensions(1).build(), television().build()]);
    let preferred = edid.modes[0];
    assert!(preferred.preferred);
    assert_eq!(
        preferred.code,
        Some(VideoCode::Cta {
            vic: 16,
            aspect: Aspect::SixteenNine,
        }),
        "the preferred timing is code 16's: the listing names its code"
    );
    let code_of = |vic| {
        edid.modes.iter().find(
            |mode| matches!(mode.code, Some(VideoCode::Cta { vic: listed, .. }) if listed == vic),
        )
    };
    assert_eq!(
        code_of(4).map(|mode| mode.timing.horizontal.active),
        Some(1280)
    );
    assert_eq!(
        code_of(97).map(|mode| (mode.timing.horizontal.active, mode.timing.clock_khz)),
        Some((3840, 594_000))
    );
    assert!(
        edid.modes
            .iter()
            .any(|mode| mode.code == Some(VideoCode::Hdmi(1))),
        "the HDMI block's 4K code"
    );
    assert_eq!(
        edid.modes
            .iter()
            .filter(
                |mode| mode.timing.horizontal.active == 1280 && mode.timing.vertical.active == 720
            )
            .count(),
        1,
        "the detailed 720p and code 4 are one timing"
    );

    assert!(edid.audio.basic);
    let [pcm, ac3] = edid.audio.descriptors.as_slice() else {
        panic!("two descriptors");
    };
    assert_eq!((pcm.format, pcm.channels), (AudioFormat::Lpcm, 2));
    assert_eq!(
        pcm.rates.iter().collect::<Vec<_>>(),
        [32_000, 44_100, 48_000]
    );
    assert_eq!(
        pcm.lpcm_sizes()
            .map(|sizes| sizes.iter().collect::<Vec<_>>()),
        Some(vec![16, 20, 24])
    );
    assert_eq!((ac3.format, ac3.channels), (AudioFormat::Ac3, 6));
    assert_eq!(ac3.lpcm_sizes(), None);
    let speakers = edid.audio.speakers.expect("allocated");
    let surround = Speakers::FRONT | Speakers::LFE | Speakers::FRONT_CENTRE | Speakers::REAR;
    assert_eq!(speakers.bits() & surround, surround);
    assert_eq!(speakers.bits() & Speakers::REAR_CENTRE, 0);

    let hdmi = edid.hdmi.expect("an HDMI sink");
    assert_eq!(hdmi.physical_address, 0x1000);
    assert_eq!(hdmi.max_tmds_khz, Some(300_000));
    let forum = hdmi.forum.expect("an HDMI 2.0 sink");
    assert_eq!(forum.max_tmds_khz, Some(600_000));
    assert!(forum.scdc && forum.read_request && forum.scrambles_below_340);
    assert!(edid.rgb_quantization_selectable);
}

#[test]
fn a_sink_with_no_hdmi_block_is_a_dvi_sink() {
    let edid = parse(&[monitor().build()]);
    assert_eq!(edid.hdmi, None);
    assert!(!edid.audio.basic);
}

#[test]
fn a_data_block_running_past_its_collection_ends_it() {
    let cta = Cta::new()
        .block(VIDEO, &[4])
        .raw(&[VIDEO << 5 | 31, 16])
        .detailed(dtd_720p())
        .build();
    let edid = parse(&[Base::new().extensions(1).build(), cta]);
    assert!(edid.modes.iter().all(|mode| mode.code
        != Some(VideoCode::Cta {
            vic: 16,
            aspect: Aspect::SixteenNine
        })));
    assert!(has(&edid, 1280, 720));
}

#[test]
fn an_extension_failing_its_checksum_is_skipped() {
    let mut cta = television().build();
    cta[10] ^= 0xFF;
    let edid = parse(&[monitor().extensions(1).build(), cta]);
    assert_eq!(edid.hdmi, None);
    assert!(edid.audio.descriptors.is_empty());
}

#[test]
fn an_hdmi_forum_override_states_how_many_blocks_there_are() {
    let first = Cta::new()
        .block(EXTENDED, &[0x78, 2])
        .block(VIDEO, &[4])
        .build();
    let second = Cta::new().block(VIDEO, &[16]).build();
    let base = Base::new().extensions(1).build();
    assert_eq!(block_count(&base), Ok(2));
    assert_eq!(block_count(&[base, first].concat()), Ok(3));
    let edid = parse(&[base, first, second]);
    assert!(
        has(&edid, 1920, 1080),
        "the block only the override declares is read"
    );
    assert_eq!(
        block_count(&Base::new().extensions(255).build()),
        Ok(MAX_BLOCKS),
        "a sink declaring 255 is read to the bound"
    );
}

#[test]
fn a_displayid_section_adds_its_detailed_timings() {
    let timing = synth::displayid_timing(24_150, [2560, 48, 32, 80], [1440, 3, 5, 33], 0x80);
    let edid = parse(&[
        Base::new().extensions(1).build(),
        synth::displayid(&[(0x03, &timing)]),
    ]);
    let mode = edid.modes[0];
    assert!(mode.preferred);
    assert_eq!(mode.timing.horizontal.active, 2560);
    assert_eq!(mode.timing.vertical.active, 1440);
    assert_eq!(mode.timing.clock_khz, 241_500);
    assert_eq!(mode.timing.hsync, Polarity::Positive);

    let mut broken = synth::displayid(&[(0x03, &timing)]);
    broken[6] ^= 1;
    synth::checksum(&mut broken);
    let edid = parse(&[Base::new().extensions(1).build(), broken]);
    assert!(
        edid.modes.is_empty(),
        "a section failing its own checksum is skipped"
    );
}

#[test]
fn every_code_a_television_may_list_is_a_timing() {
    for vic in (1..=127).chain(193..=219) {
        let edid = parse(&[
            Base::new().extensions(1).build(),
            Cta::new().block(VIDEO, &[vic]).build(),
        ]);
        let [mode] = edid.modes.as_slice() else {
            panic!("code {vic} names one timing");
        };
        assert!(
            matches!(mode.code, Some(VideoCode::Cta { vic: listed, .. }) if listed == vic),
            "code {vic}"
        );
        assert!(
            (23_000..=241_000).contains(&mode.timing.refresh_mhz()),
            "code {vic}"
        );
    }
    for reserved in [0, 128, 220, 255] {
        let edid = parse(&[
            Base::new().extensions(1).build(),
            Cta::new().block(VIDEO, &[reserved]).build(),
        ]);
        assert!(edid.modes.is_empty(), "code {reserved}");
    }
}

#[test]
fn an_eld_states_its_display_s_link_name_and_audio() {
    let bytes = synth::eld(0, b"MONITOR", &[[0x09, 0x07, 0x07]], 0x01);
    let eld = Eld::parse(&bytes).expect("a valid ELD");
    assert_eq!(eld.connection, Connection::Hdmi);
    assert_eq!(
        eld.name.as_ref().map(tairix_inline::ArrayString::as_str),
        Some("MONITOR")
    );
    assert_eq!(eld.audio.descriptors.len(), 1);
    assert_eq!(eld.audio.descriptors[0].format, AudioFormat::Lpcm);
    assert_eq!(eld.audio.speakers.map(Speakers::bits), Some(0x01));
    let dp = Eld::parse(&synth::eld(1, b"", &[], 0)).expect("valid");
    assert_eq!((dp.connection, dp.name), (Connection::DisplayPort, None));
}

#[test]
fn a_malformed_eld_is_refused() {
    let good = synth::eld(0, b"MONITOR", &[[0x09, 0x07, 0x07]], 0x01);
    let mut version = good.clone();
    version[0] = 0x08;
    assert_eq!(Eld::parse(&version), Err(EldError::Version));
    assert_eq!(Eld::parse(&good[..good.len() - 4]), Err(EldError::Length));
    let mut descriptors = good.clone();
    descriptors[5] = 0xF0;
    assert_eq!(Eld::parse(&descriptors), Err(EldError::Length));
    let mut link = good.clone();
    link[5] |= 0x08;
    assert_eq!(Eld::parse(&link), Err(EldError::Connection));
    let mut long = good;
    long[4] = 0x1F;
    assert_eq!(Eld::parse(&long), Err(EldError::Length));
    assert_eq!(Eld::parse(&[]), Err(EldError::Length));
}
