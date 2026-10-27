//! Deterministic fuzz harness for the SMB 3 engine: every decoder, the framer,
//! the client connection core and the client, fed what a hostile server could
//! send.
//!
//! The invariants:
//!
//! * no decoder panics on any bytes, and a message or information class one
//!   accepts re-encodes to one that decodes to the same value; a listing's
//!   entries and a change response's records walk to their end;
//! * the framer yields the same frames however a stream is split;
//! * nothing received makes the connection panic or hold more than
//!   [`MAX_CREDITS`], and after its first failure every call answers that
//!   failure;
//! * a sealed frame with any byte changed is never accepted;
//! * a share listing given any changed answer still ends, and once it has
//!   opened its pipe it closes it;
//! * against the test server with one of its frames changed, the client never
//!   establishes a session whose final response was changed, nor — on 3.1.1 —
//!   one whose negotiation or setup was; on 3.0 and 3.0.2 it never hands out a
//!   tree after a changed dialect, capability, GUID or security mode.
//!
//! A per-run-seeded `Prng` draws noise, mutates messages the encoders built,
//! and splices them. A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep
//! once from a fresh, logged seed; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend it to a wall-clock budget.

use tairix_abi::time::Time64;
use tairix_fuzzseed::Prng;
use tairix_smb::client::{Client, Event, Mechanism, Request};
use tairix_smb::connection::{Connection, Outgoing, Received, MAX_CREDITS};
use tairix_smb::crypt::{
    KeyInputs, PreauthHash, SessionKeys, Transform, NONCE_SEED_LEN, TRANSFORM_HEADER_LEN,
};
use tairix_smb::file::create::{
    Access, CreateAction, CreateOptions, CreateRequest, CreateResponse, DiskId, Disposition,
    Durable, MaximalAccess, OplockLevel, ShareAccess,
};
use tairix_smb::file::dir::{
    write_entries, EntryRecord, ListingFlags, QueryDirectoryRequest, QueryDirectoryResponse,
};
use tairix_smb::file::info::{
    BasicInfo, BasicUpdate, Class, DispositionInfo, EndOfFileInfo, FsAbilities, FsAttribute,
    FsFullSize, FsVolume, InternalInfo, NetworkOpenInfo, QueryInfoRequest, QueryInfoResponse,
    RenameInfo, SetInfoRequest, SetInfoResponse, StandardInfo,
};
use tairix_smb::file::io::{
    CloseRequest, CloseResponse, ClosedAttributes, FlushRequest, ReadRequest, ReadResponse,
    WriteRequest, WriteResponse,
};
use tairix_smb::file::lease::{BreakNotice, Lease, LeaseAck, LeaseBreak, LeaseState, OplockBreak};
use tairix_smb::file::name::{decode_name, SharePath};
use tairix_smb::file::notify::{
    write_changes, Action, Change, ChangeNotifyRequest, ChangeNotifyResponse, NotifyFilter,
};
use tairix_smb::file::{FileAttributes, FileTimes, NO_FILE};
use tairix_smb::frame::{self, Framer, PREFIX_LEN};
use tairix_smb::header::{Address, Command, Flags, Header, HEADER_LEN};
use tairix_smb::ioctl::{IoctlRequest, IoctlResponse, IS_FSCTL};
use tairix_smb::negotiate::{
    check_validate_negotiate, Capabilities, Cipher, Dialect, NegotiateContext, NegotiateRequest,
    NegotiateResponse, Offer, SecurityMode, SigningAlgorithm,
};
use tairix_smb::ntlm::{Challenge, NtlmCredential, NtlmInitiator, NtlmInputs};
use tairix_smb::peer::{Hints, Peer, PeerConfig, DISK_SHARE};
use tairix_smb::rpc::{
    read_share_enum, read_share_enum_request, write_share_enum, write_share_enum_request, Bind,
    BindAck, Pdu, RequestFragment, ResponseFragment, ShareKind, ShareListing, ShareRecord,
    ACCEPTANCE, FSCTL_PIPE_TRANSCEIVE, NDR, SHARE_ENUM, SPECIAL, SRVSVC, TEMPORARY,
};
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

/// Whether `$type`'s decoder accepts `$input`. A value it accepts, re-encoded
/// behind the first `$prefix` bytes of `$input` — a message's header, or none
/// for an information class — must decode to the same value, unless it holds
/// what this engine never sends, which the encoder refuses. A macro, so a
/// value that borrows its input is held to it too.
macro_rules! stable {
    ($input:expr, $type:ty) => {
        stable!($input, $type, HEADER_LEN)
    };
    ($input:expr, $type:ty, $prefix:expr) => {{
        let input: &[u8] = $input;
        match <$type>::decode(input) {
            Err(_) => false,
            Ok(value) => {
                let mut enc = Encoder::new();
                enc.bytes(&input[..$prefix]);
                value.encode(&mut enc);
                match enc.finish() {
                    Ok(again) => assert_eq!(<$type>::decode(&again), Ok(value)),
                    Err(err) => assert!(
                        matches!(err, SmbError::Protocol | SmbError::TooLarge),
                        "{err:?}"
                    ),
                }
                true
            }
        }
    }};
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
    classes(input);
    let _ = decode_name(input);
    let _ = SharePath::decode(input);
    if let Ok(response) = QueryInfoResponse::decode(input) {
        classes(response.output);
    }
    if let Ok(request) = SetInfoRequest::decode(input) {
        classes(&request.buffer);
    }
    if let Ok(response) = QueryDirectoryResponse::decode(input) {
        for entry in response.entries().flatten() {
            let _ = entry.name();
        }
    }
    if let Ok(response) = ChangeNotifyResponse::decode(input) {
        let _ = response.changes().count();
    }
    [
        stable!(input, NegotiateRequest),
        stable!(input, NegotiateResponse),
        stable!(input, SessionSetupRequest),
        stable!(input, SessionSetupResponse),
        stable!(input, TreeConnectRequest),
        stable!(input, TreeConnectResponse),
        stable!(input, IoctlRequest),
        stable!(input, IoctlResponse),
        stable!(input, ErrorResponse),
        stable!(input, CreateRequest),
        stable!(input, CreateResponse),
        stable!(input, CloseRequest),
        stable!(input, CloseResponse),
        stable!(input, FlushRequest),
        stable!(input, ReadRequest),
        stable!(input, ReadResponse<'_>),
        stable!(input, WriteRequest<'_>),
        stable!(input, WriteResponse),
        stable!(input, QueryDirectoryRequest),
        stable!(input, QueryDirectoryResponse<'_>),
        stable!(input, QueryInfoRequest),
        stable!(input, QueryInfoResponse<'_>),
        stable!(input, SetInfoRequest),
        stable!(input, SetInfoResponse),
        stable!(input, ChangeNotifyRequest),
        stable!(input, ChangeNotifyResponse<'_>),
        stable!(input, BreakNotice),
        stable!(input, LeaseAck),
        stable!(input, Pdu<'_>, 0),
        share_stubs(input),
    ]
    .into_iter()
    .filter(|&accepted| accepted)
    .count()
}

/// Both `NetrShareEnum` stub decoders over `stub`; whether either accepted
/// it. A listing that omitted nothing, written back, reads the same.
fn share_stubs(stub: &[u8]) -> bool {
    let request = read_share_enum_request(stub).is_ok();
    let Ok(list) = read_share_enum(stub) else {
        return request;
    };
    if list.omitted == 0 {
        let records: Vec<ShareRecord<'_>> = list
            .shares
            .iter()
            .map(|share| ShareRecord {
                name: &share.name,
                share_type: share.kind.code()
                    | if share.special { SPECIAL } else { 0 }
                    | if share.temporary { TEMPORARY } else { 0 },
                remark: &share.remark,
            })
            .collect();
        let mut enc = Encoder::new();
        write_share_enum(&mut enc, &records, 0);
        assert_eq!(read_share_enum(&enc.finish().expect("encodes")), Ok(list));
    }
    true
}

/// Every information-class decoder over `buffer`.
fn classes(buffer: &[u8]) {
    stable!(buffer, BasicInfo, 0);
    stable!(buffer, BasicUpdate, 0);
    stable!(buffer, StandardInfo, 0);
    stable!(buffer, InternalInfo, 0);
    stable!(buffer, NetworkOpenInfo, 0);
    stable!(buffer, EndOfFileInfo, 0);
    stable!(buffer, DispositionInfo, 0);
    stable!(buffer, RenameInfo, 0);
    stable!(buffer, FsFullSize, 0);
    stable!(buffer, FsAttribute, 0);
    stable!(buffer, FsVolume, 0);
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
    let header = header(Command::Echo, true, rng.next_u64() % 8, status);
    let sample = Sample::new();
    // Each area weighted by its kinds of message, so every kind is drawn alike.
    match rng.below(38) {
        0..9 => session_template(rng, bytes, &header),
        9..11 => create_template(rng, &sample, &header),
        11..18 => data_template(rng, &sample, &bytes, &header),
        18..20 => listing_template(rng, &sample, &header),
        20..26 => information_template(rng, &sample, &header),
        26..28 => watch_template(rng, &sample, &header),
        28..31 => lease_template(rng, &sample, &header),
        _ => rpc_template(rng),
    }
}

/// A DCE/RPC PDU or `NetrShareEnum` stub, carried on a pipe without an SMB
/// header of its own.
fn rpc_template(rng: &mut Prng) -> Vec<u8> {
    let stub = share_enum_stub(rng.below(4));
    let mut enc = Encoder::new();
    match rng.below(7) {
        0 => Pdu::Bind(Bind {
            call_id: 1,
            max_xmit_frag: 4280,
            max_recv_frag: 4280,
            context: 0,
            abstract_syntax: SRVSVC,
            transfer_syntax: NDR,
        })
        .encode(&mut enc),
        1 => Pdu::BindAck(BindAck {
            call_id: 1,
            max_xmit_frag: 4280,
            max_recv_frag: 4280,
            assoc_group: 0x1234,
            secondary_address: b"\\PIPE\\srvsvc\0",
            result: ACCEPTANCE,
            reason: 0,
            transfer_syntax: NDR,
        })
        .encode(&mut enc),
        2 => Pdu::BindNak {
            call_id: 1,
            reason: 4,
        }
        .encode(&mut enc),
        3 => {
            let mut request = Encoder::new();
            write_share_enum_request(&mut request, r"\\nas.example");
            let request = request.finish().expect("encodes");
            Pdu::Request(RequestFragment {
                call_id: 2,
                first: true,
                last: true,
                context: 0,
                opnum: SHARE_ENUM,
                stub: &request,
            })
            .encode(&mut enc);
        }
        4 => Pdu::Response(ResponseFragment {
            call_id: 2,
            first: true,
            last: true,
            context: 0,
            stub: &stub,
        })
        .encode(&mut enc),
        5 => Pdu::Fault {
            call_id: 2,
            status: 0x1c01_0002,
        }
        .encode(&mut enc),
        _ => return stub,
    }
    enc.finish().expect("encodes")
}

/// A `NetrShareEnum` answer listing `count` shares, one a name no tree
/// connect takes.
fn share_enum_stub(count: usize) -> Vec<u8> {
    let names = ["share", "IPC$", r"..\outside", "printer"];
    let records: Vec<ShareRecord<'_>> = names[..count]
        .iter()
        .map(|name| ShareRecord {
            name,
            share_type: ShareKind::Disk.code() | SPECIAL,
            remark: "a remark",
        })
        .collect();
    let mut enc = Encoder::new();
    write_share_enum(&mut enc, &records, 0);
    enc.finish().expect("encodes")
}

/// The answers a server gives a whole listing, in order, its answer split
/// across two fragments.
fn listing_answers() -> Vec<Vec<u8>> {
    let header = |command| header(command, true, 1, NtStatus::SUCCESS);
    let pipe = [0x42; 16];
    let time = Time64::UNIX_EPOCH;
    let opened = message(&header(Command::Create), |enc| {
        CreateResponse {
            oplock: OplockLevel::None,
            reparse_point: false,
            action: CreateAction::Opened,
            times: FileTimes {
                created: time,
                accessed: time,
                written: time,
                changed: time,
            },
            allocation_size: 0,
            end_of_file: 0,
            attributes: FileAttributes::NORMAL,
            file_id: pipe,
            lease: None,
            durable_timeout_ms: None,
            maximal_access: None,
            disk_id: None,
        }
        .encode(enc);
    });
    let written = message(&header(Command::Write), |enc| {
        WriteResponse { count: 72 }.encode(enc);
    });
    let mut ack = Encoder::new();
    Pdu::BindAck(BindAck {
        call_id: 1,
        max_xmit_frag: 4280,
        max_recv_frag: 4280,
        assoc_group: 0x1234,
        secondary_address: b"\\PIPE\\srvsvc\0",
        result: ACCEPTANCE,
        reason: 0,
        transfer_syntax: NDR,
    })
    .encode(&mut ack);
    let ack = ack.finish().expect("encodes");
    let acked = message(&header(Command::Read), |enc| {
        ReadResponse { data: &ack }.encode(enc);
    });
    let stub = share_enum_stub(3);
    let (head, tail) = stub.split_at(stub.len() / 2);
    let fragment = |first, last, stub| {
        let mut enc = Encoder::new();
        Pdu::Response(ResponseFragment {
            call_id: 2,
            first,
            last,
            context: 0,
            stub,
        })
        .encode(&mut enc);
        enc.finish().expect("encodes")
    };
    let first = message(&header(Command::Ioctl), |enc| {
        IoctlResponse {
            ctl_code: FSCTL_PIPE_TRANSCEIVE,
            file_id: pipe,
            output: fragment(true, false, head),
        }
        .encode(enc);
    });
    let last = fragment(false, true, tail);
    let rest = message(&header(Command::Read), |enc| {
        ReadResponse { data: &last }.encode(enc);
    });
    let closed = message(&header(Command::Close), |enc| {
        CloseResponse { attributes: None }.encode(enc);
    });
    vec![opened, written, acked, first, rest, closed]
}

/// A listing given a server's answers with one of them changed: whatever the
/// change, the listing ends, and once it has opened the pipe its last
/// request is that pipe's close.
fn listing_round(rng: &mut Prng, answers: &[Vec<u8>]) {
    let mut listing = ShareListing::new("nas.example").expect("a listing");
    let target = rng.below(answers.len());
    let mut closing = false;
    for (index, answer) in answers.iter().enumerate() {
        let Some(command) = listing.command() else {
            break;
        };
        closing = command == Command::Close;
        let mut answer = answer.clone();
        if index == target {
            mutate(rng, &mut answer, 0);
        }
        listing.answer(&answer);
        if closing && listing.command().is_none() {
            break;
        }
    }
    if listing.command() == Some(Command::Close) {
        listing.answer(answers.last().expect("a close"));
        closing = true;
    }
    if listing.command().is_none() {
        let finished = listing.finish();
        assert!(
            closing || finished.is_err(),
            "a listing ended with its pipe open"
        );
    }
}

/// A session or tree message behind `header`, its command's.
fn session_template(rng: &mut Prng, bytes: Vec<u8>, header: &Header) -> Vec<u8> {
    let header = |command| Header { command, ..*header };
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

/// The values the file templates carry.
struct Sample {
    time: Time64,
    times: FileTimes,
    lease: Lease,
    path: SharePath,
    file_id: [u8; 16],
}

impl Sample {
    fn new() -> Self {
        let time = Time64::new(-86_400, 100).expect("a time");
        Self {
            time,
            times: FileTimes {
                created: time,
                accessed: time,
                written: Time64::from_secs(4_102_444_800),
                changed: time,
            },
            lease: Lease {
                key: [0x6c; 16],
                state: LeaseState::READ.with(LeaseState::HANDLE),
                parent: Some([0x70; 16]),
                epoch: 3,
                breaking: false,
            },
            path: ["docs", "2026", "report.txt"]
                .iter()
                .try_fold(SharePath::root(), |path, name| path.join(name))
                .expect("a path"),
            file_id: [0x15; 16],
        }
    }
}

/// A `CREATE` request or response behind `header`.
fn create_template(rng: &mut Prng, sample: &Sample, header: &Header) -> Vec<u8> {
    let header = Header {
        command: Command::Create,
        ..*header
    };
    if rng.below(2) == 0 {
        return message(&header, |enc| {
            CreateRequest {
                path: sample.path.clone(),
                access: Access::READ_DATA.with(Access::WRITE_DATA),
                attributes: FileAttributes::NONE,
                share: ShareAccess::READ,
                disposition: Disposition::OpenIf,
                options: CreateOptions::NON_DIRECTORY_FILE,
                lease: Some(sample.lease),
                durable: Some(Durable::Request {
                    timeout_ms: 0,
                    create_guid: [0x47; 16],
                }),
                maximal_access: true,
                disk_id: true,
            }
            .encode(enc);
        });
    }
    message(&header, |enc| {
        CreateResponse {
            oplock: OplockLevel::Lease,
            reparse_point: false,
            action: CreateAction::Created,
            times: sample.times,
            allocation_size: 4096,
            end_of_file: 23,
            attributes: FileAttributes::ARCHIVE,
            file_id: sample.file_id,
            lease: Some(sample.lease),
            durable_timeout_ms: Some(60_000),
            maximal_access: Some(MaximalAccess {
                status: NtStatus::SUCCESS,
                access: Access::from_bits(0x001f_01ff),
            }),
            disk_id: Some(DiskId {
                file: 42,
                volume: 7,
            }),
        }
        .encode(enc);
    })
}

/// A `READ`, `WRITE`, `FLUSH` or `CLOSE` behind `header`, carrying `bytes`.
fn data_template(rng: &mut Prng, sample: &Sample, bytes: &[u8], header: &Header) -> Vec<u8> {
    let with = |command| Header { command, ..*header };
    let file_id = sample.file_id;
    match rng.below(7) {
        0 => message(&with(Command::Read), |enc| {
            ReadRequest::new(file_id, 4096, 65_536).encode(enc);
        }),
        1 => message(&with(Command::Read), |enc| {
            ReadResponse { data: bytes }.encode(enc);
        }),
        2 => message(&with(Command::Write), |enc| {
            WriteRequest {
                offset: 1 << 40,
                file_id,
                write_through: true,
                data: bytes,
            }
            .encode(enc);
        }),
        3 => message(&with(Command::Write), |enc| {
            WriteResponse { count: 23 }.encode(enc);
        }),
        4 => message(&with(Command::Flush), |enc| {
            FlushRequest { file_id }.encode(enc);
        }),
        5 => message(&with(Command::Close), |enc| {
            CloseRequest {
                post_query: true,
                file_id,
            }
            .encode(enc);
        }),
        _ => message(&with(Command::Close), |enc| {
            CloseResponse {
                attributes: Some(ClosedAttributes {
                    times: sample.times,
                    allocation_size: 4096,
                    end_of_file: 23,
                    attributes: FileAttributes::ARCHIVE,
                }),
            }
            .encode(enc);
        }),
    }
}

/// A `QUERY_DIRECTORY` request, or a response listing dot entries, a name
/// both sides carry and one they do not.
fn listing_template(rng: &mut Prng, sample: &Sample, header: &Header) -> Vec<u8> {
    let header = Header {
        command: Command::QueryDirectory,
        ..*header
    };
    if rng.below(2) == 0 {
        return message(&header, |enc| {
            QueryDirectoryRequest {
                flags: ListingFlags::RESTART_SCANS,
                file_index: 0,
                file_id: sample.file_id,
                output_length: 65_536,
            }
            .encode(enc);
        });
    }
    let records: Vec<EntryRecord<'_>> = [".", "..", "report.txt", "a:stream"]
        .into_iter()
        .map(|name| EntryRecord {
            file_index: 0,
            times: sample.times,
            end_of_file: 23,
            allocation_size: 4096,
            attributes: FileAttributes::ARCHIVE,
            ea_size: 0,
            file_id: 42,
            name,
        })
        .collect();
    let mut entries = Encoder::new();
    write_entries(&mut entries, &records);
    let output = entries.finish().expect("encodes");
    message(&header, |enc| {
        QueryDirectoryResponse { output: &output }.encode(enc);
    })
}

/// A `QUERY_INFO` or `SET_INFO` request or response behind `header`.
fn information_template(rng: &mut Prng, sample: &Sample, header: &Header) -> Vec<u8> {
    let with = |command| Header { command, ..*header };
    match rng.below(6) {
        0 => message(&with(Command::QueryInfo), |enc| {
            QueryInfoRequest {
                class: Class::NetworkOpen,
                output_length: 4096,
                file_id: sample.file_id,
            }
            .encode(enc);
        }),
        1 => message(&with(Command::SetInfo), |enc| {
            let rename = RenameInfo {
                replace: true,
                target: sample.path.clone(),
            };
            SetInfoRequest::new(Class::Rename, sample.file_id, |enc| rename.encode(enc))
                .expect("encodes")
                .encode(enc);
        }),
        2 => message(&with(Command::SetInfo), |enc| SetInfoResponse.encode(enc)),
        kind => {
            let mut output = Encoder::new();
            match kind {
                3 => NetworkOpenInfo {
                    times: sample.times,
                    allocation_size: 4096,
                    end_of_file: 23,
                    attributes: FileAttributes::ARCHIVE,
                }
                .encode(&mut output),
                4 => FsAttribute {
                    abilities: FsAbilities::UNICODE_ON_DISK,
                    max_name_length: 255,
                    name: String::from("NTFS"),
                }
                .encode(&mut output),
                _ => FsVolume {
                    created: sample.time,
                    serial: 7,
                    label: String::from("Backup"),
                }
                .encode(&mut output),
            }
            let output = output.finish().expect("encodes");
            message(&with(Command::QueryInfo), |enc| {
                QueryInfoResponse { output: &output }.encode(enc);
            })
        }
    }
}

/// A `CHANGE_NOTIFY` request, or a response with a record no path names.
fn watch_template(rng: &mut Prng, sample: &Sample, header: &Header) -> Vec<u8> {
    let header = Header {
        command: Command::ChangeNotify,
        ..*header
    };
    if rng.below(2) == 0 {
        return message(&header, |enc| {
            ChangeNotifyRequest {
                watch_tree: true,
                output_length: 4096,
                file_id: sample.file_id,
                filter: NotifyFilter::FILE_NAME.with(NotifyFilter::SIZE),
            }
            .encode(enc);
        });
    }
    let changes = [
        Change {
            action: Some(Action::RenamedOld),
            path: sample.path.clone(),
        },
        Change {
            action: None,
            path: sample.path.clone(),
        },
    ];
    let mut records = Encoder::new();
    write_changes(&mut records, &changes);
    let output = records.finish().expect("encodes");
    message(&header, |enc| {
        ChangeNotifyResponse { output: &output }.encode(enc);
    })
}

/// A lease or oplock break, or a lease break's acknowledgment.
fn lease_template(rng: &mut Prng, sample: &Sample, header: &Header) -> Vec<u8> {
    let header = Header {
        command: Command::OplockBreak,
        ..*header
    };
    message(&header, |enc| match rng.below(3) {
        0 => BreakNotice::Lease(LeaseBreak {
            epoch: 4,
            ack_required: true,
            key: sample.lease.key,
            current: sample.lease.state,
            new: LeaseState::READ,
        })
        .encode(enc),
        1 => BreakNotice::Oplock(OplockBreak {
            level: 1,
            file_id: sample.file_id,
        })
        .encode(enc),
        _ => LeaseAck {
            key: sample.lease.key,
            state: LeaseState::READ,
        }
        .encode(enc),
    })
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
                    let _ = client.authenticate(ntlm(&config), [0x3c; NONCE_SEED_LEN], None);
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
                .authenticate(ntlm(&config), [2; NONCE_SEED_LEN], None)
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
    let listings = listing_answers();
    // The floor, by construction: an untouched exchange lists every share.
    let mut untouched = ShareListing::new("nas.example").expect("a listing");
    for answer in &listings {
        untouched.answer(answer);
    }
    assert_eq!(
        untouched
            .finish()
            .map(|list| (list.shares.len(), list.omitted)),
        Ok((2, 1))
    );
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
        listing_round(&mut rng, &listings);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
