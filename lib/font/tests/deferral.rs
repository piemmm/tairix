//! A thread that draws without waiting must never queue behind a fetch in
//! flight, which holds the font channel for its whole round trip.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use tairix_abi::font_ipc::FamilyKey;
use tairix_abi::Errno;
use tairix_font::{
    defer_fetches, fetch_deferred, has_deferred, set_font_transport, BitmapFont, FontTransport,
    SolidTestTransport,
};

/// Far beyond what measuring a word costs, and short of a hung test.
const DRAW_BOUND: Duration = Duration::from_secs(30);

/// A transport that parks inside its call until the test opens its gate.
struct Stalled {
    entered: Sender<()>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl FontTransport for Stalled {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let _ = self.entered.send(());
        let (open, opened) = &*self.gate;
        let mut open = open.lock().expect("the gate");
        while !*open {
            open = opened.wait(open).expect("the gate");
        }
        SolidTestTransport.call(request, reply)
    }
}

#[test]
fn a_deferring_draw_never_waits_for_a_fetch_in_flight() {
    let font = BitmapFont::new(FamilyKey::new("inter").expect("a family key"), 20);
    defer_fetches();
    let (entered, inside) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    set_font_transport(Box::new(Stalled {
        entered,
        gate: Arc::clone(&gate),
    }));

    assert!(font.text_width("A") > 0, "measured provisionally");
    assert!(has_deferred(), "the miss is recorded, not fetched");
    let fetcher = thread::spawn(fetch_deferred);
    inside
        .recv_timeout(DRAW_BOUND)
        .expect("the fetch reaches the service");

    let (drew, drawn) = mpsc::channel();
    let drawer = thread::spawn(move || {
        let _ = drew.send(font.text_width("Hello"));
    });
    let measured = drawn.recv_timeout(DRAW_BOUND);

    let (open, opened) = &*gate;
    *open.lock().expect("the gate") = true;
    opened.notify_all();
    fetcher.join().expect("the fetch ends");
    drawer.join().expect("the draw ends");
    assert!(
        measured.is_ok(),
        "a deferring draw waited for the fetch in flight"
    );
}
