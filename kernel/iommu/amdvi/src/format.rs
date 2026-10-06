//! What the unit reads from memory: page-table entries, device table
//! entries, commands, event records and interrupt remapping entries.
//!
//! Layouts follow the AMD I/O Virtualization Technology (IOMMU)
//! specification, AMD rev. 3.08 §2.2–§2.5.

use tairix_kernel_iommu_api::{
    Access, Command, Fault, FaultReason, InterruptTarget, Pte, PteFormat, IO_PAGE_SIZE,
    MESSAGE_WINDOW,
};

/// Address bits 51:12 of a table entry.
pub(crate) const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;

const PRESENT: u64 = 1 << 0;
const NEXT_LEVEL_SHIFT: u32 = 9;
const NEXT_LEVEL: u64 = 0b111 << NEXT_LEVEL_SHIFT;
const FORCE_COHERENT: u64 = 1 << 60;
const READ: u64 = 1 << 61;
const WRITE: u64 = 1 << 62;

/// The v1 host page-table format. A directory entry names the level of the
/// table it points at; a leaf names level 0 and maps its own level's natural
/// size.
pub(crate) struct HostTables;

impl PteFormat for HostTables {
    /// 4 KiB, 2 MiB and 1 GiB leaves.
    fn leaf_allowed(&self, level: u32) -> bool {
        level <= 2
    }

    fn table(&self, phys: u64, level: u32) -> u64 {
        (phys & ADDRESS) | PRESENT | READ | WRITE | (u64::from(level) << NEXT_LEVEL_SHIFT)
    }

    fn leaf(&self, phys: u64, _level: u32, access: Access) -> u64 {
        let mut entry = (phys & ADDRESS) | PRESENT | FORCE_COHERENT;
        if access.read() {
            entry |= READ;
        }
        if access.write() {
            entry |= WRITE;
        }
        entry
    }

    fn decode(&self, entry: u64, level: u32) -> Pte {
        if entry & PRESENT == 0 {
            return Pte::Absent;
        }
        match (entry & NEXT_LEVEL) >> NEXT_LEVEL_SHIFT {
            0 => match entry & (READ | WRITE) {
                READ => Pte::Leaf(entry & ADDRESS, Access::READ),
                WRITE => Pte::Leaf(entry & ADDRESS, Access::WRITE),
                READ_WRITE => Pte::Leaf(entry & ADDRESS, Access::READ_WRITE),
                // A leaf granting nothing is one this family never writes.
                _ => Pte::Absent,
            },
            next if next == u64::from(level) && level > 0 => Pte::Table(entry & ADDRESS),
            // Not an entry this family writes.
            _ => Pte::Absent,
        }
    }
}

const READ_WRITE: u64 = READ | WRITE;

/// Words of one device table entry.
pub(crate) const DTE_WORDS: usize = 4;

const DTE_VALID: u64 = 1 << 0;
const DTE_TRANSLATION_VALID: u64 = 1 << 1;
const DTE_MODE_SHIFT: u32 = 9;
const DTE_SUPPRESS_ALL: u64 = 1 << 34;
const DTE_INTERRUPTS_VALID: u64 = 1 << 0;
const DTE_TABLE_LENGTH_SHIFT: u32 = 1;
const DTE_TABLE_POINTER: u64 = 0x000F_FFFF_FFFF_FFC0;
const DTE_REMAP: u64 = 0b10 << 60;

/// Word 0 of a device whose DMA is refused: valid, with no translation
/// information, so its walk ends in an abort. A mode of zero would pass it
/// through.
pub(crate) const DTE_BLOCKED: u64 = DTE_VALID;

/// Word 0 of a device whose DMA `levels` of tables rooted at `root`
/// translate.
pub(crate) fn dte_translated(root: u64, levels: u32) -> u64 {
    DTE_VALID
        | DTE_TRANSLATION_VALID
        | (u64::from(levels) << DTE_MODE_SHIFT)
        | (root & ADDRESS)
        | READ
        | WRITE
}

/// Word 1: the domain the device's translations are cached under, and
/// whether its faults go unrecorded. ATS stays refused: the IOTLB bit is
/// clear.
pub(crate) fn dte_domain(domain: u16, silenced: bool) -> u64 {
    let suppress = if silenced { DTE_SUPPRESS_ALL } else { 0 };
    u64::from(domain) | suppress
}

/// Word 2 while remapping is off: interrupts pass as the device sends them.
pub(crate) const DTE_INTERRUPTS_PASS: u64 = 0;

/// Word 2 once remapping is on: through the `2^length`-entry table at
/// `table`, or, with none, every fixed interrupt refused.
pub(crate) fn dte_interrupts(table: Option<(u64, u32)>) -> u64 {
    match table {
        Some((phys, length)) => {
            DTE_INTERRUPTS_VALID
                | (u64::from(length) << DTE_TABLE_LENGTH_SHIFT)
                | (phys & DTE_TABLE_POINTER)
                | DTE_REMAP
        }
        None => DTE_INTERRUPTS_VALID,
    }
}

const OPCODE_SHIFT: u32 = 60;
const COMPLETION_WAIT: u64 = 1;
const INVALIDATE_DEVICE: u64 = 2;
const INVALIDATE_PAGES: u64 = 3;
const INVALIDATE_INTERRUPTS: u64 = 5;
const INVALIDATE_ALL: u64 = 8;
const WAIT_STORE: u64 = 1 << 0;
const PAGES_SPAN: u64 = 1 << 0;
const PAGES_DIRECTORIES: u64 = 1 << 1;
/// The whole domain: a span over every address, directories included.
const EVERY_PAGE: u64 = 0x7FFF_FFFF_FFFF_F000 | PAGES_SPAN | PAGES_DIRECTORIES;
/// The highest address bit a span may reach before it is the whole domain.
const SPAN_TOP_BIT: u32 = 51;

/// A wait the unit completes by storing `token` at `store`, once every
/// command before it is done.
pub(crate) fn completion_wait(token: u32, store: u64) -> Command {
    let low = store & 0xFFFF_FFF8;
    let high = (store >> 32) & 0x000F_FFFF;
    [
        low | WAIT_STORE | (high << 32) | (COMPLETION_WAIT << OPCODE_SHIFT),
        u64::from(token),
    ]
}

/// A wait that stores nothing: what stands in for a command the unit
/// rejected, so the buffer runs on past it.
pub(crate) const FENCE: Command = [COMPLETION_WAIT << OPCODE_SHIFT, 0];

/// Drop the unit's copy of `device`'s table entry.
pub(crate) fn invalidate_device(device: u16) -> Command {
    [u64::from(device) | (INVALIDATE_DEVICE << OPCODE_SHIFT), 0]
}

fn invalidate_pages(domain: u16, address: u64) -> Command {
    [
        (u64::from(domain) << 32) | (INVALIDATE_PAGES << OPCODE_SHIFT),
        address,
    ]
}

/// Drop every translation the unit caches for `domain`.
pub(crate) fn invalidate_domain(domain: u16) -> Command {
    invalidate_pages(domain, EVERY_PAGE)
}

/// Drop what the unit caches for `[iova, iova + len)` of `domain`, the
/// directories above it included: the smallest naturally aligned span
/// holding it, which the span encoding names by the address bits below its
/// size set.
pub(crate) fn invalidate_range(domain: u16, iova: u64, len: u64) -> Command {
    let last = iova.saturating_add(len.max(1) - 1);
    let differ = (iova ^ last) & !(IO_PAGE_SIZE - 1);
    if differ == 0 {
        return invalidate_pages(domain, (iova & ADDRESS) | PAGES_DIRECTORIES);
    }
    let top = differ.ilog2();
    if top > SPAN_TOP_BIT {
        return invalidate_domain(domain);
    }
    let span = (iova | ((1 << top) - 1)) & ADDRESS;
    invalidate_pages(domain, span | PAGES_SPAN | PAGES_DIRECTORIES)
}

/// Drop the unit's copies of `device`'s remapping entries.
pub(crate) fn invalidate_interrupts(device: u16) -> Command {
    [
        u64::from(device) | (INVALIDATE_INTERRUPTS << OPCODE_SHIFT),
        0,
    ]
}

/// Drop everything the unit caches.
pub(crate) const INVALIDATE_EVERYTHING: Command = [INVALIDATE_ALL << OPCODE_SHIFT, 0];

/// Words of one event record.
pub(crate) const EVENT_WORDS: usize = 2;

const EVENT_ILLEGAL_DEVICE: u64 = 1;
const EVENT_PAGE_FAULT: u64 = 2;
const EVENT_DEVICE_TABLE_ERROR: u64 = 3;
const EVENT_PAGE_TABLE_ERROR: u64 = 4;
const EVENT_INVALID_REQUEST: u64 = 8;
const FLAG_INTERRUPT: u64 = 1 << 3;
const FLAG_WRITE: u64 = 1 << 5;
const FLAG_PERMISSION: u64 = 1 << 6;
/// `TR`: the request asked for a translation.
const FLAG_TRANSLATION: u64 = 1 << 8;
/// An invalid-request event's type, in its flags.
const INVALID_TYPE_SHIFT: u32 = 9;
/// Its types for a transaction: one presenting an address as translated by a
/// device the entry does not let translate, and a posted write to the
/// interrupt range of a device whose interrupts the entry refuses.
const INVALID_PRETRANSLATED: u64 = 0b001;
const INVALID_INTERRUPT: u64 = 0b101;

/// The code of the event `record` holds: zero for a slot the unit has not
/// finished writing.
pub(crate) fn event_code(record: [u64; EVENT_WORDS]) -> u64 {
    record[0] >> OPCODE_SHIFT
}

fn low16(word: u64) -> u16 {
    let [low, high, ..] = word.to_le_bytes();
    u16::from_le_bytes([low, high])
}

/// The fault one event record describes, where it names a device's request.
///
/// An interrupt request is a write to the message window, so a page fault
/// says it refused one only with that address as well as the flag. An
/// invalid-request event's type says what the device did: a use of ATS, which
/// no entry allows, an interrupt an entry refuses, or a request no entry could
/// make valid — port I/O, a read of the interrupt range, an address the
/// fabric reserves.
pub(crate) fn decode_event(record: [u64; EVENT_WORDS]) -> Option<Fault> {
    let [low, address] = record;
    let flags = (low >> 48) & 0xFFF;
    let interrupt = flags & FLAG_INTERRUPT != 0 && MESSAGE_WINDOW.contains(&address);
    let invalid = (flags >> INVALID_TYPE_SHIFT) & 0b111;
    let reason = match event_code(record) {
        EVENT_PAGE_FAULT | EVENT_INVALID_REQUEST if flags & FLAG_TRANSLATION != 0 => {
            FaultReason::Translated
        }
        EVENT_PAGE_FAULT if interrupt => FaultReason::Interrupt,
        EVENT_PAGE_FAULT if flags & FLAG_PERMISSION != 0 => FaultReason::Denied,
        EVENT_PAGE_FAULT => FaultReason::Unmapped,
        EVENT_INVALID_REQUEST if invalid == INVALID_PRETRANSLATED => FaultReason::Translated,
        EVENT_INVALID_REQUEST if invalid == INVALID_INTERRUPT => FaultReason::Interrupt,
        EVENT_ILLEGAL_DEVICE | EVENT_INVALID_REQUEST => FaultReason::Malformed,
        code @ (EVENT_DEVICE_TABLE_ERROR | EVENT_PAGE_TABLE_ERROR) => {
            FaultReason::Other(low16(code))
        }
        // Command and invalidation errors name no device's request.
        _ => return None,
    };
    Some(Fault {
        stream: u32::from(low16(low)),
        iova: address & !(IO_PAGE_SIZE - 1),
        write: flags & FLAG_WRITE != 0,
        reason,
    })
}

const IRTE_REMAP: u64 = 1 << 0;

/// A remapping entry delivering `target`: 32 bits with an 8-bit
/// destination, or 128 with a 32-bit one where `extended`; [`None`] for a
/// destination the format cannot name. Fixed delivery, physical destination.
pub(crate) fn irte(target: InterruptTarget, extended: bool) -> Option<[u64; 2]> {
    let vector = u64::from(target.vector);
    let destination = u64::from(target.destination);
    if extended {
        Some([
            IRTE_REMAP | ((destination & 0x00FF_FFFF) << 8),
            vector | ((destination >> 24) << 56),
        ])
    } else {
        let destination = u8::try_from(target.destination).ok()?;
        Some([
            IRTE_REMAP | (u64::from(destination) << 8) | (vector << 16),
            0,
        ])
    }
}

/// An interrupt message's trigger mode bit, which a remapped interrupt keeps
/// from the request that raised it.
const MESSAGE_LEVEL: u32 = 1 << 15;

/// The MSI data raising entry `index`: the data's low bits name the entry,
/// its trigger mode bit the trigger.
pub(crate) fn message_data(index: u8, level: bool) -> u32 {
    let trigger = if level { MESSAGE_LEVEL } else { 0 };
    u32::from(index) | trigger
}

/// An IO-APIC redirection entry's trigger mode bit.
const REDIRECTION_LEVEL: u64 = 1 << 15;

/// The IO-APIC redirection entry raising entry `index`: compatibility format
/// with the index in the vector field and fixed delivery, so the message's
/// data names it.
pub(crate) fn redirection(index: u8, level: bool) -> u64 {
    let trigger = if level { REDIRECTION_LEVEL } else { 0 };
    u64::from(index) | trigger
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_names_the_level_it_points_at_and_a_leaf_level_zero() {
        let format = HostTables;
        let table = format.table(0x1234_5000, 2);
        assert_eq!(table & NEXT_LEVEL, 2 << 9);
        assert_eq!(format.decode(table, 2), Pte::Table(0x1234_5000));
        assert_eq!(format.decode(table, 1), Pte::Absent, "names another level");
        let leaf = format.leaf(0x8000_0000, 1, Access::READ_WRITE);
        assert_eq!(leaf & NEXT_LEVEL, 0);
        assert_eq!(leaf & FORCE_COHERENT, FORCE_COHERENT);
        assert_eq!(
            format.decode(leaf, 1),
            Pte::Leaf(0x8000_0000, Access::READ_WRITE)
        );
        let read_only = format.leaf(0x8000_0000, 0, Access::READ);
        assert_eq!(
            format.decode(read_only, 0),
            Pte::Leaf(0x8000_0000, Access::READ)
        );
        assert_eq!(format.decode(0, 0), Pte::Absent);
        assert!(!format.leaf_allowed(3));
    }

    #[test]
    fn commands_carry_their_opcode_in_the_second_dword() {
        let wait = completion_wait(7, 0x0000_0012_3456_7008);
        assert_eq!(wait[0] >> 60, 1);
        assert_eq!(wait[0] & 0xFFFF_FFF8, 0x3456_7008);
        assert_eq!((wait[0] >> 32) & 0xF_FFFF, 0x12);
        assert_eq!(wait[0] & WAIT_STORE, WAIT_STORE);
        assert_eq!(wait[1], 7);
        assert_eq!(invalidate_device(0x00A0), [0x00A0 | (2 << 60), 0]);
        assert_eq!(
            invalidate_domain(5),
            [(5 << 32) | (3 << 60), 0x7FFF_FFFF_FFFF_F003]
        );
        assert_eq!(invalidate_interrupts(0x18)[0] >> 60, 5);
        assert_eq!(INVALIDATE_EVERYTHING[0] >> 60, 8);
    }

    /// The span a page invalidation names, as the unit decodes it: one page,
    /// or the aligned power of two below the lowest clear bit from 12 up.
    fn span_of(address: u64) -> (u64, u64) {
        if address & PAGES_SPAN == 0 {
            return (address & ADDRESS, IO_PAGE_SIZE);
        }
        let ones = (address >> 12).trailing_ones();
        let size = 1u64 << (12 + ones + 1);
        (address & ADDRESS & !(size - 1), size)
    }

    #[test]
    fn a_range_invalidation_names_the_smallest_aligned_span_holding_it() {
        for (iova, len) in [
            (0x4000_0000, 0x1000),
            (0x4000_0000, 0x2000),
            (0x4000_1000, 0x2000),
            (0x4000_3000, 0x0020_0000),
            (0x7FFF_F000, 0x2000),
            (0x1234_5000, 0x10_0000),
        ] {
            let [low, address] = invalidate_range(9, iova, len);
            assert_eq!(low, (9 << 32) | (3 << 60));
            assert_eq!(address & PAGES_DIRECTORIES, PAGES_DIRECTORIES);
            let (base, size) = span_of(address);
            assert!(
                base <= iova && iova + len <= base + size,
                "{iova:#x}+{len:#x} not held"
            );
            assert!(
                size == IO_PAGE_SIZE || (iova - base < size / 2 && iova + len - base > size / 2),
                "{iova:#x}+{len:#x} fits a half of {base:#x}+{size:#x}"
            );
        }
        assert_eq!(invalidate_range(9, 0x4000_0000, 0x1000)[1] & PAGES_SPAN, 0);
        let [_, everything] = invalidate_range(9, 0, 1 << 52);
        assert_eq!(span_of(everything), (0, 1 << 52));
        assert_eq!(
            invalidate_range(9, 0, 1 << 53),
            invalidate_domain(9),
            "past bit 51 the whole domain"
        );
    }

    /// An event record as the unit writes it.
    fn event(code: u64, device: u16, flags: u64, address: u64) -> [u64; EVENT_WORDS] {
        [
            u64::from(device) | (flags << 48) | (code << OPCODE_SHIFT),
            address,
        ]
    }

    #[test]
    fn an_event_record_names_its_device_address_and_reason() {
        let write_fault = event(EVENT_PAGE_FAULT, 0x0018, FLAG_WRITE, 0x4000_0123);
        assert_eq!(
            decode_event(write_fault),
            Some(Fault {
                stream: 0x0018,
                iova: 0x4000_0000,
                write: true,
                reason: FaultReason::Unmapped,
            })
        );
        let reasons = [
            (
                event(EVENT_PAGE_FAULT, 1, FLAG_TRANSLATION, 0),
                FaultReason::Translated,
            ),
            (
                event(EVENT_PAGE_FAULT, 1, FLAG_INTERRUPT, 0xFEE0_0000),
                FaultReason::Interrupt,
            ),
            (
                event(EVENT_PAGE_FAULT, 1, FLAG_PERMISSION, 0x1000),
                FaultReason::Denied,
            ),
            (event(EVENT_ILLEGAL_DEVICE, 1, 0, 0), FaultReason::Malformed),
            (
                event(EVENT_INVALID_REQUEST, 1, INVALID_PRETRANSLATED << 9, 0),
                FaultReason::Translated,
            ),
            (
                event(EVENT_INVALID_REQUEST, 1, FLAG_TRANSLATION, 0),
                FaultReason::Translated,
            ),
            (
                event(EVENT_INVALID_REQUEST, 1, 0x0A00, 0xFD_F821_0300),
                FaultReason::Interrupt,
            ),
            (
                event(EVENT_INVALID_REQUEST, 1, 0b010 << 9, 0xFD_FC00_0080),
                FaultReason::Malformed,
            ),
            (
                event(EVENT_INVALID_REQUEST, 1, 0, 0),
                FaultReason::Malformed,
            ),
            (
                event(EVENT_PAGE_TABLE_ERROR, 1, 0, 0),
                FaultReason::Other(4),
            ),
        ];
        for (record, reason) in reasons {
            assert_eq!(decode_event(record).map(|fault| fault.reason), Some(reason));
        }
        assert_eq!(
            decode_event(event(EVENT_PAGE_FAULT, 1, FLAG_INTERRUPT, 0x4000_0000))
                .map(|fault| fault.reason),
            Some(FaultReason::Unmapped),
            "a DMA address is no interrupt whatever the flag says"
        );
        assert_eq!(decode_event(event(5, 0, 0, 0)), None, "an illegal command");
        assert_eq!(event_code(event(0, 3, 0, 0)), 0);
    }

    #[test]
    fn an_entry_names_its_destination_in_the_width_its_format_has() {
        let near = InterruptTarget {
            vector: 0x41,
            destination: 3,
            level: false,
        };
        assert_eq!(irte(near, false), Some([1 | (3 << 8) | (0x41 << 16), 0]));
        let far = InterruptTarget {
            destination: 0x0102_0304,
            ..near
        };
        assert_eq!(irte(far, false), None);
        assert_eq!(
            irte(far, true),
            Some([1 | (0x02_0304 << 8), 0x41 | (0x01 << 56)])
        );
        assert_eq!(redirection(9, true), 9 | (1 << 15));
        assert_eq!(message_data(9, false), 9);
        assert_eq!(message_data(9, true), 9 | (1 << 15));
    }

    #[test]
    fn a_device_entry_blocks_translates_and_remaps_as_asked() {
        assert_eq!(
            DTE_BLOCKED & DTE_TRANSLATION_VALID,
            0,
            "no translation information"
        );
        let translated = dte_translated(0x7000_0000, 4);
        assert_eq!((translated >> 9) & 0b111, 4);
        assert_eq!(translated & ADDRESS, 0x7000_0000);
        assert_eq!(dte_domain(9, true), 9 | DTE_SUPPRESS_ALL);
        let remapped = dte_interrupts(Some((0x1234_5000, 8)));
        assert_eq!(remapped & 1, 1);
        assert_eq!((remapped >> 1) & 0xF, 8);
        assert_eq!(remapped & DTE_TABLE_POINTER, 0x1234_5000);
        assert_eq!(remapped >> 60, 0b10);
        assert_eq!(
            dte_interrupts(None),
            1,
            "valid, every fixed interrupt refused"
        );
    }
}
