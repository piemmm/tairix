//! `cue-v1`: every request and reply round-trips, and every malformed frame
//! is refused rather than guessed at.

extern crate alloc;

use alloc::vec::Vec;

use super::*;
use crate::ipc::is_reserved_endpoint;
use crate::origin::CapabilitySummary;
use crate::ProcId;

fn origin(trust_domain: TrustDomain, uid: u32, pid: u64, caps: &[CapabilityId]) -> Origin {
    let mut summary = CapabilitySummary::EMPTY;
    for cap in caps {
        summary.insert(*cap);
    }
    Origin::new(
        trust_domain,
        uid,
        0,
        pid,
        ProcId::from_raw([0x5A; 16]),
        summary,
        0,
    )
}

fn user(uid: u32) -> Origin {
    origin(TrustDomain::User, uid, 0x1234, &[])
}

fn frame(request: &CueRequest<'_>) -> Vec<u8> {
    let mut out = [0u8; CUE_REQUEST_MAX];
    let len = request.encode(&mut out).expect("encodes");
    out[..len].to_vec()
}

const DOCUMENT: &str = "sound.enabled = true\nsound.gain = -6dB\n";

fn policy(document: &str) -> CueRequest<'_> {
    let mut files = [0u64; SoundEvent::ALL.len()];
    files[SoundEvent::Error.index()] = 7;
    files[SoundEvent::Bell.index()] = u64::MAX;
    CueRequest::Policy { document, files }
}

#[test]
fn the_cue_authority_is_a_reserved_rendezvous() {
    assert!(is_reserved_endpoint(SOUND_CUE_ENDPOINT));
}

#[test]
fn every_event_has_one_discriminant_one_name_and_its_authority() {
    let decoded: Vec<SoundEvent> = (0..=u8::MAX)
        .filter_map(|raw| SoundEvent::from_u8(raw).ok())
        .collect();
    assert_eq!(decoded, SoundEvent::ALL);
    for event in SoundEvent::ALL {
        assert_eq!(SoundEvent::from_u8(event.as_u8()), Ok(event));
        assert_eq!(SoundEvent::from_name(event.name()), Some(event));
        assert_eq!(SoundEvent::ALL[event.index()], event);
    }
    let lifecycle: Vec<&str> = SoundEvent::ALL
        .into_iter()
        .filter(|event| event.authority() == CueAuthority::Lifecycle)
        .map(SoundEvent::name)
        .collect();
    assert_eq!(
        lifecycle,
        [
            "startup",
            "shutdown",
            "login",
            "logout",
            "device_attached",
            "device_detached"
        ]
    );
    assert_eq!(SoundEvent::from_name("Startup"), None);
}

/// Only the attested principal that made each transition owns its sound, and
/// no application event is anybody's to own.
#[test]
fn each_lifecycle_event_is_owned_only_by_the_principal_that_made_it() {
    let init = origin(TrustDomain::User, 0, INIT_PID, &[]);
    let power = origin(
        TrustDomain::User,
        1000,
        0x1234,
        &[CapabilityId::SYSTEM_POWER],
    );
    let login = user(LOGIN_UID);
    let devmgr = user(DEVMGR_UID);
    let principals = [&init, &power, &login, &devmgr];
    let owner = |event: SoundEvent| match event {
        SoundEvent::Startup => Some(0),
        SoundEvent::Shutdown => Some(1),
        SoundEvent::Login | SoundEvent::Logout => Some(2),
        SoundEvent::DeviceAttached | SoundEvent::DeviceDetached => Some(3),
        _ => None,
    };
    for event in SoundEvent::ALL {
        assert_eq!(
            owner(event).is_some(),
            event.authority() == CueAuthority::Lifecycle
        );
        for (index, principal) in principals.iter().enumerate() {
            assert_eq!(
                event.owned_by(principal),
                owner(event) == Some(index),
                "{event:?} from principal {index}"
            );
        }
        for bystander in [user(1000), user(SOUNDD_UID), user(0)] {
            assert!(!event.owned_by(&bystander), "{event:?}");
        }
        // A kernel-domain record owns nothing, whatever its fields read.
        let forged = origin(
            TrustDomain::Kernel,
            LOGIN_UID,
            INIT_PID,
            &[CapabilityId::SYSTEM_POWER],
        );
        assert!(!event.owned_by(&forged), "{event:?}");
    }
}

#[test]
fn only_the_cue_service_account_is_the_cue_authority() {
    assert!(from_cue_authority(&user(SOUNDD_UID)));
    assert!(!from_cue_authority(&user(1000)));
    assert!(!from_cue_authority(&user(0)));
    assert!(!from_cue_authority(&origin(
        TrustDomain::Kernel,
        SOUNDD_UID,
        7,
        &[]
    )));
}

#[test]
fn a_cue_round_trips_for_every_event_and_answer() {
    for event in SoundEvent::ALL {
        for answer in [Answer::Started, Answer::PlayedOut] {
            let request = CueRequest::Cue { event, answer };
            let frame = frame(&request);
            assert_eq!(frame.len(), CUE_REQUEST_LEN);
            assert_eq!(CueRequest::from_bytes(&frame), Ok(request));
        }
    }
}

#[test]
fn a_malformed_cue_is_refused() {
    let good = frame(&CueRequest::Cue {
        event: SoundEvent::Bell,
        answer: Answer::Started,
    });
    assert_eq!(
        CueRequest::from_bytes(&good[..HEADER_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    assert_eq!(
        CueRequest::from_bytes(&good[..CUE_REQUEST_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut long = [0u8; CUE_REQUEST_LEN + 1];
    long[..CUE_REQUEST_LEN].copy_from_slice(&good);
    assert_eq!(CueRequest::from_bytes(&long), Err(Errno::LengthOutOfRange));

    let flipped = |at: usize, value: u8| {
        let mut frame = good.clone();
        frame[at] = value;
        CueRequest::from_bytes(&frame).map(|_| ())
    };
    assert_eq!(flipped(0, b'X'), Err(Errno::BadMagic));
    assert_eq!(flipped(4, 2), Err(Errno::AbiVersionUnsupported));
    assert_eq!(flipped(6, 0), Err(Errno::OutOfRange));
    assert_eq!(
        flipped(6, 2),
        Err(Errno::BufferTooSmall),
        "a policy's frame"
    );
    assert_eq!(flipped(6, 3), Err(Errno::OutOfRange));
    assert_eq!(flipped(7, 1), Err(Errno::BadMagic));
    assert_eq!(flipped(HEADER_LEN, 0), Err(Errno::OutOfRange));
    assert_eq!(flipped(HEADER_LEN, 14), Err(Errno::OutOfRange));
    assert_eq!(flipped(HEADER_LEN + 1, 2), Err(Errno::OutOfRange));
    for at in HEADER_LEN + 2..CUE_REQUEST_LEN {
        assert_eq!(flipped(at, 1), Err(Errno::BadMagic), "reserved byte {at}");
    }
}

#[test]
fn a_policy_round_trips_its_document_and_every_file_slot() {
    for document in ["", DOCUMENT, "sound.theme = Héloïse\n"] {
        let request = policy(document);
        let frame = frame(&request);
        assert_eq!(
            frame.len(),
            CUE_REQUEST_MAX - POLICY_DOCUMENT_MAX + document.len()
        );
        assert_eq!(CueRequest::from_bytes(&frame), Ok(request));
    }
    let longest = "a".repeat(POLICY_DOCUMENT_MAX);
    let frame = frame(&policy(&longest));
    assert_eq!(frame.len(), CUE_REQUEST_MAX);
    assert_eq!(CueRequest::from_bytes(&frame), Ok(policy(&longest)));
}

/// What the decode would refuse, the encoder never writes.
#[test]
fn a_policy_no_decode_would_take_is_never_encoded() {
    let mut out = [0u8; CUE_REQUEST_MAX];
    let over = "a".repeat(POLICY_DOCUMENT_MAX + 1);
    assert_eq!(policy(&over).encode(&mut out), Err(Errno::LengthOutOfRange));
    assert_eq!(policy("a\tb").encode(&mut out), Err(Errno::OutOfRange));
    assert_eq!(policy("a\rb").encode(&mut out), Err(Errno::OutOfRange));
    let mut short = [0u8; CUE_REQUEST_LEN];
    assert_eq!(policy("").encode(&mut short), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_malformed_policy_is_refused() {
    let good = frame(&policy(DOCUMENT));
    let at_len = good.len() - DOCUMENT.len() - 4;
    assert_eq!(
        CueRequest::from_bytes(&good[..at_len + 3]),
        Err(Errno::BufferTooSmall)
    );
    assert_eq!(
        CueRequest::from_bytes(&good[..good.len() - 1]),
        Err(Errno::BufferTooSmall),
        "a document shorter than it says"
    );
    let mut long = good.clone();
    long.push(b'x');
    assert_eq!(
        CueRequest::from_bytes(&long),
        Err(Errno::LengthOutOfRange),
        "a byte past the document"
    );
    let altered = |at: usize, value: u8| {
        let mut frame = good.clone();
        frame[at] = value;
        CueRequest::from_bytes(&frame).map(|_| ())
    };
    assert_eq!(altered(at_len + 2, 1), Err(Errno::BadMagic));
    assert_eq!(altered(at_len + 3, 1), Err(Errno::BadMagic));
    let body = good.len() - DOCUMENT.len();
    assert_eq!(altered(body, 0xFF), Err(Errno::OutOfRange), "not UTF-8");
    assert_eq!(
        altered(body, b'\t'),
        Err(Errno::OutOfRange),
        "a control character"
    );
    // A declared length past the bound is refused even where the frame is
    // exactly that long.
    let over = POLICY_DOCUMENT_MAX + 1;
    let mut oversized = good[..body].to_vec();
    oversized.resize(body + over, b'a');
    oversized[at_len..at_len + 2]
        .copy_from_slice(&u16::try_from(over).expect("fits").to_le_bytes());
    assert_eq!(
        CueRequest::from_bytes(&oversized),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn every_answer_round_trips() {
    let silences = [
        Silence::Disabled,
        Silence::NoAsset,
        Silence::Undecodable,
        Silence::Coalesced,
        Silence::TooMany,
        Silence::NoDevice,
    ];
    let decoded: Vec<Silence> = (0..=u8::MAX)
        .filter_map(|raw| Silence::from_u8(raw).ok())
        .collect();
    assert_eq!(decoded, silences);
    let answers = silences
        .into_iter()
        .map(|silence| Ok(CueOutcome::Silent(silence)))
        .chain([
            Ok(CueOutcome::Played),
            Err(Errno::PermissionDenied),
            Err(Errno::SeatNotOwner),
            Err(Errno::LimitExceeded),
        ]);
    for answer in answers {
        assert_eq!(decode_cue_reply(&encode_cue_reply(answer)), answer);
    }
}

#[test]
fn a_malformed_answer_is_refused() {
    let played = encode_cue_reply(Ok(CueOutcome::Played));
    assert_eq!(
        decode_cue_reply(&played[..CUE_REPLY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut long = [0u8; CUE_REPLY_LEN + 1];
    long[..CUE_REPLY_LEN].copy_from_slice(&played);
    assert_eq!(decode_cue_reply(&long), Err(Errno::LengthOutOfRange));

    let flipped = |frame: [u8; CUE_REPLY_LEN], at: usize, value: u8| {
        let mut frame = frame;
        frame[at] = value;
        decode_cue_reply(&frame)
    };
    let silent = encode_cue_reply(Ok(CueOutcome::Silent(Silence::NoAsset)));
    assert_eq!(flipped(played, STATUS_REPLY_LEN, 0), Err(Errno::OutOfRange));
    assert_eq!(flipped(played, STATUS_REPLY_LEN, 3), Err(Errno::OutOfRange));
    assert_eq!(
        flipped(played, STATUS_REPLY_LEN + 1, 2),
        Err(Errno::BadMagic),
        "a played cue carries no silence"
    );
    assert_eq!(
        flipped(silent, STATUS_REPLY_LEN + 1, 0),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        flipped(silent, STATUS_REPLY_LEN + 1, 7),
        Err(Errno::OutOfRange)
    );
    for at in STATUS_REPLY_LEN + 2..CUE_REPLY_LEN {
        assert_eq!(
            flipped(silent, at, 1),
            Err(Errno::BadMagic),
            "reserved byte {at}"
        );
    }
    let mut undefined = [0u8; CUE_REPLY_LEN];
    undefined[..STATUS_REPLY_LEN].copy_from_slice(&(-9999i32).to_le_bytes());
    assert_eq!(decode_cue_reply(&undefined), Err(Errno::OutOfRange));
}

/// A request is answered in its operation's shape even when its body is
/// refused, so a policy's refusal is the status word its sender reads.
#[test]
fn a_frame_names_its_operation_whatever_its_body_holds() {
    let policy = frame(&policy(DOCUMENT));
    assert!(names_policy(&policy));
    let mut dirty = policy.clone();
    dirty.push(b'x');
    assert!(CueRequest::from_bytes(&dirty).is_err());
    assert!(
        names_policy(&dirty),
        "a refused body still names its operation"
    );
    let cue = frame(&CueRequest::Cue {
        event: SoundEvent::Bell,
        answer: Answer::Started,
    });
    assert!(!names_policy(&cue));
    let mut versioned = policy.clone();
    versioned[4] = 2;
    assert!(
        !names_policy(&versioned),
        "an operation is read only under v1"
    );
    let mut reserved = policy;
    reserved[7] = 1;
    assert!(!names_policy(&reserved));
    assert!(!names_policy(&cue[..HEADER_LEN - 1]));
}
