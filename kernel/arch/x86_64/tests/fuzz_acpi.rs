//! Fuzz harness for the DMA-remapping tables the x86_64 port reads from
//! firmware: the DMAR, the IVRS and the VIOT.
//!
//! Firmware writes these bytes, so for any input at all each parser refuses
//! cleanly or yields a table every query then answers without panicking.
//! Tables are drawn with the right signature, a valid checksum most of the
//! time and node lists whose lengths and cross-references are biased towards
//! the boundaries, so the walk gets past the header into the node checks, and
//! queries land inside what the tables describe.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use core::hint::black_box;

use tairix_abi::HwNode;
use tairix_arch_api::{DiscoveryError, HwNodeSink};
use tairix_arch_x86_64::acpi::UnitNodes;
use tairix_arch_x86_64::dmar::{BridgeBuses, Dmar, SourceId, DMAR_SIGNATURE};
use tairix_arch_x86_64::ivrs::{DeviceEntry, Ivrs, IVRS_SIGNATURE};
use tairix_arch_x86_64::viot::{self, Viot, VIOT_SIGNATURE};
use tairix_fuzzseed::Prng;

const SMOKE_ITERATIONS: u64 = 20_000;

/// The SDT header and each table's own fixed fields, which all three end at.
const HEADER_LEN: usize = 48;
const MAX_NODES: usize = 8;
const FIRST_ID: u32 = 0x800A_0000;
const MMIO_WINDOW: u64 = 0x200;

/// A node length biased to the sizes the parsers distinguish.
fn node_len(rng: &mut Prng) -> usize {
    match rng.below(6) {
        0 => rng.below(8),
        1 => 12,
        2 => 16,
        3 => 24,
        4 => 32,
        _ => 4 + rng.below(60),
    }
}

/// A value that is right one draw in `odds`, else `wrong`.
fn mostly<T>(rng: &mut Prng, odds: usize, right: T, wrong: impl FnOnce(&mut Prng) -> T) -> T {
    if rng.below(odds) == 0 {
        wrong(rng)
    } else {
        right
    }
}

/// A page-aligned, non-zero address most of the time.
fn page(rng: &mut Prng) -> u64 {
    let aligned = (u64::from(rng.next_u32()) + 1) << 12;
    mostly(rng, 16, aligned, Prng::next_u64)
}

/// Where a record states its length: the offset, and the bytes it takes.
#[derive(Copy, Clone)]
struct Length(usize, usize);

/// A structure's length, at offset 2 as two bytes.
const STRUCTURE: Length = Length(2, 2);

/// A DMAR device scope's length, at offset 1 as one byte.
const SCOPE: Length = Length(1, 1);

/// Append a record of `fixed` bytes starting with `head`, then what `tail`
/// writes, its length stated where `length` says.
fn record(
    rng: &mut Prng,
    buf: &mut Vec<u8>,
    head: &[u8],
    fixed: usize,
    length: Length,
    tail: impl FnOnce(&mut Prng, &mut Vec<u8>),
) {
    let start = buf.len();
    buf.resize(start + fixed, 0);
    rng.fill(&mut buf[start..]);
    buf[start..start + head.len()].copy_from_slice(head);
    tail(rng, buf);
    let len = buf.len() - start;
    let stated = mostly(rng, 16, len, |rng| rng.below(len + 8));
    let Length(at, width) = length;
    buf[start + at..start + at + width].copy_from_slice(&stated.to_le_bytes()[..width]);
}

/// A DMAR device scope: its kind, length, enumeration id, start bus and path.
fn dmar_scopes(rng: &mut Prng, buf: &mut Vec<u8>) {
    for _ in 0..rng.below(3) {
        let hops = 1 + rng.below(2);
        let pci = 1 + rng.next_u8() % 2;
        let kind = mostly(rng, 4, pci, |rng| 1 + rng.next_u8() % 5);
        record(rng, buf, &[kind], 6 + 2 * hops, SCOPE, |rng, buf| {
            let path = buf.len() - 2 * hops;
            for [device, function] in buf[path..].as_chunks_mut::<2>().0 {
                if rng.below(8) != 0 {
                    *device %= 32;
                    *function %= 8;
                }
            }
        });
    }
}

/// What a drawn VIOT PCI range numbers, so a query can land inside it.
#[derive(Copy, Clone)]
struct Span {
    segment: u16,
    requesters: (u16, u16),
}

/// Write a PCI range's numbering at `at`: one segment, a short requester run
/// and a low first endpoint most of the time. Returns what it numbers.
fn pci_range_numbering(rng: &mut Prng, buf: &mut [u8], at: usize) -> Span {
    let low = rng.next_u16() % 2;
    let segment = mostly(rng, 8, low, Prng::next_u16);
    let first = rng.next_u16() % 0x200;
    let last = first.saturating_add(rng.next_u16() % 0x40);
    let small = u32::from(rng.next_u16() % 0x400);
    let endpoint = mostly(rng, 8, small, Prng::next_u32);
    buf[at + 4..at + 8].copy_from_slice(&endpoint.to_le_bytes());
    buf[at + 8..at + 10].copy_from_slice(&segment.to_le_bytes());
    buf[at + 10..at + 12].copy_from_slice(&segment.to_le_bytes());
    buf[at + 12..at + 14].copy_from_slice(&first.to_le_bytes());
    buf[at + 14..at + 16].copy_from_slice(&last.to_le_bytes());
    Span {
        segment,
        requesters: (first, last),
    }
}

/// An IVHD's device entries, each as long as its type says.
fn ivhd_entries(rng: &mut Prng, buf: &mut Vec<u8>) {
    const KINDS: [u8; 10] = [0x01, 0x02, 0x03, 0x04, 0x42, 0x43, 0x46, 0x47, 0x48, 0xF0];
    for _ in 0..rng.below(5) {
        let known = KINDS[rng.below(KINDS.len())];
        let kind = mostly(rng, 8, known, Prng::next_u8);
        let start = buf.len();
        let len = if kind == 0xF0 {
            let uid = rng.below(4);
            22 + uid
        } else {
            4 << (kind >> 6)
        };
        buf.resize(start + len, 0);
        rng.fill(&mut buf[start..]);
        buf[start] = kind;
        if kind == 0xF0 {
            buf[start + 21] = u8::try_from(len - 22).unwrap_or(0);
        }
    }
}

/// A DMAR remapping unit, reserved region or structure of any other type.
fn dmar_node(rng: &mut Prng, buf: &mut Vec<u8>) {
    match rng.below(3) {
        0 => record(rng, buf, &[0, 0], 16, STRUCTURE, |rng, buf| {
            let at = buf.len() - 8;
            let base = page(rng);
            buf[at..].copy_from_slice(&base.to_le_bytes());
            dmar_scopes(rng, buf);
        }),
        1 => record(rng, buf, &[1, 0], 24, STRUCTURE, |rng, buf| {
            let at = buf.len() - 16;
            let base = page(rng);
            let pages = 1 + u64::from(rng.next_u8());
            let end = base.wrapping_add(pages << 12).wrapping_sub(1);
            let limit = mostly(rng, 16, end, Prng::next_u64);
            buf[at..at + 8].copy_from_slice(&base.to_le_bytes());
            buf[at + 8..].copy_from_slice(&limit.to_le_bytes());
            dmar_scopes(rng, buf);
        }),
        _ => {
            let (kind, len) = (rng.next_u8() % 8, node_len(rng).max(4));
            record(rng, buf, &[kind, 0], len, STRUCTURE, |_, _| {});
        }
    }
}

/// An IVRS hardware definition, memory definition or block of any other type.
fn ivrs_node(rng: &mut Prng, buf: &mut Vec<u8>) {
    match rng.below(3) {
        0 => {
            let (kind, fixed) = [(0x10, 24), (0x11, 40), (0x40, 40)][rng.below(3)];
            record(rng, buf, &[kind], fixed, STRUCTURE, |rng, buf| {
                let base = page(rng);
                let at = buf.len() - fixed + 8;
                buf[at..at + 8].copy_from_slice(&base.to_le_bytes());
                ivhd_entries(rng, buf);
            });
        }
        1 => {
            let kind = 0x20 + rng.next_u8() % 3;
            record(rng, buf, &[kind], 32, STRUCTURE, |rng, buf| {
                let at = buf.len() - 16;
                let start = page(rng);
                let pages = (1 + u64::from(rng.next_u8())) << 12;
                let len = mostly(rng, 16, pages, Prng::next_u64);
                buf[at..at + 8].copy_from_slice(&start.to_le_bytes());
                buf[at + 8..].copy_from_slice(&len.to_le_bytes());
            });
        }
        _ => {
            let (kind, len) = (rng.next_u8(), node_len(rng).max(4));
            record(rng, buf, &[kind], len, STRUCTURE, |_, _| {});
        }
    }
}

/// `count` VIOT nodes and the header fields that find them, returning the
/// spans its PCI ranges number.
fn viot_nodes(rng: &mut Prng, buf: &mut Vec<u8>, count: usize) -> Vec<Span> {
    let mut nodes = Vec::with_capacity(count);
    let mut spans = Vec::new();
    for _ in 0..count {
        let known = 1 + rng.next_u8() % 4;
        let kind = mostly(rng, 8, known, Prng::next_u8);
        let fixed = mostly(rng, 4, if kind <= 2 { 24 } else { 16 }, node_len);
        let at = buf.len();
        record(rng, buf, &[kind], fixed.max(4), STRUCTURE, |_, _| {});
        if kind == 1 && buf.len() >= at + 16 {
            spans.push(pci_range_numbering(rng, buf, at));
        }
        nodes.push((at, kind));
    }
    let counted = u16::try_from(count).unwrap_or(0);
    let stated = mostly(rng, 8, counted, Prng::next_u16);
    buf[36..38].copy_from_slice(&stated.to_le_bytes());
    let header = u16::try_from(HEADER_LEN).unwrap_or(0);
    let first = mostly(rng, 8, header, Prng::next_u16);
    buf[38..40].copy_from_slice(&first.to_le_bytes());
    let units: Vec<usize> = nodes
        .iter()
        .filter(|&&(_, kind)| kind == 3 || kind == 4)
        .map(|&(at, _)| at)
        .collect();
    // Only an endpoint node names a unit, most of the time a real one.
    for &(start, kind) in &nodes {
        if (kind == 1 || kind == 2) && buf.len() >= start + 18 && !units.is_empty() {
            let unit = units[rng.below(units.len())];
            let named = mostly(rng, 8, u16::try_from(unit).unwrap_or(0), Prng::next_u16);
            buf[start + 16..start + 18].copy_from_slice(&named.to_le_bytes());
        }
    }
    spans
}

/// A table of `signature` whose fixed fields and nodes are mostly well formed,
/// with a valid checksum most of the time, and the spans its VIOT ranges
/// number.
fn draw_table(rng: &mut Prng, signature: [u8; 4], buf: &mut Vec<u8>) -> Vec<Span> {
    buf.clear();
    buf.resize(HEADER_LEN, 0);
    buf[..4].copy_from_slice(&signature);
    rng.fill(&mut buf[8..HEADER_LEN]);
    let count = rng.below(MAX_NODES + 1);
    let spans = match signature {
        DMAR_SIGNATURE => {
            buf[36] = mostly(rng, 16, 47, Prng::next_u8);
            for _ in 0..count {
                dmar_node(rng, buf);
            }
            Vec::new()
        }
        IVRS_SIGNATURE => {
            for _ in 0..count {
                ivrs_node(rng, buf);
            }
            Vec::new()
        }
        _ => viot_nodes(rng, buf, count),
    };
    let whole = u32::try_from(buf.len()).unwrap_or(0);
    let total = mostly(rng, 16, whole, Prng::next_u32);
    buf[4..8].copy_from_slice(&total.to_le_bytes());
    if rng.below(16) != 0 {
        buf[9] = 0;
        let sum = buf.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
        buf[9] = 0u8.wrapping_sub(sum);
    }
    spans
}

/// Holds `room` nodes, then refuses.
struct Room(usize);

impl HwNodeSink for Room {
    fn emit(&mut self, _node: HwNode) -> Result<(), DiscoveryError> {
        self.0 = self
            .0
            .checked_sub(1)
            .ok_or(DiscoveryError::MalformedSource)?;
        Ok(())
    }
}

/// Answers every bridge with an arbitrary bus window.
struct Bridges(u8, u8);

impl BridgeBuses for Bridges {
    fn bus_range(&self, _bridge: SourceId) -> Option<(u8, u8)> {
        Some((self.0, self.1))
    }
}

/// What reading one table reached, so the sweep can show it gets past the
/// header and into the queries.
#[derive(Default)]
struct Reached {
    /// The table parsed with a unit in it.
    unit: bool,
    /// A query landed on a function a unit translates.
    endpoint: bool,
}

fn exercise_viot(rng: &mut Prng, bytes: &[u8], spans: &[Span]) -> Reached {
    let Ok(table) = Viot::parse(bytes) else {
        return Reached::default();
    };
    let units = table.units().count();
    let (segment, requester) = match spans.get(rng.below(spans.len() + 1)) {
        Some(span) => {
            let (first, last) = span.requesters;
            let run = usize::from(last.saturating_sub(first)) + 1;
            let offset = u16::try_from(rng.below(run)).unwrap_or(0);
            (span.segment, first.saturating_add(offset))
        }
        None => (rng.next_u16(), rng.next_u16()),
    };
    black_box(table.covers(segment));
    let mut sink = Room(rng.below(units + 2));
    let placed = viot::emit_unit_nodes(
        &table,
        FIRST_ID,
        b"virtio,pci-iommu",
        b"virtio,mmio",
        MMIO_WINDOW,
        &mut sink,
    )
    .unwrap_or_default();
    assert!(placed.emitted <= units);
    let nodes = UnitNodes {
        emitted: mostly(rng, 4, units, |rng| rng.below(units + 1)),
        ..UnitNodes::default()
    };
    black_box(table.strands(nodes, segment));
    let found = viot::endpoint(&table, FIRST_ID, nodes, segment, requester);
    if let Some((unit, stream)) = found {
        assert!(
            unit >= FIRST_ID && unit - FIRST_ID < u32::try_from(nodes.emitted).unwrap_or(u32::MAX)
        );
        black_box(viot::contested(&table, FIRST_ID, segment, unit, stream));
    }
    black_box(viot::contested(
        &table,
        FIRST_ID,
        segment,
        rng.next_u32(),
        rng.next_u32(),
    ));
    Reached {
        unit: units > 0,
        endpoint: found.is_some(),
    }
}

fn exercise_dmar(rng: &mut Prng, bytes: &[u8]) -> Reached {
    let Ok(table) = Dmar::parse(bytes) else {
        return Reached::default();
    };
    let bridges = Bridges(rng.next_u8(), rng.next_u8());
    let mut named = None;
    for unit in table.units() {
        black_box((unit.segment(), unit.include_pci_all(), unit.register_base()));
        black_box(unit.register_len());
        for scope in unit.scopes() {
            let resolution = scope.resolve(&bridges);
            if let Some(source) = resolution {
                named.get_or_insert((unit.segment(), source));
            }
            black_box((scope.kind(), scope.enumeration_id(), resolution));
        }
    }
    for region in table.reserved_regions() {
        black_box((region.segment(), region.base(), region.len()));
        for scope in region.scopes() {
            black_box(scope.resolve(&bridges));
        }
    }
    let (segment, source) =
        named.unwrap_or_else(|| (rng.next_u16(), SourceId::from_raw(rng.next_u16())));
    let found = table.unit_for(segment, source, &bridges);
    if let Some(index) = found {
        assert!(index < table.units().count());
    }
    Reached {
        unit: table.units().next().is_some(),
        endpoint: found.is_some(),
    }
}

fn exercise_ivrs(rng: &mut Prng, bytes: &[u8]) -> Reached {
    let Ok(table) = Ivrs::parse(bytes) else {
        return Reached::default();
    };
    let drawn = rng.next_u16();
    for unit in table.units() {
        black_box((unit.device(), unit.register_base(), unit.segment()));
        assert!(unit.entries().count() <= bytes.len());
        black_box((unit.covers(drawn), unit.alias_of(drawn)));
        assert!(unit.ioapics().count() <= bytes.len());
    }
    for definition in table.memory_definitions() {
        black_box(definition.names(rng.next_u16(), drawn));
        black_box((definition.base(), definition.len()));
    }
    let named = table.units().find_map(|unit| {
        let device = match unit.entries().next()? {
            DeviceEntry::All => drawn,
            DeviceEntry::Devices(devices) | DeviceEntry::Alias { devices, .. } => *devices.start(),
            DeviceEntry::Special { device, .. } | DeviceEntry::Acpi(device) => device,
        };
        Some((unit.segment(), device))
    });
    let (segment, device) = named.unwrap_or_else(|| (rng.next_u16(), drawn));
    let found = table.unit_for(segment, device);
    if let Some(index) = found {
        assert!(index < table.units().count());
    }
    Reached {
        unit: table.units().next().is_some(),
        endpoint: found.is_some(),
    }
}

#[test]
fn arbitrary_firmware_tables_are_refused_or_read_without_panicking() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "arbitrary_firmware_tables_are_refused_or_read_without_panicking",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut buf = Vec::new();
    let mut units = [0u64; 3];
    let mut endpoints = [0u64; 3];
    let mut tally = |table: usize, reached: Reached| {
        units[table] += u64::from(reached.unit);
        endpoints[table] += u64::from(reached.endpoint);
    };
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let spans = draw_table(&mut rng, VIOT_SIGNATURE, &mut buf);
            tally(0, exercise_viot(&mut rng, &buf, &spans));
            draw_table(&mut rng, DMAR_SIGNATURE, &mut buf);
            tally(1, exercise_dmar(&mut rng, &buf));
            draw_table(&mut rng, IVRS_SIGNATURE, &mut buf);
            tally(2, exercise_ivrs(&mut rng, &buf));
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
    assert!(
        units.iter().all(|&tables| tables > 0),
        "every table type was read with a unit: {units:?}"
    );
    assert!(
        endpoints.iter().all(|&hits| hits > 0),
        "every table type answered a query it covers: {endpoints:?}"
    );
}
