use super::{
    decode_buffer_reply, decode_configure_reply, decode_mode_reply, decode_planes_reply,
    decode_present_reply, decode_stats_reply, encode_buffer_reply, encode_configure_reply,
    encode_mode_reply, encode_planes_reply, encode_present_reply, encode_stats_reply, BufferGrant,
    ConfiguredFrames, DamageList, DisplayEvent, DisplayRequest, DisplayStats, GrantedBuffer,
    LayerList, Planes, PresentReply, BODY_AT, DAMAGE_RECT_LEN, DISPLAY_BUFFER_REPLY_LEN,
    DISPLAY_CONFIGURE_REPLY_LEN, DISPLAY_EVENT_MAGIC, DISPLAY_MAX_FRAMES, DISPLAY_MODE_REPLY_LEN,
    DISPLAY_PLANES_REPLY_LEN, DISPLAY_PRESENT_REPLY_LEN, DISPLAY_REQUEST_MAGIC,
    DISPLAY_STATS_REPLY_LEN, LAYERS_AT, LAYER_LEN, NO_RECT, PRESENT_RECTS_AT, QUERY_REFRESH_AT,
};
use crate::driver::display::{
    AccelCaps, AccelLayer, BufferId, CursorCaps, DamageRect, DisplayDeviceReport, DisplayFormat,
    DisplayMode, DisplayPower, OutputMode, Presented, RefreshRate, MAX_ACCEL_LAYERS,
    MAX_DAMAGE_RECTS,
};
use crate::origin::ProcId;
use crate::Errno;

/// Byte offset of the statistics record inside its reply frame.
const STATS_BODY_AT: usize = 4;

fn max_rects() -> u32 {
    u32::try_from(MAX_DAMAGE_RECTS).expect("the bound fits a u32")
}

/// The `index`th of a column of rectangles that touch and never overlap.
fn rect(index: u32) -> DamageRect {
    DamageRect {
        x: 10,
        y: 20 + index * 40,
        width_px: 30,
        height_px: 40,
    }
}

/// One rectangle more than the bound allows, so a test can slice out any
/// length it needs (this crate allocates nowhere, tests included).
fn rects() -> [DamageRect; MAX_DAMAGE_RECTS + 1] {
    let mut all = [NO_RECT; MAX_DAMAGE_RECTS + 1];
    for (index, slot) in all.iter_mut().enumerate() {
        *slot = rect(u32::try_from(index).expect("a small index"));
    }
    all
}

fn damage(count: u32) -> DamageList {
    let count = usize::try_from(count).expect("a small count");
    DamageList::new(&rects()[..count]).expect("a list within the bound")
}

fn id(raw: u32) -> BufferId {
    BufferId::new(raw).expect("a buffer id")
}

fn grantor() -> ProcId {
    ProcId::from_bytes(&[7u8; 16]).expect("an instance")
}

fn sample_configure() -> DisplayRequest {
    DisplayRequest::Configure {
        seat_id: 0,
        width_px: 640,
        height_px: 480,
        format: DisplayFormat::Bgra8888,
        events_port: 0x9047_0001,
    }
}

fn sample_present() -> DisplayRequest {
    DisplayRequest::Present {
        seat_id: 0,
        buffer: id(2),
        damage: damage(1),
    }
}

fn layer(raw: u32) -> AccelLayer {
    AccelLayer {
        buffer: id(raw),
        src: rect(0),
        dst_x: -5,
        dst_y: 7,
        dst_width_px: 60,
        dst_height_px: 80,
        opacity: 200,
    }
}

fn sample_layers(count: usize) -> DisplayRequest {
    let all: [AccelLayer; MAX_ACCEL_LAYERS] =
        core::array::from_fn(|index| layer(u32::try_from(index).expect("small") + 1));
    DisplayRequest::PresentLayers {
        seat_id: 4,
        layers: LayerList::new(&all[..count]).expect("within the bound"),
    }
}

#[test]
fn requests_round_trip() {
    for request in [
        DisplayRequest::Query { seat_id: 3 },
        sample_configure(),
        sample_present(),
        DisplayRequest::QueryStats,
        DisplayRequest::SetPower {
            seat_id: 3,
            power: DisplayPower::Off,
        },
        DisplayRequest::SetPower {
            seat_id: 3,
            power: DisplayPower::On,
        },
        DisplayRequest::QueryPlanes { seat_id: 3 },
        DisplayRequest::AllocBuffer {
            seat_id: 3,
            width_px: 64,
            height_px: 32,
        },
        DisplayRequest::FreeBuffer {
            seat_id: 3,
            buffer: id(9),
        },
        sample_layers(1),
        sample_layers(MAX_ACCEL_LAYERS),
        DisplayRequest::SetCursor {
            seat_id: 3,
            buffer: id(5),
            hot_x: 2,
            hot_y: 3,
        },
        DisplayRequest::MoveCursor {
            seat_id: 3,
            x: -4,
            y: 1080,
        },
        DisplayRequest::HideCursor { seat_id: 3 },
    ] {
        let bytes = request.to_le_bytes();
        assert_eq!(
            DisplayRequest::from_bytes(&bytes),
            Ok(request),
            "{request:?}"
        );
    }
}

#[test]
fn a_power_request_names_a_known_state_and_nothing_more() {
    let good = DisplayRequest::SetPower {
        seat_id: 3,
        power: DisplayPower::Off,
    }
    .to_le_bytes();
    for bad in [0, 3, u8::MAX] {
        let mut unknown = good;
        unknown[BODY_AT] = bad;
        assert_eq!(DisplayRequest::from_bytes(&unknown), Err(Errno::OutOfRange));
    }
    let mut smuggled = good;
    smuggled[BODY_AT + 1] = 1;
    assert_eq!(DisplayRequest::from_bytes(&smuggled), Err(Errno::BadMagic));
}

/// A whole frame's damage travels in one request: every list length the
/// bound allows survives the wire, rectangles and order intact.
#[test]
fn a_present_carries_a_whole_damage_list() {
    for count in 1..=max_rects() {
        let request = DisplayRequest::Present {
            seat_id: 7,
            buffer: id(1),
            damage: damage(count),
        };
        let decoded = DisplayRequest::from_bytes(&request.to_le_bytes());
        assert_eq!(decoded, Ok(request), "{count} rectangles");
        let Ok(DisplayRequest::Present { damage, .. }) = decoded else {
            panic!("a present decodes as one");
        };
        assert_eq!(u32::try_from(damage.rects().len()), Ok(count));
        assert_eq!(damage.rects().last(), Some(&rect(count - 1)));
    }
}

/// The list's bound and its "no empty rectangle" rule hold at
/// construction, so a caller cannot build one the wire would refuse.
#[test]
fn a_damage_list_is_bounded_and_never_empty() {
    assert_eq!(DamageList::new(&[]).err(), Some(Errno::LengthOutOfRange));
    assert_eq!(
        DamageList::new(&rects()).err(),
        Some(Errno::LengthOutOfRange),
        "one rectangle past the bound"
    );
    let with_empty = [
        rect(0),
        DamageRect {
            x: 0,
            y: 0,
            width_px: 0,
            height_px: 1,
        },
    ];
    assert_eq!(
        DamageList::new(&with_empty).err(),
        Some(Errno::LengthOutOfRange)
    );
    assert_eq!(damage(2), damage(2));
    assert_ne!(damage(2), damage(3));
}

/// A count past the bound, and a rectangle hidden in a slot the count does
/// not reach, are both refused rather than partly honoured.
#[test]
fn a_present_refuses_a_smuggled_rectangle() {
    let encoded = DisplayRequest::Present {
        seat_id: 0,
        buffer: id(1),
        damage: damage(2),
    }
    .to_le_bytes();
    let count_at = BODY_AT + 4;
    let mut over = encoded;
    over[count_at..count_at + 4].copy_from_slice(&(max_rects() + 1).to_le_bytes());
    assert_eq!(
        DisplayRequest::from_bytes(&over),
        Err(Errno::LengthOutOfRange)
    );
    let mut none = encoded;
    none[count_at..count_at + 4].copy_from_slice(&0u32.to_le_bytes());
    none[PRESENT_RECTS_AT..].fill(0);
    assert_eq!(
        DisplayRequest::from_bytes(&none),
        Err(Errno::LengthOutOfRange),
        "a present that names nothing changed"
    );
    let mut behind = encoded;
    behind[PRESENT_RECTS_AT + 2 * DAMAGE_RECT_LEN] = 1;
    assert_eq!(DisplayRequest::from_bytes(&behind), Err(Errno::BadMagic));
}

#[test]
fn a_present_names_a_buffer_and_no_empty_rectangle() {
    let mut no_buffer = sample_present().to_le_bytes();
    no_buffer[BODY_AT..BODY_AT + 4].fill(0);
    assert_eq!(
        DisplayRequest::from_bytes(&no_buffer),
        Err(Errno::OutOfRange)
    );
    for at in [PRESENT_RECTS_AT + 8, PRESENT_RECTS_AT + 12] {
        let mut empty = sample_present().to_le_bytes();
        empty[at..at + 4].fill(0);
        assert_eq!(
            DisplayRequest::from_bytes(&empty),
            Err(Errno::LengthOutOfRange)
        );
    }
}

#[test]
fn magics_are_the_ascii_tags() {
    assert_eq!(DISPLAY_REQUEST_MAGIC, u32::from_le_bytes(*b"DSP1"));
    assert_eq!(DISPLAY_EVENT_MAGIC, u32::from_le_bytes(*b"DSPE"));
}

#[test]
fn decode_fails_closed_on_malformed_framing() {
    let good = sample_configure().to_le_bytes();
    assert_eq!(
        DisplayRequest::from_bytes(&good[..DisplayRequest::WIRE_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut bad_magic = good;
    bad_magic[0] ^= 0xFF;
    assert_eq!(DisplayRequest::from_bytes(&bad_magic), Err(Errno::BadMagic));
    let mut bad_version = good;
    bad_version[4] = 9;
    assert_eq!(
        DisplayRequest::from_bytes(&bad_version),
        Err(Errno::AbiVersionUnsupported)
    );
    for op in [0u8, 13, u8::MAX] {
        let mut bad_op = good;
        bad_op[6] = op;
        assert_eq!(DisplayRequest::from_bytes(&bad_op), Err(Errno::OutOfRange));
    }
}

/// Every operation's reserved tail is refused dirty, wherever it starts.
#[test]
fn decode_refuses_dirty_reserved_tails() {
    for (request, tail) in [
        (DisplayRequest::Query { seat_id: 0 }, BODY_AT),
        (sample_configure(), BODY_AT + 24),
        (sample_configure(), BODY_AT + 9),
        (sample_present(), PRESENT_RECTS_AT + DAMAGE_RECT_LEN),
        (DisplayRequest::QueryStats, 8),
        (DisplayRequest::QueryPlanes { seat_id: 0 }, BODY_AT),
        (
            DisplayRequest::AllocBuffer {
                seat_id: 0,
                width_px: 1,
                height_px: 1,
            },
            BODY_AT + 8,
        ),
        (
            DisplayRequest::FreeBuffer {
                seat_id: 0,
                buffer: id(1),
            },
            BODY_AT + 4,
        ),
        (sample_layers(1), BODY_AT + 4),
        (sample_layers(1), LAYERS_AT + LAYER_LEN),
        (sample_layers(1), LAYERS_AT + 37),
        (
            DisplayRequest::SetCursor {
                seat_id: 0,
                buffer: id(1),
                hot_x: 0,
                hot_y: 0,
            },
            BODY_AT + 12,
        ),
        (
            DisplayRequest::MoveCursor {
                seat_id: 0,
                x: 0,
                y: 0,
            },
            BODY_AT + 8,
        ),
        (DisplayRequest::HideCursor { seat_id: 0 }, BODY_AT),
    ] {
        let mut dirty = request.to_le_bytes();
        dirty[tail] = 1;
        assert_eq!(
            DisplayRequest::from_bytes(&dirty),
            Err(Errno::BadMagic),
            "{request:?} at {tail}"
        );
    }
}

#[test]
fn a_configure_or_allocation_names_a_drawable_extent() {
    for (width, height) in [(0u32, 480u32), (640, 0)] {
        let mut configure = sample_configure().to_le_bytes();
        configure[BODY_AT..BODY_AT + 4].copy_from_slice(&width.to_le_bytes());
        configure[BODY_AT + 4..BODY_AT + 8].copy_from_slice(&height.to_le_bytes());
        assert_eq!(
            DisplayRequest::from_bytes(&configure),
            Err(Errno::LengthOutOfRange)
        );
        let mut alloc = DisplayRequest::AllocBuffer {
            seat_id: 0,
            width_px: 1,
            height_px: 1,
        }
        .to_le_bytes();
        alloc[BODY_AT..BODY_AT + 4].copy_from_slice(&width.to_le_bytes());
        alloc[BODY_AT + 4..BODY_AT + 8].copy_from_slice(&height.to_le_bytes());
        assert_eq!(
            DisplayRequest::from_bytes(&alloc),
            Err(Errno::LengthOutOfRange)
        );
    }
    let mut format = sample_configure().to_le_bytes();
    format[BODY_AT + 8] = 9;
    assert_eq!(DisplayRequest::from_bytes(&format), Err(Errno::OutOfRange));
}

#[test]
fn a_layer_list_is_bounded_never_empty_and_every_layer_whole() {
    assert_eq!(LayerList::new(&[]).err(), Some(Errno::LengthOutOfRange));
    let past = [layer(1); MAX_ACCEL_LAYERS + 1];
    assert_eq!(LayerList::new(&past).err(), Some(Errno::LengthOutOfRange));
    let good = sample_layers(2).to_le_bytes();
    let mut over = good;
    over[BODY_AT..BODY_AT + 4]
        .copy_from_slice(&(u32::try_from(MAX_ACCEL_LAYERS).expect("small") + 1).to_le_bytes());
    assert_eq!(
        DisplayRequest::from_bytes(&over),
        Err(Errno::LengthOutOfRange)
    );
    let second = LAYERS_AT + LAYER_LEN;
    for (at, refused) in [
        (second, Errno::OutOfRange),
        (second + 4 + 8, Errno::LengthOutOfRange),
        (second + 28, Errno::LengthOutOfRange),
        (second + 32, Errno::LengthOutOfRange),
    ] {
        let mut broken = good;
        broken[at..at + 4].fill(0);
        assert_eq!(
            DisplayRequest::from_bytes(&broken),
            Err(refused),
            "the second layer's field at {at}"
        );
    }
}

#[test]
fn mode_replies_round_trip_ok_and_error() {
    let mode = DisplayMode {
        width_px: 1920,
        height_px: 1080,
        stride_bytes: 7680,
        format: DisplayFormat::Rgba8888,
    };
    for refresh in [
        None,
        RefreshRate::from_mhz(59_940),
        Some(RefreshRate::ASSUMED),
    ] {
        let output = OutputMode { mode, refresh };
        assert_eq!(
            decode_mode_reply(&encode_mode_reply(Ok(Some(output)))),
            Ok(Some(output))
        );
    }
    assert_eq!(decode_mode_reply(&encode_mode_reply(Ok(None))), Ok(None));
    for refused in [Errno::SeatNotOwner, Errno::SeatRevoked] {
        assert_eq!(
            decode_mode_reply(&encode_mode_reply(Err(refused))),
            Err(refused)
        );
    }
}

#[test]
fn mode_reply_decode_fails_closed() {
    let mode = DisplayMode {
        width_px: 640,
        height_px: 480,
        stride_bytes: 2560,
        format: DisplayFormat::Bgra8888,
    };
    let good = encode_mode_reply(Ok(Some(OutputMode {
        mode,
        refresh: RefreshRate::from_mhz(75_000),
    })));
    assert_eq!(
        decode_mode_reply(&good[..DISPLAY_MODE_REPLY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut bad_status = good;
    bad_status[0] = 1;
    assert_eq!(decode_mode_reply(&bad_status), Err(Errno::OutOfRange));
    let mut bad_format = good;
    bad_format[16] = 9;
    assert_eq!(decode_mode_reply(&bad_format), Err(Errno::OutOfRange));
    let mut dirty = good;
    dirty[19] = 1;
    assert_eq!(decode_mode_reply(&dirty), Err(Errno::BadMagic));
    let mut neither = good;
    neither[17] = 2;
    assert_eq!(decode_mode_reply(&neither), Err(Errno::BadMagic));
    let mut absent_with_mode = good;
    absent_with_mode[17] = 0;
    assert_eq!(decode_mode_reply(&absent_with_mode), Err(Errno::BadMagic));
    let mut thin_stride = good;
    thin_stride[12..16].copy_from_slice(&2559u32.to_le_bytes());
    assert_eq!(
        decode_mode_reply(&thin_stride),
        Err(Errno::LengthOutOfRange)
    );
    // A rate outside the bound is refused, never clamped into it.
    for rate in [RefreshRate::MIN_MHZ - 1, RefreshRate::MAX_MHZ + 1, u32::MAX] {
        let mut wild = good;
        wild[QUERY_REFRESH_AT..QUERY_REFRESH_AT + 4].copy_from_slice(&rate.to_le_bytes());
        assert_eq!(decode_mode_reply(&wild), Err(Errno::OutOfRange), "{rate}");
    }
    let mut rate_without_output = encode_mode_reply(Ok(None));
    rate_without_output[QUERY_REFRESH_AT..QUERY_REFRESH_AT + 4]
        .copy_from_slice(&60_000u32.to_le_bytes());
    assert_eq!(
        decode_mode_reply(&rate_without_output),
        Err(Errno::BadMagic)
    );
}

#[test]
fn an_output_stating_no_rate_is_paced_at_the_assumed_one() {
    let stated = RefreshRate::from_mhz(144_000);
    assert_eq!(
        stated.map(|rate| RefreshRate::or_assumed(Some(rate))),
        stated
    );
    assert_eq!(RefreshRate::or_assumed(None), RefreshRate::ASSUMED);
}

#[test]
fn a_rate_s_frame_is_its_period() {
    assert_eq!(RefreshRate::ASSUMED.frame_ns(), 16_666_666);
    assert_eq!(
        RefreshRate::from_mhz(144_000).map(RefreshRate::frame_ns),
        Some(6_944_444)
    );
    assert_eq!(
        RefreshRate::from_mhz(RefreshRate::MIN_MHZ).map(RefreshRate::frame_ns),
        Some(1_000_000_000)
    );
    assert_eq!(
        RefreshRate::from_mhz(RefreshRate::MAX_MHZ).map(RefreshRate::frame_ns),
        Some(1_000_000)
    );
    assert_eq!(RefreshRate::from_mhz(0), None);
}

fn frames(count: usize) -> ConfiguredFrames {
    let grants: [BufferGrant; DISPLAY_MAX_FRAMES as usize] = core::array::from_fn(|index| {
        let raw = u32::try_from(index).expect("small") + 1;
        BufferGrant {
            buffer: id(raw),
            grant: 0x100 + u64::from(raw),
        }
    });
    ConfiguredFrames::new(2560, grantor(), &grants[..count]).expect("within the bound")
}

#[test]
fn configure_replies_round_trip_every_frame_count() {
    for count in 1..=DISPLAY_MAX_FRAMES as usize {
        let reply = frames(count);
        let decoded = decode_configure_reply(&encode_configure_reply(Ok(reply)));
        assert_eq!(decoded, Ok(reply));
        assert_eq!(decoded.map(|frames| frames.count()), Ok(count));
    }
    assert_eq!(
        decode_configure_reply(&encode_configure_reply(Err(Errno::Stale))),
        Err(Errno::Stale)
    );
    assert_eq!(
        ConfiguredFrames::new(2560, grantor(), &[]).err(),
        Some(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_configure_reply_decode_fails_closed() {
    let good = encode_configure_reply(Ok(frames(2)));
    assert_eq!(
        decode_configure_reply(&good[..DISPLAY_CONFIGURE_REPLY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let first = 16 + 16;
    for (at, value, refused) in [
        (4, 0u32, Errno::LengthOutOfRange),
        (4, DISPLAY_MAX_FRAMES + 1, Errno::LengthOutOfRange),
        (8, 0, Errno::LengthOutOfRange),
        (12, 1, Errno::BadMagic),
        (first, 0, Errno::OutOfRange),
        (first + 4, 1, Errno::BadMagic),
        (first + 8, 0, Errno::OutOfRange),
        (first + 2 * 16, 1, Errno::BadMagic),
    ] {
        let mut broken = good;
        broken[at..at + 4].copy_from_slice(&value.to_le_bytes());
        if at == first + 8 {
            broken[at + 4..at + 8].fill(0);
        }
        assert_eq!(
            decode_configure_reply(&broken),
            Err(refused),
            "the word at {at}"
        );
    }
    let mut kernel = good;
    kernel[16..32].copy_from_slice(&ProcId::KERNEL.to_le_bytes());
    assert_eq!(decode_configure_reply(&kernel), Err(Errno::OutOfRange));
}

#[test]
fn buffer_replies_round_trip_and_fail_closed() {
    let granted = GrantedBuffer {
        grant: BufferGrant {
            buffer: id(12),
            grant: 0x77,
        },
        stride_bytes: 256,
        grantor: grantor(),
    };
    let good = encode_buffer_reply(Ok(granted));
    assert_eq!(decode_buffer_reply(&good), Ok(granted));
    assert_eq!(
        decode_buffer_reply(&encode_buffer_reply(Err(Errno::NoSpace))),
        Err(Errno::NoSpace)
    );
    assert_eq!(
        decode_buffer_reply(&good[..DISPLAY_BUFFER_REPLY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut no_stride = good;
    no_stride[4..8].fill(0);
    assert_eq!(
        decode_buffer_reply(&no_stride),
        Err(Errno::LengthOutOfRange)
    );
    let mut no_buffer = good;
    no_buffer[8..12].fill(0);
    assert_eq!(decode_buffer_reply(&no_buffer), Err(Errno::OutOfRange));
}

#[test]
fn present_replies_round_trip_and_fail_closed() {
    for presented in [Presented::Shown, Presented::Queued] {
        let reply = PresentReply { presented, seq: 41 };
        assert_eq!(
            decode_present_reply(&encode_present_reply(Ok(reply))),
            Ok(reply)
        );
    }
    assert_eq!(
        decode_present_reply(&encode_present_reply(Err(Errno::Busy))),
        Err(Errno::Busy)
    );
    let good = encode_present_reply(Ok(PresentReply {
        presented: Presented::Shown,
        seq: 1,
    }));
    assert_eq!(
        decode_present_reply(&good[..DISPLAY_PRESENT_REPLY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    for (at, value) in [(4, 0u8), (4, 3), (5, 1)] {
        let mut broken = good;
        broken[at] = value;
        assert_eq!(decode_present_reply(&broken), Err(Errno::BadMagic));
    }
}

fn caps() -> AccelCaps {
    AccelCaps {
        max_layers: 6,
        max_width_px: 1920,
        max_height_px: 1080,
        per_layer_opacity: true,
        hw_scaling: true,
    }
}

const CURSOR: CursorCaps = CursorCaps {
    max_width_px: 64,
    max_height_px: 64,
};

#[test]
fn planes_replies_round_trip_every_shape() {
    for planes in [
        Planes {
            accel: Some(caps()),
            cursor: Some(CURSOR),
        },
        Planes {
            accel: None,
            cursor: Some(CURSOR),
        },
        Planes {
            accel: Some(AccelCaps {
                per_layer_opacity: false,
                hw_scaling: false,
                ..caps()
            }),
            cursor: None,
        },
        Planes {
            accel: None,
            cursor: None,
        },
    ] {
        assert_eq!(
            decode_planes_reply(&encode_planes_reply(Ok(planes))),
            Ok(planes)
        );
    }
}

#[test]
fn a_planes_reply_decode_fails_closed() {
    let good = encode_planes_reply(Ok(Planes {
        accel: Some(caps()),
        cursor: Some(CURSOR),
    }));
    assert_eq!(
        decode_planes_reply(&good[..DISPLAY_PLANES_REPLY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut unknown = good;
    unknown[4] |= 1 << 4;
    assert_eq!(decode_planes_reply(&unknown), Err(Errno::BadMagic));
    for (at, value) in [
        (8usize, 0u32),
        (8, u32::try_from(MAX_ACCEL_LAYERS).expect("small") + 1),
        (12, 0),
        (20, 0),
    ] {
        let mut broken = good;
        broken[at..at + 4].copy_from_slice(&value.to_le_bytes());
        assert_eq!(
            decode_planes_reply(&broken),
            Err(Errno::LengthOutOfRange),
            "the word at {at}"
        );
    }
    let mut unflagged = good;
    unflagged[4..8].fill(0);
    assert_eq!(
        decode_planes_reply(&unflagged),
        Err(Errno::LengthOutOfRange),
        "limits beside flags saying there is nothing"
    );
}

#[test]
fn events_round_trip_and_fail_closed() {
    let event = DisplayEvent::Presented { seq: u64::MAX };
    let good = event.to_le_bytes();
    assert_eq!(DisplayEvent::from_bytes(&good), Ok(event));
    assert_eq!(
        DisplayEvent::from_bytes(&good[..DisplayEvent::WIRE_LEN - 1]),
        Err(Errno::LengthOutOfRange)
    );
    let mut magic = good;
    magic[0] ^= 1;
    assert_eq!(DisplayEvent::from_bytes(&magic), Err(Errno::BadMagic));
    let mut version = good;
    version[4] = 2;
    assert_eq!(
        DisplayEvent::from_bytes(&version),
        Err(Errno::AbiVersionUnsupported)
    );
    let mut kind = good;
    kind[6] = 9;
    assert_eq!(DisplayEvent::from_bytes(&kind), Err(Errno::OutOfRange));
}

fn stats_mode() -> DisplayMode {
    DisplayMode {
        width_px: 640,
        height_px: 480,
        stride_bytes: 2560,
        format: DisplayFormat::Bgra8888,
    }
}

/// An accelerated device with memory of its own and a cursor plane: every
/// optional part of the reply populated.
fn accelerated_stats() -> DisplayStats {
    DisplayStats {
        seat_id: 3,
        busy_ns: 4_000_000,
        idle_ns: 96_000_000,
        device: DisplayDeviceReport {
            mem_resident_bytes: 8 << 20,
            mem_total_bytes: 256 << 20,
            accel: Some(caps()),
            cursor: Some(CURSOR),
        },
        mode: Some(stats_mode()),
    }
}

#[test]
fn stats_reply_round_trips_every_device_shape() {
    let software = DisplayStats {
        seat_id: 0,
        busy_ns: 0,
        idle_ns: 0,
        device: DisplayDeviceReport::SOFTWARE,
        mode: Some(stats_mode()),
    };
    for stats in [
        accelerated_stats(),
        software,
        DisplayStats {
            mode: None,
            ..accelerated_stats()
        },
    ] {
        assert_eq!(
            decode_stats_reply(&encode_stats_reply(Ok(stats))),
            Ok(stats)
        );
    }
    let refused = encode_stats_reply(Err(Errno::PermissionDenied));
    assert_eq!(refused.len(), DISPLAY_STATS_REPLY_LEN);
    assert_eq!(decode_stats_reply(&refused), Err(Errno::PermissionDenied));
}

#[test]
fn stats_reply_decode_fails_closed() {
    let good = encode_stats_reply(Ok(accelerated_stats()));
    let flags = STATS_BODY_AT + 66;
    assert_eq!(
        decode_stats_reply(&good[..DISPLAY_STATS_REPLY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut bad_status = good;
    bad_status[0] = 1;
    assert_eq!(decode_stats_reply(&bad_status), Err(Errno::OutOfRange));
    let mut unknown_flag = good;
    unknown_flag[flags] |= 1 << 5;
    assert_eq!(decode_stats_reply(&unknown_flag), Err(Errno::BadMagic));
    for dirty in [STATS_BODY_AT + 65, STATS_BODY_AT + 71] {
        let mut broken = good;
        broken[dirty] = 1;
        assert_eq!(decode_stats_reply(&broken), Err(Errno::BadMagic));
    }
    let mut over_resident = good;
    over_resident[STATS_BODY_AT + 16..STATS_BODY_AT + 24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        decode_stats_reply(&over_resident),
        Err(Errno::LengthOutOfRange)
    );
    let mut unflagged_caps = good;
    unflagged_caps[flags] &= !1;
    assert_eq!(
        decode_stats_reply(&unflagged_caps),
        Err(Errno::LengthOutOfRange)
    );
    let mut no_layers = good;
    no_layers[STATS_BODY_AT + 40..STATS_BODY_AT + 44].fill(0);
    assert_eq!(decode_stats_reply(&no_layers), Err(Errno::LengthOutOfRange));
    let mut unflagged_cursor = good;
    unflagged_cursor[flags] &= !(1 << 4);
    assert_eq!(
        decode_stats_reply(&unflagged_cursor),
        Err(Errno::LengthOutOfRange)
    );
    let mut empty_cursor = good;
    empty_cursor[STATS_BODY_AT + 72..STATS_BODY_AT + 76].fill(0);
    assert_eq!(
        decode_stats_reply(&empty_cursor),
        Err(Errno::LengthOutOfRange)
    );
    let mut bad_format = good;
    bad_format[STATS_BODY_AT + 64] = 9;
    assert_eq!(decode_stats_reply(&bad_format), Err(Errno::OutOfRange));
    let mut stray_mode = good;
    stray_mode[flags] &= !(1 << 2);
    assert_eq!(
        decode_stats_reply(&stray_mode),
        Err(Errno::LengthOutOfRange)
    );
}
