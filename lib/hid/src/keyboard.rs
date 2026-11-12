//! The keyboard decoder: a keyboard application's modifier and key fields read
//! into the set of keys held, and every change between sets a key edge.

use alloc::vec::Vec;

use tairix_abi::input::{KeyEdge, PhysicalKey};
use tairix_abi::DriverError;
use tairix_inline::BitSet256;

use crate::descriptor::{CollectionIndex, Field, ReportDescriptor, ReportId, ReportKind};
use crate::usages::{KEY_ERROR_ROLL_OVER, PAGE_KEYBOARD};
use crate::{in_application, try_push, Decoded, Reading, SeatSink};

/// The keys the last of one report held.
#[derive(Clone, Copy, Debug)]
struct Held {
    report: ReportId,
    keys: BitSet256,
}

/// One keyboard application.
#[derive(Debug)]
pub struct KeyboardDecoder {
    fields: Vec<usize>,
    held: Vec<Held>,
    /// The modifiers are a bitmap of their own, so a modifier usage in the
    /// key array is not a second word on them.
    modifier_bitmap: bool,
}

fn is_modifier(usage: u16) -> bool {
    PhysicalKey::from_usage(usage).is_some_and(PhysicalKey::is_modifier)
}

impl KeyboardDecoder {
    /// The decoder for `application`, or `None` when it reads no key or
    /// memory for it runs out.
    #[must_use]
    pub fn new(model: &ReportDescriptor, application: CollectionIndex) -> Option<Self> {
        let mut fields = Vec::new();
        let mut held: Vec<Held> = Vec::new();
        for (index, field) in model.fields().iter().enumerate() {
            let keyboard = field.kind == ReportKind::Input
                && field.size <= 32
                && in_application(model, field, application)
                && model.usages(field).iter().any(|entry| {
                    entry
                        .nth(0)
                        .is_some_and(|usage| usage.page == PAGE_KEYBOARD)
                });
            if !keyboard {
                continue;
            }
            try_push(&mut fields, index)?;
            if !held.iter().any(|held| held.report == field.report) {
                try_push(
                    &mut held,
                    Held {
                        report: field.report,
                        keys: BitSet256::EMPTY,
                    },
                )?;
            }
        }
        if fields.is_empty() {
            return None;
        }
        let modifier_bitmap = fields.iter().any(|&index| {
            let field = &model.fields()[index];
            field.flags.is_variable()
                && (0..field.count).any(|element| {
                    model
                        .element_usage(field, element)
                        .is_some_and(|usage| usage.page == PAGE_KEYBOARD && is_modifier(usage.id))
                })
        });
        Some(Self {
            fields,
            held,
            modifier_bitmap,
        })
    }

    /// Decode `report` into key edges on `sink`.
    ///
    /// # Errors
    ///
    /// What `sink` refuses.
    pub fn decode(
        &mut self,
        model: &ReportDescriptor,
        report: &[u8],
        sink: &mut dyn SeatSink,
    ) -> Result<Decoded, DriverError> {
        self.read(model, report)
            .applied(|read| self.apply(read, sink))
    }

    /// Whether `report` is one this keyboard reads.
    pub(crate) fn claims(&self, report: &[u8]) -> bool {
        self.slot(report).is_some()
    }

    /// What decoding `report` would answer, applying none of it.
    pub(crate) fn outcome(&self, model: &ReportDescriptor, report: &[u8]) -> Decoded {
        self.read(model, report).outcome()
    }

    fn slot(&self, report: &[u8]) -> Option<usize> {
        self.held
            .iter()
            .position(|held| held.report.matches(report))
    }

    fn read(&self, model: &ReportDescriptor, report: &[u8]) -> Reading<KeyRead> {
        let Some(slot) = self.slot(report) else {
            return Reading::NotMine;
        };
        let id = self.held[slot].report;
        let mut keys = BitSet256::EMPTY;
        for &index in &self.fields {
            let field = &model.fields()[index];
            if field.report != id {
                continue;
            }
            match read_keys(model, field, report, self.modifier_bitmap, &mut keys) {
                Read::Keys => {}
                Read::Phantom => return Reading::Read(KeyRead::Phantom),
                Read::Malformed => return Reading::Malformed,
            }
        }
        Reading::Read(KeyRead::Keys { slot, keys })
    }

    fn apply(&mut self, read: KeyRead, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
        let KeyRead::Keys { slot, keys } = read else {
            return Ok(());
        };
        let before = self.held_keys();
        self.held[slot].keys = keys;
        let after = self.held_keys();
        emit(before.difference(&after), false, sink)?;
        emit(after.difference(&before), true, sink)
    }

    /// Release every key held.
    ///
    /// # Errors
    ///
    /// What `sink` refuses.
    pub fn release(&mut self, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
        let held = self.held_keys();
        for held in &mut self.held {
            held.keys = BitSet256::EMPTY;
        }
        emit(held, false, sink)
    }

    fn held_keys(&self) -> BitSet256 {
        self.held
            .iter()
            .fold(BitSet256::EMPTY, |keys, held| keys.union(&held.keys))
    }
}

/// One edge per key in `keys`: a press puts its modifiers down before its
/// other keys, and a release lets the other keys go before the modifiers, so
/// the seat reads a shifted key under the shift it was typed with. A usage the
/// page names no key is no edge.
fn emit(keys: BitSet256, pressed: bool, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
    let modifiers_first = pressed;
    for pass in [modifiers_first, !modifiers_first] {
        for usage in keys.iter().filter(|&usage| is_modifier(usage) == pass) {
            if let Some(key) = PhysicalKey::from_usage(usage) {
                sink.key(&KeyEdge { key, pressed })?;
            }
        }
    }
    Ok(())
}

enum Read {
    Keys,
    Phantom,
    Malformed,
}

/// What one report sets a keyboard holding.
#[derive(Clone, Copy)]
enum KeyRead {
    /// An `ErrorRollOver` report, which changes nothing.
    Phantom,
    Keys {
        slot: usize,
        keys: BitSet256,
    },
}

/// The keys `field` holds in `report`. A modifier bitmap must be wholly in
/// the report; a key slot past a short report holds no key, as a clipped
/// report still carries the keys that arrived.
fn read_keys(
    model: &ReportDescriptor,
    field: &Field,
    report: &[u8],
    modifier_bitmap: bool,
    keys: &mut BitSet256,
) -> Read {
    for element in 0..field.count {
        let usage = if field.flags.is_variable() {
            let Some(usage) = model.element_usage(field, element) else {
                continue;
            };
            match field.value(report, element) {
                Some(0) => continue,
                Some(_) => usage,
                None if is_modifier(usage.id) => return Read::Malformed,
                None => continue,
            }
        } else {
            let Some(value) = field.value(report, element) else {
                continue;
            };
            match model.array_usage(field, value) {
                Some(usage) if modifier_bitmap && is_modifier(usage.id) => continue,
                Some(usage) => usage,
                None => continue,
            }
        };
        if usage.page != PAGE_KEYBOARD || usage.id == 0 {
            continue;
        }
        if usage.id == KEY_ERROR_ROLL_OVER {
            return Read::Phantom;
        }
        keys.insert(usage.id);
    }
    Read::Keys
}

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
