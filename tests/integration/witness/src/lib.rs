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

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tairix_log::{Event, EventId, FieldValue};
use tairix_util::fmt::format_hex_u64;

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

/// The login screen's witnesses that its frame reached a display: the system
/// service store's greeter bundle was verified and loaded, and then the
/// process the kernel names as serving the display endpoint reported the first
/// client frame its display took — a report the service makes only once a
/// present was accepted, and the kernel attributes to its emitter. The greeter
/// is the only graphical client alive at that point, so the frame is its own;
/// ordering it after the load is what attributes it.
pub struct GreeterFrame {
    loaded: AtomicBool,
    /// The process serving the display endpoint, [`NO_SERVER`] until the
    /// kernel names one.
    display: AtomicU64,
    presented: AtomicBool,
}

/// No process is known to serve the display endpoint.
const NO_SERVER: u64 = u64::MAX;

/// The records a [`GreeterFrame`] reads.
#[derive(Copy, Clone, Debug)]
pub struct GreeterRecords {
    /// The bundle loader's record of a verified, loaded bundle.
    pub app_loaded: EventId,
    /// The IPC layer's record of a call endpoint bound, naming its process.
    pub endpoint_created: EventId,
    /// The display service's record of the first frame its display took.
    pub first_present: EventId,
}

impl GreeterFrame {
    /// Neither witness in.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            loaded: AtomicBool::new(false),
            display: AtomicU64::new(NO_SERVER),
            presented: AtomicBool::new(false),
        }
    }

    /// Read `event`, an audit record or a process's own record as the kernel
    /// passes it on, answering whether both witnesses are in.
    pub fn observe(&self, event: &Event<'_>, records: GreeterRecords) -> bool {
        if event.id == records.app_loaded {
            let greeter = tairix_abi::GREETER_RUN_PATH
                .rsplit_once('/')
                .map(|(bundle, _)| bundle);
            if greeter.is_some() && field_str(event, "bundle") == greeter {
                self.loaded.store(true, Ordering::Release);
            }
        } else if event.id == records.endpoint_created {
            let mut hex = [0u8; 16];
            let endpoint = format_hex_u64(tairix_abi::display_ipc::DISPLAY_ENDPOINT, &mut hex);
            if let (Some(bound), Some(process)) =
                (field_str(event, "endpoint"), field_u64(event, "process"))
            {
                if bound == endpoint {
                    self.display.store(process, Ordering::Release);
                }
            }
        } else if event.id == records.first_present && self.loaded.load(Ordering::Acquire) {
            let server = self.display.load(Ordering::Acquire);
            // The kernel stamps a process's record with its emitter first.
            if server != NO_SERVER && field_u64(event, "task") == Some(server) {
                self.presented.store(true, Ordering::Release);
            }
        }
        self.loaded.load(Ordering::Acquire) && self.presented.load(Ordering::Acquire)
    }
}

impl Default for GreeterFrame {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;

    use super::{
        field, field_str, field_u64, names_bundle, sector0_byte, GreeterFrame, GreeterRecords,
    };
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

    #[test]
    fn a_greeter_frame_is_the_display_server_s_first_frame_after_the_greeter_loaded() {
        const RECORDS: GreeterRecords = GreeterRecords {
            app_loaded: EventId(11_001),
            endpoint_created: EventId(3_040),
            first_present: EventId(15_001),
        };
        let event = |id, fields: &'static [Field<'static>]| Event {
            level: Level::Info,
            id,
            message: "",
            fields,
        };
        let leak = |fields: std::vec::Vec<Field<'static>>| -> &'static [Field<'static>] {
            std::boxed::Box::leak(fields.into_boxed_slice())
        };
        let text = |value: std::string::String| -> &'static str {
            std::boxed::Box::leak(value.into_boxed_str())
        };
        let greeter = text(format!("{SYSTEM_SERVICE_STORE}/greeter.app"));
        let display = text(format!(
            "{:016x}",
            tairix_abi::display_ipc::DISPLAY_ENDPOINT
        ));
        let loaded = leak(std::vec![Field {
            key: "bundle",
            value: FieldValue::Str(greeter),
        }]);
        let bound = |endpoint: &'static str, process: u64| {
            leak(std::vec![
                Field {
                    key: "endpoint",
                    value: FieldValue::Str(endpoint),
                },
                Field {
                    key: "process",
                    value: FieldValue::UnsignedInt(process),
                },
            ])
        };
        let shown_by = |task: u64| {
            leak(std::vec![Field {
                key: "task",
                value: FieldValue::UnsignedInt(task),
            }])
        };
        let witness = GreeterFrame::new();
        let observe = |id, fields| witness.observe(&event(id, fields), RECORDS);
        assert!(!observe(RECORDS.endpoint_created, bound(display, 40)));
        assert!(
            !observe(RECORDS.first_present, shown_by(40)),
            "a frame before the greeter is nobody's"
        );
        assert!(!observe(RECORDS.app_loaded, loaded));
        assert!(
            !observe(RECORDS.first_present, shown_by(41)),
            "a report from a process not serving the display proves nothing"
        );
        let other = text(format!("{:016x}", 7));
        assert!(!observe(RECORDS.endpoint_created, bound(other, 41)));
        assert!(!observe(RECORDS.first_present, shown_by(41)));
        assert!(observe(RECORDS.first_present, shown_by(40)));
    }
}
