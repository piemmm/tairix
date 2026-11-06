//! Host tests of the recorder protocol: every message is its own encoding, and
//! every one that is not exactly a message is refused whole.

use super::*;

/// A way of spoiling an encoded message.
type Spoil = fn(&mut [u8]);

fn session_round_trip(message: &SessionMessage) -> ([u8; RECORDER_SESSION_MESSAGE_MAX], usize) {
    let mut out = [0u8; RECORDER_SESSION_MESSAGE_MAX];
    let len = message.encode(&mut out).expect("encodes");
    assert_eq!(SessionMessage::decode(&out[..len]), Ok(*message));
    (out, len)
}

fn recorder_round_trip(message: &RecorderMessage) -> ([u8; RECORDER_MESSAGE_MAX], usize) {
    let mut out = [0u8; RECORDER_MESSAGE_MAX];
    let len = message.encode(&mut out).expect("encodes");
    assert_eq!(RecorderMessage::decode(&out[..len]), Ok(*message));
    (out, len)
}

fn start() -> RecordStart {
    RecordStart {
        slot: RecorderSlot::new(1920, 1080).expect("a slot"),
        sound: RecordSound::Both,
        output: 7,
        slots: [11, 12],
        started_ns: 0x0123_4567_89AB_CDEF,
    }
}

fn damage() -> RecorderDamage {
    RecorderDamage::new(&[
        DamageRect {
            x: 0,
            y: 0,
            width_px: 10,
            height_px: 4,
        },
        DamageRect {
            x: 20,
            y: 30,
            width_px: 5,
            height_px: 6,
        },
    ])
    .expect("damage")
}

#[test]
fn every_message_round_trips_to_its_own_encoding() {
    session_round_trip(&SessionMessage::Start(start()));
    session_round_trip(&SessionMessage::Frame {
        slot: 1,
        at_ns: 99,
        damage: damage(),
    });
    session_round_trip(&SessionMessage::Stop);
    recorder_round_trip(&RecorderMessage::Hello);
    recorder_round_trip(&RecorderMessage::Ready);
    recorder_round_trip(&RecorderMessage::Released { slot: 0 });
    recorder_round_trip(&RecorderMessage::Finished {
        bytes: 1 << 40,
        ending: RecordEnding::DiskFull,
    });
    recorder_round_trip(&RecorderMessage::Finished {
        bytes: 0,
        ending: RecordEnding::Stopped,
    });
    recorder_round_trip(&RecorderMessage::Unstarted(Errno::PermissionDenied));
}

#[test]
fn a_start_of_another_protocol_area_sound_or_output_is_refused() {
    let cases: [(Spoil, Errno); 5] = [
        (|wire| wire[2] ^= 1, Errno::AbiVersionUnsupported),
        (|wire| wire[4..8].fill(0), Errno::OutOfRange),
        (|wire| wire[1] = 4, Errno::OutOfRange),
        (|wire| wire[16..24].fill(0), Errno::OutOfRange),
        (|wire| wire[13] = 1, Errno::BadMagic),
    ];
    for (spoil, refusal) in cases {
        let (mut wire, len) = session_round_trip(&SessionMessage::Start(start()));
        spoil(&mut wire);
        assert_eq!(SessionMessage::decode(&wire[..len]), Err(refusal));
    }
}

#[test]
fn a_frame_naming_no_change_or_a_slot_past_the_ring_is_refused() {
    let frame = SessionMessage::Frame {
        slot: 0,
        at_ns: 5,
        damage: damage(),
    };
    let (mut wire, len) = session_round_trip(&frame);
    wire[1] = 2;
    assert_eq!(SessionMessage::decode(&wire[..len]), Err(Errno::OutOfRange));
    let (mut wire, len) = session_round_trip(&frame);
    wire[2] = 1;
    assert_eq!(
        SessionMessage::decode(&wire[..len]),
        Err(Errno::BadMagic),
        "a rectangle behind the count is refused"
    );
    wire[2] = 0;
    wire[FRAME_RECTS_AT..].fill(0);
    assert_eq!(
        SessionMessage::decode(&wire[..len]),
        Err(Errno::LengthOutOfRange),
        "a frame names what changed"
    );
    wire[2] = 65;
    assert_eq!(
        SessionMessage::decode(&wire[..len]),
        Err(Errno::LengthOutOfRange)
    );
    let mut out = [0u8; RECORDER_SESSION_MESSAGE_MAX];
    let past = SessionMessage::Frame {
        slot: 2,
        at_ns: 5,
        damage: damage(),
    };
    assert_eq!(past.encode(&mut out), Err(Errno::OutOfRange));
}

#[test]
fn a_message_of_the_wrong_length_or_with_a_stray_byte_is_refused() {
    let messages = [
        RecorderMessage::Released { slot: 1 },
        RecorderMessage::Finished {
            bytes: 9,
            ending: RecordEnding::Stopped,
        },
        RecorderMessage::Unstarted(Errno::NoSpace),
    ];
    for message in messages {
        let (wire, len) = recorder_round_trip(&message);
        assert_eq!(
            RecorderMessage::decode(&wire[..len - 1]),
            Err(Errno::BufferTooSmall),
            "{message:?}"
        );
        let mut longer = [0u8; RECORDER_MESSAGE_MAX + 1];
        longer[..len].copy_from_slice(&wire[..len]);
        assert_eq!(
            RecorderMessage::decode(&longer[..=len]),
            Err(Errno::LengthOutOfRange),
            "{message:?}"
        );
    }
    for (message, stray) in [
        (
            RecorderMessage::Finished {
                bytes: 9,
                ending: RecordEnding::Stopped,
            },
            3,
        ),
        (RecorderMessage::Unstarted(Errno::NoSpace), 2),
    ] {
        let (mut wire, len) = recorder_round_trip(&message);
        wire[stray] = 1;
        assert_eq!(
            RecorderMessage::decode(&wire[..len]),
            Err(Errno::BadMagic),
            "{message:?}"
        );
    }
    assert_eq!(RecorderMessage::decode(&[0x7F]), Err(Errno::BadMagic));
    assert_eq!(RecorderMessage::decode(&[]), Err(Errno::LengthOutOfRange));
    for op in [OP_HELLO, OP_READY] {
        assert_eq!(
            RecorderMessage::decode(&[op, 0]),
            Err(Errno::LengthOutOfRange)
        );
    }
    assert_eq!(
        SessionMessage::decode(&[OP_STOP, 0]),
        Err(Errno::LengthOutOfRange)
    );
    let (wire, _) = session_round_trip(&SessionMessage::Start(start()));
    assert_eq!(
        SessionMessage::decode(&wire[..START_SLOTS_AT]),
        Err(Errno::BufferTooSmall),
        "a start without its slots' grants"
    );
}

/// A recorder names no process: the session has its child's from the kernel,
/// so a `Hello` carrying one, a claim a recorder could forge, is no `Hello`.
#[test]
fn a_hello_names_no_process() {
    let (wire, len) = recorder_round_trip(&RecorderMessage::Hello);
    assert_eq!(wire[..len], [OP_HELLO]);
    let mut named = [0x5C; 24];
    named[0] = OP_HELLO;
    named[1..8].fill(0);
    assert_eq!(
        RecorderMessage::decode(&named),
        Err(Errno::LengthOutOfRange)
    );
}

/// The slots are the session's to grant: a `Start` naming no grant for one,
/// or one grant for both, is refused.
#[test]
fn a_start_granting_no_slot_or_one_slot_twice_is_refused() {
    let (mut wire, len) = session_round_trip(&SessionMessage::Start(start()));
    wire[START_SLOTS_AT + 8..START_SLOTS_AT + 16].copy_from_slice(&11u64.to_le_bytes());
    assert_eq!(
        SessionMessage::decode(&wire[..len]),
        Err(Errno::OutOfRange),
        "one grant for both slots"
    );
    wire[START_SLOTS_AT..START_SLOTS_AT + 8].fill(0);
    wire[START_SLOTS_AT + 8..START_SLOTS_AT + 16].copy_from_slice(&12u64.to_le_bytes());
    assert_eq!(
        SessionMessage::decode(&wire[..len]),
        Err(Errno::OutOfRange),
        "a slot with no grant"
    );
}

/// The slots reach the recorder in the session's `Start`, so a `Ready`
/// carrying grants is no `Ready`.
#[test]
fn a_ready_carries_no_grant() {
    let (wire, len) = recorder_round_trip(&RecorderMessage::Ready);
    assert_eq!(wire[..len], [OP_READY]);
    let mut granting = [0u8; 24];
    granting[0] = OP_READY;
    granting[8..16].copy_from_slice(&3u64.to_le_bytes());
    granting[16..24].copy_from_slice(&4u64.to_le_bytes());
    assert_eq!(
        RecorderMessage::decode(&granting),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_release_past_the_ring_or_an_unknown_ending_is_refused() {
    let (mut wire, len) = recorder_round_trip(&RecorderMessage::Released { slot: 1 });
    wire[1] = 2;
    assert_eq!(
        RecorderMessage::decode(&wire[..len]),
        Err(Errno::OutOfRange)
    );
    let (mut wire, len) = recorder_round_trip(&RecorderMessage::Finished {
        bytes: 1,
        ending: RecordEnding::DiskFull,
    });
    wire[1] = 3;
    assert_eq!(
        RecorderMessage::decode(&wire[..len]),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_slot_is_laid_out_as_a_window_frame_of_the_area() {
    let slot = RecorderSlot::new(7, 5).expect("a slot");
    assert_eq!((slot.len(), slot.stride()), (7 * 5 * 4, 28));
    let mode = slot.display_mode();
    assert_eq!(
        (mode.width_px, mode.height_px, mode.stride_bytes),
        (7, 5, 28)
    );
    assert_eq!(RecorderSlot::new(0, 5), Err(Errno::OutOfRange));
    assert_eq!(RecorderSlot::new(5, 0), Err(Errno::OutOfRange));
    assert_eq!(RecorderSlot::new(u32::MAX, 2), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_sound_choice_says_what_it_hears() {
    let heard: [(bool, bool); 4] =
        RecordSound::ALL.map(|sound| (sound.microphone(), sound.desktop()));
    assert_eq!(
        heard,
        [(false, false), (true, false), (false, true), (true, true)]
    );
}

#[test]
fn the_widest_frame_is_the_longest_message_the_session_sends() {
    let rects: [DamageRect; RECORDER_DAMAGE_RECTS] = core::array::from_fn(|index| DamageRect {
        x: u32::try_from(index).expect("small") * 4,
        y: 0,
        width_px: 2,
        height_px: 2,
    });
    let frame = SessionMessage::Frame {
        slot: 1,
        at_ns: u64::MAX,
        damage: RecorderDamage::new(&rects).expect("damage"),
    };
    let (_, len) = session_round_trip(&frame);
    assert_eq!(len, RECORDER_SESSION_MESSAGE_MAX);
}
