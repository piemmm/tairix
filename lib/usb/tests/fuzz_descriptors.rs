//! Deterministic fuzz harness for the `lib/usb` descriptor decoders: every
//! descriptor enumeration reads is written by whatever device was plugged in.
//!
//! [`DeviceDescriptor::decode`], [`InterfaceInfo::decode_all`],
//! [`HubDescriptor::decode`], and the string descriptor decoders
//! ([`StringHeader`], [`first_langid`], [`SerialNumber::decode`]) are checked
//! against a naive model of what they must accept:
//!
//! * no input panics — each returns a value or a refusal;
//! * a device descriptor decodes exactly when it is one, each field read
//!   from its own offset;
//! * every decoded interface is the first default setting of its interface
//!   number in the chain, every endpoint it carries an endpoint descriptor
//!   there, and no two endpoints share an endpoint context or name the
//!   default control endpoint's;
//! * a hub descriptor decodes exactly when it is of the type the hub's speed
//!   serves and carries its characteristics;
//! * a string descriptor is accepted exactly when both answers are the same
//!   well-formed descriptor, and a serial is 1..=126 code units, exactly the
//!   ones delivered;
//! * an alternate setting decodes only as the chain states it — its own
//!   interface descriptor, each endpoint one that follows it, none twice —
//!   and whether an interface is control-only, and which numbers a
//!   configuration declares, are what the chain says; an isochronous budget
//!   never moves more than a bus interval can, and an explicit feedback
//!   report is read or refused, never panicked on.
//!
//! A per-run-seeded `Prng` mutates well-formed descriptors (the shapes the
//! engine serves, and forged ones it must refuse), assembles random
//! descriptor chains, and feeds pure noise. A plain `cargo test` runs the
//! [`SMOKE_ITERATIONS`] sweep once from a fresh, logged seed; `cargo xtask
//! fuzz` exports `TAIRIX_FUZZ_BUDGET_SECS` to extend the loop to a wall-clock
//! budget.

use tairix_abi::usb_urb::{UsbSpeed, ISO_MAX_PACKET_BYTES};
use tairix_abi::DriverError;
use tairix_fuzzseed::Prng;
use tairix_usb::alternate::{
    alternate_setting, interface_numbers, is_control_only, MAX_ALT_ENDPOINTS,
};
use tairix_usb::device::{
    first_langid, DeviceDescriptor, HubDescriptor, InterfaceInfo, PeriodicShape, SerialNumber,
    StringHeader, INT_TRANSFER_MAX,
};
use tairix_usb::periodic::{EndpointDescriptor, FeedbackDecoder, PeriodicBudget};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// Largest noise buffer.
const MAX_NOISE: usize = 1024;

/// UTF-16 code units the longest string descriptor carries: a one-byte
/// `bLength` less the 2-byte header, halved.
const MAX_SERIAL_UNITS: usize = 126;

/// `bDescriptorType` of a string descriptor.
const STRING: u8 = 0x03;

/// The default control endpoint's context, which also marks an interface
/// with no interrupt endpoint.
const DCI_CONTROL: u8 = 1;

/// Endpoint addresses drawn often enough to collide: endpoint zero both ways,
/// the low endpoints, the highest, and reserved address bits set.
const ADDRESSES: [u8; 12] = [
    0x00, 0x80, 0x01, 0x81, 0x02, 0x82, 0x83, 0x04, 0x0F, 0x8F, 0x91, 0x71,
];

/// Interface class triples: HID boot keyboard and mouse, HID without boot,
/// mass storage BOT, UAS and CBI, a hub, and a class nothing serves.
const CLASSES: [[u8; 3]; 8] = [
    [0x03, 0x01, 0x01],
    [0x03, 0x01, 0x02],
    [0x03, 0x00, 0x00],
    [0x08, 0x06, 0x50],
    [0x08, 0x06, 0x62],
    [0x08, 0x04, 0x00],
    [0x09, 0x00, 0x00],
    [0xFF, 0x42, 0x01],
];

/// `template` with a handful of bytes flipped, then cut short or extended.
fn mutate(template: &[u8], rng: &mut Prng) -> Vec<u8> {
    let mut bytes = template.to_vec();
    for _ in 0..rng.at_most(8) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        bytes[at] ^= rng.next_u8();
    }
    match rng.below(3) {
        0 => bytes.truncate(rng.at_most(bytes.len())),
        1 => bytes.extend((0..rng.at_most(16)).map(|_| rng.next_u8())),
        _ => {}
    }
    bytes
}

/// A well-formed device descriptor with random identity fields.
fn device_descriptor(rng: &mut Prng) -> [u8; 18] {
    let mut bytes = [0u8; 18];
    rng.fill(&mut bytes);
    bytes[0] = 18;
    bytes[1] = 0x01;
    bytes[17] = bytes[17].max(1);
    bytes
}

/// A device descriptor decodes exactly when its length, type, and
/// configuration count are valid, every field from its own offset.
fn check_device_descriptor(bytes: &[u8; 18]) {
    let valid = bytes[0] >= 18 && bytes[1] == 0x01 && bytes[17] != 0;
    let le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    match DeviceDescriptor::decode(bytes) {
        Ok(descriptor) => {
            assert!(valid, "accepted {bytes:02x?}");
            assert_eq!(
                (
                    descriptor.vendor_id,
                    descriptor.product_id,
                    descriptor.device_release
                ),
                (le(8), le(10), le(12))
            );
            assert_eq!(
                (
                    descriptor.device_class,
                    descriptor.device_subclass,
                    descriptor.device_protocol
                ),
                (bytes[4], bytes[5], bytes[6])
            );
            assert_eq!(
                (
                    descriptor.serial_number_index,
                    descriptor.num_configurations
                ),
                (bytes[16], bytes[17])
            );
            assert_eq!(descriptor.is_hub(), bytes[4] == 0x09);
        }
        Err(err) => {
            assert!(!valid, "refused {bytes:02x?}");
            assert_eq!(err, DriverError::BadMagic);
        }
    }
}

/// An interface descriptor.
const fn interface(number: u8, alternate: u8, class: [u8; 3], endpoints: u8) -> [u8; 9] {
    [
        9, 0x04, number, alternate, endpoints, class[0], class[1], class[2], 0,
    ]
}

/// An endpoint descriptor.
const fn endpoint(address: u8, attributes: u8, max_packet: u16, interval: u8) -> [u8; 7] {
    let [low, high] = max_packet.to_le_bytes();
    [7, 0x05, address, attributes, low, high, interval]
}

/// A `SuperSpeed` endpoint companion bursting `burst` more packets, moving
/// `bytes` an interval.
const fn companion(burst: u8, bytes: u16) -> [u8; 6] {
    let [low, high] = bytes.to_le_bytes();
    [6, 0x30, burst, 0, low, high]
}

/// A HID class descriptor declaring a `report_len`-byte Report Descriptor.
const fn hid(report_len: u16) -> [u8; 9] {
    let [low, high] = report_len.to_le_bytes();
    [9, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, low, high]
}

/// A configuration descriptor chain: its header, `wTotalLength` filled in,
/// then `body`.
fn configuration(value: u8, body: &[&[u8]]) -> Vec<u8> {
    let mut chain = vec![9u8, 0x02, 0, 0, 1, value, 0, 0x80, 50];
    for descriptor in body {
        chain.extend_from_slice(descriptor);
    }
    let total = u16::try_from(chain.len()).unwrap_or(u16::MAX);
    chain[2..4].copy_from_slice(&total.to_le_bytes());
    chain
}

/// A `SuperSpeed` touch pad's interface: a long report descriptor and an
/// endpoint bursting three packets an interval.
fn superspeed_touchpad() -> Vec<u8> {
    configuration(
        1,
        &[
            &interface(0, 0, [0x03, 0x00, 0x00], 1),
            &hid(700),
            &endpoint(0x81, 0x03, 1024 | 2 << 11, 4),
            &companion(2, 3072),
        ],
    )
}

/// A `SuperSpeed` storage interface whose bulk pipes each burst what the
/// companion after them states.
fn superspeed_storage() -> Vec<u8> {
    configuration(
        1,
        &[
            &interface(0, 0, [0x08, 0x06, 0x50], 2),
            &endpoint(0x81, 0x02, 1024, 0),
            &companion(15, 0),
            &endpoint(0x02, 0x02, 1024, 0),
            &companion(3, 0),
        ],
    )
}

/// The configurations the engine serves, and forged ones it must refuse
/// parts of.
fn configuration_seeds() -> Vec<Vec<u8>> {
    let (keyboard, mouse) = ([0x03, 0x01, 0x01], [0x03, 0x01, 0x02]);
    vec![
        superspeed_touchpad(),
        superspeed_storage(),
        configuration(
            1,
            &[
                &interface(0, 0, keyboard, 1),
                &hid(63),
                &endpoint(0x81, 0x03, 8, 10),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, mouse, 1),
                &hid(50),
                &endpoint(0x81, 0x03, 4, 4),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, [0x08, 0x06, 0x50], 2),
                &endpoint(0x83, 0x02, 512, 0),
                &endpoint(0x04, 0x02, 512, 0),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, [0x08, 0x06, 0x62], 4),
                &endpoint(0x01, 0x02, 512, 0),
                &endpoint(0x82, 0x02, 512, 0),
                &endpoint(0x83, 0x02, 512, 0),
                &endpoint(0x04, 0x02, 512, 0),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, [0x08, 0x04, 0x00], 3),
                &endpoint(0x81, 0x02, 64, 0),
                &endpoint(0x02, 0x02, 64, 0),
                &endpoint(0x83, 0x03, 2, 32),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, keyboard, 1),
                &hid(63),
                &endpoint(0x81, 0x03, 8, 10),
                &interface(1, 0, mouse, 1),
                &hid(64),
                &endpoint(0x82, 0x03, 8, 10),
                &interface(1, 1, mouse, 1),
                &endpoint(0x83, 0x03, 8, 10),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, [0x09, 0x00, 0x00], 1),
                &endpoint(0x81, 0x03, 1, 12),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, [0x08, 0x06, 0x50], 3),
                &endpoint(0x80, 0x02, 512, 0),
                &endpoint(0x81, 0x02, 512, 0),
                &endpoint(0x02, 0x02, 512, 0),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, keyboard, 1),
                &endpoint(0x81, 0x03, 8, 10),
                &interface(1, 0, mouse, 2),
                &endpoint(0x81, 0x03, 8, 10),
                &endpoint(0x82, 0x03, 8, 10),
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, keyboard, 1),
                &endpoint(0x81, 0x03, 8, 10),
                &interface(0, 0, mouse, 1),
                &endpoint(0x82, 0x03, 8, 10),
                &interface(1, 0, mouse, 1),
                &endpoint(0x82, 0x03, 8, 10),
            ],
        ),
    ]
}

/// A configuration chain of random descriptors, lengths and all.
fn random_configuration(rng: &mut Prng) -> Vec<u8> {
    let mut body: Vec<Vec<u8>> = Vec::new();
    for _ in 0..rng.at_most(12) {
        let descriptor = match rng.below(6) {
            0 | 1 => interface(
                rng.next_u8() % 4,
                u8::from(rng.below(4) == 0),
                *rng.pick(&CLASSES),
                rng.next_u8() % 5,
            )
            .to_vec(),
            2 | 3 => endpoint(
                *rng.pick(&ADDRESSES),
                rng.next_u8() % 4,
                *rng.pick(&[0, 1, 8, 64, 512, 0x07FF, 0xFFFF]),
                rng.next_u8(),
            )
            .to_vec(),
            4 => hid(rng.next_u16()).to_vec(),
            5 if rng.below(2) == 0 => companion(rng.next_u8(), rng.next_u16()).to_vec(),
            _ => {
                let mut junk = vec![0u8; 2 + rng.at_most(14)];
                rng.fill(&mut junk);
                junk[0] = u8::try_from(junk.len()).unwrap_or(u8::MAX);
                junk
            }
        };
        body.push(descriptor);
    }
    let body: Vec<&[u8]> = body.iter().map(Vec::as_slice).collect();
    configuration(rng.next_u8(), &body)
}

/// What a configuration's descriptor chain holds, walked the naive way.
#[derive(Default)]
struct Chain {
    /// The first default-setting interface descriptor of each interface
    /// number, in order: its number and class triple.
    interfaces: Vec<(u8, u32)>,
    /// Every endpoint descriptor.
    endpoints: Vec<Endpoint>,
}

/// One endpoint descriptor of a chain.
struct Endpoint {
    address: u8,
    /// The transfer type.
    kind: u8,
    /// `wMaxPacketSize` bits 0:10.
    max_packet: u16,
    interval: u8,
    /// The `bMaxBurst` and `wBytesPerInterval` of a companion immediately
    /// following it.
    companion: Option<(u8, u16)>,
}

/// What a decoded endpoint says of the companion that followed its
/// descriptor.
#[derive(Clone, Copy)]
enum Follower {
    /// An interrupt endpoint keeps the whole companion.
    Interrupt(Option<(u8, u16)>),
    /// A bulk endpoint keeps its burst, `0` without one.
    Bulk(u8),
}

impl Follower {
    fn matches(self, companion: Option<(u8, u16)>) -> bool {
        match self {
            Self::Interrupt(stated) => stated == companion,
            Self::Bulk(burst) => burst == companion.map_or(0, |(burst, _)| burst),
        }
    }
}

impl Chain {
    fn walk(buf: &[u8]) -> Self {
        let mut chain = Self::default();
        let mut offset = usize::from(buf[0]);
        let mut after_endpoint = false;
        while let Some(rest) = buf.get(offset..).filter(|rest| rest.len() >= 2) {
            let len = usize::from(rest[0]);
            let Some(descriptor) = rest.get(..len).filter(|_| len >= 2) else {
                break;
            };
            let follows_endpoint = core::mem::take(&mut after_endpoint);
            match descriptor[1] {
                0x04 if len >= 9
                    && descriptor[3] == 0
                    && !chain
                        .interfaces
                        .iter()
                        .any(|&(number, _)| number == descriptor[2]) =>
                {
                    chain.interfaces.push((
                        descriptor[2],
                        u32::from_be_bytes([0, descriptor[5], descriptor[6], descriptor[7]]),
                    ));
                }
                0x05 if len >= 7 => {
                    chain.endpoints.push(Endpoint {
                        address: descriptor[2],
                        kind: descriptor[3] & 0x03,
                        max_packet: u16::from_le_bytes([descriptor[4], descriptor[5]]) & 0x07FF,
                        interval: descriptor[6],
                        companion: None,
                    });
                    after_endpoint = true;
                }
                0x30 if len >= 6 && follows_endpoint => {
                    if let Some(endpoint) = chain.endpoints.last_mut() {
                        endpoint.companion = Some((
                            descriptor[2],
                            u16::from_le_bytes([descriptor[4], descriptor[5]]),
                        ));
                    }
                }
                _ => {}
            }
            offset += len;
        }
        chain
    }

    /// Whether an endpoint descriptor of the chain is the one a decoded
    /// endpoint at `dci` of transfer type `kind` was read from, with the
    /// companion that followed it.
    fn has_endpoint(
        &self,
        dci: u8,
        kind: u8,
        max_packet: u16,
        interval: Option<u8>,
        follower: Follower,
    ) -> bool {
        self.endpoints.iter().any(|endpoint| {
            endpoint.address & 0x0F == dci >> 1
                && (endpoint.address & 0x80 != 0) == (dci % 2 == 1)
                && endpoint.kind == kind
                && endpoint.max_packet == max_packet
                && interval.is_none_or(|interval| interval == endpoint.interval)
                && follower.matches(endpoint.companion)
        })
    }
}

/// Every decoded interface is a default-setting interface of the chain, in
/// order, and every endpoint it carries an endpoint descriptor there with a
/// context of its own.
fn check_configuration(buf: &[u8]) {
    let Ok(decoded) = InterfaceInfo::decode_all(buf) else {
        return;
    };
    let served: Vec<InterfaceInfo> = decoded.iter().map_while(|entry| *entry).collect();
    assert!(!served.is_empty(), "accepted with nothing decoded");
    assert!(
        decoded[served.len()..].iter().all(Option::is_none),
        "a gap in {decoded:?}"
    );
    let chain = Chain::walk(buf);
    let mut interfaces = chain.interfaces.iter();
    let mut contexts = 0u32;
    for iface in &served {
        assert_eq!(iface.configuration_value, buf[5]);
        assert!(
            interfaces.any(|&found| found == (iface.interface_number, iface.class24)),
            "{iface:?} is not the first default setting of its number in {buf:02x?}, in order"
        );
        if iface.int_dci == DCI_CONTROL {
            assert_eq!(iface.int_shape, PeriodicShape::default());
            assert_eq!(iface.int_b_interval, 0);
        } else {
            assert_ne!(
                iface.int_shape.max_packet, 0,
                "a periodic endpoint moving nothing"
            );
            assert!(iface.int_shape.transactions <= 3);
            // A transfer is armed to at least one interval's payload, so no
            // speed may let an interval outrun the buffer it lands in.
            for speed in 0..=u8::MAX {
                let (_, payload) = iface.int_shape.payload(speed);
                assert!(
                    usize::try_from(payload).is_ok_and(|bytes| bytes <= INT_TRANSFER_MAX),
                    "{iface:?} moves {payload} bytes an interval at speed {speed}"
                );
            }
        }
        assert!(iface.bulk_in2.dci == 0 || iface.bulk_in.dci != 0);
        assert!(iface.bulk_out2.dci == 0 || iface.bulk_out.dci != 0);
        let interrupt = (iface.int_dci != DCI_CONTROL).then_some((
            iface.int_dci,
            0x03,
            iface.int_shape.max_packet,
            Some(iface.int_b_interval),
            true,
            Follower::Interrupt(iface.int_shape.companion),
        ));
        let bulk = [
            (iface.bulk_in, true),
            (iface.bulk_in2, true),
            (iface.bulk_out, false),
            (iface.bulk_out2, false),
        ]
        .into_iter()
        .filter(|&(pipe, _)| pipe.dci != 0)
        .map(|(pipe, is_in)| {
            (
                pipe.dci,
                0x02,
                pipe.max_packet,
                None,
                is_in,
                Follower::Bulk(pipe.max_burst),
            )
        });
        for (dci, kind, max_packet, interval, is_in, follower) in interrupt.into_iter().chain(bulk)
        {
            assert!(
                (2..=31).contains(&dci) && (dci % 2 == 1) == is_in,
                "{iface:?}: DCI {dci} names no device endpoint of its direction"
            );
            assert!(
                chain.has_endpoint(dci, kind, max_packet, interval, follower),
                "{iface:?}: DCI {dci} was read from no endpoint descriptor of {buf:02x?}"
            );
            assert_eq!(
                contexts & (1 << dci),
                0,
                "two endpoints on context {dci} in {buf:02x?}"
            );
            contexts |= 1 << dci;
        }
    }
}

/// The hub descriptors the engine is served: 4- and 7-port USB 2.0 hubs, one
/// with every TT Think Time bit set, a `SuperSpeed` hub, and the
/// configuration-shaped garble a Realtek RTS5411 answered with.
const HUB_SEEDS: [&[u8]; 4] = [
    &[9, 0x29, 4, 0x00, 0x00, 0x32, 0x00, 0xFF],
    &[9, 0x29, 7, 0xE0, 0x00, 0x32, 0x64, 0x00, 0xFF],
    &[12, 0x2A, 4, 0x09, 0x00, 0x32, 0x00, 0xFF, 0, 0, 0, 0],
    &[0x09, 0x02, 0x29, 0x00, 0x01, 0x01, 0x00, 0xA0, 0x32],
];

/// A hub descriptor decodes exactly when it is of the type a hub of its
/// speed serves and reaches `wHubCharacteristics`; its ports are `bNbrPorts`,
/// and its think time bits 5:6 of the characteristics, behind a USB 2.0 hub
/// only.
fn check_hub_descriptor(answer: &[u8], superspeed: bool) {
    let served = if superspeed { 0x2A } else { 0x29 };
    let expected = match *answer {
        [_, kind, ports, low, high, ..] if kind == served => Some(HubDescriptor {
            ports,
            tt_think_time: if superspeed {
                0
            } else {
                u8::try_from((u16::from_le_bytes([low, high]) >> 5) & 0b11).unwrap_or(u8::MAX)
            },
        }),
        _ => None,
    };
    assert_eq!(
        HubDescriptor::decode(answer, superspeed),
        expected,
        "{answer:02x?} from a hub (SuperSpeed: {superspeed})"
    );
}

/// A well-formed string descriptor carrying `units`.
fn string_descriptor(units: &[u16]) -> Vec<u8> {
    let mut descriptor = vec![0, STRING];
    for unit in units {
        descriptor.extend_from_slice(&unit.to_le_bytes());
    }
    descriptor[0] = u8::try_from(descriptor.len()).unwrap_or(u8::MAX);
    descriptor
}

/// Up to [`MAX_SERIAL_UNITS`] code units: printable, arbitrary, or
/// surrogates, which must be kept as delivered.
fn random_units(rng: &mut Prng) -> Vec<u16> {
    let count = rng.at_most(MAX_SERIAL_UNITS);
    (0..count)
        .map(|_| match rng.below(3) {
            0 => u16::from(b'0' + rng.next_u8() % 43),
            1 => 0xD800 | (rng.next_u16() & 0x07FF),
            _ => rng.next_u16(),
        })
        .collect()
}

/// A header decodes exactly when it is two bytes naming an even string
/// descriptor length at least its own.
fn check_header(answer: &[u8]) {
    let expected = match *answer {
        [len, STRING] if len >= 2 && len.is_multiple_of(2) => Some(usize::from(len)),
        _ => None,
    };
    assert_eq!(
        StringHeader::decode(answer).map(StringHeader::descriptor_len),
        expected,
        "header {answer:02x?}"
    );
}

/// What follows the header is accepted exactly when the whole answer is the
/// descriptor that header claimed.
fn check_payload(header: StringHeader, answer: &[u8]) {
    let len = header.descriptor_len();
    let expected = match answer {
        [claimed, STRING, rest @ ..] if usize::from(*claimed) == len && answer.len() == len => {
            Some(rest)
        }
        _ => None,
    };
    assert_eq!(
        header.payload(answer),
        expected,
        "{answer:02x?} after a {len}-byte header"
    );
}

/// A LANGID table's first entry is read exactly when the table is whole
/// LANGIDs and lists one.
fn check_langid(table: &[u8]) {
    let expected = match table {
        [low, high, ..] if table.len().is_multiple_of(2) => Some(u16::from_le_bytes([*low, *high])),
        _ => None,
    };
    assert_eq!(first_langid(table), expected, "LANGID table {table:02x?}");
}

/// A serial is 1..=126 code units, exactly the ones delivered.
fn check_serial(payload: &[u8]) {
    let (pairs, rest) = payload.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    let whole = rest.is_empty() && (1..=MAX_SERIAL_UNITS).contains(&units.len());
    match SerialNumber::decode(payload) {
        Some(serial) => {
            assert!(whole, "accepted {payload:02x?}");
            assert_eq!(Some(serial), SerialNumber::new(&units));
        }
        None => assert!(!whole, "refused {payload:02x?}"),
    }
}

/// Serials are built from 1..=126 code units, and two are equal exactly when
/// their units are.
fn check_serial_equality(units: &[u16], other: &[u16]) {
    let valid = |units: &[u16]| (1..=MAX_SERIAL_UNITS).contains(&units.len());
    assert_eq!(SerialNumber::new(units).is_some(), valid(units));
    if let (Some(serial), Some(other_serial)) = (SerialNumber::new(units), SerialNumber::new(other))
    {
        assert_eq!(
            serial == other_serial,
            units == other,
            "{units:04x?} vs {other:04x?}"
        );
    }
}

/// `units` altered the ways two serials most nearly collide.
fn near_copy(units: &[u16], rng: &mut Prng) -> Vec<u16> {
    let mut other = units.to_vec();
    match rng.below(4) {
        0 => other.push(0),
        1 => {
            other.pop();
        }
        2 if !other.is_empty() => {
            let at = rng.below(other.len());
            other[at] ^= 1 << rng.below(16);
        }
        _ => {}
    }
    other
}

/// Feed a device's answer to the header request, `first`, and to the
/// whole-descriptor request, `second`, through every string decoder.
fn exercise_strings(first: &[u8], second: &[u8]) {
    check_header(first);
    if let Some(header) = StringHeader::decode(first) {
        check_payload(header, second);
        if let Some(payload) = header.payload(second) {
            check_langid(payload);
            check_serial(payload);
        }
    }
    check_langid(second);
    check_serial(second);
}

#[test]
fn device_descriptor_decode_is_exact_for_any_input() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "device_descriptor_decode_is_exact_for_any_input",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let mut bytes = device_descriptor(&mut rng);
        check_device_descriptor(&bytes);

        // The accept/refuse boundary of each checked field.
        bytes[0] = *rng.pick(&[0, 17, 18, 19, 255]);
        bytes[1] = *rng.pick(&[0x00, 0x01, 0x02]);
        bytes[17] = *rng.pick(&[0, 1, 255]);
        check_device_descriptor(&bytes);

        for _ in 0..rng.at_most(4) {
            let at = rng.below(bytes.len());
            bytes[at] ^= rng.next_u8();
        }
        check_device_descriptor(&bytes);

        rng.fill(&mut bytes);
        check_device_descriptor(&bytes);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

#[test]
fn configuration_decode_keeps_every_endpoint_to_a_context_of_its_own() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "configuration_decode_keeps_every_endpoint_to_a_context_of_its_own",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let seeds = configuration_seeds();
    for seed in &seeds {
        assert!(
            InterfaceInfo::decode_all(seed).is_ok(),
            "seed {seed:02x?} reaches no interface"
        );
        check_configuration(seed);
    }
    let mut iteration: u64 = 0;
    loop {
        check_configuration(&mutate(rng.pick(&seeds), &mut rng));

        let random = random_configuration(&mut rng);
        check_configuration(&random);
        check_configuration(&mutate(&random, &mut rng));

        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        if noise.len() >= 9 && rng.below(2) == 0 {
            noise[..2].copy_from_slice(&[9, 0x02]);
        }
        check_configuration(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

#[test]
fn hub_descriptor_decode_is_exact_for_any_input() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hub_descriptor_decode_is_exact_for_any_input",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    for seed in HUB_SEEDS {
        check_hub_descriptor(seed, false);
        check_hub_descriptor(seed, true);
    }
    let mut iteration: u64 = 0;
    loop {
        let superspeed = rng.below(2) == 0;
        check_hub_descriptor(&mutate(rng.pick(&HUB_SEEDS), &mut rng), superspeed);

        // The type of either speed's descriptor over random fields.
        let mut answer = vec![0u8; rng.at_most(16)];
        rng.fill(&mut answer);
        if let Some(kind) = answer.get_mut(1) {
            *kind = *rng.pick(&[0x29, 0x2A, 0x02]);
        }
        check_hub_descriptor(&answer, superspeed);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

#[test]
fn string_decoders_accept_only_whole_well_formed_descriptors() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "string_decoders_accept_only_whole_well_formed_descriptors",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        // A device answering both requests with one well-formed descriptor:
        // a LANGID table or a serial, empty up to the longest.
        let units = random_units(&mut rng);
        let descriptor = string_descriptor(&units);
        exercise_strings(&descriptor[..2], &descriptor);

        // One that changes its answer between the requests, or damages it.
        let first = if rng.below(4) == 0 {
            mutate(&descriptor[..2], &mut rng)
        } else {
            descriptor[..2].to_vec()
        };
        exercise_strings(&first, &mutate(&descriptor, &mut rng));

        // Noise for both answers.
        let mut first = vec![0u8; rng.at_most(3)];
        rng.fill(&mut first);
        let mut second = vec![0u8; rng.at_most(300)];
        rng.fill(&mut second);
        exercise_strings(&first, &second);

        check_serial_equality(&units, &near_copy(&units, &mut rng));
        let mut long = random_units(&mut rng);
        long.extend((0..rng.at_most(4)).map(|_| rng.next_u16()));
        check_serial_equality(&long, &near_copy(&long, &mut rng));

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

/// Every descriptor of a configuration chain after its header, as the walk
/// reads it, or `None` when the chain is malformed anywhere.
fn walk(buf: &[u8]) -> Option<Vec<&[u8]>> {
    if buf.len() < 9
        || buf[0] < 9
        || buf[1] != 0x02
        || u16::from_le_bytes([buf[2], buf[3]]) < u16::from(buf[0])
    {
        return None;
    }
    let mut rest = buf.get(usize::from(buf[0])..)?;
    let mut out = Vec::new();
    while rest.len() >= 2 {
        let length = usize::from(rest[0]);
        if length < 2 || length > rest.len() {
            return None;
        }
        let (descriptor, after) = rest.split_at(length);
        if descriptor[1] == 0x04 && length < 9 {
            return None;
        }
        out.push(descriptor);
        rest = after;
    }
    Some(out)
}

/// One `(interface, alternate)` setting the chain states: the endpoint
/// addresses that follow its interface descriptor, in order, and whether an
/// endpoint descriptor among them is too short to be one.
struct Stated {
    at: (u8, u8),
    endpoints: Vec<u8>,
    short: bool,
}

fn settings(chain: &[&[u8]]) -> Vec<Stated> {
    let mut out: Vec<Stated> = Vec::new();
    for descriptor in chain {
        match descriptor[1] {
            0x04 => out.push(Stated {
                at: (descriptor[2], descriptor[3]),
                endpoints: Vec::new(),
                short: false,
            }),
            0x05 => {
                if let Some(setting) = out.last_mut() {
                    if descriptor.len() < 7 {
                        setting.short = true;
                    } else {
                        setting.endpoints.push(descriptor[2]);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The alternate-setting readers say only what `buf`'s chain states.
fn check_alternates(buf: &[u8]) {
    let chain = walk(buf);
    let numbers = interface_numbers(buf);
    match &chain {
        None => assert_eq!(numbers, Err(DriverError::BadMagic)),
        Some(chain) => {
            let numbers = numbers.expect("a well-formed chain");
            for number in 0..=u8::MAX {
                let declared = chain
                    .iter()
                    .any(|descriptor| descriptor[1] == 0x04 && descriptor[2] == number);
                assert_eq!(numbers.contains(u16::from(number)), declared);
            }
        }
    }
    let stated = chain.as_deref().map(settings).unwrap_or_default();
    for interface in 0..4u8 {
        let control_only = is_control_only(buf, interface);
        let own: Vec<_> = stated
            .iter()
            .filter(|setting| setting.at.0 == interface)
            .collect();
        if chain.is_none() || own.iter().any(|setting| setting.short) {
            assert_eq!(control_only, Err(DriverError::BadMagic));
        } else {
            assert_eq!(
                control_only,
                Ok(own.len() == 1 && own[0].endpoints.is_empty()),
                "interface {interface} of {buf:02x?}"
            );
        }
        for alternate in 0..3u8 {
            let decoded = alternate_setting(buf, interface, alternate);
            let found: Vec<_> = stated
                .iter()
                .filter(|setting| setting.at == (interface, alternate))
                .collect();
            match decoded {
                Ok(setting) => {
                    assert_eq!(
                        (setting.interface, setting.alternate),
                        (interface, alternate)
                    );
                    assert_eq!(found.len(), 1, "one setting decoded from {buf:02x?}");
                    assert!(!found[0].short);
                    let addresses: Vec<u8> = setting
                        .endpoints()
                        .map(|endpoint| endpoint.address)
                        .collect();
                    assert_eq!(addresses, found[0].endpoints, "{buf:02x?}");
                    assert!(addresses.len() <= MAX_ALT_ENDPOINTS);
                    let mut seen = 0u32;
                    for endpoint in setting.endpoints() {
                        assert!(endpoint.number() != 0 && endpoint.address & 0x70 == 0);
                        assert_eq!(
                            seen & (1 << endpoint.dci()),
                            0,
                            "two endpoints, one context"
                        );
                        seen |= 1 << endpoint.dci();
                    }
                    assert_eq!(setting.dci_mask(), seen);
                }
                Err(DriverError::NotFound) => assert!(chain.is_some() && found.is_empty()),
                Err(err) => assert_eq!(err, DriverError::BadMagic),
            }
        }
    }
}

/// Configurations with alternate settings: a USB Audio function — a control
/// interface, then a streaming interface whose settings bring an asynchronous
/// data endpoint and its feedback endpoint, then a larger one — and a
/// `SuperSpeed` streaming interface whose endpoint a companion completes.
fn alternate_seeds() -> Vec<Vec<u8>> {
    let (control, streaming) = ([0x01, 0x01, 0x00], [0x01, 0x02, 0x00]);
    vec![
        configuration(
            1,
            &[
                &interface(0, 0, control, 0),
                &[9, 0x24, 0x01, 0x00, 0x01, 30, 0, 1, 1],
                &interface(1, 0, streaming, 0),
                &interface(1, 1, streaming, 2),
                &[9, 0x05, 0x01, 0x05, 200, 0, 4, 0, 0x81],
                &[9, 0x05, 0x81, 0x11, 4, 0, 4, 0, 0],
                &interface(1, 2, streaming, 1),
                &[9, 0x05, 0x01, 0x09, 0x90, 0x01, 4, 0, 0],
            ],
        ),
        configuration(
            1,
            &[
                &interface(0, 0, streaming, 0),
                &interface(0, 1, streaming, 1),
                &endpoint(0x81, 0x05, 1024, 1),
                &[6, 0x30, 3, 1, 0x00, 0x20],
            ],
        ),
    ]
}

/// Every isochronous budget an endpoint descriptor yields at every speed is
/// one a bus interval can move, and a report a feedback decoder reads is
/// within the window it was built for.
fn check_periodic(descriptor: &[u8]) {
    let Ok(endpoint) = EndpointDescriptor::decode(descriptor) else {
        return;
    };
    assert!(endpoint.number() != 0 && endpoint.address & 0x70 == 0);
    for speed in [
        UsbSpeed::Low,
        UsbSpeed::Full,
        UsbSpeed::High,
        UsbSpeed::Super,
    ] {
        if let Ok(budget) = PeriodicBudget::isochronous(&endpoint, speed) {
            assert!(budget.max_esit_payload != 0);
            assert!(budget.max_esit_payload <= ISO_MAX_PACKET_BYTES);
            let (tbc, tlbpc) = budget.burst_counts(budget.max_esit_payload);
            assert!(tbc <= 3 && tlbpc <= 15);
        }
        let mut decoder = FeedbackDecoder::new(speed, 48_000);
        if let Some(rate) = decoder.decode(descriptor) {
            let nominal = decoder.nominal().per_frame_q16();
            assert!(rate.per_frame_q16().abs_diff(nominal) <= nominal / 8);
        }
    }
}

#[test]
fn alternate_settings_and_periodic_endpoints_decode_only_what_the_chain_states() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "alternate_settings_and_periodic_endpoints_decode_only_what_the_chain_states",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let seeds = alternate_seeds();
    for seed in &seeds {
        assert!(alternate_setting(seed, 0, 0).is_ok() || alternate_setting(seed, 0, 1).is_ok());
        check_alternates(seed);
    }
    let mut iteration: u64 = 0;
    loop {
        check_alternates(&mutate(rng.pick(&seeds), &mut rng));
        let random = random_configuration(&mut rng);
        check_alternates(&random);
        check_alternates(&mutate(&random, &mut rng));

        let mut noise = vec![0u8; rng.at_most(16)];
        rng.fill(&mut noise);
        if noise.len() >= 2 && rng.below(2) == 0 {
            noise[1] = 0x05;
            noise[0] = u8::try_from(noise.len()).unwrap_or(u8::MAX);
        }
        check_periodic(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
