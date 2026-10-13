//! Deterministic fuzz harness for `lib/virtio_input`: everything a hostile
//! device writes reaches it — the answers to its configuration queries (the
//! wheel and multi-touch probes) and its event stream (keys, relative motion,
//! both wheel resolutions, the multi-touch slot protocol, dropped frames),
//! with short and failed completions among them.
//!
//! Held to: no input panics; a device either opens or is refused; a drain
//! never reports more than it was given room for; no touch frame carries more
//! contacts than a frame holds, nor one contact twice; and the frame a
//! surface's driver sends when the device stops reporting leaves nothing down.
//!
//! The device double speaks the virtio 1.1 §5.8 layout itself, and draws its
//! event codes from those the Linux `input-event-codes.h` namespaces name,
//! which the device logic keeps private. A plain `cargo test` runs the
//! [`SMOKE_ITERATIONS`] sweep once from a fresh, logged seed; `cargo xtask
//! fuzz` extends it to a wall-clock budget.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use tairix_abi::driver::input::{InputEvent, InputEventKind};
use tairix_abi::touch::{ContactPhase, TOUCH_CONTACTS_MAX};
use tairix_fuzzseed::Prng;
use tairix_virtio::{ChainView, MockHost, MockTransport, VirtioError};
use tairix_virtio_input::{Report, VirtioInput};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// The configuration window (virtio 1.1 §5.8.4): `select`, `subsel`, `size`,
/// five reserved bytes, then the answer union.
const CFG_SELECT: usize = 0;
const CFG_SIZE: usize = 2;
const CFG_ANSWER: usize = 8;
const CFG_ANSWER_LEN: usize = 128;

/// The event queue and the bytes of one event (virtio 1.1 §5.8.2, §5.8.6).
const EVENT_QUEUE: u16 = 0;
const EVENT_LEN: usize = 8;

/// `EV_SYN`, `EV_KEY`, `EV_REL` and `EV_ABS`, and the codes of each the
/// device logic acts on: frame ends and drops; keys, touchpad buttons and the
/// gear buttons a wheel is reported through; motion and both wheel
/// resolutions; the multi-touch slot protocol.
const CODES: [(u16, &[u16]); 4] = [
    (0x00, &[0x00, 0x03]),
    (0x01, &[30, 0x110, 0x111, 0x112, 0x14A, 0x150, 0x151]),
    (0x02, &[0x00, 0x01, 0x06, 0x08, 0x0B, 0x0C]),
    (0x03, &[0x00, 0x01, 0x2F, 0x35, 0x36, 0x37, 0x39]),
];

/// The configuration selectors the device logic asks: `PROP_BITS`,
/// `EV_BITS` and `ABS_INFO`.
const SELECTORS: [u8; 3] = [0x10, 0x11, 0x12];

/// A bitmap answer setting every code through the gear buttons.
const BITMAP_LEN: usize = 64;

/// One `struct virtio_input_event`, drawn from the codes the logic acts on,
/// edge values, and noise.
fn event(rng: &mut Prng) -> (u16, u16, i32) {
    let (kind, code) = if rng.below(8) == 0 {
        (rng.next_u16(), rng.next_u16())
    } else {
        let (kind, codes) = *rng.pick(&CODES);
        let code = if rng.below(6) == 0 {
            rng.next_u16() % 0x300
        } else {
            *rng.pick(codes)
        };
        (kind, code)
    };
    let value = match rng.below(6) {
        0 => 0,
        1 => 1,
        2 => -1,
        3 => *rng.pick(&[i32::MIN, i32::MAX, i32::from(u16::MAX), 9]),
        4 => i32::from(rng.next_u16() % 64) - 8,
        _ => rng.next_u32().cast_signed(),
    };
    (kind, code, value)
}

/// Answer the configuration query in `window`: a selector the logic asks
/// about answered with full bitmaps, ranges or noise, anything else with
/// nothing or noise, at a stated size that may exceed what the union holds.
fn answer(rng: &mut Prng, window: &mut [u8]) {
    let asked = window.get(CFG_SELECT).copied().unwrap_or(0);
    let Some(union) = window.get_mut(CFG_ANSWER..CFG_ANSWER + CFG_ANSWER_LEN) else {
        return;
    };
    union.fill(0);
    let size = if SELECTORS.contains(&asked) && rng.below(4) != 0 {
        match rng.below(3) {
            0 => {
                union[..BITMAP_LEN].fill(0xFF);
                BITMAP_LEN
            }
            1 => {
                rng.fill(&mut union[..20]);
                20
            }
            _ => {
                let len = rng.at_most(CFG_ANSWER_LEN);
                rng.fill(&mut union[..len]);
                len
            }
        }
    } else {
        rng.below(3).saturating_sub(1) * rng.at_most(255)
    };
    if let Some(stated) = window.get_mut(CFG_SIZE) {
        *stated = u8::try_from(size).unwrap_or(u8::MAX);
    }
}

/// The events a device has still to deliver.
type Pending = Rc<RefCell<VecDeque<(u16, u16, i32)>>>;

/// A device that answers its configuration from `rng` and delivers what is
/// pending, one event to a posted buffer, completing some short and failing
/// some outright.
fn device(rng: &mut Prng) -> (MockTransport, Pending) {
    let queue_max = *rng.pick(&[1, 2, 8, 64, 256]);
    let mut transport = MockTransport::new(2, queue_max, 0, CFG_ANSWER + CFG_ANSWER_LEN);
    let mut answers = Prng::new(rng.next_u64());
    transport.install_config_responder(Box::new(move |window: &mut [u8]| {
        answer(&mut answers, window);
    }));
    let pending: Pending = Rc::new(RefCell::new(VecDeque::new()));
    let delivered = Rc::clone(&pending);
    let mut completions = Prng::new(rng.next_u64());
    transport.install_shim(
        EVENT_QUEUE,
        Box::new(move |chain: &mut ChainView<'_>| {
            let into = chain
                .device_write
                .first_mut()
                .ok_or(VirtioError::DeviceFault)?;
            if completions.below(64) == 0 {
                return Err(VirtioError::DeviceFault);
            }
            let Some((kind, code, value)) = delivered.borrow_mut().pop_front() else {
                return Ok(0);
            };
            let mut bytes = [0u8; EVENT_LEN];
            bytes[0..2].copy_from_slice(&kind.to_le_bytes());
            bytes[2..4].copy_from_slice(&code.to_le_bytes());
            bytes[4..8].copy_from_slice(&value.to_le_bytes());
            let written = if completions.below(32) == 0 {
                completions.at_most(EVENT_LEN)
            } else {
                EVENT_LEN
            }
            .min(into.len());
            into[..written].copy_from_slice(&bytes[..written]);
            Ok(u32::try_from(written).unwrap_or(0))
        }),
    );
    (transport, pending)
}

/// Hold every report a drain gave to what a frame may carry.
fn check(reports: &[Report]) {
    for report in reports {
        let Report::Touch(frame) = report else {
            continue;
        };
        let contacts = frame.contacts();
        assert!(contacts.len() <= TOUCH_CONTACTS_MAX, "{frame:?}");
        for (at, contact) in contacts.iter().enumerate() {
            assert!(
                contacts[..at].iter().all(|other| other.id != contact.id),
                "a contact reported twice in {frame:?}"
            );
        }
    }
}

#[test]
fn a_hostile_device_is_decoded_or_refused_and_never_overreports() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_hostile_device_is_decoded_or_refused_and_never_overreports",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let blank = Report::Event(InputEvent {
        kind: InputEventKind::Key,
        reserved0: 0,
        code: 0,
        value: 0,
    });
    let mut iteration: u64 = 0;
    loop {
        let (transport, pending) = device(&mut rng);
        let shared = transport.into_shared();
        let host = MockHost::new();
        host.attach(&shared);
        if let Ok(mut input) = VirtioInput::open(Rc::clone(&shared), &host) {
            let mut reports = [blank; 32];
            for _ in 0..rng.at_most(12) {
                let burst = rng.at_most(80);
                pending
                    .borrow_mut()
                    .extend((0..burst).map(|_| event(&mut rng)));
                let room = 1 + rng.below(reports.len());
                if let Ok(got) = input.poll_reports(&mut reports[..room]) {
                    assert!(got <= room, "{got} reports in room for {room}");
                    check(&reports[..got]);
                }
            }
            if let Some(lifted) = input.touch_lifted() {
                assert!(
                    lifted
                        .contacts()
                        .iter()
                        .all(|contact| contact.phase == ContactPhase::Up),
                    "a contact left down: {lifted:?}"
                );
            }
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
