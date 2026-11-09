//! The desktop session's on-screen announcements a freestanding QEMU
//! integration kernel's PASS witnesses gate on.
//!
//! Only the session knows that a served window's first frame, or a menu
//! chain's plates, reached the display, and it states each in a record of its
//! own. The kernel delivers what userland emits to a guest's diagnostic sink;
//! the records' ids are generated from the session's own definitions, so a
//! guest links none of the desktop.
//!
//! Test scaffolding: nothing in TAIRiX itself links it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::sync::atomic::{AtomicU64, Ordering};

use tairix_log::Event;

mod ids {
    include!(concat!(env!("OUT_DIR"), "/announcements.rs"));
}

/// The window `event` announces is on screen: the session's record of a
/// served window's first frame reaching the display, by the window's id.
#[must_use]
pub fn window_shown(event: &Event<'_>) -> Option<u64> {
    if event.id.0 != ids::WINDOW_SHOWN {
        return None;
    }
    tairix_itest_witness::field_u64(event, "window").filter(|&window| window != 0)
}

/// Whether `event` is the session's record of a menu chain's plates reaching
/// the display.
#[must_use]
pub fn menu_shown(event: &Event<'_>) -> bool {
    event.id.0 == ids::MENU_SHOWN
}

/// A floor no window is above: the tally is not armed.
const UNARMED: u64 = u64::MAX;

/// The windows the desktop announces on screen once the tally is armed, up to
/// `N`: each counted the first time it is announced, and only above every
/// window announced before arming.
///
/// So a window shown again — its content released and restored — counts
/// once, and one already up when the tally was armed not at all. Ids grow
/// with each connection and each window a connection names, so a window a
/// client that connected later creates is always above the floor.
pub struct ShownWindows<const N: usize> {
    /// The highest window announced.
    highest: AtomicU64,
    /// The highest announced before arming, or [`UNARMED`].
    floor: AtomicU64,
    /// The windows counted, in the order first announced; zero is free.
    seen: [AtomicU64; N],
}

impl<const N: usize> ShownWindows<N> {
    /// A tally not yet armed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            highest: AtomicU64::new(0),
            floor: AtomicU64::new(UNARMED),
            seen: [const { AtomicU64::new(0) }; N],
        }
    }

    /// Arm the tally above every window announced so far. Arming again moves
    /// nothing.
    pub fn arm(&self) {
        let floor = self.highest.load(Ordering::Acquire);
        let _ = self
            .floor
            .compare_exchange(UNARMED, floor, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Note the desktop's announcement that window `window` is on screen,
    /// answering how many windows have been counted since arming.
    pub fn note(&self, window: u64) -> usize {
        self.highest.fetch_max(window, Ordering::AcqRel);
        if window > self.floor.load(Ordering::Acquire) {
            for slot in &self.seen {
                match slot.compare_exchange(0, window, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => break,
                    Err(held) if held == window => break,
                    Err(_) => {}
                }
            }
        }
        self.count()
    }

    /// How many windows have been counted since arming.
    #[must_use]
    pub fn count(&self) -> usize {
        self.seen
            .iter()
            .filter(|slot| slot.load(Ordering::Acquire) != 0)
            .count()
    }
}

impl<const N: usize> Default for ShownWindows<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{ids, menu_shown, window_shown, ShownWindows};
    use tairix_log::{Event, EventId, Field, FieldValue, Level};

    /// A record `id` naming `window`, under the kernel's stamp of its task.
    fn record(id: u32, window: Option<u64>, run: impl FnOnce(&Event<'_>)) {
        let mut fields = [
            Field {
                key: "task",
                value: FieldValue::UnsignedInt(7),
            },
            Field {
                key: "window",
                value: FieldValue::UnsignedInt(0),
            },
        ];
        let fields = match window {
            Some(window) => {
                fields[1].value = FieldValue::UnsignedInt(window);
                &fields[..]
            }
            None => &fields[..1],
        };
        run(&Event {
            level: Level::Info,
            id: EventId(id),
            message: "",
            fields,
        });
    }

    #[test]
    fn a_window_shown_record_names_its_window() {
        record(ids::WINDOW_SHOWN, Some(0x3_0000_0001), |event| {
            assert_eq!(window_shown(event), Some(0x3_0000_0001));
        });
    }

    #[test]
    fn another_record_or_one_naming_no_window_names_none() {
        record(ids::MENU_SHOWN, Some(0x3_0000_0001), |event| {
            assert_eq!(window_shown(event), None);
            assert!(menu_shown(event));
        });
        record(ids::WINDOW_SHOWN, None, |event| {
            assert_eq!(window_shown(event), None);
            assert!(!menu_shown(event));
        });
        record(ids::WINDOW_SHOWN, Some(0), |event| {
            assert_eq!(window_shown(event), None);
        });
    }

    #[test]
    fn an_unarmed_tally_counts_nothing() {
        let shown = ShownWindows::<4>::new();
        assert_eq!(shown.note(5), 0);
        assert_eq!(shown.note(6), 0);
    }

    #[test]
    fn each_window_counts_once_however_often_it_is_shown() {
        let shown = ShownWindows::<4>::new();
        shown.arm();
        assert_eq!(shown.note(0x1_0000_0001), 1);
        assert_eq!(shown.note(0x1_0000_0001), 1, "shown again");
        assert_eq!(shown.note(0x1_0000_0002), 2);
        assert_eq!(shown.note(0x2_0000_0001), 3, "a later connection's");
    }

    /// A window up before the tally was armed, shown again after it, is not
    /// one the armed act opened.
    #[test]
    fn a_window_up_before_arming_does_not_count_when_shown_again() {
        let shown = ShownWindows::<4>::new();
        assert_eq!(shown.note(0x1_0000_0003), 0);
        shown.arm();
        assert_eq!(shown.note(0x1_0000_0003), 0);
        assert_eq!(shown.note(0x1_0000_0002), 0, "older still");
        assert_eq!(shown.note(0x1_0000_0004), 1);
    }

    #[test]
    fn arming_again_moves_nothing() {
        let shown = ShownWindows::<4>::new();
        shown.arm();
        assert_eq!(shown.note(9), 1);
        shown.arm();
        assert_eq!(shown.note(10), 2);
    }

    #[test]
    fn the_tally_holds_at_most_its_capacity() {
        let shown = ShownWindows::<2>::new();
        shown.arm();
        assert_eq!(shown.note(1), 1);
        assert_eq!(shown.note(2), 2);
        assert_eq!(shown.note(3), 2);
    }
}
