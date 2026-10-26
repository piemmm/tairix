//! Deterministic fuzz harness for the SMB 3 engine: every decoder, the framer
//! and the client connection core, fed what a hostile server could send.
//!
//! The invariants:
//!
//! * no decoder panics on any bytes, and a message one accepts re-encodes to a
//!   message that decodes to the same value;
//! * the framer yields the same frames however a stream is split;
//! * nothing received makes the connection panic or hold more than
//!   [`MAX_CREDITS`], and after its first failure every call answers that
//!   failure;
//! * a sealed frame with any byte changed is never accepted.
//!
//! A per-run-seeded `Prng` draws noise, mutates messages the encoders built,
//! and splices them. A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep
//! once from a fresh, logged seed; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend it to a wall-clock budget.

use core::fmt::Debug;

use tairix_fuzzseed::Prng;
use tairix_smb::connection::{Connection, Outgoing, Received, MAX_CREDITS};
use tairix_smb::crypt::{KeyInputs, PreauthHash, SessionKeys, Transform, TRANSFORM_HEADER_LEN};
use tairix_smb::frame::{self, Framer, PREFIX_LEN};
use tairix_smb::header::{Address, Command, Flags, Header, HEADER_LEN};
use tairix_smb::ioctl::{IoctlRequest, IoctlResponse, IS_FSCTL, NO_FILE};
use tairix_smb::negotiate::{
    check_validate_negotiate, Capabilities, Cipher, Dialect, NegotiateContext, NegotiateRequest,
    NegotiateResponse, Offer, SecurityMode, SigningAlgorithm,
};
use tairix_smb::session::{SessionFlags, SessionSetupRequest, SessionSetupResponse};
use tairix_smb::simple::{decode_empty, encode_empty, ErrorResponse};
use tairix_smb::status::NtStatus;
use tairix_smb::tree::{ShareType, TreeConnectRequest, TreeConnectResponse};
use tairix_smb::wire::Encoder;
use tairix_smb::{Role, SmbError};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 4_000;

/// Largest noise input.
const MAX_NOISE: usize = 512;

const SESSION: u64 = 0x0000_1000_0000_0025;

fn header(command: Command, response: bool, message_id: u64, status: NtStatus) -> Header {
    Header {
        credit_charge: 1,
        status,
        command,
        credits: 1,
        flags: if response {
            Flags::SERVER_TO_REDIR
        } else {
            Flags::NONE
        },
        next_command: 0,
        message_id,
        address: Address::Sync { tree_id: 1 },
        session_id: SESSION,
        signature: [0; 16],
    }
}

fn message(header: &Header, body: impl FnOnce(&mut Encoder)) -> Vec<u8> {
    let mut enc = Encoder::new();
    header.encode(&mut enc);
    body(&mut enc);
    enc.finish().expect("encodes")
}

/// Whether `decode` accepts `input`. A value it accepts, re-encoded behind
/// `input`'s own header, must decode to the same value — unless it holds what
/// this engine never sends, which the encoder refuses.
fn stable<T: PartialEq + Debug>(
    input: &[u8],
    decode: fn(&[u8]) -> Result<T, SmbError>,
    encode: fn(&T, &mut Encoder),
) -> bool {
    let Ok(value) = decode(input) else {
        return false;
    };
    let mut enc = Encoder::new();
    enc.bytes(&input[..HEADER_LEN]);
    encode(&value, &mut enc);
    match enc.finish() {
        Ok(again) => assert_eq!(decode(&again), Ok(value)),
        Err(err) => assert!(
            matches!(err, SmbError::Protocol | SmbError::TooLarge),
            "{err:?}"
        ),
    }
    true
}

/// Every decoder over `input`; how many message decoders accepted it.
fn decoders(input: &[u8]) -> usize {
    let _ = Header::decode(input);
    let _ = Transform::parse(input);
    let _ = frame::messages(input);
    let _ = decode_empty(input);
    if let Ok(response) = NegotiateResponse::decode(input) {
        let _ = offer().accept(&response);
        let _ = check_validate_negotiate(input.get(HEADER_LEN..).unwrap_or_default(), &response);
    }
    [
        stable(input, NegotiateRequest::decode, NegotiateRequest::encode),
        stable(input, NegotiateResponse::decode, NegotiateResponse::encode),
        stable(
            input,
            SessionSetupRequest::decode,
            SessionSetupRequest::encode,
        ),
        stable(
            input,
            SessionSetupResponse::decode,
            SessionSetupResponse::encode,
        ),
        stable(
            input,
            TreeConnectRequest::decode,
            TreeConnectRequest::encode,
        ),
        stable(
            input,
            TreeConnectResponse::decode,
            TreeConnectResponse::encode,
        ),
        stable(input, IoctlRequest::decode, IoctlRequest::encode),
        stable(input, IoctlResponse::decode, IoctlResponse::encode),
        stable(input, ErrorResponse::decode, ErrorResponse::encode),
    ]
    .into_iter()
    .filter(|&accepted| accepted)
    .count()
}

fn offer() -> Offer {
    Offer {
        floor: Dialect::Smb300,
        client_guid: [0x11; 16],
        salt: [0x22; 32],
        server_name: String::from("nas.example"),
    }
}

/// A well-formed message of some kind, for the mutator to break.
fn template(rng: &mut Prng) -> Vec<u8> {
    let mut bytes = vec![0; rng.at_most(96)];
    rng.fill(&mut bytes);
    let status = *rng.pick(&[
        NtStatus::SUCCESS,
        NtStatus::PENDING,
        NtStatus::MORE_PROCESSING_REQUIRED,
        NtStatus::ACCESS_DENIED,
    ]);
    let message_id = rng.next_u64() % 8;
    let header = |command| header(command, true, message_id, status);
    match rng.below(9) {
        0 => message(&header(Command::Negotiate), |enc| {
            offer().request().encode(enc);
        }),
        1 => message(&header(Command::Negotiate), |enc| {
            NegotiateResponse {
                security_mode: SecurityMode::SIGNING_ENABLED,
                dialect: Dialect::Smb311.code(),
                server_guid: [0x33; 16],
                capabilities: Capabilities::LEASING,
                max_transact_size: 8 << 20,
                max_read_size: 8 << 20,
                max_write_size: 8 << 20,
                system_time: 0,
                server_start_time: 0,
                security_buffer: bytes,
                contexts: vec![
                    NegotiateContext::PreauthIntegrity {
                        hash_algorithms: vec![1],
                        salt: vec![0x44; 32],
                    },
                    NegotiateContext::Encryption {
                        ciphers: vec![Cipher::Aes128Gcm.code()],
                    },
                    NegotiateContext::Signing {
                        algorithms: vec![SigningAlgorithm::AesGmac.code()],
                    },
                ],
            }
            .encode(enc);
        }),
        2 => message(&header(Command::SessionSetup), |enc| {
            SessionSetupRequest {
                binding: false,
                security_mode: SecurityMode::SIGNING_REQUIRED,
                capabilities: Capabilities::DFS,
                previous_session_id: 0,
                security_buffer: bytes,
            }
            .encode(enc);
        }),
        3 => message(&header(Command::SessionSetup), |enc| {
            SessionSetupResponse {
                session_flags: SessionFlags::ENCRYPT_DATA,
                security_buffer: bytes,
            }
            .encode(enc);
        }),
        4 => message(&header(Command::TreeConnect), |enc| {
            TreeConnectRequest {
                path: String::from(r"\\nas.example\share"),
            }
            .encode(enc);
        }),
        5 => message(&header(Command::TreeConnect), |enc| {
            TreeConnectResponse {
                share_type: ShareType::Disk,
                share_flags: 0,
                capabilities: 0,
                maximal_access: 0x001f_01ff,
            }
            .encode(enc);
        }),
        6 => message(&header(Command::Ioctl), |enc| {
            IoctlRequest {
                ctl_code: 0x0014_0204,
                file_id: NO_FILE,
                input: bytes,
                max_output_response: 24,
                flags: IS_FSCTL,
            }
            .encode(enc);
        }),
        7 => message(&header(Command::Ioctl), |enc| {
            IoctlResponse {
                ctl_code: 0x0014_0204,
                file_id: NO_FILE,
                output: bytes,
            }
            .encode(enc);
        }),
        _ => message(&header(Command::Echo), |enc| {
            ErrorResponse {
                context_count: 0,
                data: bytes,
            }
            .encode(enc);
        }),
    }
}

/// Change one to four distinct bytes of `bytes[from..]`: each is changed
/// once, so no two mutations can cancel.
fn mutate(rng: &mut Prng, bytes: &mut [u8], from: usize) {
    let span = bytes.len().saturating_sub(from);
    if span == 0 {
        return;
    }
    let mut changed = Vec::new();
    for _ in 0..=rng.below(4).min(span - 1) {
        let mut at = rng.below(span);
        while changed.contains(&at) {
            at = (at + 1) % span;
        }
        changed.push(at);
        bytes[from + at] ^= u8::try_from(rng.below(255) + 1).unwrap_or(1);
    }
}

#[test]
fn a_mutation_always_changes_what_it_is_given() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_mutation_always_changes_what_it_is_given",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    for len in 1..8 {
        for _ in 0..2_000 {
            let original = vec![0x5a; len + 2];
            let mut mutated = original.clone();
            mutate(&mut rng, &mut mutated, 2);
            assert_ne!(mutated, original);
            assert_eq!(mutated[..2], original[..2], "nothing before `from` changes");
        }
    }
}

fn framed(frame_bytes: &[u8]) -> Vec<u8> {
    let mut out = frame::prefix(frame_bytes.len()).expect("fits").to_vec();
    out.extend_from_slice(frame_bytes);
    out
}

fn keys(role: Role) -> SessionKeys {
    let preauth = PreauthHash::new();
    let inputs = KeyInputs {
        dialect: Dialect::Smb311,
        cipher: Cipher::Aes128Gcm,
        signing: SigningAlgorithm::AesGmac,
        session_key: &[0x5a; 16],
        preauth: &preauth,
    };
    SessionKeys::derive(&inputs, role, &[9; 12]).expect("derives")
}

const OUTSTANDING: u64 = 6;

/// A connection past `NEGOTIATE` whose session holds keys and has
/// [`OUTSTANDING`] echoes waiting, ids 1 onwards.
fn connection() -> Connection {
    let mut conn = Connection::new();
    let negotiate = Outgoing {
        command: Command::Negotiate,
        session_id: 0,
        tree_id: 0,
        body: &encode_empty,
        credit_charge: 0,
        flags: Flags::NONE,
    };
    conn.send(&[negotiate]).expect("sends");
    let mut answer = header(Command::Negotiate, true, 0, NtStatus::SUCCESS);
    answer.credits = 32;
    conn.receive(&framed(&message(&answer, encode_empty)))
        .expect("negotiates");
    conn.install_keys(SESSION, keys(Role::Client))
        .expect("installs");
    for _ in 0..OUTSTANDING {
        let echo = Outgoing {
            command: Command::Echo,
            session_id: SESSION,
            tree_id: 1,
            body: &encode_empty,
            credit_charge: 1,
            flags: Flags::NONE,
        };
        conn.send(&[echo]).expect("sends");
    }
    conn.take_output();
    while conn.next_received().is_some() {}
    conn
}

/// After a failure, every call answers it.
fn check_latched(conn: &mut Connection, failure: SmbError) {
    assert_eq!(conn.failure(), Some(failure));
    assert_eq!(conn.receive(&[0; 8]), Err(failure));
    let echo = Outgoing {
        command: Command::Echo,
        session_id: SESSION,
        tree_id: 1,
        body: &encode_empty,
        credit_charge: 1,
        flags: Flags::NONE,
    };
    assert_eq!(conn.send(&[echo]).map(|_| ()), Err(failure));
}

fn deliver(conn: &mut Connection, bytes: &[u8]) -> Result<(), SmbError> {
    let result = conn.receive(bytes);
    assert!(conn.credits() <= MAX_CREDITS);
    if let Err(err) = result {
        check_latched(conn, err);
    }
    result
}

/// An echo response sealed by the server for request `id`.
fn sealed_answer(server: &mut SessionKeys, id: u64) -> (Vec<u8>, Vec<u8>) {
    let plain = message(
        &header(Command::Echo, true, id, NtStatus::SUCCESS),
        encode_empty,
    );
    let mut frame = vec![0; TRANSFORM_HEADER_LEN];
    frame.extend_from_slice(&plain);
    server.seal(SESSION, &mut frame).expect("seals");
    (framed(&frame), plain)
}

fn connection_round(rng: &mut Prng, server: &mut SessionKeys) {
    let mut noise = vec![0; rng.at_most(MAX_NOISE)];
    rng.fill(&mut noise);
    let _ = deliver(&mut connection(), &noise);

    // Noise behind a plausible prefix, so it reaches the message parsers.
    let mut conn = connection();
    let _ = deliver(&mut conn, &framed(&template(rng)));

    // A genuine sealed answer, split anywhere, is accepted whole.
    let id = 1 + rng.next_u64() % OUTSTANDING;
    let (frame, plain) = sealed_answer(server, id);
    let mut conn = connection();
    let mut rest = &frame[..];
    while !rest.is_empty() {
        let take = 1 + rng.below(rest.len());
        deliver(&mut conn, &rest[..take]).expect("a genuine frame is accepted");
        rest = &rest[take..];
    }
    match conn.next_received() {
        Some(Received::Response {
            message, sealed, ..
        }) => {
            assert!(sealed);
            assert_eq!(*message, plain[..]);
        }
        other => panic!("{other:?}"),
    }

    // The same frame with any byte past its prefix changed is refused.
    let (original, _) = sealed_answer(server, id);
    let mut tampered = original.clone();
    mutate(rng, &mut tampered, PREFIX_LEN);
    let mut conn = connection();
    assert!(
        deliver(&mut conn, &tampered).is_err(),
        "a tampered frame was accepted"
    );
    assert!(conn.next_received().is_none());
}

fn framer_round(rng: &mut Prng) {
    let bodies: Vec<Vec<u8>> = (0..=rng.below(4))
        .map(|_| {
            let mut body = vec![0; rng.at_most(300)];
            rng.fill(&mut body);
            body
        })
        .collect();
    let stream: Vec<u8> = bodies.iter().flat_map(|body| framed(body)).collect();
    let mut framer = Framer::new();
    let mut seen = Vec::new();
    let mut rest = &stream[..];
    while !rest.is_empty() {
        let take = 1 + rng.below(rest.len());
        framer.push(&rest[..take]).expect("pushes");
        rest = &rest[take..];
        while let Some(frame) = framer.next_frame().expect("frames") {
            let (buf, at) = frame.into_parts();
            seen.push(buf[at..].to_vec());
        }
    }
    assert_eq!(seen, bodies);
}

#[test]
fn hostile_bytes_never_panic_and_never_pass_for_sealed_traffic() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_bytes_never_panic_and_never_pass_for_sealed_traffic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut server = keys(Role::Server);
    let mut iteration: u64 = 0;
    loop {
        let mut noise = vec![0; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        decoders(&noise);

        let valid = template(&mut rng);
        assert!(decoders(&valid) > 0, "a well-formed template was refused");
        let mut mutated = valid.clone();
        mutate(&mut rng, &mut mutated, 0);
        decoders(&mutated);
        let mut spliced = template(&mut rng);
        spliced.extend_from_slice(&valid[..rng.at_most(valid.len())]);
        decoders(&spliced);

        connection_round(&mut rng, &mut server);
        framer_round(&mut rng);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
