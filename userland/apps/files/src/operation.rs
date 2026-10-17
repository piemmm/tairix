//! What an event to an operation's own progress window means, how often that
//! window shows a new count, and the words a refusal is stated in.
//!
//! # Why this is its own module
//!
//! The `Run` binary around it is a freestanding program — it only exists when
//! the crate is built for a bare-metal target — so nothing inside it can be
//! reached by a host test. Which press cancels a running operation is worth
//! testing: it is a pure function of the event and the window's client, so it
//! lives here, compiles on the host, and is covered by the tests beside it.
//!
//! # No I/O, and no authority
//!
//! Nothing here touches the operation, the filesystem, or the window. It
//! classifies one event and the program acts on the answer. Nothing classifies
//! the folder window's own input for an operation: it is a window of its own,
//! and the folder goes on being used while it runs.

use alloc::string::String;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_abi::window_ipc::WindowEvent;
use tairix_abi::Errno;
use tairix_browse::render::progress_cancel_at;
use tairix_browse::DeletePlan;
use tairix_geometry::{Rect, Scale};
use tairix_theme::{Theme, Timeline};

use crate::sidebar::press_point;

/// What an event to an operation's progress window asks of the operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProgressControl {
    /// The user asked to cancel (Escape, or a click on the Cancel button).
    Cancel,
    /// The desktop needs the window's pixels again: it is presented only as
    /// its count moves, which a stalled operation may not do.
    Redraw,
    /// Nothing that affects the operation.
    Ignore,
}

/// Classify `event` to an operation's progress window, whose client is
/// `client`: a cancel (Escape, or a primary press on the drawn Cancel
/// button), a redraw, or nothing.
#[must_use]
pub fn progress_control(
    client: Rect,
    scale: Scale,
    theme: &Theme,
    event: &WindowEvent,
) -> ProgressControl {
    match event {
        WindowEvent::Key {
            key:
                KeyInput::Pressed {
                    key: KeyValue::Named(NamedKeyCode::Escape),
                    ..
                },
            ..
        } => ProgressControl::Cancel,
        WindowEvent::Pointer { x, y, action, .. } => match press_point(*action, *x, *y) {
            Some(point) if progress_cancel_at(client, scale, theme, point) => {
                ProgressControl::Cancel
            }
            _ => ProgressControl::Ignore,
        },
        WindowEvent::RedrawRequested { .. } => ProgressControl::Redraw,
        _ => ProgressControl::Ignore,
    }
}

/// Whether a permission flip may be computed from the mode a Properties window
/// shows: not while its last write is in flight, nor once that lands until the
/// node is read again. Until then the window shows the mode as it was, and a
/// flip of that would quietly undo the write before it.
#[must_use]
pub const fn mode_flip_allowed(writing: bool, rereading: bool) -> bool {
    !writing && !rereading
}

/// Whether a new count is shown now, at `now_ns`: once a desktop frame since
/// the progress window last showed one at `shown_ns`.
#[must_use]
pub const fn progress_due(now_ns: u64, shown_ns: u64) -> bool {
    now_ns.saturating_sub(shown_ns) >= Timeline::FRAME_NS
}

/// Whether a delete confirmation for `asked`, whose plan has just come back,
/// may open: only over the very selection it plans, `planned_now`, in a
/// window with nothing else open (`busy`). A confirmation opened over anything
/// else would take a key meant for that as its answer.
#[must_use]
pub fn confirmation_may_open(
    asked: &DeletePlan,
    planned_now: Option<&DeletePlan>,
    busy: bool,
) -> bool {
    !busy && planned_now == Some(asked)
}

/// Whether a confirmation to empty `trash`, whose contents have just been
/// read, may open: only in a window still showing it, `shown`, with nothing
/// else open (`busy`).
#[must_use]
pub fn empty_confirmation_may_open(trash: &[String], shown: &[String], busy: bool) -> bool {
    !busy && trash == shown
}

/// The words a refusal is stated in, naming the item by what the user sees,
/// the last component of its `path`: "could not delete “Notes”: its volume is
/// read-only".
#[must_use]
pub fn item_refusal(verb: &str, path: &[impl AsRef<str>], reason: &str) -> String {
    let name = path.last().map_or("", AsRef::as_ref);
    alloc::format!("could not {verb} \u{201c}{name}\u{201d}: {reason}")
}

/// Why a copy could not make its destination file or folder, from the
/// kernel's answer.
#[must_use]
pub const fn create_refusal(error: Errno) -> &'static str {
    match error {
        Errno::AlreadyExists => "a destination of that name already exists",
        Errno::NoSpace => "the destination is full",
        Errno::PermissionDenied => "the destination folder may not be written to",
        Errno::ReadOnly => "the destination volume is read-only",
        _ => device_refusal(error, "the destination could not be created"),
    }
}

/// Why an item could not be removed, from the kernel's answer.
#[must_use]
pub const fn remove_refusal(error: Errno) -> &'static str {
    match error {
        Errno::PermissionDenied => "it may not be removed",
        Errno::ReadOnly => "its volume is read-only",
        Errno::NotEmpty => "a folder in it was not empty",
        Errno::NotFound => "it was no longer there",
        _ => device_refusal(error, "it could not be removed"),
    }
}

/// Why an item could not be renamed into place, from the kernel's answer.
#[must_use]
pub const fn move_refusal(error: Errno) -> &'static str {
    match error {
        Errno::AlreadyExists => "an item with that name appeared at the destination",
        Errno::PermissionDenied => "it may not be moved",
        Errno::ReadOnly => "its volume is read-only",
        Errno::NotFound => "it was no longer there",
        _ => device_refusal(error, "it could not be moved"),
    }
}

/// Why the Trash could not be read to be emptied, from the kernel's answer.
#[must_use]
pub const fn trash_read_refusal(error: Errno) -> &'static str {
    match error {
        Errno::PermissionDenied => "the Trash may not be read",
        Errno::NotFound => "there is no Trash folder",
        _ => device_refusal(error, "the Trash could not be read"),
    }
}

/// Why a change to an item's permissions, owner or attributes was refused,
/// from the kernel's answer.
#[must_use]
pub const fn change_refusal(error: Errno) -> &'static str {
    match error {
        Errno::PermissionDenied => "you may not change it",
        Errno::ReadOnly => "its volume is read-only",
        Errno::NotFound => "it was no longer there",
        Errno::NotSupported => "its volume cannot store that",
        Errno::NoSpace => "its volume has no room for it",
        _ => device_refusal(error, "the volume refused it"),
    }
}

/// Why a folder could not be listed, from the kernel's answer.
#[must_use]
pub const fn read_refusal(error: Errno) -> &'static str {
    match error {
        Errno::PermissionDenied => "a folder in it may not be read",
        Errno::NotFound => "a folder in it was no longer there",
        _ => device_refusal(error, "a folder in it could not be read"),
    }
}

/// The device's own failure, which reads the same whatever was asked of it,
/// or `otherwise`.
pub(crate) const fn device_refusal(error: Errno, otherwise: &'static str) -> &'static str {
    match error {
        Errno::DeviceOffline => "its device was removed",
        Errno::DeviceFault | Errno::MediumError => "its device failed",
        _ => otherwise,
    }
}

#[cfg(test)]
#[path = "operation_tests.rs"]
mod tests;
