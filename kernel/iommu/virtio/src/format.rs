//! The virtio-iommu device's wire formats (virtio 1.3 §5.13): its feature
//! bits, configuration, requests, probe properties and fault reports.

use core::ops::Range;

use tairix_kernel_iommu_api::{Access, Fault, FaultReason, IommuError, IO_PAGE_SIZE};

/// The device's feature bits.
pub(crate) mod feature {
    pub const INPUT_RANGE: u64 = 1 << 0;
    pub const DOMAIN_RANGE: u64 = 1 << 1;
    pub const MAP_UNMAP: u64 = 1 << 2;
    pub const BYPASS: u64 = 1 << 3;
    pub const PROBE: u64 = 1 << 4;
    /// Offered by the model, to prove a feature the family does not use is
    /// not taken.
    #[cfg(test)]
    pub const MMIO: u64 = 1 << 5;
    pub const BYPASS_CONFIG: u64 = 1 << 6;
}

/// Offsets into the device's configuration.
pub(crate) mod config {
    pub const PAGE_SIZE_MASK: usize = 0;
    pub const INPUT_START: usize = 8;
    pub const INPUT_END: usize = 16;
    pub const DOMAIN_START: usize = 24;
    pub const DOMAIN_END: usize = 28;
    pub const PROBE_SIZE: usize = 32;
    pub const BYPASS: usize = 36;
    /// Bytes of configuration a virtio-iommu has.
    pub const LEN: usize = 40;
}

/// The request queue and the event queue.
pub(crate) const REQUEST_QUEUE: u16 = 0;
pub(crate) const EVENT_QUEUE: u16 = 1;

const T_ATTACH: u8 = 1;
const T_DETACH: u8 = 2;
const T_MAP: u8 = 3;
const T_UNMAP: u8 = 4;
const T_PROBE: u8 = 5;

const MAP_F_READ: u32 = 1 << 0;
const MAP_F_WRITE: u32 = 1 << 1;

/// Bytes of a request's tail: its status and three reserved bytes.
pub(crate) const TAIL_LEN: usize = 4;

/// The device-readable part of a probe request: its head, endpoint and 64
/// reserved bytes.
pub(crate) const PROBE_REQUEST_LEN: usize = 72;

/// The most configuration bytes this family takes a probe to fill: far
/// beyond what a device's properties need, so a device asking for more is
/// misbehaving.
pub(crate) const PROBE_SIZE_MAX: u32 = 1 << 16;

/// Bytes of one fault report, as a descriptor's length names them.
pub(crate) const FAULT_BYTES: u32 = 24;

/// [`FAULT_BYTES`] as a buffer's length.
pub(crate) const FAULT_LEN: usize = FAULT_BYTES as usize;

/// A request, as the device reads it. A range is `first..=last`: the device
/// takes inclusive ends, which also name a range reaching the top of the
/// IOVA space.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Request {
    Attach {
        domain: u32,
        endpoint: u32,
    },
    Detach {
        domain: u32,
        endpoint: u32,
    },
    Map {
        domain: u32,
        first: u64,
        last: u64,
        phys: u64,
        access: Access,
    },
    Unmap {
        domain: u32,
        first: u64,
        last: u64,
    },
    Probe {
        endpoint: u32,
    },
}

/// The longest request but a probe's, and its tail: a map's 36 bytes.
pub(crate) const REQUEST_MAX: usize = 36;

impl Request {
    /// Write the device-readable part into `out`, answering its length.
    /// `out` holds [`REQUEST_MAX`] bytes, or [`PROBE_REQUEST_LEN`] for a
    /// probe.
    pub(crate) fn encode(&self, out: &mut [u8]) -> usize {
        let mut put = Writer { out, at: 0 };
        match self {
            Self::Attach { domain, endpoint } => {
                put.head(T_ATTACH);
                put.u32(*domain);
                put.u32(*endpoint);
                // Flags, then reserved: neither bypasses.
                put.u32(0);
                put.u32(0);
            }
            Self::Detach { domain, endpoint } => {
                put.head(T_DETACH);
                put.u32(*domain);
                put.u32(*endpoint);
                put.u64(0);
            }
            Self::Map {
                domain,
                first,
                last,
                phys,
                access,
            } => {
                let mut flags = 0;
                if access.read() {
                    flags |= MAP_F_READ;
                }
                if access.write() {
                    flags |= MAP_F_WRITE;
                }
                put.head(T_MAP);
                put.u32(*domain);
                put.u64(*first);
                put.u64(*last);
                put.u64(*phys);
                put.u32(flags);
            }
            Self::Unmap {
                domain,
                first,
                last,
            } => {
                put.head(T_UNMAP);
                put.u32(*domain);
                put.u64(*first);
                put.u64(*last);
                put.u32(0);
            }
            Self::Probe { endpoint } => {
                put.head(T_PROBE);
                put.u32(*endpoint);
                for _ in 0..8 {
                    put.u64(0);
                }
            }
        }
        put.at
    }
}

struct Writer<'o> {
    out: &'o mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        if let Some(slot) = self.out.get_mut(self.at..self.at + bytes.len()) {
            slot.copy_from_slice(bytes);
        }
        self.at += bytes.len();
    }

    fn head(&mut self, kind: u8) {
        self.bytes(&[kind, 0, 0, 0]);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }
}

/// What the device said of a request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Ok,
    /// Its parameters were invalid: a map over a mapping or a reserved
    /// region, an attach the device cannot make.
    Invalid,
    /// An address or range it cannot take, or an unmap that would split a
    /// mapping.
    Range,
    /// The domain or endpoint does not exist.
    NoEntry,
    /// The device ran out of room.
    NoMemory,
    /// Unsupported, an I/O or device error, a bad address, or a code this
    /// family does not know.
    Failed,
}

impl Status {
    pub(crate) const fn of(code: u8) -> Self {
        match code {
            0 => Self::Ok,
            4 => Self::Invalid,
            5 => Self::Range,
            6 => Self::NoEntry,
            8 => Self::NoMemory,
            _ => Self::Failed,
        }
    }
}

const PROBE_T_NONE: u16 = 0;
const PROBE_T_RESV_MEM: u16 = 1;
/// The property type's field: the low twelve bits, above them reserved.
const PROBE_TYPE_MASK: u16 = 0xFFF;
/// Bytes of a reserved-memory property after its header.
const RESV_MEM_LEN: u16 = 20;

/// Hand `sink` every reserved region the probe properties in `properties`
/// name, widened to whole pages. A property whose reserved bits are set is
/// ignored, as the specification requires; a region of a subtype this family
/// does not know is reserved all the same.
///
/// # Errors
///
/// [`IommuError::Hardware`] for a reserved-memory property cut short or
/// running backwards: the window it meant to keep out is unknown, so none of
/// the probe is trusted.
pub(crate) fn reserved_regions(
    properties: &[u8],
    sink: &mut dyn FnMut(Range<u64>),
) -> Result<(), IommuError> {
    let mut at = 0;
    while let Some(header) = properties.get(at..at + 4) {
        let kind = u16::from_le_bytes([header[0], header[1]]);
        let len = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if kind == PROBE_T_NONE {
            return Ok(());
        }
        let body = properties.get(at + 4..at + 4 + len);
        if kind & !PROBE_TYPE_MASK == 0 && kind & PROBE_TYPE_MASK == PROBE_T_RESV_MEM {
            if let Some(region) = reserved_region(body.ok_or(IommuError::Hardware)?)? {
                sink(region);
            }
        }
        at += 4 + len;
    }
    Ok(())
}

/// The pages a reserved-memory property's `[start, end]` reaches into, or
/// [`None`] for one within the top page, which no domain hands out.
fn reserved_region(body: &[u8]) -> Result<Option<Range<u64>>, IommuError> {
    if body.len() < usize::from(RESV_MEM_LEN) {
        return Err(IommuError::Hardware);
    }
    let word = |at: usize| {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&body[at..at + 8]);
        u64::from_le_bytes(bytes)
    };
    let (start, end) = (word(4), word(12));
    if end < start {
        return Err(IommuError::Hardware);
    }
    let first = start & !(IO_PAGE_SIZE - 1);
    let top = !(IO_PAGE_SIZE - 1);
    let past = (end & top).checked_add(IO_PAGE_SIZE).unwrap_or(top);
    Ok((first < past).then_some(first..past))
}

const FAULT_R_DOMAIN: u8 = 1;
const FAULT_R_MAPPING: u8 = 2;
const FAULT_F_WRITE: u32 = 1 << 1;
const FAULT_F_ADDRESS: u32 = 1 << 8;

/// What a fault report says, before the family classes a mapping fault by
/// the endpoint's domain.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Report {
    /// The endpoint is attached to no domain.
    Unattached(Fault),
    /// The endpoint's domain does not map the address for the access.
    Mapping(Fault),
    /// Another reason, by its code.
    Other(Fault),
}

/// The fault report in `record`, or [`None`] where the report must be
/// ignored: one whose reserved bytes are set. An address the device left out
/// reads as zero.
pub(crate) fn report(record: &[u8; FAULT_LEN]) -> Option<Report> {
    if record[1..4] != [0; 3] {
        return None;
    }
    let u32_at = |at: usize| {
        u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]])
    };
    let flags = u32_at(4);
    let mut address = [0; 8];
    address.copy_from_slice(&record[16..24]);
    let iova = if flags & FAULT_F_ADDRESS == 0 {
        0
    } else {
        u64::from_le_bytes(address) & !(IO_PAGE_SIZE - 1)
    };
    let fault = |reason| Fault {
        stream: u32_at(8),
        iova,
        write: flags & FAULT_F_WRITE != 0,
        reason,
    };
    Some(match record[0] {
        FAULT_R_DOMAIN => Report::Unattached(fault(FaultReason::Blocked)),
        FAULT_R_MAPPING => Report::Mapping(fault(FaultReason::Unmapped)),
        code => Report::Other(fault(FaultReason::Other(u16::from(code)))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_names_an_inclusive_end_and_its_access() {
        let mut out = [0xFF; REQUEST_MAX];
        let len = Request::Map {
            domain: 7,
            first: 0x1000,
            last: 0x2FFF,
            phys: 0x8000_0000,
            access: Access::READ,
        }
        .encode(&mut out);
        assert_eq!(len, REQUEST_MAX);
        assert_eq!(out[..8], [T_MAP, 0, 0, 0, 7, 0, 0, 0]);
        assert_eq!(out[8..16], 0x1000u64.to_le_bytes());
        assert_eq!(out[16..24], 0x2FFFu64.to_le_bytes(), "the end is inclusive");
        assert_eq!(out[24..32], 0x8000_0000u64.to_le_bytes());
        assert_eq!(out[32..36], MAP_F_READ.to_le_bytes());
    }

    #[test]
    fn every_request_is_as_long_as_its_layout() {
        let mut out = [0; PROBE_REQUEST_LEN];
        for (request, len) in [
            (
                Request::Attach {
                    domain: 1,
                    endpoint: 2,
                },
                20,
            ),
            (
                Request::Detach {
                    domain: 1,
                    endpoint: 2,
                },
                20,
            ),
            (
                Request::Unmap {
                    domain: 1,
                    first: 0x1000,
                    last: u64::MAX,
                },
                28,
            ),
            (Request::Probe { endpoint: 9 }, PROBE_REQUEST_LEN),
        ] {
            assert_eq!(request.encode(&mut out), len, "{request:?}");
        }
    }

    #[test]
    fn reserved_regions_are_widened_to_pages_and_end_at_a_null_property() {
        let mut properties = [0u8; 96];
        let mut put = |at: usize, kind: u16, subtype: u8, start: u64, end: u64| {
            properties[at..at + 2].copy_from_slice(&kind.to_le_bytes());
            properties[at + 2..at + 4].copy_from_slice(&RESV_MEM_LEN.to_le_bytes());
            properties[at + 4] = subtype;
            properties[at + 8..at + 16].copy_from_slice(&start.to_le_bytes());
            properties[at + 16..at + 24].copy_from_slice(&end.to_le_bytes());
        };
        put(0, PROBE_T_RESV_MEM, 1, 0xFEE0_0000, 0xFEEF_FFFF);
        put(24, PROBE_T_RESV_MEM | 0x1000, 0, 0x1000, 0x1FFF);
        put(48, PROBE_T_RESV_MEM, 7, 0x2_0010, 0x2_2000);
        let mut regions = alloc::vec::Vec::new();
        reserved_regions(&properties, &mut |region| regions.push(region)).unwrap();
        assert_eq!(
            regions,
            [0xFEE0_0000..0xFEF0_0000, 0x2_0000..0x2_3000],
            "a property with reserved bits is ignored, an unknown subtype reserved"
        );
    }

    fn property(len: u16, start: u64, end: u64) -> [u8; 24] {
        let mut out = [0u8; 24];
        out[..2].copy_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out[8..16].copy_from_slice(&start.to_le_bytes());
        out[16..24].copy_from_slice(&end.to_le_bytes());
        out
    }

    /// The window a malformed reserved-memory property meant to keep out is
    /// unknown, so the probe is not trusted at all.
    #[test]
    fn a_reserved_memory_property_cut_short_or_backwards_refuses_the_probe() {
        let whole = property(RESV_MEM_LEN, 0x1000, 0x1FFF);
        let mut sink = |_: Range<u64>| {};
        assert_eq!(
            reserved_regions(&whole[..10], &mut sink),
            Err(IommuError::Hardware)
        );
        let short = property(RESV_MEM_LEN - 4, 0x1000, 0x1FFF);
        assert_eq!(
            reserved_regions(&short, &mut sink),
            Err(IommuError::Hardware)
        );
        let backwards = property(RESV_MEM_LEN, 0x2000, 0x1FFF);
        assert_eq!(
            reserved_regions(&backwards, &mut sink),
            Err(IommuError::Hardware)
        );
    }

    /// A region reaching the top page ends where a domain's addresses do, and
    /// one inside that page keeps nothing out.
    #[test]
    fn a_region_reaching_the_top_page_ends_page_aligned() {
        let top = !(IO_PAGE_SIZE - 1);
        let mut regions = alloc::vec::Vec::new();
        let reaching = property(RESV_MEM_LEN, top - 0x2000, u64::MAX);
        reserved_regions(&reaching, &mut |region| regions.push(region)).unwrap();
        assert_eq!(regions, alloc::vec![(top - 0x2000)..top]);
        regions.clear();
        let within = property(RESV_MEM_LEN, top + 0x10, u64::MAX);
        reserved_regions(&within, &mut |region| regions.push(region)).unwrap();
        assert!(regions.is_empty());
    }

    /// What the device writes into a probe or a report is the device's to
    /// choose: any bytes at all decode, or refuse, without a panic, and every
    /// region kept out is whole pages and inside the space.
    #[test]
    fn arbitrary_device_answers_decode_or_refuse_cleanly() {
        let mut rng = tairix_fuzzseed::Prng::new(tairix_fuzzseed::start(
            "arbitrary_device_answers_decode_or_refuse_cleanly",
            tairix_fuzzseed::FUZZ_SEED_ENV,
        ));
        let mut properties = alloc::vec::Vec::new();
        for _ in 0..if cfg!(miri) { 64 } else { 20_000 } {
            properties.clear();
            for _ in 0..rng.below(5) {
                let mut head = property(RESV_MEM_LEN, rng.next_u64(), rng.next_u64());
                if rng.below(4) == 0 {
                    head[..4].copy_from_slice(&rng.next_u32().to_le_bytes());
                }
                properties.extend_from_slice(&head[..rng.below(head.len() + 1)]);
            }
            let _ = reserved_regions(&properties, &mut |region| {
                assert!(region.start < region.end);
                assert_eq!(region.start % IO_PAGE_SIZE, 0);
                assert_eq!(region.end % IO_PAGE_SIZE, 0);
            });
            let mut record = [0u8; FAULT_LEN];
            rng.fill(&mut record);
            if let Some(Report::Mapping(fault) | Report::Unattached(fault) | Report::Other(fault)) =
                report(&record)
            {
                assert_eq!(fault.iova % IO_PAGE_SIZE, 0);
            }
        }
    }

    #[test]
    fn a_report_is_decoded_or_ignored_where_its_reserved_bytes_are_set() {
        let mut record = [0u8; FAULT_LEN];
        record[0] = FAULT_R_MAPPING;
        record[4..8].copy_from_slice(&(FAULT_F_WRITE | FAULT_F_ADDRESS).to_le_bytes());
        record[8..12].copy_from_slice(&0x10u32.to_le_bytes());
        record[16..24].copy_from_slice(&0x1234_5678u64.to_le_bytes());
        assert_eq!(
            report(&record),
            Some(Report::Mapping(Fault {
                stream: 0x10,
                iova: 0x1234_5000,
                write: true,
                reason: FaultReason::Unmapped,
            }))
        );
        record[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert!(
            matches!(report(&record), Some(Report::Mapping(fault)) if fault.iova == 0 && !fault.write),
            "an address left out reads as zero"
        );
        record[2] = 1;
        assert_eq!(report(&record), None);
    }
}
