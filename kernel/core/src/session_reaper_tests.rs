//! The reaper's queue: what it takes, and in what order it ends it.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::ProcId;
use tairix_kernel_sec::CapTable;
use tairix_sync::{RwLock, SpinLock};

use super::SessionReaper;
use crate::test_sink::TestSink;

fn table() -> &'static RwLock<CapTable> {
    Box::leak(Box::new(RwLock::new(CapTable::new())))
}

fn reaper(caps: &'static RwLock<CapTable>) -> SessionReaper {
    SessionReaper {
        caps,
        audit: Box::leak(Box::new(TestSink::new())),
        ending: SpinLock::new(VecDeque::new()),
        serving: AtomicBool::new(false),
    }
}

fn anchor(byte: u8) -> ProcId {
    ProcId::from_raw([byte; 16])
}

#[test]
fn nothing_is_handed_to_a_reaper_that_cannot_yet_be_woken() {
    let caps = table();
    let reaper = reaper(caps);
    assert!(
        !reaper.queue(caps, anchor(1)),
        "the walk stays where it ended"
    );
    assert_eq!(reaper.next(), None);
}

#[test]
fn a_serving_reaper_ends_its_own_tables_sessions_oldest_first() {
    let caps = table();
    let reaper = reaper(caps);
    reaper.serving.store(true, Ordering::Release);
    assert!(reaper.queue(caps, anchor(1)));
    assert!(reaper.queue(caps, anchor(2)));
    assert_eq!(reaper.next(), Some(anchor(1)));
    assert_eq!(reaper.next(), Some(anchor(2)));
    assert_eq!(reaper.next(), None);
}

#[test]
fn a_session_over_another_table_is_left_where_it_ended() {
    let reaper = reaper(table());
    reaper.serving.store(true, Ordering::Release);
    assert!(!reaper.queue(table(), anchor(1)));
    assert_eq!(reaper.next(), None);
}
