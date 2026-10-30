//! The boot's record of the supervisor protections every CPU runs the kernel
//! under, on the ports whose protections depend on what their CPU offers.

use tairix_log::{Event, Field, FieldValue, Level, Sink};

/// Record `held`: each protection the port holds every CPU to where the CPU
/// offers it, and whether it is in force. At Warn when any is not, since the
/// kernel then runs with a hardware barrier the port knows how to use absent.
pub(crate) fn log<const N: usize>(sink: &(dyn Sink + Sync), held: [(&'static str, bool); N]) {
    let (level, message) = if held.iter().all(|(_, in_force)| *in_force) {
        (Level::Info, "supervisor protections in force")
    } else {
        (
            Level::Warn,
            "the cpu does not offer every supervisor protection",
        )
    };
    let fields = held.map(|(key, in_force)| Field {
        key,
        value: FieldValue::Bool(in_force),
    });
    tairix_log::log(
        sink,
        &Event {
            level,
            id: crate::KERNEL_BOOT_PROTECTIONS,
            message,
            fields: &fields,
        },
    );
}

#[cfg(test)]
mod tests {
    use std::string::{String, ToString};
    use std::sync::Mutex;
    use std::vec::Vec;

    use tairix_log::{Event, FieldValue, Level, Sink};

    /// One record: its level, its id, and each protection it names.
    type Record = (Level, u32, Vec<(String, bool)>);

    struct Recorded(Mutex<Vec<Record>>);

    impl Sink for Recorded {
        fn write_event(&self, event: &Event<'_>) {
            let held = event
                .fields
                .iter()
                .filter_map(|field| match field.value {
                    FieldValue::Bool(in_force) => Some((field.key.to_string(), in_force)),
                    _ => None,
                })
                .collect();
            self.0
                .lock()
                .expect("unpoisoned")
                .push((event.level, event.id.0, held));
        }
    }

    fn recorded<const N: usize>(held: [(&'static str, bool); N]) -> Record {
        let sink = Recorded(Mutex::new(Vec::new()));
        super::log(&sink, held);
        let mut records = sink.0.into_inner().expect("unpoisoned");
        assert_eq!(records.len(), 1, "one record per boot");
        records.remove(0)
    }

    fn named(held: &[(&str, bool)]) -> Vec<(String, bool)> {
        held.iter()
            .map(|&(key, in_force)| (key.to_string(), in_force))
            .collect()
    }

    #[test]
    fn every_protection_held_is_recorded_at_info() {
        let (level, id, held) = recorded([("smep", true), ("smap", true)]);
        assert_eq!(level, Level::Info);
        assert_eq!(id, crate::KERNEL_BOOT_PROTECTIONS.0);
        assert_eq!(held, named(&[("smep", true), ("smap", true)]));
    }

    #[test]
    fn a_protection_the_cpu_lacks_is_recorded_at_warn() {
        let (level, id, held) = recorded([("pan", false)]);
        assert_eq!(level, Level::Warn);
        assert_eq!(id, crate::KERNEL_BOOT_PROTECTIONS.0);
        assert_eq!(held, named(&[("pan", false)]));
        let (level, _, _) = recorded([("smep", true), ("smap", false)]);
        assert_eq!(level, Level::Warn, "one missing protection is enough");
    }
}
