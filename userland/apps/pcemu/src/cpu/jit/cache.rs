//! The code cache: the blocks installed, by where each begins; how often
//! the interpreter came to each place a block could begin; and the requests
//! to compile those that grew hot.
//!
//! A block is watched in the store granules its bytes, and the queue's
//! read-ahead past them, were read from, from the moment it is requested: a
//! granule that goes stale drops every block and request over it, so no
//! block runs over bytes that changed. A granule that keeps going stale is
//! one the guest writes as it runs — data beside its code, or code it
//! builds — and the blocks over it are left to the interpreter, not compiled
//! over and over.
//!
//! Both tables are direct-mapped and fixed in size, so a guest that runs
//! from more places than they hold costs only its own speed: a block a new
//! one displaces is dropped, and two places sharing a heat counter only
//! grow hot sooner.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::{span, Code};
use crate::cpu::linear;
use crate::cpu::model::CpuModel;

/// Blocks the cache holds at most.
pub const SLOTS: usize = 4096;
/// Heat counters.
const HEAT: usize = 4096;
/// Arrivals that make a place hot.
pub const HOT: u8 = 32;
/// Times a granule may go stale before the blocks over it are left to the
/// interpreter.
const CHURN: u8 = 4;
/// Stale granules after which every granule's count of them halves, so a
/// guest that rewrote code once long ago is compiled again.
const CHURN_DECAY: u32 = 1024;
/// Blocks waiting to be asked for at most.
const QUEUED: usize = 16;

/// Where a block begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// Its code segment.
    pub cs: u16,
    /// Its first byte's offset.
    pub ip: u16,
}

impl Key {
    fn index(self, len: usize) -> usize {
        let mixed = tairix_mix::mix32(u32::from(self.cs) << 16 | u32::from(self.ip));
        mixed as usize % len
    }
}

/// A request to compile the block at `key`: its model and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Its number, which only the answer to this request names: one before
    /// it is never taken for it, though it asks for the same place.
    pub number: u32,
    /// Where the block begins.
    pub key: Key,
    /// The processor it runs on.
    pub model: CpuModel,
    /// The bytes from `key`: the block's instructions, in order.
    pub bytes: Vec<u8>,
}

impl Code for Request {
    fn byte(&self, addr: u32) -> Option<u8> {
        byte_of(self.key, &self.bytes, addr)
    }
}

/// The byte at physical `addr` of `bytes`, the code from `key`: none where it
/// lies outside them, or outside `key`'s segment.
pub(crate) fn byte_of(key: Key, bytes: &[u8], addr: u32) -> Option<u8> {
    let offset = addr.wrapping_sub(u32::from(key.cs) << 4) & 0xF_FFFF;
    let offset = u16::try_from(offset).ok()?;
    bytes.get(usize::from(offset.wrapping_sub(key.ip))).copied()
}

/// The store translated code is read from, watched in granules.
pub trait Watch: Code {
    /// The granule of the store `addr` shows, where code may be read from
    /// it.
    fn granule(&self, addr: u32) -> Option<u32>;
    /// Starts watching `granule`.
    fn watch(&mut self, granule: u32);
    /// Stops watching `granule`.
    fn unwatch(&mut self, granule: u32);
    /// The granules gone stale since the last call, no longer watched.
    fn take_stale(&mut self) -> Vec<u32>;
}

/// What a slot holds.
#[derive(Clone, Debug)]
enum State {
    /// Waiting to be asked for.
    Queued,
    /// Asked for as the request this numbers; the code not yet installed.
    Requested(u32),
    /// Installed: the executor's handle to the code.
    Installed(u64),
    /// Its compiler refused it: left to the interpreter.
    Refused,
}

#[derive(Clone, Debug)]
struct Entry {
    key: Key,
    state: State,
    /// The granules it was read from, each once.
    granules: Vec<u32>,
}

/// The code cache.
#[derive(Debug)]
pub struct Cache {
    model: CpuModel,
    slots: Vec<Option<Entry>>,
    heat: Vec<u8>,
    /// Each granule's blocks and requests: how many read from it.
    readers: Vec<u16>,
    /// Each granule's times gone stale while watched, saturating.
    churn: Vec<u8>,
    /// Stale granules since the counts last halved.
    stale_since: u32,
    /// The blocks waiting to be asked for, oldest first: where each begins,
    /// and its bytes.
    queued: VecDeque<(Key, Vec<u8>)>,
    /// The number the next request takes.
    next: u32,
    /// Whether places are still counted and asked for.
    requesting: bool,
    /// Handles of installed code dropped, for the executor to give back.
    released: Vec<u64>,
}

impl Cache {
    /// An empty cache for `model`'s code.
    #[must_use]
    pub fn new(model: CpuModel) -> Self {
        Self {
            model,
            slots: vec![None; SLOTS],
            heat: vec![0; HEAT],
            readers: Vec::new(),
            churn: Vec::new(),
            stale_since: 0,
            queued: VecDeque::new(),
            next: 0,
            requesting: true,
            released: Vec::new(),
        }
    }

    /// The code installed for the block at `key`.
    #[must_use]
    pub fn lookup(&self, key: Key) -> Option<u64> {
        match self.slots[key.index(SLOTS)].as_ref() {
            Some(Entry {
                key: held,
                state: State::Installed(code),
                ..
            }) if *held == key => Some(*code),
            _ => None,
        }
    }

    /// The interpreter came to `key`, where a block may begin and none is
    /// installed: once the place is hot, the block there is queued to be
    /// asked for from `memory`, watched in it from now.
    pub fn arrived(&mut self, key: Key, memory: &mut dyn Watch) {
        if !self.requesting {
            return;
        }
        let counter = &mut self.heat[key.index(HEAT)];
        *counter = counter.saturating_add(1);
        if *counter < HOT {
            return;
        }
        *counter = 0;
        if self.queued.len() >= QUEUED {
            return;
        }
        let slot = key.index(SLOTS);
        if self.slots[slot]
            .as_ref()
            .is_some_and(|entry| entry.key == key)
        {
            return;
        }
        let code: &dyn Code = &*memory;
        let Some(len) = span(self.model, key.cs, key.ip, code) else {
            return;
        };
        let bytes: Option<Vec<u8>> = (0..len)
            .map(|i| code.byte(linear(key.cs, key.ip.wrapping_add(i))))
            .collect();
        let Some(bytes) = bytes else {
            return;
        };
        // The queue reads ahead past the last instruction: a store there,
        // made by the block itself, must reach the interpreter's queue.
        let read = len + u16::from(self.model.queue_len());
        let mut granules = Vec::new();
        for i in 0..read {
            let addr = linear(key.cs, key.ip.wrapping_add(i));
            match memory.granule(addr) {
                Some(granule) if !granules.contains(&granule) => granules.push(granule),
                None if i < len => return,
                Some(_) | None => {}
            }
        }
        if granules.iter().any(|&granule| self.churned(granule)) {
            return;
        }
        self.evict(slot, memory);
        for &granule in &granules {
            self.read_from(granule, memory);
        }
        self.slots[slot] = Some(Entry {
            key,
            state: State::Queued,
            granules,
        });
        self.queued.push_back((key, bytes));
    }

    /// The oldest block waiting to be asked for, numbered now.
    pub fn next_request(&mut self) -> Option<Request> {
        let (key, bytes) = self.queued.pop_front()?;
        let number = self.next;
        self.next = self.next.wrapping_add(1);
        if let Some(entry) = self.slots[key.index(SLOTS)]
            .as_mut()
            .filter(|entry| entry.key == key)
        {
            entry.state = State::Requested(number);
        }
        Some(Request {
            number,
            key,
            model: self.model,
            bytes,
        })
    }

    /// Whether the block at `key` waits on request `number`.
    fn awaits(&self, key: Key, number: u32) -> bool {
        self.slots[key.index(SLOTS)].as_ref().is_some_and(|entry| {
            entry.key == key && matches!(entry.state, State::Requested(n) if n == number)
        })
    }

    /// Installs `code` for the block at `key`, answering request `number`:
    /// false where that request is gone — dropped, displaced, asked for
    /// again, or over bytes that changed — and the code is the caller's to
    /// give back.
    pub fn install(&mut self, key: Key, number: u32, code: u64) -> bool {
        if !self.awaits(key, number) {
            return false;
        }
        if let Some(entry) = self.slots[key.index(SLOTS)].as_mut() {
            entry.state = State::Installed(code);
        }
        true
    }

    /// Request `number`, for the block at `key`, was refused: the block is
    /// left to the interpreter until something displaces it.
    pub fn refuse(&mut self, key: Key, number: u32, memory: &mut dyn Watch) {
        if !self.awaits(key, number) {
            return;
        }
        let slot = key.index(SLOTS);
        self.evict(slot, memory);
        self.slots[slot] = Some(Entry {
            key,
            state: State::Refused,
            granules: Vec::new(),
        });
    }

    /// Counts and asks for no more places: every block waiting to be asked
    /// for, or asked for, is dropped, and those installed run on.
    pub fn stop_requesting(&mut self, memory: &mut dyn Watch) {
        self.requesting = false;
        for slot in 0..SLOTS {
            let waiting = self.slots[slot]
                .as_ref()
                .is_some_and(|entry| matches!(entry.state, State::Queued | State::Requested(_)));
            if waiting {
                self.evict(slot, memory);
            }
        }
    }

    /// Drops every block and request over a granule gone stale in `memory`.
    pub fn sweep(&mut self, memory: &mut dyn Watch) {
        let stale = memory.take_stale();
        if stale.is_empty() {
            return;
        }
        for &granule in &stale {
            self.note_churn(granule);
            if let Some(readers) = self.readers.get_mut(granule as usize) {
                // The granule is no longer watched: its readers go now.
                *readers = 0;
            }
        }
        for slot in 0..SLOTS {
            let over = self.slots[slot]
                .as_ref()
                .is_some_and(|entry| entry.granules.iter().any(|g| stale.contains(g)));
            if over {
                self.evict(slot, memory);
            }
        }
    }

    /// Drops everything: the store was replaced, or the machine reset.
    pub fn flush(&mut self, memory: &mut dyn Watch) {
        for slot in 0..SLOTS {
            self.evict(slot, memory);
        }
        self.heat.fill(0);
        self.churn.clear();
        self.stale_since = 0;
        self.queued.clear();
    }

    /// Handles of installed code dropped since the last call.
    pub fn take_released(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.released)
    }

    /// Empties slot `slot`, releasing its code and its granules.
    fn evict(&mut self, slot: usize, memory: &mut dyn Watch) {
        let Some(entry) = self.slots[slot].take() else {
            return;
        };
        if let State::Installed(code) = entry.state {
            self.released.push(code);
        }
        // Nothing waits to be asked for once its block's place is gone.
        self.queued.retain(|(key, _)| *key != entry.key);
        for granule in entry.granules {
            if let Some(readers) = self.readers.get_mut(granule as usize) {
                match *readers {
                    0 => {}
                    1 => {
                        *readers = 0;
                        memory.unwatch(granule);
                    }
                    _ => *readers -= 1,
                }
            }
        }
    }

    /// One more block reads from `granule`, which `memory` watches.
    fn read_from(&mut self, granule: u32, memory: &mut dyn Watch) {
        let at = granule as usize;
        if self.readers.len() <= at {
            self.readers.resize(at + 1, 0);
        }
        let readers = &mut self.readers[at];
        if *readers == 0 {
            memory.watch(granule);
        }
        *readers = readers.saturating_add(1);
    }

    fn note_churn(&mut self, granule: u32) {
        let at = granule as usize;
        if self.churn.len() <= at {
            self.churn.resize(at + 1, 0);
        }
        self.churn[at] = self.churn[at].saturating_add(1);
        self.stale_since += 1;
        if self.stale_since >= CHURN_DECAY {
            self.stale_since = 0;
            for count in &mut self.churn {
                *count /= 2;
            }
        }
    }

    fn churned(&self, granule: u32) -> bool {
        self.churn
            .get(granule as usize)
            .is_some_and(|&count| count >= CHURN)
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
