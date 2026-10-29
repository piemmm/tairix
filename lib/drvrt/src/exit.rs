//! How a driver process ends: in the one [`DriverExit`] vocabulary, each
//! ending stating its reason with the node it ends on before the process
//! goes.

pub use tairix_abi::driver::DriverExit;
use tairix_abi::DriverError;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};

/// A driver ended for a failure; recorded at error.
pub const DRIVER_ENDED: EventId = EventId(24_000);
/// A driver left its node to another, the node offering nothing it serves.
pub const DRIVER_DECLINED: EventId = EventId(24_001);
/// A driver ended with its device, which was removed.
pub const DRIVER_REMOVED: EventId = EventId(24_002);

/// The most detail fields an ending carries beyond its kind and node.
pub const MAX_DETAIL: usize = 4;

/// The record an ending is written under, and its level.
const fn record(exit: DriverExit) -> (EventId, Level) {
    match exit {
        DriverExit::Declined => (DRIVER_DECLINED, Level::Info),
        DriverExit::Removed => (DRIVER_REMOVED, Level::Info),
        _ => (DRIVER_ENDED, Level::Error),
    }
}

/// A detail field naming the step that failed.
#[must_use]
pub const fn step(text: &'static str) -> Field<'static> {
    Field {
        key: "step",
        value: FieldValue::Str(text),
    }
}

/// A detail field carrying the typed refusal the failure had.
#[must_use]
pub fn error(err: DriverError) -> Field<'static> {
    Field {
        key: "error",
        value: FieldValue::SignedInt(i64::from(err as i32)),
    }
}

/// Record in `sink` that the driver of `node` ends for `exit`, with up to
/// [`MAX_DETAIL`] fields of `detail`, and answer the status to exit with.
#[must_use]
pub fn end_with(sink: &dyn Sink, node: Option<u32>, exit: DriverExit, detail: &[Field<'_>]) -> i32 {
    let (id, level) = record(exit);
    let mut fields = [Field::UNSET; 2 + MAX_DETAIL];
    fields[0] = Field {
        key: "exit",
        value: FieldValue::Str(exit.name()),
    };
    let mut len = 1;
    if let Some(node) = node {
        fields[len] = Field {
            key: "node",
            value: FieldValue::UnsignedInt(u64::from(node)),
        };
        len += 1;
    }
    for field in detail.iter().take(MAX_DETAIL) {
        fields[len] = *field;
        len += 1;
    }
    log(
        sink,
        &Event {
            level,
            id,
            message: exit.reason(),
            fields: &fields[..len],
        },
    );
    exit.code()
}

/// Record through the system log that this driver ends for `exit`, naming
/// the node it was loaded for, and answer the status to exit with.
#[cfg(target_os = "none")]
#[must_use]
pub fn end(exit: DriverExit, detail: &[Field<'_>]) -> i32 {
    let node = tairix_rt::hw_self_node().ok().map(|node| node.node());
    end_with(&tairix_rt::LogSink, node, exit, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    /// One record as written: its id, level, message and fields.
    type Written = (EventId, Level, String, Vec<(String, String)>);

    #[derive(Default)]
    struct Recorded(RefCell<Vec<Written>>);

    impl Sink for Recorded {
        fn write_event(&self, event: &Event<'_>) {
            let fields = event
                .fields
                .iter()
                .map(|field| (field.key.into(), format!("{}", field.value)))
                .collect();
            self.0
                .borrow_mut()
                .push((event.id, event.level, event.message.into(), fields));
        }
    }

    #[test]
    fn every_code_states_its_reason_once_with_its_node() {
        for &exit in DriverExit::ALL {
            let sink = Recorded::default();
            let code = end_with(&sink, Some(0x17), exit, &[step("the window would not map")]);
            assert_eq!(code, exit.code());
            let records = sink.0.borrow();
            assert_eq!(records.len(), 1, "{exit:?} records once");
            let (id, level, message, fields) = &records[0];
            assert_eq!(message, exit.reason());
            assert_eq!(
                fields.as_slice(),
                [
                    ("exit".into(), exit.name().into()),
                    ("node".into(), "23".into()),
                    ("step".into(), "the window would not map".into()),
                ]
            );
            let failure = exit.code() != 0;
            assert_eq!(*id == DRIVER_ENDED, failure);
            assert_eq!(*level == Level::Error, failure);
        }
    }

    #[test]
    fn a_node_unknown_is_left_out_and_detail_past_the_bound_is_dropped() {
        let sink = Recorded::default();
        let detail = [step("a"), step("b"), step("c"), step("d"), step("e")];
        let _ = end_with(&sink, None, DriverExit::NoHost, &detail);
        let records = sink.0.borrow();
        assert_eq!(records[0].3.len(), 1 + MAX_DETAIL);
        assert_eq!(records[0].3[0].0, "exit");
    }
}
