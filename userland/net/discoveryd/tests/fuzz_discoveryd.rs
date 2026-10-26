//! Deterministic fuzz harness for `discoveryd`'s trust boundary.
//!
//! The decoder parses what any peer on the segment sent and may be
//! compromised by it; the front must stay whole whatever its decoder then
//! says. The invariants:
//!
//! 1. Both directions' codecs are canonical: every frame an encoder writes
//!    decodes to what was encoded, and a frame that decodes re-encodes to
//!    exactly its bytes.
//! 2. The real decoder, fed any frame, never panics, ends its session on a
//!    frame out of order, and emits only frames the front believes.
//! 3. Against a decoder saying anything at all, the front never panics, never
//!    asks to be woken at or before the instant it last acted on, configures
//!    every decoder first and once under fresh keys and tells it its links
//!    before relaying from one or handing it a fetch made there, spaces its
//!    ticks, contains a decoder that says what no decoder says, relays again
//!    once the replacement starts, delivers a tick owed while its queue was
//!    full once the queue drains, sends only to its protocols' groups, and
//!    opens a fetch only to a WS-Discovery sender heard on that link.
//! 4. Any call bytes from any caller are answered with a reply the client's
//!    decoders read, and never panic the front. Each sweep first names a
//!    computer: a session asks for computers, one announces itself, and its
//!    fetched metadata is collected. Across random calls from several
//!    accounts and processes against a real decoder after that — joins,
//!    stops, collects, closes, exits, foreign doorbell ports, doorbells that
//!    block or vanish, segment traffic, computers announcing themselves and
//!    their metadata fetched, link edges and decoder exits — its tables stay
//!    consistent after every step, and once every client has gone nothing
//!    they asked for is left behind.
//!
//! Every structural case runs on every iteration; the random draws choose
//! content within it.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use std::cell::{Cell, RefCell};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::rc::Rc;

use tairix_abi::discovery_ipc::{
    Answer, Change, CollectReply, DiscoveryRequest, Entry, Families, Query, ServiceTypeField,
    Transport, DISCOVERY_MAX_REPLY,
};
use tairix_abi::net::{
    SocketAddr, SocketDatagram, SocketDelivery, SocketId, SocketLinkEvent, SocketStreamEvent,
    StreamCloseReason,
};
use tairix_abi::net_ipc::{address_parts, IF_NAME_LEN};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::time::NANOS_PER_MILLI as MS;
use tairix_abi::{CapabilityId, CapabilitySummary, Errno, Origin, ProcId, TrustDomain};
use tairix_discoveryd::decoder::Decoder;
use tairix_discoveryd::front::{
    Caller, Front, Host, Protocol, Sockets, MIN_TICK_INTERVAL_NS, OUTBOUND_QUEUE,
};
use tairix_discoveryd::grants::Grants;
use tairix_discoveryd::wire::{
    Budgets, Edge, FetchRequest, Form, FromDecoder, FromWsd, ToDecoder, ToWsd, CACHE_KEY_LEN,
    DATAGRAM_HEADER_LEN, GROUP_BURST, MAX_FROM_DECODER, MAX_TO_DECODER, RNG_KEY_LEN, WSD_BURST,
};
use tairix_fuzzseed::Prng;
use tairix_log::{Event, Sink};
use tairix_net::dns::Name;
use tairix_net::mdns::{
    Destination, MessageWriter, RData, Record, Section, GROUP_V4, GROUP_V6, MAX_QUESTIONS,
};
use tairix_net::rate::BucketLevel;
use tairix_net::wsd;
use tairix_sandbox::loopback::LoopbackSession;
use tairix_sandbox::proto::{ProtoError, FRAME_HEADER_LEN};
use tairix_sandbox::session::{
    FrameOut, SessionDescriptors, SessionService, SessionStep, SessionTransport,
};
use tairix_sandbox::supervise::SessionLauncher;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 200;

const SEC: u64 = 1_000 * MS;

/// The kinds of thing a hostile decoder says, one per round in turn.
const SAYINGS: usize = 17;

/// Both send budgets as full as they can be, at the start of time.
const FULL: Budgets = Budgets {
    group: BucketLevel {
        scaled: GROUP_BURST as u64 * SEC,
        at: 0,
    },
    wsd: BucketLevel {
        scaled: WSD_BURST as u64 * SEC,
        at: 0,
    },
};

/// The sockets the front listens on.
const V4: u32 = 1;
const V6: u32 = 2;
const WSD_V4: u32 = 3;

const INTERFACES: [&[u8]; 3] = [b"eth0", b"wlan0", b"bond1"];

#[derive(Clone, Copy)]
struct Quiet;

impl Sink for Quiet {
    fn write_event(&self, _event: &Event<'_>) {}
}

/// What the front sent, rang and fetched.
#[derive(Default)]
struct World {
    draws: u8,
    sent: Vec<(Protocol, SocketAddr)>,
    /// How the next doorbell post goes: taken, or this refusal.
    ring: Option<Errno>,
    /// Doorbells refused for want of room.
    blocked: u32,
    /// Every WS-Discovery datagram's link and sender the front was handed.
    heard: Vec<([u8; IF_NAME_LEN], [u8; 16])>,
    /// Fetch connections opened and not yet closed.
    open: Vec<SocketId>,
    opened: u32,
}

/// A stand-in host: each draw a fresh byte pattern, so generations differ.
#[derive(Clone, Default)]
struct Keys(Rc<RefCell<World>>);

impl Host for Keys {
    fn fill_random(&mut self, out: &mut [u8]) -> Result<(), Errno> {
        let mut world = self.0.borrow_mut();
        world.draws = world.draws.wrapping_add(1);
        out.fill(world.draws);
        Ok(())
    }

    fn transmit(
        &mut self,
        protocol: Protocol,
        _interface: [u8; IF_NAME_LEN],
        to: SocketAddr,
        _payload: &[u8],
    ) -> Result<(), Errno> {
        self.0.borrow_mut().sent.push((protocol, to));
        Ok(())
    }

    fn fetch_open(
        &mut self,
        interface: [u8; IF_NAME_LEN],
        to: SocketAddr,
    ) -> Result<SocketId, Errno> {
        let mut world = self.0.borrow_mut();
        assert!(
            world.heard.contains(&(interface, to.addr)),
            "a fetch went to {to:?}, which sent nothing on that link"
        );
        world.opened += 1;
        let socket = 1000 + world.opened;
        world.open.push(socket);
        Ok(socket)
    }

    fn fetch_send(&mut self, socket: SocketId, bytes: &[u8]) -> Result<usize, Errno> {
        assert!(
            self.0.borrow().open.contains(&socket),
            "a send on a closed fetch"
        );
        Ok(bytes.len())
    }

    fn fetch_close(&mut self, socket: SocketId) {
        let mut world = self.0.borrow_mut();
        assert!(world.open.contains(&socket), "a fetch closed twice");
        world.open.retain(|open| *open != socket);
    }

    fn ring(&mut self, _port: u64, _doorbell: &[u8]) -> Result<(), Errno> {
        let mut world = self.0.borrow_mut();
        if world.ring == Some(Errno::WouldBlock) {
            world.blocked += 1;
        }
        world.ring.map_or(Ok(()), Err)
    }

    fn watch(&mut self, _peer: ProcId) -> Result<(), Errno> {
        Ok(())
    }
}

fn named(name: &[u8]) -> [u8; IF_NAME_LEN] {
    let mut out = [0u8; IF_NAME_LEN];
    out[..name.len()].copy_from_slice(name);
    out
}

fn iface(rng: &mut Prng) -> [u8; IF_NAME_LEN] {
    named(rng.pick(&INTERFACES))
}

fn peer(last: u8) -> IpAddr {
    if last.is_multiple_of(2) {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
    } else {
        IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, u16::from(last)))
    }
}

/// A well-formed announcement of a drawn host, so the engines' cache path
/// is reached rather than only the parser's refusal.
fn announcement(rng: &mut Prng) -> Vec<u8> {
    let hosts = ["printer.local", "scanner.local", "host.local"];
    let mut out = vec![0u8; 512];
    let mut writer = MessageWriter::new(&mut out, 0, true).expect("room for a header");
    let mut record = Record::unique(
        Name::encode(rng.pick(&hosts)).expect("fixed names encode"),
        RData::A(Ipv4Addr::from(rng.next_u32())),
    );
    record.ttl = [0, 1, 120, u32::MAX][rng.below(4)];
    assert!(writer.push_record(Section::Answer, &record));
    let len = writer.finish();
    out.truncate(len);
    out
}

/// A question id the front could issue.
fn question(rng: &mut Prng) -> u32 {
    u32::try_from(rng.below(MAX_QUESTIONS)).expect("the bound fits an id")
}

fn encoded(frame: &ToDecoder<'_>) -> Vec<u8> {
    let mut out = vec![0u8; MAX_TO_DECODER];
    let len = frame.encode(&mut out).expect("a well-formed frame fits");
    out.truncate(len);
    out
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let declared = u32::try_from(payload.len()).expect("a test frame is short");
    let mut out = declared.to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

/// Invariant 1.
fn exercise_codecs(rng: &mut Prng, noise: &[u8]) {
    if let Ok(frame) = ToDecoder::decode(noise) {
        assert_eq!(encoded(&frame), noise, "an accepted frame is canonical");
    }
    if let Ok(frame) = FromDecoder::decode(noise) {
        assert_eq!(
            from_decoder(&frame),
            noise,
            "an accepted frame is canonical"
        );
    }

    let mut cache_key = [0u8; CACHE_KEY_LEN];
    let mut rng_key = [0u8; RNG_KEY_LEN];
    rng.fill(&mut cache_key);
    rng.fill(&mut rng_key);
    let shapes = [
        ToDecoder::Configure {
            cache_key: &cache_key,
            rng_key: &rng_key,
        },
        ToDecoder::Datagram {
            now: rng.next_u64(),
            interface: iface(rng),
            source: peer(rng.next_u8()),
            port: rng.next_u16(),
            to_group: true,
            payload: noise,
        },
        ToDecoder::Tick {
            now: rng.next_u64(),
        },
        ToDecoder::Link {
            now: rng.next_u64(),
            interface: iface(rng),
            edge: if rng.next_u64() & 1 == 0 {
                Edge::Up(Budgets {
                    group: BucketLevel {
                        scaled: rng.next_u64() % (u64::from(GROUP_BURST) * SEC + 1),
                        at: rng.next_u64(),
                    },
                    wsd: BucketLevel {
                        scaled: rng.next_u64() % (u64::from(WSD_BURST) * SEC + 1),
                        at: rng.next_u64(),
                    },
                })
            } else {
                Edge::Down
            },
        },
        ToDecoder::Ask {
            now: rng.next_u64(),
            question: question(rng),
            form: *rng.pick(&FORMS),
            name: b"\x07printer\x05local\x00",
        },
        ToDecoder::Stop {
            question: question(rng),
        },
        ToDecoder::Replay {
            question: question(rng),
            token: rng.next_u32(),
        },
        ToDecoder::Wsd(ToWsd::Datagram {
            now: rng.next_u64(),
            interface: iface(rng),
            source: peer(rng.next_u8()),
            payload: noise,
        }),
        ToDecoder::Wsd(ToWsd::Fetched {
            now: rng.next_u64(),
            interface: iface(rng),
            fetch: rng.next_u32(),
            response: noise,
        }),
        ToDecoder::Wsd(ToWsd::FetchFailed {
            now: rng.next_u64(),
            interface: iface(rng),
            fetch: rng.next_u32(),
        }),
    ];
    for shape in shapes {
        let mut bytes = encoded(&shape);
        assert_eq!(ToDecoder::decode(&bytes), Ok(shape));
        let at = rng.below(bytes.len());
        bytes[at] ^= 1 << rng.below(8);
        if let Ok(mutated) = ToDecoder::decode(&bytes) {
            assert_eq!(
                encoded(&mutated),
                bytes,
                "a mutated frame is refused or canonical"
            );
        }
    }
}

/// Invariant 1, for the decoder's frames.
fn exercise_decoder_codec(rng: &mut Prng, noise: &[u8]) {
    let label = if noise.is_empty() {
        &b"x"[..]
    } else {
        &noise[..noise.len().min(63)]
    };
    let answer = Entry::Answer {
        request: rng.next_u32(),
        interface: iface(rng),
        change: [Change::Added, Change::Refreshed, Change::Retired][rng.below(3)],
        ttl: rng.next_u32(),
        answer: Answer::Instance { label },
    };
    let payload = if noise.is_empty() {
        &b"q"[..]
    } else {
        &noise[..noise.len().min(1232)]
    };
    for shape in [
        FromDecoder::Deadline(None),
        FromDecoder::Deadline(Some(rng.next_u64())),
        FromDecoder::Answer(answer),
        FromDecoder::Held {
            token: rng.next_u32(),
            entry: answer,
        },
        FromDecoder::Replayed {
            token: rng.next_u32(),
        },
        FromDecoder::Transmit {
            interface: iface(rng),
            at: rng.next_u64(),
            to: Destination::Group,
            payload,
        },
        FromDecoder::Transmit {
            interface: iface(rng),
            at: rng.next_u64(),
            to: Destination::Peer {
                addr: peer(rng.next_u8()),
                port: rng.next_u16(),
            },
            payload,
        },
        FromDecoder::Linked {
            interface: iface(rng),
            up: rng.next_u64() & 1 == 0,
        },
        FromDecoder::Stopped {
            question: question(rng),
        },
        FromDecoder::Wsd(FromWsd::Transmit {
            interface: iface(rng),
            at: rng.next_u64(),
            payload,
        }),
        FromDecoder::Wsd(FromWsd::Fetch(FetchRequest {
            interface: iface(rng),
            fetch: rng.next_u32(),
            addr: peer(rng.next_u8()),
            port: rng.next_u16() | 1,
            path: rng.pick(&["/", "/meta", "/a/b?c=d"]),
            endpoint: rng.pick(&["urn:uuid:0001", "uuid:x", "http://[fe80::1]/e"]),
            name: *rng.pick(&[None, Some("OFFICE"), Some("nas.example")]),
        })),
    ] {
        let mut bytes = from_decoder(&shape);
        assert_eq!(FromDecoder::decode(&bytes), Ok(shape));
        let at = rng.below(bytes.len());
        bytes[at] ^= 1 << rng.below(8);
        if let Ok(mutated) = FromDecoder::decode(&bytes) {
            assert_eq!(
                from_decoder(&mutated),
                bytes,
                "a mutated frame is refused or canonical"
            );
        }
    }
}

const FORMS: [Form; 8] = [
    Form::Instance,
    Form::Service,
    Form::Text,
    Form::AddressV4,
    Form::AddressV6,
    Form::Pointer,
    Form::Type,
    Form::Computer,
];

fn from_decoder(frame: &FromDecoder<'_>) -> Vec<u8> {
    let mut out = vec![0u8; MAX_FROM_DECODER];
    let len = frame.encode(&mut out).expect("a well-formed frame fits");
    out.truncate(len);
    out
}

/// Holds every frame the decoder emits to the front's reader.
struct Believed;

impl FrameOut for Believed {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        assert!(
            FromDecoder::decode(payload).is_ok(),
            "the decoder sends only frames the front believes"
        );
        Ok(())
    }
}

/// Invariant 2: only a configuration is taken first, and only a datagram for a
/// link it was told of after.
fn exercise_decoder_order(rng: &mut Prng, noise: &[u8], configure: &[u8]) {
    let early = [
        encoded(&ToDecoder::Tick {
            now: rng.next_u64(),
        }),
        encoded(&ToDecoder::Datagram {
            now: rng.next_u64(),
            interface: iface(rng),
            source: peer(rng.next_u8()),
            port: 5353,
            to_group: true,
            payload: noise,
        }),
    ];
    for frame in early {
        assert_eq!(
            Decoder::new().handle(&frame, &mut Believed),
            SessionStep::Finished,
            "nothing but a configuration is taken first"
        );
    }
    // The first datagrams below are for links it is told of; any other is
    // one the front would never relay.
    let mut unlinked = Decoder::new();
    assert_eq!(
        unlinked.handle(configure, &mut Believed),
        SessionStep::Continue
    );
    let stray = encoded(&ToDecoder::Datagram {
        now: 0,
        interface: named(b"eth0"),
        source: peer(rng.next_u8()),
        port: 5353,
        to_group: true,
        payload: noise,
    });
    assert_eq!(
        unlinked.handle(&stray, &mut Believed),
        SessionStep::Finished
    );
    let mut unlinked = Decoder::new();
    assert_eq!(
        unlinked.handle(configure, &mut Believed),
        SessionStep::Continue
    );
    let stray = encoded(&ToDecoder::Wsd(ToWsd::Datagram {
        now: 0,
        interface: named(b"eth0"),
        source: peer(rng.next_u8()),
        payload: noise,
    }));
    assert_eq!(
        unlinked.handle(&stray, &mut Believed),
        SessionStep::Finished,
        "nor a WS-Discovery datagram for a link it was not told of"
    );
}

/// A computer announcing itself, from a small pool of endpoints and
/// addresses so announcements collide, its metadata at the sender's address
/// or under a name its URL gives, which is fetched there too.
fn hello(rng: &mut Prng, from: IpAddr) -> Vec<u8> {
    let host = match (rng.below(3), from) {
        (0, _) => String::from("OFFICE"),
        (_, IpAddr::V4(v4)) => v4.to_string(),
        (_, IpAddr::V6(v6)) => format!("[{v6}]"),
    };
    format!(
        r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsd="http://schemas.xmlsoap.org/ws/2005/04/discovery" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07"><soap:Header><wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Hello</wsa:Action><wsa:MessageID>urn:uuid:m{}</wsa:MessageID><wsd:AppSequence InstanceId="1" MessageNumber="{}"/></soap:Header><soap:Body><wsd:Hello><wsa:EndpointReference><wsa:Address>urn:uuid:{}</wsa:Address></wsa:EndpointReference><wsd:Types>wsdp:Device pub:Computer</wsd:Types><wsd:XAddrs>http://{host}:5357/meta</wsd:XAddrs><wsd:MetadataVersion>{}</wsd:MetadataVersion></wsd:Hello></soap:Body></soap:Envelope>"#,
        rng.next_u32(),
        rng.next_u32(),
        rng.below(4),
        rng.below(3),
    )
    .into_bytes()
}

/// A metadata response naming a computer from a small pool.
fn metadata(rng: &mut Prng) -> Vec<u8> {
    let body = format!(
        r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsx="http://schemas.xmlsoap.org/ws/2004/09/mex" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07"><soap:Body><wsx:Metadata><wsx:MetadataSection Dialect="http://schemas.xmlsoap.org/ws/2006/02/devprof/Relationship"><wsdp:Relationship Type="http://schemas.xmlsoap.org/ws/2006/02/devprof/host"><wsdp:Host><wsdp:Types>pub:Computer</wsdp:Types><pub:Computer>{}/Workgroup:HOME</pub:Computer></wsdp:Host></wsdp:Relationship></wsx:MetadataSection></wsx:Metadata></soap:Body></soap:Envelope>"#,
        rng.pick(&["STUDY", "LIBRARY", "OFFICE"]),
    );
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

/// Invariant 2, for links and questions that end.
fn exercise_decoder_edges(
    rng: &mut Prng,
    noise: &[u8],
    decoder: &mut Decoder,
    configure: &[u8],
    now: u64,
) {
    // A link going down and a question stopped: every frame about them after
    // is still taken.
    let down = encoded(&ToDecoder::Link {
        now,
        interface: named(b"wlan0"),
        edge: Edge::Down,
    });
    assert_eq!(decoder.handle(&down, &mut Believed), SessionStep::Continue);
    let stop = encoded(&ToDecoder::Stop { question: 1 });
    assert_eq!(decoder.handle(&stop, &mut Believed), SessionStep::Continue);
    assert_eq!(
        decoder.handle(&stop, &mut Believed),
        SessionStep::Finished,
        "a question stopped twice was never asked the second time"
    );
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.handle(configure, &mut Believed),
        SessionStep::Continue
    );
    for frame in [
        encoded(&ToDecoder::Link {
            now: 0,
            interface: named(b"eth0"),
            edge: Edge::Up(FULL),
        }),
        encoded(&ToDecoder::Datagram {
            now: 1,
            interface: named(b"eth0"),
            source: peer(rng.next_u8()),
            port: 5353,
            to_group: true,
            payload: noise,
        }),
    ] {
        assert_eq!(decoder.handle(&frame, &mut Believed), SessionStep::Continue);
    }
}

/// Invariant 2, for WS-Discovery: an announcement and noise from a drawn
/// sender, and a fetch's outcome, well-formed or not, for a drawn id.
fn feed_wsd(rng: &mut Prng, decoder: &mut Decoder, noise: &[u8], now: u64) {
    let from = peer(rng.next_u8());
    let announced = hello(rng, from);
    let answered = metadata(rng);
    let interface = iface(rng);
    let fetch = rng.next_u32() % 8;
    for frame in [
        ToWsd::Datagram {
            now,
            interface,
            source: from,
            payload: &announced,
        },
        ToWsd::Datagram {
            now,
            interface,
            source: from,
            payload: noise,
        },
        ToWsd::Fetched {
            now,
            interface,
            fetch,
            response: if rng.below(2) == 0 { &answered } else { noise },
        },
        ToWsd::FetchFailed {
            now,
            interface,
            fetch: rng.next_u32() % 8,
        },
    ] {
        assert_eq!(
            decoder.handle(&encoded(&ToDecoder::Wsd(frame)), &mut Believed),
            SessionStep::Continue,
            "WS-Discovery traffic never ends a configured decoder"
        );
    }
}

/// Invariant 2.
fn exercise_decoder(rng: &mut Prng, noise: &[u8]) {
    let configure = encoded(&ToDecoder::Configure {
        cache_key: &[rng.next_u8(); CACHE_KEY_LEN],
        rng_key: &[rng.next_u8(); RNG_KEY_LEN],
    });
    exercise_decoder_order(rng, noise, &configure);
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.handle(&configure, &mut Believed),
        SessionStep::Continue
    );
    for name in INTERFACES {
        let up = encoded(&ToDecoder::Link {
            now: 0,
            interface: named(name),
            edge: Edge::Up(FULL),
        });
        assert_eq!(decoder.handle(&up, &mut Believed), SessionStep::Continue);
    }
    let asks = [
        ("_ipp._tcp.local", Form::Instance),
        ("printer.local", Form::AddressV4),
        ("host.local", Form::AddressV6),
        ("_services._dns-sd._udp.local", Form::Type),
        (".", Form::Computer),
    ];
    for (question, (name, form)) in (0u32..).zip(asks) {
        let name = if form == Form::Computer {
            Name::from_labels(&[]).expect("the root")
        } else {
            Name::encode(name).expect("fixed names encode")
        };
        let ask = encoded(&ToDecoder::Ask {
            now: 0,
            question,
            form,
            name: name.as_wire(),
        });
        assert_eq!(decoder.handle(&ask, &mut Believed), SessionStep::Continue);
    }
    let mut now = 0u64;
    for round in 0..16u32 {
        // Now and then an instant earlier than one already seen.
        now = if rng.below(4) == 0 {
            rng.next_u64() % now.saturating_add(1)
        } else {
            now.saturating_add(u64::from(rng.next_u32()) * 1_000)
        };
        let announced = announcement(rng);
        for payload in [announced.as_slice(), noise] {
            let frame = encoded(&ToDecoder::Datagram {
                now,
                interface: iface(rng),
                source: peer(rng.next_u8()),
                port: [5353, rng.next_u16()][rng.below(2)],
                to_group: true,
                payload,
            });
            assert_eq!(
                decoder.handle(&frame, &mut Believed),
                SessionStep::Continue,
                "a datagram never ends a configured decoder"
            );
        }
        feed_wsd(rng, &mut decoder, noise, now);
        let tick = encoded(&ToDecoder::Tick { now });
        assert_eq!(decoder.handle(&tick, &mut Believed), SessionStep::Continue);
        let replay = encoded(&ToDecoder::Replay {
            question: round % 5,
            token: round,
        });
        assert_eq!(
            decoder.handle(&replay, &mut Believed),
            SessionStep::Continue
        );
    }
    exercise_decoder_edges(rng, noise, &mut decoder, &configure, now);
    // Arbitrary bytes as a frame: taken or refused, never a panic.
    let _ = decoder.handle(noise, &mut Believed);
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.handle(&configure, &mut Believed),
        SessionStep::Continue
    );
    assert_eq!(
        decoder.handle(&configure, &mut Believed),
        SessionStep::Finished,
        "a second configuration ends the session"
    );
}

/// One launched decoder as the harness drives it.
#[derive(Default)]
struct Wire {
    /// Every byte the front wrote to it.
    written: Vec<u8>,
    /// What it says next.
    says: Vec<u8>,
    /// Whether its stream ends once `says` is spent.
    ended: bool,
    /// Whether it has stopped reading.
    stalled: bool,
}

type Wires = Rc<RefCell<Vec<Wire>>>;

/// A decoder transport answering whatever its wire says — a compromised
/// decoder, as far as the front can tell.
struct Hostile {
    wires: Wires,
    index: usize,
}

impl SessionTransport for Hostile {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut wires = self.wires.borrow_mut();
        let wire = &mut wires[self.index];
        if wire.says.is_empty() {
            return if wire.ended {
                Ok(0)
            } else {
                Err(Errno::WouldBlock)
            };
        }
        let take = buf.len().min(wire.says.len());
        buf[..take].copy_from_slice(&wire.says[..take]);
        wire.says.drain(..take);
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        let mut wires = self.wires.borrow_mut();
        let wire = &mut wires[self.index];
        if wire.stalled {
            return Err(Errno::WouldBlock);
        }
        wire.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn dispose(self) -> Option<i32> {
        None
    }
}

struct Launcher {
    wires: Wires,
}

impl SessionLauncher for Launcher {
    type Transport = Hostile;

    fn launch(&mut self) -> Result<Hostile, Errno> {
        let mut wires = self.wires.borrow_mut();
        wires.push(Wire::default());
        Ok(Hostile {
            wires: self.wires.clone(),
            index: wires.len() - 1,
        })
    }
}

type Under = Front<Launcher, Quiet, Keys>;

/// Tell the front every interface is up on the IPv4 socket.
fn links_up<L: SessionLauncher>(front: &mut Front<L, Quiet, Keys>) {
    for name in INTERFACES {
        front.on_delivery(
            0,
            &SocketDelivery::Link(SocketLinkEvent {
                socket: V4,
                interface: named(name),
                up: true,
            }),
        );
    }
}

/// The frames the front wrote to one decoder, each required to decode.
fn frames(written: &[u8]) -> Vec<ToDecoder<'_>> {
    let mut out = Vec::new();
    let mut rest = written;
    while !rest.is_empty() {
        let (header, body) = rest.split_at(FRAME_HEADER_LEN);
        let declared = u32::from_le_bytes(header.try_into().expect("a whole header"));
        let (frame, tail) = body.split_at(usize::try_from(declared).expect("fits"));
        out.push(
            ToDecoder::decode(frame).expect("the front writes only frames a decoder believes"),
        );
        rest = tail;
    }
    out
}

fn delivery<'a>(rng: &mut Prng, last: u8, payload: &'a [u8]) -> SocketDatagram<'a> {
    let (family, addr) = address_parts(peer(last));
    SocketDatagram {
        socket: V4,
        interface: iface(rng),
        source: SocketAddr {
            family,
            addr,
            port: 5353,
        },
        source_on_link: true,
        to_group: rng.next_u8() & 1 == 0,
        payload,
    }
}

/// A WS-Discovery datagram from `peer(last)` on a drawn link, recorded as
/// heard so the fetches it licenses can be checked.
fn wsd_delivery<'a>(
    rng: &mut Prng,
    world: &Keys,
    last: u8,
    payload: &'a [u8],
) -> SocketDatagram<'a> {
    let mut datagram = delivery(rng, last, payload);
    datagram.socket = WSD_V4;
    datagram.source.port = wsd::PORT;
    world
        .0
        .borrow_mut()
        .heard
        .push((datagram.interface, datagram.source.addr));
    datagram
}

/// One turn of the owner's loop at `now`, holding the invariant that keeps
/// it from spinning.
fn wake<L: SessionLauncher>(front: &mut Front<L, Quiet, Keys>, now: u64) {
    front.on_wake(now).expect("keys draw");
    if let Some(at) = front.wake_at() {
        assert!(at > now, "a wake at {at} after acting at {now} would spin");
    }
}

/// Every write turn the front asks for, into a decoder taking everything.
fn drain<L: SessionLauncher>(front: &mut Front<L, Quiet, Keys>, now: u64) {
    for _ in 0..10_000 {
        if !front.wants_write() {
            return;
        }
        front.on_decoder_writable(now);
    }
    panic!("the queue never drained into a decoder reading everything");
}

/// Read turns until the front has taken everything its decoder said.
fn hear(front: &mut Under, wires: &Wires, now: u64) {
    for _ in 0..10_000 {
        let pending = wires
            .borrow()
            .last()
            .is_some_and(|wire| !wire.says.is_empty() || wire.ended);
        if !pending || !front.wants_read() {
            return;
        }
        front.on_decoder_readable(now);
    }
    panic!("the front stopped taking what its decoder said");
}

/// Start a replacement if the decoder is contained. Only called with the
/// queue drained, so a front not taking datagrams has no live decoder.
fn revive(front: &mut Under, now: &mut u64) {
    if front.wants_deliveries() {
        return;
    }
    let at = front
        .wake_at()
        .expect("a contained decoder's replacement is always due");
    *now = (*now).max(at);
    wake(front, *now);
    drain(front, *now);
    assert!(front.wants_deliveries(), "the replacement is live");
}

/// Load what the live decoder says in a round of `kind`; `true` when it is
/// something no decoder says, so the front must contain it.
fn say(wires: &Wires, kind: usize, rng: &mut Prng, noise: &[u8], now: u64) -> bool {
    let mut wires = wires.borrow_mut();
    let wire = wires.last_mut().expect("launched");
    let (said, condemned) = match kind {
        0 => {
            let at = rng.next_u64() % now.saturating_add(SEC);
            (
                framed(&from_decoder(&FromDecoder::Deadline(Some(at)))),
                false,
            )
        }
        1 => (
            framed(&from_decoder(&FromDecoder::Deadline(Some(rng.next_u64())))),
            false,
        ),
        2 => (framed(&from_decoder(&FromDecoder::Deadline(None))), false),
        3 => (framed(&[0xFF]), true),
        4 => (u32::MAX.to_le_bytes().to_vec(), true),
        // An answer to a question the front never asked.
        9 => (
            framed(&from_decoder(&FromDecoder::Answer(Entry::Answer {
                request: u32::MAX,
                interface: named(b"eth0"),
                change: Change::Added,
                ttl: 1,
                answer: Answer::Instance { label: b"x" },
            }))),
            true,
        ),
        // A datagram for the group: sent within budget once the link has
        // settled, never a reason to contain the decoder.
        10 => (
            framed(&from_decoder(&FromDecoder::Transmit {
                interface: named(b"eth0"),
                at: now,
                to: Destination::Group,
                payload: b"query",
            })),
            false,
        ),
        // A datagram built at an instant the front has not reached: the
        // decoder knows no time the front did not tell it.
        13 => (
            framed(&from_decoder(&FromDecoder::Transmit {
                interface: named(b"eth0"),
                at: now + 1,
                to: Destination::Group,
                payload: b"query",
            })),
            true,
        ),
        // An acknowledgement of a link it was never told of.
        11 => (
            framed(&from_decoder(&FromDecoder::Linked {
                interface: named(b"zz9"),
                up: true,
            })),
            true,
        ),
        // A stop it was never told of: no client asked this front anything.
        12 => (
            framed(&from_decoder(&FromDecoder::Stopped {
                question: question(rng),
            })),
            true,
        ),
        14..=16 => wsd_said(kind, rng, now),
        // A whole frame of noise: believed or contained, and aligned either way.
        5 => (framed(&noise[..noise.len().min(64)]), false),
        6 => {
            wire.ended = true;
            (noise.to_vec(), true)
        }
        7 => {
            wire.ended = true;
            (Vec::new(), true)
        }
        _ => (Vec::new(), false),
    };
    wire.says.extend_from_slice(&said);
    condemned
}

/// What the decoder's WS-Discovery engines say in a round of `kind`, framed,
/// and whether it is something no decoder says.
fn wsd_said(kind: usize, rng: &mut Prng, now: u64) -> (Vec<u8>, bool) {
    match kind {
        // A WS-Discovery datagram for the group, within its budget.
        14 => (
            framed(&from_decoder(&FromDecoder::Wsd(FromWsd::Transmit {
                interface: named(b"eth0"),
                at: now,
                payload: b"probe",
            }))),
            false,
        ),
        // One built at an instant the front has not reached.
        15 => (
            framed(&from_decoder(&FromDecoder::Wsd(FromWsd::Transmit {
                interface: named(b"eth0"),
                at: now + 1,
                payload: b"probe",
            }))),
            true,
        ),
        // A fetch of a peer it may or may not have heard, by address or by
        // the name its URL gave: made or refused, never a reason to contain
        // the decoder.
        _ => (
            framed(&from_decoder(&FromDecoder::Wsd(FromWsd::Fetch(
                FetchRequest {
                    interface: iface(rng),
                    fetch: rng.next_u32(),
                    addr: peer(rng.next_u8()),
                    port: 5357,
                    path: "/meta",
                    endpoint: "urn:uuid:0001",
                    name: *rng.pick(&[None, Some("OFFICE")]),
                },
            )))),
            false,
        ),
    }
}

fn set_stalled(wires: &Wires, stalled: bool) {
    wires.borrow_mut().last_mut().expect("launched").stalled = stalled;
}

/// Invariant 3.
fn exercise_front(rng: &mut Prng, noise: &[u8]) {
    let wires: Wires = Rc::default();
    let world = Keys::default();
    let mut front = Front::new(
        Launcher {
            wires: wires.clone(),
        },
        Quiet,
        world.clone(),
        Sockets {
            v4: Some(V4),
            v6: Some(V6),
            wsd_v4: Some(WSD_V4),
            wsd_v6: None,
        },
        Grants::default(),
    )
    .expect("the buffers commit");
    let mut now = 0u64;
    let mut last = 0u8;
    wake(&mut front, now);
    drain(&mut front, now);
    links_up(&mut front);
    drain(&mut front, now);

    for round in 0..2 * SAYINGS {
        revive(&mut front, &mut now);
        wake(&mut front, now);
        last = last.wrapping_add(1);
        let announced = wsd_delivery(rng, &world, last, noise);
        front.on_delivery(now, &SocketDelivery::Datagram(announced));
        front.on_delivery(now, &SocketDelivery::Datagram(delivery(rng, last, noise)));
        drain(&mut front, now);
        {
            let wires = wires.borrow();
            let written = frames(&wires.last().expect("launched").written);
            assert!(
                matches!(written.last(),
                    Some(ToDecoder::Datagram { now: at, payload, .. })
                        if *at == now && *payload == noise),
                "a live decoder is relayed to"
            );
        }
        let condemned = say(&wires, round % SAYINGS, rng, noise, now);
        hear(&mut front, &wires, now);
        if condemned {
            assert!(!front.wants_deliveries(), "the decoder was contained");
            assert!(front.wake_at().is_some(), "and its replacement is due");
        }
        now = now.saturating_add(u64::from(rng.next_u32() % 300) * MS);
    }

    owed_tick(rng, &mut front, &wires, &mut now, &mut last);
    check_written(&wires, &world);
}

/// Every decoder was configured first and once under fresh keys, ticked no
/// closer than the floor, and relayed to only from links it was told of; and
/// the front sent only to the groups.
fn check_written(wires: &Wires, world: &Keys) {
    let mut last_tick: Option<u64> = None;
    let mut last_key: Option<[u8; CACHE_KEY_LEN]> = None;
    for wire in wires.borrow().iter() {
        let written = frames(&wire.written);
        let Some(ToDecoder::Configure { cache_key, .. }) = written.first() else {
            panic!("every decoder is configured first");
        };
        assert_ne!(last_key, Some(**cache_key), "every decoder is keyed afresh");
        last_key = Some(**cache_key);
        let mut linked: Vec<[u8; IF_NAME_LEN]> = Vec::new();
        for frame in &written[1..] {
            match frame {
                ToDecoder::Configure { .. } => panic!("a decoder is configured once"),
                ToDecoder::Tick { now: at } => {
                    if let Some(previous) = last_tick {
                        assert!(*at >= previous + MIN_TICK_INTERVAL_NS, "ticks are spaced");
                    }
                    last_tick = Some(*at);
                }
                ToDecoder::Link {
                    interface,
                    edge: Edge::Up(_),
                    ..
                } => linked.push(*interface),
                ToDecoder::Link {
                    interface,
                    edge: Edge::Down,
                    ..
                } => linked.retain(|name| name != interface),
                ToDecoder::Datagram { interface, .. }
                | ToDecoder::Wsd(ToWsd::Datagram { interface, .. }) => assert!(
                    linked.contains(interface),
                    "a datagram is relayed only from a link the decoder runs"
                ),
                ToDecoder::Wsd(
                    ToWsd::Fetched { interface, .. } | ToWsd::FetchFailed { interface, .. },
                ) => assert!(
                    linked.contains(interface),
                    "a fetch is answered only on a link the decoder runs"
                ),
                ToDecoder::Ask { .. } | ToDecoder::Stop { .. } | ToDecoder::Replay { .. } => {}
            }
        }
    }
    // Whatever the decoders asked, the front sent only to its protocols'
    // groups.
    for (protocol, to) in &world.0.borrow().sent {
        let (groups, port) = match protocol {
            Protocol::MulticastDns => ([IpAddr::V4(GROUP_V4), IpAddr::V6(GROUP_V6)], 5353),
            Protocol::WsDiscovery => (
                [IpAddr::V4(wsd::GROUP_V4), IpAddr::V6(wsd::GROUP_V6)],
                wsd::PORT,
            ),
        };
        let group = groups
            .iter()
            .any(|group| address_parts(*group).1 == to.addr && to.port == port);
        assert!(group, "sent to {to:?}, which is not a {protocol:?} group");
    }
}

/// A decoder that asks for time already come and then stops reading with its
/// queue full to the byte: the tick it is owed waits on room, and follows the
/// held datagram once there is some.
fn owed_tick(rng: &mut Prng, front: &mut Under, wires: &Wires, now: &mut u64, last: &mut u8) {
    revive(front, now);
    {
        let mut wires = wires.borrow_mut();
        let wire = wires.last_mut().expect("launched");
        wire.says
            .extend_from_slice(&framed(&from_decoder(&FromDecoder::Deadline(Some(*now)))));
    }
    hear(front, wires, *now);
    set_stalled(wires, true);
    let mut exact = vec![0u8; OUTBOUND_QUEUE / 32 - FRAME_HEADER_LEN - DATAGRAM_HEADER_LEN];
    rng.fill(&mut exact);
    let mut relayed = 0;
    while front.wants_deliveries() {
        *last = last.wrapping_add(1);
        front.on_delivery(
            *now,
            &SocketDelivery::Datagram(delivery(rng, *last, &exact)),
        );
        relayed += 1;
        assert!(relayed <= 33, "the queue holds exactly 32 and one held");
    }
    *now += MIN_TICK_INTERVAL_NS;
    wake(front, *now);
    set_stalled(wires, false);
    drain(front, *now);
    {
        let wires = wires.borrow();
        let written = frames(&wires.last().expect("launched").written);
        let tail = &written[written.len().saturating_sub(2)..];
        assert!(
            matches!(tail, [ToDecoder::Datagram { .. }, ToDecoder::Tick { now: at }] if *at == *now),
            "the owed tick followed the held datagram once the queue drained"
        );
    }
}

/// A real decoder answering in process, which the harness can make exit.
struct Mortal {
    session: LoopbackSession<Decoder>,
    dead: Rc<Cell<bool>>,
}

impl SessionTransport for Mortal {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        if self.dead.get() {
            return Ok(0);
        }
        self.session.read(buf)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        if self.dead.get() {
            return Err(Errno::BrokenPipe);
        }
        self.session.write(buf)
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn dispose(self) -> Option<i32> {
        None
    }
}

/// Launches real decoders; setting `dead` ends the live one.
struct Mortals {
    dead: Rc<Cell<bool>>,
}

impl SessionLauncher for Mortals {
    type Transport = Mortal;

    fn launch(&mut self) -> Result<Mortal, Errno> {
        self.dead.set(false);
        Ok(Mortal {
            session: LoopbackSession::new(Decoder::new()),
            dead: self.dead.clone(),
        })
    }
}

type Lived = Front<Mortals, Quiet, Keys>;

/// Turns until a decoder answering in process has said all it will.
fn settle(front: &mut Lived, now: u64) {
    for _ in 0..64 {
        drain(front, now);
        if !front.wants_read() {
            return;
        }
        front.on_decoder_readable(now);
        if !front.wants_write() {
            return;
        }
    }
    panic!("the front and its decoder never settled");
}

/// A caller owning one port.
struct Asker {
    origin: Origin,
    owned: u64,
}

impl Caller for Asker {
    fn origin(&self) -> &Origin {
        &self.origin
    }

    fn owns_port(&self, port: u64) -> bool {
        port == self.owned
    }
}

const POOL_HOSTS: [&str; 2] = ["printer.local", "nas.local"];
const POOL_TYPES: [&[u8]; 2] = [b"_ipp", b"_http"];

/// A query from the small pool every client draws from, so requests join.
fn pooled<'a>(rng: &mut Prng, hosts: &'a [Name; 2]) -> Query<'a> {
    match rng.below(5) {
        4 => Query::Computers,
        0 | 1 => Query::Browse {
            service: ServiceTypeField {
                name: &POOL_TYPES[rng.below(2)][1..],
                transport: Transport::Tcp,
            },
        },
        2 => Query::Types,
        _ => Query::Host {
            name: hosts[rng.below(2)].as_wire(),
            families: Families::BOTH,
        },
    }
}

/// A response answering questions in the clients' pool, goodbyes among them.
fn pool_response(rng: &mut Prng) -> Vec<u8> {
    let mut out = vec![0u8; 512];
    let mut writer = MessageWriter::new(&mut out, 0, true).expect("room for a header");
    for _ in 0..=rng.below(3) {
        let service = *rng.pick(&POOL_TYPES);
        let mut record = match rng.below(3) {
            0 => Record::unique(
                Name::encode(rng.pick(&POOL_HOSTS)).expect("fixed names encode"),
                RData::A(Ipv4Addr::new(192, 168, 1, rng.next_u8())),
            ),
            1 => Record::shared(
                Name::from_labels(&[service, b"_tcp", b"local"]).expect("a type"),
                RData::Ptr(
                    Name::from_labels(&[
                        rng.pick(&[b"Hall".as_slice(), b"Lobby"]),
                        service,
                        b"_tcp",
                        b"local",
                    ])
                    .expect("an instance"),
                ),
            ),
            _ => Record::shared(
                Name::encode("_services._dns-sd._udp.local").expect("the meta-query"),
                RData::Ptr(Name::from_labels(&[service, b"_tcp", b"local"]).expect("a type")),
            ),
        };
        record.ttl = [0, 1, 120, 4500][rng.below(4)];
        assert!(writer.push_record(Section::Answer, &record));
    }
    let len = writer.finish();
    out.truncate(len);
    out
}

/// What the client sweeps reached, so one reaching nothing fails rather than
/// passing vacuously.
#[derive(Default)]
struct Reached {
    opened: u32,
    started: u32,
    answers: u32,
    computers: u32,
    blocked: u32,
    deaths: u32,
}

/// Whether `reply` is one the client reads for `call`, and the id it carries.
fn read_reply(call: &DiscoveryRequest<'_>, reply: &[u8], reached: &mut Reached) -> Option<u32> {
    let refused =
        reply.len() == tairix_abi::reply::STATUS_REPLY_LEN && decode_status_reply(reply).is_err();
    match call {
        DiscoveryRequest::Open { .. } | DiscoveryRequest::Start { .. } => {
            let id = tairix_abi::discovery_ipc::decode_id_reply(reply).ok();
            assert!(id.is_some() || refused, "an id or a refusal");
            id
        }
        DiscoveryRequest::Collect { .. } => {
            if let Ok(collected) = CollectReply::parse(reply) {
                for entry in collected.entries() {
                    if let Entry::Answer { answer, .. } = entry {
                        reached.answers += 1;
                        if matches!(answer, Answer::Computer { .. }) {
                            reached.computers += 1;
                        }
                    }
                }
            } else {
                assert!(refused, "entries or a refusal");
            }
            None
        }
        _ => {
            assert_eq!(reply.len(), tairix_abi::reply::STATUS_REPLY_LEN, "a status");
            None
        }
    }
}

/// The clients one invariant-4 sweep drives against a front over a killable
/// decoder, and what each holds.
struct Crowd {
    world: Keys,
    dead: Rc<Cell<bool>>,
    front: Lived,
    clients: Vec<Asker>,
    opened: Vec<(usize, u32)>,
    started: Vec<(usize, u32, u32)>,
    gone: [bool; 5],
    now: u64,
    bytes: Vec<u8>,
    reply: Vec<u8>,
}

impl Crowd {
    /// The client that names each sweep's first computer.
    const BROWSER: usize = 0;

    /// The front with its links up, and five processes over three accounts,
    /// so accounts span processes. The first, [`Self::BROWSER`], may browse
    /// computers; the rest hold what they draw.
    fn new(rng: &mut Prng) -> Self {
        let world = Keys::default();
        let dead: Rc<Cell<bool>> = Rc::default();
        let mut front = Front::new(
            Mortals { dead: dead.clone() },
            Quiet,
            world.clone(),
            Sockets {
                v4: Some(V4),
                v6: None,
                wsd_v4: Some(WSD_V4),
                wsd_v6: None,
            },
            Grants::default(),
        )
        .expect("the buffers commit");
        wake(&mut front, 0);
        settle(&mut front, 0);
        links_up(&mut front);
        settle(&mut front, 0);
        let clients = (0..5u8)
            .map(|n| {
                let browser = usize::from(n) == Self::BROWSER;
                let mut summary = CapabilitySummary::EMPTY;
                if rng.below(8) != 0 || browser {
                    summary.insert(CapabilityId::NET);
                }
                if rng.below(4) != 0 || browser {
                    summary.insert(CapabilityId::NET_DISCOVER_ALL);
                }
                Asker {
                    origin: Origin::new(
                        TrustDomain::User,
                        1000 + u32::from(n % 3),
                        100,
                        u64::from(n) + 7,
                        ProcId::from_raw([n + 1; 16]),
                        summary,
                        tairix_abi::ORIGIN_CONSOLE_NONE,
                    ),
                    owned: 100 + u64::from(n),
                }
            })
            .collect();
        Self {
            world,
            dead,
            front,
            clients,
            opened: Vec::new(),
            started: Vec::new(),
            gone: [false; 5],
            now: 0,
            bytes: vec![0u8; 512],
            reply: vec![0u8; DISCOVERY_MAX_REPLY],
        }
    }

    /// A session of `who`'s, now and then one it never opened.
    fn session_for(&self, rng: &mut Prng, who: usize) -> u32 {
        let mine: Vec<u32> = self
            .opened
            .iter()
            .filter(|&&(client, _)| client == who)
            .map(|&(_, session)| session)
            .collect();
        if mine.is_empty() || rng.below(8) == 0 {
            rng.next_u32()
        } else {
            *rng.pick(&mine)
        }
    }

    /// One step: the call `who` makes, or `None` after acting on the front
    /// directly. Exits and decoder deaths are rare enough that most sweeps
    /// hear answers between them.
    fn step<'h>(
        &mut self,
        rng: &mut Prng,
        hosts: &'h [Name; 2],
        who: usize,
        session: u32,
        foreign: bool,
        reached: &mut Reached,
    ) -> Option<DiscoveryRequest<'h>> {
        let now = self.now;
        match rng.below(20) {
            0 | 1 => Some(DiscoveryRequest::Open {
                deliver_port: self.clients[if foreign {
                    (who + 1) % self.clients.len()
                } else {
                    who
                }]
                .owned,
            }),
            2..=4 => Some(DiscoveryRequest::Start {
                session,
                query: pooled(rng, hosts),
            }),
            5 => {
                let theirs: Vec<u32> = self
                    .started
                    .iter()
                    .filter(|&&(client, held, _)| client == who && held == session)
                    .map(|&(_, _, request)| request)
                    .collect();
                let request = if theirs.is_empty() {
                    rng.next_u32()
                } else {
                    *rng.pick(&theirs)
                };
                Some(DiscoveryRequest::Stop { session, request })
            }
            6..=8 => Some(DiscoveryRequest::Collect {
                session,
                capacity: rng.next_u32() % 16_384,
            }),
            9 => Some(DiscoveryRequest::Close { session }),
            10 => {
                if !self.gone[who] && rng.below(4) == 0 {
                    self.gone[who] = true;
                    self.front
                        .on_peer_exit(now, self.clients[who].origin.proc_id());
                    self.opened.retain(|&(client, _)| client != who);
                    self.started.retain(|&(client, _, _)| client != who);
                }
                None
            }
            11 | 12 => {
                let payload = pool_response(rng);
                let last = rng.next_u8() & !1;
                self.front.on_delivery(
                    now,
                    &SocketDelivery::Datagram(delivery(rng, last, &payload)),
                );
                None
            }
            13 => {
                self.announce_computer(rng, now);
                None
            }
            16 => {
                self.answer_fetch(rng, now);
                None
            }
            14 => {
                self.front.on_delivery(
                    now,
                    &SocketDelivery::Link(SocketLinkEvent {
                        socket: V4,
                        interface: iface(rng),
                        up: rng.below(2) == 0,
                    }),
                );
                None
            }
            15 => {
                if self.front.wants_deliveries() && rng.below(4) == 0 {
                    self.dead.set(true);
                    settle(&mut self.front, now);
                    assert!(!self.front.wants_deliveries(), "the exit was noticed");
                    reached.deaths += 1;
                }
                None
            }
            _ => {
                self.now = match self.front.wake_at() {
                    Some(at) if rng.below(4) != 0 => at.max(now),
                    _ => now + rng.below(3000) as u64 * MS,
                };
                wake(&mut self.front, self.now);
                self.front.retry_rings();
                None
            }
        }
    }

    /// Name one computer before the random steps, so every sweep reaches the
    /// naming path and the steps after meet a computer the front knows.
    fn name_a_computer(&mut self, rng: &mut Prng, reached: &mut Reached) {
        let browser = Self::BROWSER;
        let open = DiscoveryRequest::Open {
            deliver_port: self.clients[browser].owned,
        };
        self.serve(&open, browser, false, reached);
        let &(_, session) = self.opened.last().expect("the browser opens a session");
        let start = DiscoveryRequest::Start {
            session,
            query: Query::Computers,
        };
        self.serve(&start, browser, false, reached);
        settle(&mut self.front, self.now);
        self.announce_computer(rng, self.now);
        settle(&mut self.front, self.now);
        let socket = *self
            .world
            .0
            .borrow()
            .open
            .last()
            .expect("its metadata is fetched");
        let response = metadata(rng);
        self.deliver_fetch(self.now, socket, &response, StreamCloseReason::PeerClosed);
        settle(&mut self.front, self.now);
        let named = reached.computers;
        let collect = DiscoveryRequest::Collect {
            session,
            capacity: u32::try_from(DISCOVERY_MAX_REPLY).expect("fits"),
        };
        self.serve(&collect, browser, false, reached);
        assert_eq!(
            reached.computers,
            named + 1,
            "the computer announced and described is named once"
        );
    }

    /// A computer from the announcers' pool says Hello on a drawn link.
    fn announce_computer(&mut self, rng: &mut Prng, now: u64) {
        let last = 2 * (rng.next_u8() % 4);
        let payload = hello(rng, peer(last));
        let datagram = wsd_delivery(rng, &self.world, last, &payload);
        self.front
            .on_delivery(now, &SocketDelivery::Datagram(datagram));
    }

    /// Carry one open fetch connection to its end: a whole response, or a
    /// connection cut off.
    fn answer_fetch(&mut self, rng: &mut Prng, now: u64) {
        let open = self.world.0.borrow().open.clone();
        if open.is_empty() {
            return;
        }
        let socket = *rng.pick(&open);
        let response = metadata(rng);
        let reason = if rng.below(4) == 0 {
            StreamCloseReason::Reset
        } else {
            StreamCloseReason::PeerClosed
        };
        self.deliver_fetch(now, socket, &response, reason);
    }

    /// `response` whole on `socket`'s fetch, then the connection closed for
    /// `reason`.
    fn deliver_fetch(
        &mut self,
        now: u64,
        socket: SocketId,
        response: &[u8],
        reason: StreamCloseReason,
    ) {
        for event in [
            SocketStreamEvent::Connected { socket },
            SocketStreamEvent::Data {
                socket,
                payload: response,
            },
            SocketStreamEvent::Closed { socket, reason },
        ] {
            self.front.on_stream(now, &event);
        }
    }

    /// Serve `call` as `who`, and keep what it opened or started.
    fn serve(
        &mut self,
        call: &DiscoveryRequest<'_>,
        who: usize,
        foreign: bool,
        reached: &mut Reached,
    ) {
        let Self {
            front,
            clients,
            bytes,
            reply,
            now,
            ..
        } = self;
        let len = call.encode(bytes).expect("encodes");
        let len = front.serve(*now, &clients[who], &bytes[..len], reply);
        let id = read_reply(call, &reply[..len], reached);
        match (call, id) {
            (DiscoveryRequest::Open { .. }, Some(id)) => {
                assert!(!foreign, "a session opened on another's port");
                reached.opened += 1;
                self.opened.push((who, id));
            }
            (DiscoveryRequest::Open { .. }, None) if foreign => {
                assert_eq!(
                    decode_status_reply(&reply[..len]),
                    Err(Errno::PermissionDenied)
                );
            }
            (DiscoveryRequest::Start { session, .. }, Some(id)) => {
                reached.started += 1;
                self.started.push((who, *session, id));
            }
            (DiscoveryRequest::Close { session }, _) => {
                self.opened.retain(|&held| held != (who, *session));
                self.started
                    .retain(|&(client, held, _)| client != who || held != *session);
            }
            _ => {}
        }
    }

    /// Once every client has gone and a live decoder has said all it will,
    /// nothing they asked for is left behind.
    fn assert_left_idle(mut self, reached: &mut Reached) {
        for (client, asker) in self.clients.iter().enumerate() {
            if !self.gone[client] {
                self.front.on_peer_exit(self.now, asker.origin.proc_id());
            }
        }
        if !self.front.wants_deliveries() {
            self.now = self
                .now
                .max(self.front.wake_at().expect("a replacement is due"));
            wake(&mut self.front, self.now);
        }
        settle(&mut self.front, self.now);
        self.front.sync_room_watches(&mut |_| true, &mut |_| {});
        self.front.assert_idle();
        reached.blocked += self.world.0.borrow().blocked;
    }
}

/// Invariant 4.
fn exercise_clients(rng: &mut Prng, noise: &[u8], reached: &mut Reached) {
    let hosts = POOL_HOSTS.map(|host| Name::encode(host).expect("fixed names encode"));
    let mut crowd = Crowd::new(rng);
    crowd.name_a_computer(rng, reached);
    for _ in 0..96 {
        crowd.world.0.borrow_mut().ring = match rng.below(4) {
            0 => Some(Errno::WouldBlock),
            1 => Some(Errno::NotFound),
            _ => None,
        };
        let who = rng.below(crowd.clients.len());
        let session = crowd.session_for(rng, who);
        let foreign = rng.below(4) == 0;
        let call = crowd.step(rng, &hosts, who, session, foreign, reached);
        if let Some(call) = call.filter(|_| !crowd.gone[who]) {
            crowd.serve(&call, who, foreign, reached);
        }
        settle(&mut crowd.front, crowd.now);
        crowd
            .front
            .sync_room_watches(&mut |_| rng.below(4) != 0, &mut |_| {});
        crowd.front.assert_consistent();
    }
    let Crowd {
        front,
        clients,
        reply,
        now,
        ..
    } = &mut crowd;
    let len = front.serve(*now, &clients[0], noise, reply);
    assert!(len > 0, "even noise is answered, with its refusal");
    crowd.assert_left_idle(reached);
}

/// One sweep names a computer whatever it draws: the naming path is reached
/// by construction, where the random steps alone rarely completed it.
#[test]
fn every_client_sweep_names_a_computer() {
    for seed in [1, 40, 57, 10_194_581_964_832_627_012] {
        let mut reached = Reached::default();
        exercise_clients(&mut Prng::new(seed), &[0xA5; 32], &mut reached);
        assert!(reached.computers > 0, "seed {seed} named no computer");
    }
}

#[test]
fn hostile_frames_and_decoders_never_break_the_front() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_frames_and_decoders_never_break_the_front",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut noise = [0u8; 700];
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut reached = Reached::default();
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let size = rng.below(noise.len() + 1);
            rng.fill(&mut noise[..size]);
            exercise_codecs(&mut rng, &noise[..size]);
            exercise_decoder_codec(&mut rng, &noise[..size]);
            exercise_decoder(&mut rng, &noise[..size]);
            exercise_front(&mut rng, &noise[..size]);
            exercise_clients(&mut rng, &noise[..size], &mut reached);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
    let Reached {
        opened,
        started,
        answers,
        computers,
        blocked,
        deaths,
    } = reached;
    assert!(
        opened > 0 && started > 0 && answers > 0 && computers > 0 && blocked > 0 && deaths > 0,
        "the client sweep reached too little: {opened} opened, {started} started, \
         {answers} answers, {computers} computers named, {blocked} doorbells blocked, \
         {deaths} decoder exits"
    );
}
