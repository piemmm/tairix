//! The SD-card bring-up trace: what the EMMC2 engine reports, and the
//! kernel's own facts about the board it brings the card up on, one console
//! line each.
//!
//! The debug image's kernel log drains only as the dispatch loop runs, and
//! the bring-up holds its CPU between parks, so a line that merely queued
//! could sit behind the stall it would have explained. Each line is flushed
//! to the UART as it is written, so the last one a capture shows is where the
//! bring-up stopped. The `storage-trace` feature compiles the lines in; the
//! debug image alone enables it.

#[cfg(feature = "storage-trace")]
use tairix_drv_storage_emmc2::trace::{Trace, WaitEnd};
use tairix_log::{Field, FieldValue};

/// Write one trace line to the console and flush it.
pub(crate) fn line(message: &'static str, fields: &[Field<'_>]) {
    #[cfg(feature = "storage-trace")]
    {
        use tairix_log::Sink as _;

        tairix_arch_aarch64::SERIAL_SINK.write_event(&tairix_log::Event {
            level: tairix_log::Level::Debug,
            id: crate::unlock_service::UNLOCK_SERVICE,
            message,
            fields,
        });
        tairix_arch_aarch64::serial::flush_serial_blocking();
    }
    #[cfg(not(feature = "storage-trace"))]
    let _ = (message, fields);
}

/// Write one trace line naming `what` by its `Debug` form, under `key`.
pub(crate) fn debug_line(message: &'static str, key: &'static str, what: &dyn core::fmt::Debug) {
    #[cfg(feature = "storage-trace")]
    {
        let name = alloc::format!("{what:?}");
        line(
            message,
            &[Field {
                key,
                value: FieldValue::Str(&name),
            }],
        );
    }
    #[cfg(not(feature = "storage-trace"))]
    let _ = (message, key, what);
}

/// A field holding `value` in hex, formatted into `buf`.
pub(crate) fn hex<'b>(key: &'static str, value: u64, buf: &'b mut [u8; 16]) -> Field<'b> {
    Field {
        key,
        value: FieldValue::Str(tairix_util::fmt::format_hex_u64(value, buf)),
    }
}

/// A field holding an unsigned count, address, or rate.
pub(crate) fn unsigned(key: &'static str, value: impl TryInto<u64>) -> Field<'static> {
    Field {
        key,
        value: FieldValue::UnsignedInt(value.try_into().unwrap_or(u64::MAX)),
    }
}

/// A field holding a stable name.
pub(crate) fn text(key: &'static str, value: &'static str) -> Field<'static> {
    Field {
        key,
        value: FieldValue::Str(value),
    }
}

/// A field holding a flag.
pub(crate) fn flag(key: &'static str, value: bool) -> Field<'static> {
    Field {
        key,
        value: FieldValue::Bool(value),
    }
}

/// The engine's trace for one device, live until the bring-up reports its
/// link: the transfers after it go untraced, so the running system pays
/// nothing for it.
#[cfg(feature = "storage-trace")]
pub(crate) struct EngineTrace {
    live: core::sync::atomic::AtomicBool,
}

#[cfg(feature = "storage-trace")]
impl EngineTrace {
    /// A trace that records the bring-up to come.
    pub(crate) const fn new() -> Self {
        Self {
            live: core::sync::atomic::AtomicBool::new(true),
        }
    }

    /// Put `record` on the console while the bring-up runs.
    pub(crate) fn record(&self, record: Trace) {
        use core::sync::atomic::Ordering;

        if !self.live.load(Ordering::Relaxed) {
            return;
        }
        if matches!(record, Trace::Ready(_)) {
            self.live.store(false, Ordering::Relaxed);
        }
        render(record);
    }
}

/// One line per engine record.
#[cfg(feature = "storage-trace")]
fn render(record: Trace) {
    let [mut a, mut b] = [[0u8; 16]; 2];
    match record {
        Trace::Attempt(rung) => line("emmc2 trace: attempt", &[text("rung", rung.as_str())]),
        Trace::Stage(stage) => line("emmc2 trace: stage", &[text("stage", stage.as_str())]),
        Trace::Controller {
            version,
            caps,
            caps1,
            max_current,
        } => controller(version, caps, caps1, max_current),
        Trace::BaseClock { hz, from_board } => line(
            "emmc2 trace: base clock",
            &[unsigned("hz", hz), flag("from_board", from_board)],
        ),
        Trace::Clock {
            target_hz,
            select,
            hz,
        } => line(
            "emmc2 trace: sd clock",
            &[
                unsigned("target_hz", target_hz),
                hex("select_hex", select.into(), &mut a),
                unsigned("hz", hz),
            ],
        ),
        Trace::Command { index, arg } => line(
            "emmc2 trace: command",
            &[unsigned("cmd", index), hex("arg_hex", arg.into(), &mut a)],
        ),
        Trace::Response { index, response } => line(
            "emmc2 trace: response",
            &[
                unsigned("cmd", index),
                hex("response_hex", response.into(), &mut a),
            ],
        ),
        Trace::WaitFailed {
            register,
            wanted,
            value,
            waits,
            end,
        } => wait_failed(register, wanted, value, waits, end),
        Trace::Ocr(ocr) => line("emmc2 trace: ocr", &[hex("ocr_hex", ocr.into(), &mut a)]),
        Trace::Scr(scr) => line(
            "emmc2 trace: scr",
            &[hex("scr_hex", u64::from_be_bytes(scr), &mut a)],
        ),
        Trace::Switch {
            access_modes,
            current_limits,
            access_mode,
        } => line(
            "emmc2 trace: switch status",
            &[
                hex("access_modes_hex", access_modes.into(), &mut a),
                hex("current_limits_hex", current_limits.into(), &mut b),
                unsigned("access_mode", access_mode),
            ],
        ),
        Trace::Dma {
            data,
            table,
            stage_blocks,
        } => line(
            "emmc2 trace: dma staging",
            &[
                hex("data_device_hex", data, &mut a),
                hex("table_device_hex", table, &mut b),
                unsigned("stage_blocks", stage_blocks),
            ],
        ),
        Trace::DmaMismatch { offset, port, dma } => line(
            "emmc2 trace: dma verify mismatch",
            &[
                unsigned("offset", offset),
                hex("port_hex", port.into(), &mut a),
                hex("dma_hex", dma.into(), &mut b),
            ],
        ),
        Trace::Ready(link) => line(
            "emmc2 trace: ready",
            &[
                text("mode", link.mode.as_str()),
                unsigned("clock_hz", link.clock_hz),
                flag("dma", link.dma),
            ],
        ),
    }
}

/// The controller's version, capability and maximum-current registers.
#[cfg(feature = "storage-trace")]
fn controller(version: u32, caps: u32, caps1: u32, max_current: u32) {
    let [mut a, mut b, mut c, mut d] = [[0u8; 16]; 4];
    line(
        "emmc2 trace: controller",
        &[
            hex("version_hex", version.into(), &mut a),
            hex("caps_hex", caps.into(), &mut b),
            hex("caps1_hex", caps1.into(), &mut c),
            hex("max_current_hex", max_current.into(), &mut d),
        ],
    );
}

/// A wait on a controller register that ended without what it wanted.
#[cfg(feature = "storage-trace")]
fn wait_failed(register: usize, wanted: u32, value: u32, waits: u32, end: WaitEnd) {
    let [mut a, mut b] = [[0u8; 16]; 2];
    line(
        "emmc2 trace: wait failed",
        &[
            unsigned("register", register),
            hex("wanted_hex", wanted.into(), &mut a),
            hex("value_hex", value.into(), &mut b),
            unsigned("waits", waits),
            text("end", end.as_str()),
        ],
    );
}
