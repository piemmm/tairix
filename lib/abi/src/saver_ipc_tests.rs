extern crate alloc;

use alloc::vec;

use super::*;
use crate::saver::SaverFeeds;
use crate::switchboard_ipc::MachineReport;
use crate::APPINFO_FLAG_FEED_SLIDES;

fn host_round_trip(message: &HostMessage<'_>) -> alloc::vec::Vec<u8> {
    let mut out = vec![0u8; SAVER_HOST_MESSAGE_MAX];
    let len = message.encode(&mut out).expect("encodes");
    out.truncate(len);
    assert_eq!(HostMessage::decode(&out).as_ref(), Ok(message));
    out
}

fn saver_round_trip(message: &SaverMessage) -> alloc::vec::Vec<u8> {
    let mut out = vec![0u8; SAVER_MESSAGE_MAX];
    let len = message.encode(&mut out).expect("encodes");
    out.truncate(len);
    assert_eq!(SaverMessage::decode(&out), Ok(*message));
    out
}

fn slides() -> SaverFeeds {
    SaverFeeds::from_bits(APPINFO_FLAG_FEED_SLIDES).expect("a feed")
}

fn start() -> Start {
    let region = SaverRegion::new(1920, 1080, slides()).expect("a region");
    let mut start = Start::new(region, 150, RefreshRate::from_mhz(60_000).expect("a rate"));
    start.now_ns = 0x0123_4567_89AB_CDEF;
    start.reduced_motion = true;
    start.contrast = crate::desktop::Contrast::High;
    start.density = crate::desktop::Density::Compact;
    start.appearance = Appearance::Dark;
    let family = crate::font_ipc::FamilyKey::new("display").expect("a family");
    start.text = Some(crate::desktop::DesktopText::new(family, 18).expect("a size"));
    start.seed = [0x5E; SAVER_SEED_LEN];
    start.pressure = crate::sysinfo::MemoryBand::new(2).expect("a band");
    start.cores = 4;
    start.threads = 4;
    start.memory = 512 << 20;
    start.installed_memory = 8 << 30;
    start.account = BoundedText::new("ada").expect("a name");
    start.machine = BoundedText::new("lovelace").expect("a name");
    start.feeds = slides();
    start.slides = 12;
    start
        .push_option(OptionValue {
            key: OptionKey::new("detail").expect("a key"),
            value: SaverValue::Choice(ChoiceValue::new("maximum").expect("a value")),
        })
        .expect("room");
    start
        .push_option(OptionValue {
            key: OptionKey::new("keep").expect("a key"),
            value: SaverValue::Switch(true),
        })
        .expect("room");
    start
        .push_option(OptionValue {
            key: OptionKey::new("interval_s").expect("a key"),
            value: SaverValue::Range(-30),
        })
        .expect("room");
    start
}

#[test]
fn the_region_is_a_frame_then_a_feed_area() {
    let plain = SaverRegion::new(4, 3, SaverFeeds::NONE).expect("a region");
    assert_eq!(plain.stride(), 16);
    assert_eq!(plain.frame_len(), 48);
    assert_eq!(plain.len(), 48);
    assert_eq!(plain.feed_range(), None);
    let fed = SaverRegion::new(4, 3, slides()).expect("a region");
    assert_eq!(fed.feed_range(), Some(48..96));
    assert_eq!(fed.len(), 96);
    assert_eq!(plain.display_mode().stride_bytes, 16);
    assert_eq!(
        plain.display_mode().format,
        crate::driver::display::DisplayFormat::Rgba8888
    );
    assert_eq!(
        SaverRegion::new(u32::MAX, 1, SaverFeeds::NONE),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        SaverRegion::new(0, 3, SaverFeeds::NONE),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        SaverRegion::new(4, 0, SaverFeeds::NONE),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn every_host_message_round_trips() {
    let wall = WallReading {
        time: BoundedText::new("14:05").expect("text"),
        numeric_date: BoundedText::new("21/10/2026").expect("text"),
        long_date: BoundedText::new("Wednesday 21 October 2026").expect("text"),
        next_change_ns: 77,
    };
    let reply = [7u8; 300];
    for message in vec![
        HostMessage::Start(start()),
        HostMessage::Frame {
            serial: 9,
            now_ns: 1 << 40,
            wall,
        },
        HostMessage::Frame {
            serial: 10,
            now_ns: 5,
            wall: WallReading::unset(9),
        },
        HostMessage::Slide {
            index: 3,
            outcome: SlideOutcome::Shown,
        },
        HostMessage::Slide {
            index: 4,
            outcome: SlideOutcome::Unavailable,
        },
        HostMessage::FontReply(Ok(&reply)),
        HostMessage::FontReply(Ok(&[])),
        HostMessage::FontReply(Err(Errno::NotFound)),
        HostMessage::Kept { kept: true },
        HostMessage::Kept { kept: false },
        HostMessage::Pressure(crate::sysinfo::MemoryBand::new(3).expect("a band")),
        HostMessage::MachineLost,
    ] {
        host_round_trip(&message);
    }
    host_round_trip(&HostMessage::Machine(unread_report()));
}

fn unread_report() -> MachineReport {
    use crate::switchboard_ipc::{MachineScope, ReportPeriod, MACHINE_PERIOD_MIN_MS};
    let period = ReportPeriod::from_millis(MACHINE_PERIOD_MIN_MS).expect("a period");
    MachineReport::unread(period, MachineScope::Own)
}

#[test]
fn a_start_reads_its_options_by_key() {
    let start = start();
    assert_eq!(start.option("keep"), Some(SaverValue::Switch(true)));
    assert_eq!(start.option("interval_s"), Some(SaverValue::Range(-30)));
    assert_eq!(start.option("missing"), None);
    let mut again = start.clone();
    assert_eq!(
        again.push_option(OptionValue {
            key: OptionKey::new("keep").expect("a key"),
            value: SaverValue::Switch(false),
        }),
        Err(Errno::OutOfRange)
    );
    for n in 3..SAVER_MAX_OPTIONS {
        again
            .push_option(OptionValue {
                key: OptionKey::new(&alloc::format!("o{n}")).expect("a key"),
                value: SaverValue::Switch(false),
            })
            .expect("room");
    }
    assert_eq!(
        again.push_option(OptionValue {
            key: OptionKey::new("one_more").expect("a key"),
            value: SaverValue::Switch(false),
        }),
        Err(Errno::LengthOutOfRange)
    );
    host_round_trip(&HostMessage::Start(again));
}

#[test]
fn every_saver_message_round_trips() {
    let damage = SaverDamage::new(&[
        DamageRect {
            x: 0,
            y: 0,
            width_px: 10,
            height_px: 10,
        },
        DamageRect {
            x: 20,
            y: 0,
            width_px: 5,
            height_px: 5,
        },
    ])
    .expect("disjoint");
    let request = crate::font_ipc::FontRequest::Families.to_le_bytes();
    for (serial, damage, next) in [
        (3, Some(damage), NextFrame::At(99)),
        (4, None, NextFrame::Soon),
        (5, None, NextFrame::Idle),
    ] {
        saver_round_trip(&SaverMessage::Presented {
            serial,
            damage,
            next,
        });
    }
    for message in [
        SaverMessage::Ready,
        SaverMessage::FontCall(request),
        SaverMessage::Keep {
            title: BoundedText::new("Countryside").expect("a title"),
            width: 1920,
            height: 1080,
        },
        SaverMessage::WantSlide(11),
        SaverMessage::Unstarted(Errno::OutOfMemory),
    ] {
        saver_round_trip(&message);
    }
}

/// A frame names as many rectangles as the bound and no more, each disjoint
/// from the rest.
#[test]
fn a_presented_frame_names_up_to_its_damage_bound() {
    let apart = |i: usize| DamageRect {
        x: u32::try_from(i).expect("small") * 8,
        y: 0,
        width_px: 4,
        height_px: 4,
    };
    let rects: alloc::vec::Vec<_> = (0..SAVER_DAMAGE_RECTS).map(apart).collect();
    let full = SaverMessage::Presented {
        serial: 7,
        damage: Some(SaverDamage::new(&rects).expect("disjoint")),
        next: NextFrame::Soon,
    };
    let mut wire = saver_round_trip(&full);
    wire[1] = u8::try_from(SAVER_DAMAGE_RECTS + 1).expect("a count byte");
    assert_eq!(SaverMessage::decode(&wire), Err(Errno::LengthOutOfRange));
    wire[1] = u8::try_from(SAVER_DAMAGE_RECTS).expect("a count byte");
    // The second rectangle laid over the first.
    wire[16 + 16..16 + 20].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(SaverMessage::decode(&wire), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_stray_byte_in_any_saver_message_is_refused_or_read_as_another() {
    let damage = SaverDamage::new(&[DamageRect {
        x: 1,
        y: 2,
        width_px: 3,
        height_px: 4,
    }])
    .expect("one rect");
    for message in [
        SaverMessage::Ready,
        SaverMessage::Presented {
            serial: 3,
            damage: Some(damage),
            next: NextFrame::Soon,
        },
        SaverMessage::Keep {
            title: BoundedText::new("A").expect("a title"),
            width: 1,
            height: 1,
        },
        SaverMessage::WantSlide(1),
        SaverMessage::Unstarted(Errno::NotFound),
    ] {
        let wire = saver_round_trip(&message);
        for at in 0..wire.len() {
            let mut flipped = wire.clone();
            flipped[at] ^= 0x80;
            assert_ne!(
                SaverMessage::decode(&flipped),
                Ok(message),
                "byte {at} of {message:?} is not covered"
            );
        }
        let mut long = wire.clone();
        long.push(0);
        assert!(SaverMessage::decode(&long).is_err(), "{message:?} padded");
        assert!(
            SaverMessage::decode(&wire[..wire.len() - 1]).is_err() || wire.len() == 1,
            "{message:?} cut"
        );
    }
    assert_eq!(SaverMessage::decode(&[]), Err(Errno::LengthOutOfRange));
    assert_eq!(SaverMessage::decode(&[0x7F]), Err(Errno::BadMagic));
}

fn assert_every_byte_covered(message: &HostMessage<'_>) {
    let wire = host_round_trip(message);
    for at in 0..wire.len() {
        let mut flipped = wire.clone();
        flipped[at] ^= 0x80;
        assert_ne!(
            HostMessage::decode(&flipped).as_ref(),
            Ok(message),
            "byte {at} of {message:?} is not covered"
        );
    }
}

#[test]
fn a_stray_byte_in_any_host_message_is_refused_or_read_as_another() {
    assert_every_byte_covered(&HostMessage::Start(start()));
    assert_every_byte_covered(&HostMessage::Frame {
        serial: 1,
        now_ns: 2,
        wall: WallReading::unset(3),
    });
    assert_every_byte_covered(&HostMessage::Slide {
        index: 3,
        outcome: SlideOutcome::Shown,
    });
    assert_every_byte_covered(&HostMessage::FontReply(Ok(&[9u8; 4])));
    assert_every_byte_covered(&HostMessage::FontReply(Err(Errno::NotFound)));
    assert_every_byte_covered(&HostMessage::Kept { kept: true });
    assert_every_byte_covered(&HostMessage::Pressure(
        crate::sysinfo::MemoryBand::new(1).expect("a band"),
    ));
    assert_every_byte_covered(&HostMessage::MachineLost);
    assert_eq!(HostMessage::decode(&[]), Err(Errno::LengthOutOfRange));
    assert_eq!(HostMessage::decode(&[0x7F]), Err(Errno::BadMagic));
}

#[test]
fn damage_the_frame_format_cannot_hold_is_refused() {
    let overlapping = [(0u32, 0u32, 10u32, 10u32), (5, 5, 10, 10)];
    let mut wire = vec![0u8; SAVER_MESSAGE_MAX];
    let len = SaverMessage::Presented {
        serial: 1,
        damage: None,
        next: NextFrame::Soon,
    }
    .encode(&mut wire)
    .expect("encodes");
    wire.truncate(len);
    wire[1] = 2;
    for (index, (x, y, w, h)) in overlapping.into_iter().enumerate() {
        let base = 16 + index * 16;
        wire[base..base + 4].copy_from_slice(&x.to_le_bytes());
        wire[base + 4..base + 8].copy_from_slice(&y.to_le_bytes());
        wire[base + 8..base + 12].copy_from_slice(&w.to_le_bytes());
        wire[base + 12..base + 16].copy_from_slice(&h.to_le_bytes());
    }
    assert_eq!(SaverMessage::decode(&wire), Err(Errno::LengthOutOfRange));
    wire[1] = 9;
    assert_eq!(SaverMessage::decode(&wire), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_font_call_carries_only_a_request_the_font_service_reads() {
    let mut wire = vec![0u8; SAVER_MESSAGE_MAX];
    let len = SaverMessage::FontCall(crate::font_ipc::FontRequest::Families.to_le_bytes())
        .encode(&mut wire)
        .expect("encodes");
    wire.truncate(len);
    wire[1] ^= 0xFF;
    assert!(SaverMessage::decode(&wire).is_err());
}

#[test]
fn a_font_reply_past_its_bound_is_refused_both_ways() {
    let big = vec![0u8; SAVER_FONT_REPLY_MAX + 1];
    let mut out = vec![0u8; SAVER_HOST_MESSAGE_MAX + 8];
    assert_eq!(
        HostMessage::FontReply(Ok(&big)).encode(&mut out),
        Err(Errno::MessageTooLarge)
    );
    let mut wire = vec![0u8; SAVER_HOST_MESSAGE_MAX + 1];
    wire[0] = 5;
    assert_eq!(HostMessage::decode(&wire), Err(Errno::LengthOutOfRange));
    let mut lying = vec![0u8; 16];
    lying[0] = 5;
    lying[4] = 9;
    assert_eq!(HostMessage::decode(&lying), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_start_naming_another_protocol_or_a_slide_count_without_slides_is_refused() {
    let mut out = vec![0u8; SAVER_HOST_MESSAGE_MAX];
    let len = HostMessage::Start(start())
        .encode(&mut out)
        .expect("encodes");
    let mut wire = out[..len].to_vec();
    wire[1] = 9;
    assert_eq!(
        HostMessage::decode(&wire),
        Err(Errno::AbiVersionUnsupported)
    );
    let mut unfed = start();
    unfed.feeds = SaverFeeds::NONE;
    unfed.region = SaverRegion::new(1920, 1080, SaverFeeds::NONE).expect("a region");
    let len = HostMessage::Start(unfed).encode(&mut out).expect("encodes");
    assert_eq!(HostMessage::decode(&out[..len]), Err(Errno::OutOfRange));
    assert_eq!(
        HostMessage::Start(start()).encode(&mut [0u8; 8]),
        Err(Errno::BufferTooSmall)
    );
}
