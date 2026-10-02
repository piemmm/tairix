//! The keyboard decoder: a keyboard application's modifier and key fields read
//! into the set of keys held, and every change between sets a key edge.

use alloc::vec::Vec;

use tairix_abi::driver::input::{InputEvent, InputEventKind};
use tairix_abi::DriverError;

use crate::console::KeyboardConsole;
use crate::descriptor::{CollectionIndex, Field, ReportDescriptor, ReportId, ReportKind};
use crate::usages::{KEY_ERROR_ROLL_OVER, MODIFIER_FIRST, MODIFIER_LAST, PAGE_KEYBOARD};
use crate::{in_application, try_push, Decoded, SeatSink};

/// Keyboard usages held, one bit per usage `0..=255`; the page defines none
/// above `0xE7`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct KeySet([u64; 4]);

impl KeySet {
    fn insert(&mut self, usage: u16) {
        if let Some(word) = self.0.get_mut(usize::from(usage / 64)) {
            *word |= 1 << (usage % 64);
        }
    }

    fn union(self, other: Self) -> Self {
        let mut words = self.0;
        for (word, other) in words.iter_mut().zip(other.0) {
            *word |= other;
        }
        Self(words)
    }

    fn minus(self, other: Self) -> Self {
        let mut words = self.0;
        for (word, other) in words.iter_mut().zip(other.0) {
            *word &= !other;
        }
        Self(words)
    }

    /// The usages held, ascending.
    fn usages(self) -> impl Iterator<Item = u16> {
        self.0.into_iter().zip(0u16..).flat_map(|(mut word, base)| {
            core::iter::from_fn(move || {
                let bit = u16::try_from(word.trailing_zeros())
                    .ok()
                    .filter(|&bit| bit < 64)?;
                word &= word - 1;
                Some(base * 64 + bit)
            })
        })
    }
}

/// The keys the last of one report held.
#[derive(Clone, Copy, Debug)]
struct Held {
    report: ReportId,
    keys: KeySet,
}

/// One keyboard application.
#[derive(Debug)]
pub struct KeyboardDecoder {
    fields: Vec<usize>,
    held: Vec<Held>,
    /// The modifiers are a bitmap of their own, so a modifier usage in the
    /// key array is not a second word on them.
    modifier_bitmap: bool,
    console: KeyboardConsole,
}

const fn is_modifier(usage: u16) -> bool {
    usage >= MODIFIER_FIRST && usage <= MODIFIER_LAST
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
                        keys: KeySet::default(),
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
            console: KeyboardConsole::new(),
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
        let Some(slot) = self
            .held
            .iter()
            .position(|held| held.report.matches(report))
        else {
            return Ok(Decoded::NotMine);
        };
        let id = self.held[slot].report;
        let mut keys = KeySet::default();
        for &index in &self.fields {
            let field = &model.fields()[index];
            if field.report != id {
                continue;
            }
            match read_keys(model, field, report, self.modifier_bitmap, &mut keys) {
                Read::Keys => {}
                Read::Phantom => return Ok(Decoded::Applied),
                Read::Malformed => return Ok(Decoded::Malformed),
            }
        }
        let before = self.held_keys();
        self.held[slot].keys = keys;
        let after = self.held_keys();
        self.emit(before.minus(after), false, sink)?;
        self.emit(after.minus(before), true, sink)?;
        Ok(Decoded::Applied)
    }

    /// Release every key held.
    ///
    /// # Errors
    ///
    /// What `sink` refuses.
    pub fn release(&mut self, sink: &mut dyn SeatSink) -> Result<(), DriverError> {
        let held = self.held_keys();
        for held in &mut self.held {
            held.keys = KeySet::default();
        }
        self.emit(held, false, sink)
    }

    fn held_keys(&self) -> KeySet {
        self.held
            .iter()
            .fold(KeySet::default(), |keys, held| keys.union(held.keys))
    }

    /// One edge per usage in `keys`: a press puts its modifiers down before
    /// its other keys, and a release lets the other keys go before the
    /// modifiers, so a shifted key resolves under the shift it was typed with.
    fn emit(
        &mut self,
        keys: KeySet,
        pressed: bool,
        sink: &mut dyn SeatSink,
    ) -> Result<(), DriverError> {
        let modifiers_first = pressed;
        for pass in [modifiers_first, !modifiers_first] {
            for usage in keys.usages().filter(|&usage| is_modifier(usage) == pass) {
                let edge = InputEvent {
                    kind: InputEventKind::Key,
                    reserved0: 0,
                    code: usage,
                    value: i32::from(pressed),
                };
                if let Some(record) = self.console.feed(edge) {
                    sink.key(&record)?;
                }
            }
        }
        Ok(())
    }
}

enum Read {
    Keys,
    Phantom,
    Malformed,
}

/// The keys `field` holds in `report`. A modifier bitmap must be wholly in
/// the report; a key slot past a short report holds no key, as a clipped
/// report still carries the keys that arrived.
fn read_keys(
    model: &ReportDescriptor,
    field: &Field,
    report: &[u8],
    modifier_bitmap: bool,
    keys: &mut KeySet,
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
