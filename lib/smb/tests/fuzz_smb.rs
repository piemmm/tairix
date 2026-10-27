//! Deterministic fuzz harness for the SMB 3 engine: every decoder, the framer,
//! the client connection core and the client, fed what a hostile server could
//! send.
//!
//! The invariants:
//!
//! * no decoder panics on any bytes, and a message one accepts re-encodes to a
//!   message that decodes to the same value;
//! * the framer yields the same frames however a stream is split;
//! * nothing received makes the connection panic or hold more than
//!   [`MAX_CREDITS`], and after its first failure every call answers that
//!   failure;
//! * a sealed frame with any byte changed is never accepted;
//! * against the test server with one of its frames changed, the client never
//!   establishes a session whose final response was changed, nor — on 3.1.1 —
//!   one whose negotiation or setup was; on 3.0 and 3.0.2 it never hands out a
//!   tree after a changed dialect, capability, GUID or security mode.
//!
//! A per-run-seeded `Prng` draws noise, mutates messages the encoders built,
//! and splices them. A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep
//! once from a fresh, logged seed; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend it to a wall-clock budget.

use core::fmt::Debug;

use tairix_abi::time::Time64;
use tairix_fuzzseed::Prng;
use tairix_smb::client::{Client, Event, Mechanism, Request};
use tairix_smb::connection::{Connection, Outgoing, Received, MAX_CREDITS};
use tairix_smb::crypt::{
    KeyInputs, PreauthHash, SessionKeys, Transform, NONCE_SEED_LEN, TRANSFORM_HEADER_LEN,
};
use tairix_smb::frame::{self, Framer, PREFIX_LEN};
use tairix_smb::header::{Address, Command, Flags, Header, HEADER_LEN};
use tairix_smb::ioctl::{IoctlRequest, IoctlResponse, IS_FSCTL, NO_FILE};
use tairix_smb::negotiate::{
    check_validate_negotiate, Capabilities, Cipher, Dialect, NegotiateContext, NegotiateRequest,
    NegotiateResponse, Offer, SecurityMode, SigningAlgorithm,
};
use tairix_smb::ntlm::{Challenge, NtlmCredential, NtlmInitiator, NtlmInputs};
use tairix_smb::peer::{Hints, Peer, PeerConfig, DISK_SHARE};
use tairix_smb::session::{SessionFlags, SessionSetupRequest, SessionSetupResponse};
use tairix_smb::simple::{decode_empty, encode_empty, ErrorResponse};
use tairix_smb::spnego::{self, Reply};
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
    tokens(input);
    if let Ok(response) = NegotiateResponse::decode(input) {
        if let Ok(negotiated) = offer().accept(&response) {
            let output = input.get(HEADER_LEN..).unwrap_or_default();
            let _ = check_validate_negotiate(output, &negotiated);
        }
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

/// The authentication decoders over `token`: SPNEGO's replies and hints, and
/// NTLM's challenge, which a reply carries.
fn tokens(token: &[u8]) {
    let _ = spnego::offered(token);
    if let Ok(reply) = Reply::decode(token) {
        if let Some(inner) = reply.token {
            let _ = Challenge::decode(inner);
        }
    }
    let _ = Challenge::decode(token);
}

fn offer() -> Offer {
    Offer {
        floor: Dialect::Smb300,
        client_guid: [0x11; 16],
        salt: [0x22; 32],
        server_name: String::from(PeerConfig::new(Dialect::Smb311).host),
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

/// What a client round reached.
#[derive(Default)]
struct Reached {
    established: bool,
    tree: bool,
}

/// Where each frame of `output` lies, past its prefix.
fn spans(output: &[u8]) -> Vec<core::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < output.len() {
        let len = usize::try_from(u32::from_be_bytes([
            0,
            output[at + 1],
            output[at + 2],
            output[at + 3],
        ]))
        .expect("fits");
        out.push(at + PREFIX_LEN..at + PREFIX_LEN + len);
        at += PREFIX_LEN + len;
    }
    out
}

/// A server frame as sent and as the client was handed it.
struct Changed {
    original: Vec<u8>,
    tampered: Vec<u8>,
}

/// What `peer` answered with, the `target`th frame — counting from `seen` —
/// changed past its prefix.
fn answers(
    rng: &mut Prng,
    peer: &mut Peer,
    seen: &mut usize,
    target: usize,
) -> (Vec<u8>, Option<Changed>) {
    let mut output = peer.take_output();
    let mut changed = None;
    for span in spans(&output) {
        if *seen == target {
            let original = output[span.clone()].to_vec();
            mutate(rng, &mut output[..span.end], span.start);
            changed = Some(Changed {
                original,
                tampered: output[span].to_vec(),
            });
        }
        *seen += 1;
    }
    (output, changed)
}

/// NTLM for `config`'s account.
fn ntlm(config: &PeerConfig) -> Mechanism {
    let credential =
        NtlmCredential::new(config.user, config.domain, config.password).expect("fits");
    let mut service = String::from("cifs/");
    service.push_str(config.host);
    let inputs = NtlmInputs {
        client_challenge: [0xcc; 8],
        now: Time64::from_secs(1_790_000_000),
    };
    Mechanism::Ntlm(NtlmInitiator::new(credential, &service, inputs).expect("fits"))
}

/// Whether a `NEGOTIATE` response changed in what validate-negotiate covers.
fn negotiation_changed(original: &[u8], tampered: &[u8]) -> bool {
    match (
        NegotiateResponse::decode(original),
        NegotiateResponse::decode(tampered),
    ) {
        (Ok(a), Ok(b)) => {
            (a.dialect, a.capabilities, a.server_guid, a.security_mode)
                != (b.dialect, b.capabilities, b.server_guid, b.security_mode)
        }
        _ => true,
    }
}

/// The most frames an honest round's server sends: the `NEGOTIATE` response
/// (0), the two `SESSION_SETUP` answers (1, 2), the tree connect (3), then
/// validate-negotiate and the echo on 3.0 and 3.0.2, the echo alone on 3.1.1.
const ROUND_FRAMES: usize = 6;

/// Drive a client on `dialect` against the test server, its `target`th frame
/// changed, and say how far the client got.
fn client_round(rng: &mut Prng, dialect: Dialect, target: usize) -> Reached {
    let config = PeerConfig {
        cipher: *rng.pick(&Cipher::PREFERENCE),
        signing: *rng.pick(&[
            None,
            Some(SigningAlgorithm::AesCmac),
            Some(SigningAlgorithm::AesGmac),
        ]),
        hints: *rng.pick(&[Hints::Ntlm, Hints::Empty]),
        timestamp: *rng.pick(&[None, Some(134_051_616_000_000_000)]),
        ..PeerConfig::new(dialect)
    };
    let mut peer = Peer::new(config.clone());
    let mut client = Client::new(offer()).expect("encodes");
    let mut reached = Reached::default();
    let mut seen = 0;
    let mut changed = None;
    loop {
        if peer.receive(&client.take_output()).is_err() {
            // The server drops a client the change led astray.
            break;
        }
        let (output, frame) = answers(rng, &mut peer, &mut seen, target);
        changed = changed.or(frame);
        if output.is_empty() {
            break;
        }
        if let Err(err) = client.receive(&output) {
            assert_eq!(client.failure(), Some(err));
            assert_eq!(client.receive(&[]), Err(err), "the failure latches");
            assert_eq!(client.connect_tree(DISK_SHARE).map(|_| ()), Err(err));
            break;
        }
        while let Some(event) = client.next_event() {
            match event {
                Event::Negotiated => {
                    let _ = client.authenticate(ntlm(&config), [0x3c; NONCE_SEED_LEN]);
                }
                Event::Established { .. } => {
                    reached.established = true;
                    let _ = client.connect_tree(DISK_SHARE);
                }
                Event::TreeConnected { tree, .. } => {
                    reached.tree = true;
                    let echo = Request {
                        command: Command::Echo,
                        body: &encode_empty,
                        credit_charge: 1,
                        flags: Flags::NONE,
                    };
                    let _ = client.send(tree.id, &[echo]);
                }
                _ => {}
            }
        }
    }
    let Some(changed) = changed else {
        return reached;
    };
    if target == 2 || (dialect == Dialect::Smb311 && target < 2) {
        assert!(
            !reached.established,
            "frame {target} was changed and the session established anyway"
        );
    }
    if target >= 3 {
        assert!(
            client.failure().is_some(),
            "a changed sealed frame {target} was accepted"
        );
    }
    if target == 0
        && dialect != Dialect::Smb311
        && negotiation_changed(&changed.original, &changed.tampered)
    {
        assert!(!reached.tree, "a downgrade went unnoticed");
    }
    reached
}

/// Tokens a client and the test server exchanged, mutated, through the
/// authentication decoders.
fn token_round(rng: &mut Prng, genuine: &[Vec<u8>]) {
    let mut token = rng.pick(genuine).clone();
    tokens(&token);
    mutate(rng, &mut token, 0);
    tokens(&token);
    token.truncate(rng.at_most(token.len()));
    tokens(&token);
}

/// The security buffers of one honest authentication: the server's hints
/// and its two `SESSION_SETUP` answers.
fn genuine_tokens() -> Vec<Vec<u8>> {
    let mut client = Client::new(offer()).expect("encodes");
    let config = PeerConfig::new(Dialect::Smb311);
    let mut peer = Peer::new(config.clone());
    let mut buffers = Vec::new();
    loop {
        peer.receive(&client.take_output()).expect("honest");
        let output = peer.take_output();
        if output.is_empty() {
            break;
        }
        for span in spans(&output) {
            let message = &output[span];
            if let Ok(response) = NegotiateResponse::decode(message) {
                buffers.push(response.security_buffer);
            } else if let Ok(response) = SessionSetupResponse::decode(message) {
                buffers.push(response.security_buffer);
            }
        }
        client.receive(&output).expect("honest");
        if client.next_event() == Some(Event::Negotiated) {
            client
                .authenticate(ntlm(&config), [2; NONCE_SEED_LEN])
                .expect("starts");
        }
    }
    assert_eq!(buffers.len(), 3, "the hints and two answers");
    buffers
}

#[test]
fn hostile_bytes_never_panic_and_never_pass_for_sealed_traffic() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_bytes_never_panic_and_never_pass_for_sealed_traffic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut server = keys(Role::Server);
    let genuine = genuine_tokens();
    // The floor, by construction: untouched, every dialect reaches a tree,
    // so the sweep's changes land on a client that would otherwise get there.
    for dialect in Dialect::ALL {
        let reached = client_round(&mut rng, dialect, ROUND_FRAMES);
        assert!(reached.established && reached.tree, "{dialect:?}");
    }
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
        let dialect = *rng.pick(&Dialect::ALL);
        let target = rng.below(ROUND_FRAMES);
        client_round(&mut rng, dialect, target);
        token_round(&mut rng, &genuine);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
