//! Deterministic fuzz harness for the served device: the software device
//! served on a loopback channel and reached as a client process reaches a
//! GPU's driver (`gpu-v1`).
//!
//! Four sweeps:
//!
//! - `buffers`, `images` and `work`: the submission sweeps, run on a served
//!   device and held to the references they are held to on the device
//!   itself, so every refusal, byte and loss crosses the channel as the
//!   device gave it.
//! - `requests`: what a served round's client sent, sent again to a fresh
//!   device by it and by a second client, each frame and the region bytes
//!   written before it kept or changed: cut short, grown, bytes flipped,
//!   fields set to their edges, the op changed, region bytes flipped or
//!   moved, steps dropped and repeated. Each run first sends a fixed session
//!   waiting on its timeline whole and then misuses its wait, so every kind
//!   of answer is reached whatever the sweep draws.
//!
//! The invariants are:
//!
//! 1. Nothing panics, whatever is sent.
//! 2. Every request is answered exactly once by a reply naming its op — a
//!    held wait once its value lands, its context ends, its client goes or
//!    its timeout passes — and a frame naming no op by the shared status
//!    reply.
//! 3. Until the first change, a journal sent again is answered byte for byte
//!    as it was.
//! 4. Once every client has gone, the device's heap holds nothing a client
//!    made.

#[path = "fuzz_gpu/buffers.rs"]
mod buffers;
#[path = "fuzz_gpu/common.rs"]
mod common;
#[path = "fuzz_gpu/images.rs"]
mod images;
#[path = "fuzz_gpu/shaders.rs"]
mod shaders;
#[path = "fuzz_gpu/work.rs"]
mod work;

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use std::vec::Vec;

use common::{below, draw_word_extent, index_of, sweep, MEMORY};
use tairix_abi::gpu::{
    decode_reply, GpuOp, GpuRequest, GpuStatus, GPU_HEADER_LEN, GPU_MAX_REQUEST,
    MAX_CLIENT_CONTEXTS,
};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::Duration64;
use tairix_fuzzseed::Prng;
use tairix_gpu::testing::{Answered, Loopback, Step};
use tairix_gpu::{ContextDesc, Device};
use tairix_parallel::JobRunner;

/// The region each client grants: room for every module the sweeps make.
const REGION: usize = 1 << 20;

/// The client a journal is sent again by, beside the one that sent it.
const OTHER: u8 = 2;

/// Rounds of the request sweep between looks at the budget.
const REQUEST_ROUNDS: u32 = 4;

/// How far the clock moves at each reading once a journal is changed: work a
/// change made endless, a pass of billions of instances say, stops within as
/// many readings as its deadline has seconds.
const CHANGED_CLOCK_STEP: u64 = 1_000_000_000;

/// A served software device whose room for contexts is already what two
/// clients at their bound take, so a round that leaves the heap above where
/// it starts left something a client made.
fn loopback(runner: Arc<dyn JobRunner + Send>) -> Arc<Loopback> {
    let loopback = Loopback::new(MEMORY, runner).expect("a served device");
    let device = loopback.device();
    let contexts: Vec<tairix_gpu::Context> = (0..2 * MAX_CLIENT_CONTEXTS)
        .map(|_| {
            device
                .create_context(&ContextDesc::default())
                .expect("a context")
        })
        .collect();
    drop(contexts);
    loopback
}

/// A client of a served device of its own.
fn served(runner: Arc<dyn JobRunner + Send>) -> Device {
    loopback(runner).connect(1, REGION).expect("connected")
}

#[test]
fn served_submissions_agree_with_a_byte_array_reference() {
    sweep(
        "served_submissions_agree_with_a_byte_array_reference",
        buffers::ROUNDS,
        |rng| buffers::run_round(rng, &served),
    );
}

#[test]
fn served_images_and_their_transfers_agree_with_a_texel_reference() {
    sweep(
        "served_images_and_their_transfers_agree_with_a_texel_reference",
        images::ROUNDS,
        |rng| images::run_round(rng, &served),
    );
}

#[test]
fn served_passes_draws_and_dispatches_agree_with_their_rules_and_their_twins() {
    let mut worlds = work::Worlds::new(&served);
    sweep(
        "served_passes_draws_and_dispatches_agree_with_their_rules_and_their_twins",
        work::ROUNDS,
        |rng| worlds.run_round(rng),
    );
    worlds.exercised();
}

/// What a served device's client sent.
struct Journal {
    /// The width of the runner the device shared its work across.
    width: usize,
    /// Who granted each region, and its bytes, in grant order.
    grants: Vec<(u8, usize)>,
    steps: Vec<Step>,
}

/// What one served round's clients sent.
fn journals(rng: &mut Prng) -> Vec<Journal> {
    let loopbacks = RefCell::new(Vec::new());
    let connect = |runner: Arc<dyn JobRunner + Send>| {
        let width = runner.width();
        let loopback = loopback(runner);
        let client = loopback.connect(1, REGION).expect("connected");
        loopbacks.borrow_mut().push((width, loopback));
        client
    };
    match rng.below(4) {
        0 | 1 => buffers::run_round(rng, &connect),
        2 => images::run_round(rng, &connect),
        _ => work::Worlds::new(&connect).run_round(rng),
    }
    loopbacks
        .into_inner()
        .into_iter()
        .map(|(width, loopback)| Journal {
            width,
            grants: loopback.grants(),
            steps: loopback.take_journal(),
        })
        .collect()
}

/// A session waiting on a timeline its context made, a value it has not
/// reached: the journal every request sweep holds a wait from, whatever it
/// draws.
fn waiting_session() -> Journal {
    let runner = runner(1);
    let width = runner.width();
    let loopback = loopback(runner);
    {
        let client = loopback.connect(1, REGION).expect("connected");
        let context = client
            .create_context(&ContextDesc::default())
            .expect("a context");
        let timeline = context.create_timeline(0).expect("a timeline");
        assert_eq!(
            timeline.wait(1, Duration64::ZERO),
            Err(tairix_gpu::Error::TimedOut)
        );
    }
    Journal {
        width,
        grants: loopback.grants(),
        steps: loopback.take_journal(),
    }
}

/// A runner as wide as `width`.
fn runner(width: usize) -> Arc<dyn JobRunner + Send> {
    if width <= 1 {
        Arc::new(tairix_parallel::Serial)
    } else {
        Arc::new(tairix_parallel::Threaded::new(width))
    }
}

/// What the request sweep reached: frames naming no op, refusals by the
/// device, waits held, journals answered as they were, and frames another
/// client sent.
#[derive(Debug, Default)]
struct Tally {
    unframed: usize,
    refused: usize,
    held: usize,
    replayed: usize,
    foreign: usize,
}

impl Tally {
    fn add(&mut self, other: &Self) {
        self.unframed += other.unframed;
        self.refused += other.refused;
        self.held += other.held;
        self.replayed += other.replayed;
        self.foreign += other.foreign;
    }

    /// Every kind of answer was reached.
    fn exercised(&self) {
        std::println!("requests exercised: {self:?}");
        assert!(
            self.unframed > 0
                && self.refused > 0
                && self.held > 0
                && self.replayed > 0
                && self.foreign > 0,
            "a kind of answer went unreached: {self:?}"
        );
    }
}

/// What every reply said, held to invariant 2.
#[derive(Default)]
struct Ledger {
    /// The tickets of waits the runtime holds.
    held: Vec<u64>,
    answered: HashSet<u64>,
    tally: Tally,
}

impl Ledger {
    /// Hold `answered`, `frame`'s answer, to invariant 2, answering its own
    /// reply where it was given at once.
    fn served(&mut self, frame: &[u8], answered: Answered) -> Option<Vec<u8>> {
        let mut own = None;
        for (ticket, reply) in answered.replies {
            if ticket == answered.ticket {
                assert!(
                    self.answered.insert(ticket),
                    "ticket {ticket} answered twice"
                );
                own = Some(reply);
            } else {
                self.settled(ticket, &reply);
            }
        }
        match GpuRequest::decode(frame) {
            Err(errno) => {
                let reply = own.as_deref().expect("a frame naming no op is answered");
                assert_eq!(decode_status_reply(reply), Err(errno), "{frame:02x?}");
                self.tally.unframed += 1;
            }
            Ok(request) => {
                if let Some(reply) = own.as_deref() {
                    let decoded = decode_reply(reply, request.op);
                    let Ok((status, _)) = decoded else {
                        panic!("{:?} answered {reply:02x?}", request.op);
                    };
                    self.tally.refused += usize::from(matches!(status, GpuStatus::Device(_)));
                } else {
                    assert_eq!(request.op, GpuOp::Wait, "only a wait is held");
                    self.held.push(answered.ticket);
                    self.tally.held += 1;
                }
            }
        }
        own
    }

    /// A held wait answered.
    fn settled(&mut self, ticket: u64, reply: &[u8]) {
        let at = self
            .held
            .iter()
            .position(|&held| held == ticket)
            .unwrap_or_else(|| panic!("ticket {ticket} answered but never held"));
        self.held.swap_remove(at);
        assert!(
            self.answered.insert(ticket),
            "ticket {ticket} answered twice"
        );
        assert!(
            decode_reply(reply, GpuOp::Wait).is_ok(),
            "a wait answered {reply:02x?}"
        );
    }

    fn settle_all(&mut self, replies: Vec<(u64, Vec<u8>)>) {
        for (ticket, reply) in replies {
            self.settled(ticket, &reply);
        }
    }
}

/// `frame`, changed one way, or kept.
fn change_frame(rng: &mut Prng, frame: &mut Vec<u8>) {
    match rng.below(6) {
        0 => {
            for _ in 0..=rng.below(4) {
                let at = rng.below(frame.len().max(1));
                if let Some(byte) = frame.get_mut(at) {
                    *byte ^= 1 << below(rng, 8);
                }
            }
        }
        1 => {
            let edges = [0u64, 1, 2, u64::from(u32::MAX), u64::MAX, rng.next_u64()];
            let edge = edges[rng.below(edges.len())];
            let width = if rng.below(2) == 0 { 4 } else { 8 };
            let words = frame.len().saturating_sub(GPU_HEADER_LEN) / width;
            if words > 0 {
                let at = GPU_HEADER_LEN + rng.below(words) * width;
                frame[at..at + width].copy_from_slice(&edge.to_le_bytes()[..width]);
            }
        }
        2 => frame.truncate(rng.below(frame.len() + 1)),
        3 => {
            let more = rng.below(GPU_MAX_REQUEST + 8 - frame.len().min(GPU_MAX_REQUEST) + 1);
            frame.extend((0..more).map(|_| rng.next_u8()));
        }
        4 => {
            if frame.len() >= GPU_HEADER_LEN {
                let op = u16::try_from(rng.below(40)).expect("small");
                frame[6..8].copy_from_slice(&op.to_le_bytes());
            }
        }
        _ => {
            if frame.len() >= 6 {
                frame[4..6]
                    .copy_from_slice(&u16::try_from(rng.below(3)).expect("small").to_le_bytes());
            }
        }
    }
}

/// A wait for a value its timeline has not reached: held until a later
/// signal lands or its timeout passes. A frame of any other op is kept.
fn raise_wait(rng: &mut Prng, frame: &mut [u8]) {
    if GpuRequest::decode(frame).map(|request| request.op) != Ok(GpuOp::Wait) {
        return;
    }
    // The value follows the context's and the timeline's words.
    let at = GPU_HEADER_LEN + 16;
    let Some(value) = frame.get_mut(at..at + 8) else {
        return;
    };
    let raised = u64::from_le_bytes((&*value).try_into().expect("eight bytes"))
        .saturating_add(1 + rng.below(4) as u64);
    value.copy_from_slice(&raised.to_le_bytes());
}

/// The region bytes written before a step, one changed.
fn change_writes(rng: &mut Prng, writes: &mut [(usize, Vec<u8>)]) {
    let count = writes.len();
    let Some((at, data)) = writes.get_mut(rng.below(count.max(1))) else {
        return;
    };
    if rng.below(2) == 0 {
        let byte = rng.below(data.len().max(1));
        if let Some(byte) = data.get_mut(byte) {
            *byte ^= 1 << below(rng, 8);
        }
    } else {
        *at = index_of(draw_word_extent(rng, REGION as u64));
    }
}

/// Send `step` from `proc`, region bytes first; a wait not answered at once
/// is answered by time running out where `block` says the client waits, as
/// its client would, and is otherwise left held.
fn send(
    loopback: &Loopback,
    ledger: &mut Ledger,
    (proc, grant): (u8, u64),
    (writes, frame): (&[(usize, Vec<u8>)], &[u8]),
    block: bool,
) -> Option<Vec<u8>> {
    for (at, data) in writes {
        loopback.write_region(grant, *at, data);
    }
    let answered = loopback.serve(proc, frame);
    let ticket = answered.ticket;
    let own = ledger.served(frame, answered);
    if own.is_some() || !block {
        return own;
    }
    // The soonest timeout may be found again first: two looks a wait at most.
    for _ in 0..=2 * ledger.held.len() {
        let replies = loopback.advance().expect("a held wait has a timeout");
        let mine = replies
            .iter()
            .find(|(answered, _)| *answered == ticket)
            .map(|(_, reply)| reply.clone());
        ledger.settle_all(replies);
        if mine.is_some() {
            return mine;
        }
    }
    panic!("a wait held past its timeout");
}

/// Send `journal` again to a fresh device, its frames kept or changed as
/// `rng` draws. Where `misuse` says, it is kept whole, and after its first
/// wait every kind of answer is reached at once: the wait raised past any
/// value its timeline reaches, which is held; sent by a client naming no
/// context of it, which is refused; and cut short of a header.
fn replay(rng: &mut Prng, journal: &Journal, tally: &mut Tally, misuse: bool) {
    let Journal {
        width,
        ref grants,
        ref steps,
    } = *journal;
    let loopback = loopback(runner(width));
    let idle = loopback.heap_taken();
    for &(proc, len) in grants {
        loopback.grant(proc, len);
    }
    let grant_of = |proc: u8| {
        grants
            .iter()
            .position(|&(owner, _)| owner == proc)
            .map_or(0, |at| at as u64 + 1)
    };
    let mut ledger = Ledger::default();
    let other = open_other(&loopback, &mut ledger);
    let mut changed = false;
    let mut misused = !misuse;
    // Kept whole a time in four; else kept up to a step and changed after.
    let first_change = if misuse || rng.below(4) == 0 {
        steps.len()
    } else {
        rng.below(steps.len() + 1)
    };
    for (index, step) in steps.iter().enumerate() {
        let changing = index >= first_change;
        let repeats = match rng.below(24) {
            0 if changing => {
                changed = true;
                0
            }
            1 if changing => 2,
            _ => 1,
        };
        for _ in 0..repeats {
            let mut frame = step.frame.clone();
            let mut writes = step.writes.clone();
            let mut proc = step.proc;
            match rng.below(16) {
                _ if !changing => {}
                0..=2 => change_frame(rng, &mut frame),
                3 => change_writes(rng, &mut writes),
                4 => proc = OTHER,
                5..=7 => raise_wait(rng, &mut frame),
                _ => {}
            }
            changed |= frame != step.frame || writes != step.writes || proc != step.proc;
            if changed {
                loopback.step_clock(CHANGED_CLOCK_STEP);
            }
            let grant = if proc == OTHER { other } else { grant_of(proc) };
            ledger.tally.foreign += usize::from(proc == OTHER);
            let block = !changed || rng.below(2) == 0;
            let reply = send(
                &loopback,
                &mut ledger,
                (proc, grant),
                (&writes, &frame),
                block,
            );
            if !changed {
                assert_eq!(
                    reply.as_deref(),
                    Some(&step.reply[..]),
                    "an unchanged journal is answered as it was"
                );
                ledger.tally.replayed += 1;
            }
            changed |= repeats > 1;
        }
        if !misused
            && GpuRequest::decode(&step.frame).is_ok_and(|request| request.op == GpuOp::Wait)
        {
            misused = true;
            misuse_wait(&loopback, &mut ledger, step, (grant_of(step.proc), other));
        }
    }
    assert!(misused, "a journal misused holds a wait");
    for _ in 0..=2 * ledger.held.len() {
        let Some(replies) = loopback.advance() else {
            break;
        };
        ledger.settle_all(replies);
    }
    for &(proc, _) in grants {
        ledger.settle_all(loopback.exit(proc));
    }
    ledger.settle_all(loopback.exit(OTHER));
    assert!(ledger.held.is_empty(), "every wait answered");
    tally.add(&ledger.tally);
    assert_eq!(
        loopback.heap_taken(),
        idle,
        "every client gone, nothing of theirs is held"
    );
}

/// Grant the second client its region and open it, answering its grant.
fn open_other(loopback: &Loopback, ledger: &mut Ledger) -> u64 {
    let other = loopback.grant(OTHER, REGION);
    let mut open = vec![0; GPU_MAX_REQUEST];
    let len = GpuRequest {
        op: GpuOp::Open,
        body: &other.to_le_bytes(),
    }
    .encode(&mut open)
    .expect("framed");
    let opened = send(loopback, ledger, (OTHER, other), (&[], &open[..len]), true);
    assert!(opened.is_some(), "the second client opens");
    other
}

/// After `step`, a wait its client's context answered: the wait raised past
/// any value its timeline reaches, which is held; sent by the second client,
/// which names no context of its own and is refused; and cut short of a
/// header.
fn misuse_wait(loopback: &Loopback, ledger: &mut Ledger, step: &Step, (grant, other): (u64, u64)) {
    let mut raised = step.frame.clone();
    raised[GPU_HEADER_LEN + 16..GPU_HEADER_LEN + 24].copy_from_slice(&u64::MAX.to_le_bytes());
    let held = send(loopback, ledger, (step.proc, grant), (&[], &raised), false);
    assert!(held.is_none(), "a wait for a value never reached is held");
    ledger.tally.foreign += 1;
    send(loopback, ledger, (OTHER, other), (&[], &step.frame), true);
    let cut = &step.frame[..GPU_HEADER_LEN - 1];
    send(loopback, ledger, (step.proc, grant), (&[], cut), true);
}

#[test]
fn served_requests_of_any_shape_are_answered_once_and_leave_nothing_behind() {
    let mut tally = Tally::default();
    // Every kind of answer, reached each run whatever the sweep draws; the
    // replay draws nothing from the generator it is lent.
    replay(&mut Prng::new(0), &waiting_session(), &mut tally, true);
    sweep(
        "served_requests_of_any_shape_are_answered_once_and_leave_nothing_behind",
        REQUEST_ROUNDS,
        |rng| {
            for journal in journals(rng) {
                replay(rng, &journal, &mut tally, false);
            }
        },
    );
    tally.exercised();
}
