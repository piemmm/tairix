use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::cpu::jit::page::{PAGES, PAGE_BITS, PAGE_BYTES};
use crate::cpu::jit::translate_request;

/// A megabyte of code, watched a page to a granule as the board's memory
/// is.
struct Fake {
    bytes: Vec<u8>,
    device: Vec<bool>,
    watched: Vec<bool>,
    stale: Vec<u32>,
}

impl Fake {
    fn new() -> Self {
        Self {
            bytes: vec![0x90; 1 << 20],
            device: vec![false; PAGES],
            watched: vec![false; PAGES],
            stale: Vec::new(),
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.bytes[addr as usize] = value;
        let granule = addr >> PAGE_BITS;
        if core::mem::take(&mut self.watched[granule as usize]) {
            self.stale.push(granule);
        }
    }

    fn place(&mut self, key: Key, code: &[u8]) {
        for (i, &byte) in (0..).zip(code) {
            self.bytes[linear(key.cs, key.ip.wrapping_add(i)) as usize] = byte;
        }
    }

    fn watched(&self, addr: u32) -> bool {
        self.watched[(addr >> PAGE_BITS) as usize]
    }
}

impl Code for Fake {
    fn byte(&self, addr: u32) -> Option<u8> {
        (!self.device[(addr >> PAGE_BITS) as usize]).then(|| self.bytes[addr as usize])
    }
}

impl Watch for Fake {
    fn granule(&self, addr: u32) -> Option<u32> {
        let page = addr >> PAGE_BITS;
        (!self.device[page as usize]).then_some(page)
    }
    fn watch(&mut self, granule: u32) {
        self.watched[granule as usize] = true;
    }
    fn unwatch(&mut self, granule: u32) {
        self.watched[granule as usize] = false;
    }
    fn take_stale(&mut self) -> Vec<u32> {
        core::mem::take(&mut self.stale)
    }
}

/// `INC AX` three times, then `JMP $`: one block of five bytes.
const BLOCK: [u8; 5] = [0x40, 0x40, 0x40, 0xEB, 0xFE];

const AT: Key = Key {
    cs: 0x0100,
    ip: 0x0010,
};

fn hot(cache: &mut Cache, key: Key, memory: &mut Fake) {
    for _ in 0..HOT {
        cache.arrived(key, memory);
    }
}

/// Every block waiting to be asked for, asked for now.
fn asked(cache: &mut Cache) -> Vec<Request> {
    core::iter::from_fn(|| cache.next_request()).collect()
}

/// The place `key` made hot and asked for: its request's number.
fn asked_for(cache: &mut Cache, key: Key, memory: &mut Fake) -> u32 {
    hot(cache, key, memory);
    let requests = asked(cache);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].key, key);
    requests[0].number
}

fn setup() -> (Cache, Fake) {
    let mut memory = Fake::new();
    memory.place(AT, &BLOCK);
    (Cache::new(CpuModel::I8088), memory)
}

/// A second place whose block takes the same slot as `key`'s.
fn rival(key: Key) -> Key {
    (0..=u16::MAX)
        .map(|ip| Key { cs: key.cs, ip })
        .find(|other| *other != key && other.index(SLOTS) == key.index(SLOTS))
        .unwrap()
}

#[test]
fn a_place_is_requested_once_it_is_hot_and_only_once() {
    let (mut cache, mut memory) = setup();
    for _ in 1..HOT {
        cache.arrived(AT, &mut memory);
    }
    assert!(asked(&mut cache).is_empty());
    cache.arrived(AT, &mut memory);
    let requests = asked(&mut cache);
    assert_eq!(
        requests,
        [Request {
            number: 0,
            key: AT,
            model: CpuModel::I8088,
            bytes: BLOCK.to_vec()
        }]
    );
    hot(&mut cache, AT, &mut memory);
    assert!(asked(&mut cache).is_empty(), "asked for already");
    assert!(
        memory.watched(linear(AT.cs, AT.ip)),
        "watched from the request"
    );
}

#[test]
fn an_installed_block_is_found_and_nothing_else_is() {
    let (mut cache, mut memory) = setup();
    hot(&mut cache, AT, &mut memory);
    assert!(!cache.install(AT, 0, 7), "queued, not yet asked for");
    let number = asked(&mut cache)[0].number;
    assert_eq!(cache.lookup(AT), None, "requested, not installed");
    assert!(cache.install(AT, number, 7));
    assert_eq!(cache.lookup(AT), Some(7));
    let other = Key {
        cs: AT.cs,
        ip: AT.ip + 1,
    };
    assert_eq!(cache.lookup(other), None);
    assert!(!cache.install(other, number, 8), "never asked for");
    assert!(!cache.install(AT, number, 9), "installed already");
}

#[test]
fn an_answer_installs_only_the_request_it_answers() {
    let (mut cache, mut memory) = setup();
    let first = asked_for(&mut cache, AT, &mut memory);
    memory.write(linear(AT.cs, AT.ip), 0x40);
    cache.sweep(&mut memory);
    let second = asked_for(&mut cache, AT, &mut memory);
    assert_ne!(first, second);
    cache.refuse(AT, first, &mut memory);
    assert!(
        !cache.install(AT, first, 1),
        "the first was of bytes since changed"
    );
    assert!(cache.install(AT, second, 2), "the second asked for since");
    assert_eq!(cache.lookup(AT), Some(2));
}

#[test]
fn requests_are_numbered_as_they_are_taken_so_one_dropped_leaves_no_gap() {
    let (mut cache, mut memory) = setup();
    let keys = [AT, Key { cs: 0x5000, ip: 0 }, Key { cs: 0x6000, ip: 0 }];
    for &key in &keys {
        memory.place(key, &BLOCK);
        hot(&mut cache, key, &mut memory);
    }
    memory.write(linear(keys[1].cs, keys[1].ip), 0x40);
    cache.sweep(&mut memory);
    let requests = asked(&mut cache);
    let numbered: Vec<(Key, u32)> = requests.iter().map(|r| (r.key, r.number)).collect();
    assert_eq!(numbered, [(keys[0], 0), (keys[2], 1)]);
}

#[test]
fn a_cache_that_stops_asking_drops_what_waits_and_keeps_what_runs() {
    let (mut cache, mut memory) = setup();
    let running = asked_for(&mut cache, AT, &mut memory);
    assert!(cache.install(AT, running, 1));
    let asked_key = Key { cs: 0x5000, ip: 0 };
    let queued_key = Key { cs: 0x6000, ip: 0 };
    for key in [asked_key, queued_key] {
        memory.place(key, &BLOCK);
    }
    let waiting = asked_for(&mut cache, asked_key, &mut memory);
    hot(&mut cache, queued_key, &mut memory);
    cache.stop_requesting(&mut memory);
    assert_eq!(cache.lookup(AT), Some(1), "installed code runs on");
    assert!(
        !cache.install(asked_key, waiting, 2),
        "its answer is too late"
    );
    assert!(
        cache.next_request().is_none(),
        "nothing waits to be asked for"
    );
    for key in [asked_key, queued_key] {
        assert!(!memory.watched(linear(key.cs, key.ip)));
    }
    let fresh = Key { cs: 0x7000, ip: 0 };
    memory.place(fresh, &BLOCK);
    hot(&mut cache, fresh, &mut memory);
    assert!(cache.next_request().is_none(), "and no place is counted");
    assert!(!memory.watched(linear(fresh.cs, fresh.ip)));
}

#[test]
fn a_write_to_its_bytes_drops_it_and_its_code_is_given_back() {
    let (mut cache, mut memory) = setup();
    let number = asked_for(&mut cache, AT, &mut memory);
    assert!(cache.install(AT, number, 7));
    memory.write(linear(AT.cs, AT.ip + 2), 0x48);
    cache.sweep(&mut memory);
    assert_eq!(cache.lookup(AT), None);
    assert_eq!(cache.take_released(), [7]);
    assert!(!memory.watched(linear(AT.cs, AT.ip)));
    hot(&mut cache, AT, &mut memory);
    assert_eq!(asked(&mut cache).len(), 1, "asked for again");
}

#[test]
fn bytes_changed_before_installing_leave_nothing_to_install() {
    let (mut cache, mut memory) = setup();
    let number = asked_for(&mut cache, AT, &mut memory);
    memory.write(linear(AT.cs, AT.ip), 0x48);
    cache.sweep(&mut memory);
    assert!(!cache.install(AT, number, 7));
    assert_eq!(cache.lookup(AT), None);
}

#[test]
fn the_queue_read_ahead_past_a_block_is_watched_too() {
    let mut memory = Fake::new();
    // The block's last byte ends its page, so the queue reads ahead into the
    // next.
    let end = 3 * PAGE_BYTES;
    let key = Key {
        cs: 0,
        ip: u16::try_from(end - 5).unwrap(),
    };
    memory.place(key, &BLOCK);
    let mut cache = Cache::new(CpuModel::I8088);
    let number = asked_for(&mut cache, key, &mut memory);
    assert!(cache.install(key, number, 1));
    assert!(memory.watched(end), "the read-ahead's page");
    memory.write(end + 1, 0);
    cache.sweep(&mut memory);
    assert_eq!(cache.lookup(key), None);
}

#[test]
fn a_granule_two_blocks_read_stays_watched_until_both_are_gone() {
    let (mut cache, mut memory) = setup();
    let other = rival(AT);
    memory.place(other, &BLOCK);
    let number = asked_for(&mut cache, AT, &mut memory);
    assert!(cache.install(AT, number, 1));
    let shared = linear(AT.cs, AT.ip) >> PAGE_BITS == linear(other.cs, other.ip) >> PAGE_BITS;
    let number = asked_for(&mut cache, other, &mut memory);
    assert!(cache.install(other, number, 2));
    assert_eq!(cache.lookup(AT), None, "displaced by its rival");
    assert_eq!(cache.take_released(), [1]);
    assert!(memory.watched(linear(other.cs, other.ip)));
    if !shared {
        assert!(
            !memory.watched(linear(AT.cs, AT.ip)),
            "nothing reads it now"
        );
    }

    let (mut cache, mut memory) = setup();
    let neighbour = Key {
        cs: AT.cs,
        ip: AT.ip + 8,
    };
    memory.place(neighbour, &BLOCK);
    let at = asked_for(&mut cache, AT, &mut memory);
    let beside = asked_for(&mut cache, neighbour, &mut memory);
    assert!(cache.install(AT, at, 1));
    cache.refuse(neighbour, beside, &mut memory);
    assert!(
        memory.watched(linear(AT.cs, AT.ip)),
        "the block installed reads it still"
    );
    memory.write(linear(neighbour.cs, neighbour.ip), 0x40);
    cache.sweep(&mut memory);
    assert!(!memory.watched(linear(AT.cs, AT.ip)));
    assert_eq!(cache.take_released(), [1]);
}

#[test]
fn a_granule_rewritten_again_and_again_is_left_to_the_interpreter_for_a_while() {
    let (mut cache, mut memory) = setup();
    for round in 0..CHURN {
        let number = asked_for(&mut cache, AT, &mut memory);
        assert!(cache.install(AT, number, u64::from(round)), "round {round}");
        memory.write(linear(AT.cs, AT.ip), 0x40);
        cache.sweep(&mut memory);
    }
    hot(&mut cache, AT, &mut memory);
    assert!(asked(&mut cache).is_empty(), "churned");
    // Stale granules elsewhere, enough to halve every count.
    for n in 0..CHURN_DECAY {
        cache.note_churn(100 + n % 8);
    }
    hot(&mut cache, AT, &mut memory);
    assert_eq!(asked(&mut cache).len(), 1, "compiled again once it settled");
}

#[test]
fn code_that_cannot_be_read_whole_is_not_asked_for() {
    let (mut cache, mut memory) = setup();
    memory.device[(linear(AT.cs, AT.ip) >> PAGE_BITS) as usize] = true;
    hot(&mut cache, AT, &mut memory);
    assert!(asked(&mut cache).is_empty());

    let (mut cache, mut memory) = setup();
    memory.place(AT, &[0xE4, 0x60]);
    hot(&mut cache, AT, &mut memory);
    assert!(asked(&mut cache).is_empty(), "IN begins no block");
}

#[test]
fn requests_wait_in_a_bounded_queue() {
    let mut memory = Fake::new();
    let mut cache = Cache::new(CpuModel::I8088);
    let keys: Vec<Key> = (0..=u16::MAX)
        .step_by(64)
        .map(|ip| Key { cs: 0x2000, ip })
        .collect();
    for &key in &keys {
        memory.place(key, &BLOCK);
    }
    for &key in &keys {
        hot(&mut cache, key, &mut memory);
    }
    assert_eq!(asked(&mut cache).len(), QUEUED);
}

#[test]
fn a_refused_block_is_not_asked_for_again() {
    let (mut cache, mut memory) = setup();
    let number = asked_for(&mut cache, AT, &mut memory);
    cache.refuse(AT, number, &mut memory);
    assert!(!memory.watched(linear(AT.cs, AT.ip)));
    hot(&mut cache, AT, &mut memory);
    assert!(asked(&mut cache).is_empty());
    assert!(!cache.install(AT, number, 3));
}

#[test]
fn a_flush_gives_back_everything_and_watches_nothing() {
    let (mut cache, mut memory) = setup();
    let other = Key { cs: 0x3000, ip: 0 };
    memory.place(other, &BLOCK);
    let number = asked_for(&mut cache, AT, &mut memory);
    hot(&mut cache, other, &mut memory);
    assert!(cache.install(AT, number, 1));
    cache.flush(&mut memory);
    assert_eq!(cache.take_released(), [1]);
    assert!(memory.watched.iter().all(|&watched| !watched));
    assert_eq!(cache.lookup(AT), None);
    assert!(cache.next_request().is_none());
    let again = asked_for(&mut cache, AT, &mut memory);
    assert!(again > number, "numbers go on past a flush");
}

#[test]
fn a_request_carries_what_translating_it_needs_and_nothing_more() {
    let (mut cache, mut memory) = setup();
    hot(&mut cache, AT, &mut memory);
    let request = cache.next_request().unwrap();
    let block = translate_request(&request).unwrap();
    assert_eq!(block.instructions, 4);
    assert_eq!(
        request.byte(linear(AT.cs, AT.ip + 5)),
        None,
        "past the block"
    );
    assert_eq!(request.byte(linear(AT.cs, AT.ip)), Some(0x40));
    assert_eq!(
        request.byte(linear(AT.cs, AT.ip - 1)),
        None,
        "before the block"
    );
}
