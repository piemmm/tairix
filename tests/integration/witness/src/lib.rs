//! What the freestanding QEMU integration kernels' PASS witnesses read: an
//! audit record, and the fixtures the runner plants for them.
//!
//! A guest kernel's sink compiles only for its bare-metal target, where no
//! host test reaches it, so a reading every vertical shares lives here and is
//! proven once, as does a fixture the runner writes and a guest checks.
//!
//! Test scaffolding: nothing in TAIRiX itself links it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_log::{Event, FieldValue};

/// The value `event` carries under `key`: the first field so named.
#[must_use]
pub fn field<'e>(event: &'e Event<'_>, key: &str) -> Option<&'e FieldValue<'e>> {
    event
        .fields
        .iter()
        .find(|field| field.key == key)
        .map(|field| &field.value)
}

/// The unsigned integer `event` carries under `key`: the first such field
/// holding one, or `None`.
#[must_use]
pub fn field_u64(event: &Event<'_>, key: &str) -> Option<u64> {
    event.fields.iter().find_map(|field| match field.value {
        FieldValue::UnsignedInt(value) if field.key == key => Some(value),
        _ => None,
    })
}

/// Byte `i` of the sector the runner plants at the start of a scratch disk.
#[must_use]
pub const fn sector0_byte(i: usize) -> u8 {
    i.to_le_bytes()[0]
}

/// The text `event` carries under `key`: the first such field holding a
/// string, or `None`.
#[must_use]
pub fn field_str<'e>(event: &Event<'e>, key: &str) -> Option<&'e str> {
    event.fields.iter().find_map(|field| match field.value {
        FieldValue::Str(value) if field.key == key => Some(value),
        _ => None,
    })
}

/// Whether `bundle` — an `APP_LOADED` record's `bundle` field — is the bundle
/// `<store>/<name>.app`, spelled with the shared `lib/abi` suffix rather than
/// a path written out.
#[must_use]
pub fn names_bundle(bundle: &str, store: &str, name: &str) -> bool {
    bundle
        .strip_prefix(store)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|leaf| leaf.strip_suffix(tairix_abi::BUNDLE_SUFFIX))
        .is_some_and(|leaf| leaf == name)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;

    use super::{field, field_str, field_u64, names_bundle, sector0_byte};
    use tairix_abi::{SYSTEM_APPLICATION_STORE, SYSTEM_SERVICE_STORE};
    use tairix_log::{Event, EventId, Field, FieldValue, Level};

    #[test]
    fn a_value_reads_as_the_first_field_under_its_key() {
        let fields = [
            Field {
                key: "stopped",
                value: FieldValue::UnsignedInt(2),
            },
            Field {
                key: "stopped",
                value: FieldValue::Str("later"),
            },
        ];
        let event = Event {
            level: Level::Info,
            id: EventId(1),
            message: "",
            fields: &fields,
        };
        assert_eq!(field(&event, "stopped"), Some(&FieldValue::UnsignedInt(2)));
        assert_eq!(field(&event, "absent"), None);
    }

    #[test]
    fn a_field_reads_as_the_first_text_under_its_key() {
        let fields = [
            Field {
                key: "kind",
                value: FieldValue::UnsignedInt(3),
            },
            Field {
                key: "kind",
                value: FieldValue::Str("touch"),
            },
            Field {
                key: "kind",
                value: FieldValue::Str("key"),
            },
        ];
        let event = Event {
            level: Level::Info,
            id: EventId(1),
            message: "",
            fields: &fields,
        };
        assert_eq!(field_str(&event, "kind"), Some("touch"));
        assert_eq!(field_str(&event, "bundle"), None);
    }

    #[test]
    fn a_field_reads_as_the_first_unsigned_integer_under_its_key() {
        let fields = [
            Field {
                key: "iova",
                value: FieldValue::Str("high"),
            },
            Field {
                key: "iova",
                value: FieldValue::UnsignedInt(0x1000),
            },
            Field {
                key: "iova",
                value: FieldValue::UnsignedInt(0x2000),
            },
        ];
        let event = Event {
            level: Level::Info,
            id: EventId(1),
            message: "",
            fields: &fields,
        };
        assert_eq!(field_u64(&event, "iova"), Some(0x1000));
        assert_eq!(field_u64(&event, "stream"), None);
    }

    #[test]
    fn the_planted_sector_counts_each_byte_s_offset_modulo_256() {
        assert_eq!(sector0_byte(0), 0);
        assert_eq!(sector0_byte(255), 255);
        assert_eq!(sector0_byte(256), 0);
        assert_eq!(sector0_byte(511), 255);
    }

    #[test]
    fn a_bundle_is_named_only_by_its_own_store_name_and_suffix() {
        let store = SYSTEM_APPLICATION_STORE;
        let bundle = |path: &str| names_bundle(path, store, "terminal");
        assert!(bundle(&format!("{store}/terminal.app")));
        assert!(!bundle(&format!("{store}/terminals.app")));
        assert!(!bundle(&format!("{store}/terminal")));
        assert!(
            !bundle(&format!("{store}/games/terminal.app")),
            "a nested bundle"
        );
        assert!(!bundle(&format!("{store}x/terminal.app")), "a longer store");
        assert!(!bundle(&format!("{store}terminal.app")), "no separator");
        assert!(!names_bundle(
            &format!("{SYSTEM_SERVICE_STORE}/terminal.app"),
            store,
            "terminal"
        ));
        assert!(!bundle(""));
    }
}
