//! Running the machine with its blocks: [`run`] is [`Cpu::run`] that enters
//! an installed block wherever one begins and the processor may enter it,
//! and asks for the places the interpreter keeps coming to.
//!
//! What runs a block is an [`Execute`]: the code an installed block holds
//! is the executor's, named to the cache by a handle. A block reaches the
//! machine through its frame, the page map, and an [`Env`] for the refresh
//! helper — never the board itself.

use alloc::boxed::Box;

use super::cache::{Cache, Key, Request, Watch};
use super::frame::{at, Frame};
use super::page::PageMap;
use super::Exit;
use crate::cpu::regs::Seg;
use crate::cpu::{Bus, Cpu, Stop};

/// The waits a bus charges a running block for DRAM refresh.
pub trait Waits {
    /// As [`Bus::take_wait_states`]: the waits taken by cycle `at`.
    fn take(&mut self, at: u64) -> u32;
    /// As [`Bus::next_refresh`].
    fn next_refresh(&self) -> u64;
}

/// What a running block reaches beyond its frame and the page map: the
/// store the map's pages lie in, and the bus's refresh.
pub struct Env<'a> {
    /// The store, which begins at host address `base`.
    pub store: &'a mut [u8],
    /// Where the store begins: the page map's addresses are into it.
    pub base: u64,
    /// The bus's refresh.
    pub waits: &'a mut dyn Waits,
}

impl Env<'_> {
    /// What the refresh helper answers at cycle `at`: the waits the bus
    /// charges, the frame's deadline moved to the next refresh.
    pub fn refresh(&mut self, frame: &mut Frame, at: u64) -> u64 {
        let stolen = self.waits.take(at);
        frame.set_u64(at::REFRESH, self.waits.next_refresh());
        u64::from(stolen)
    }
}

/// What runs installed blocks.
pub trait Execute {
    /// Runs the block whose code `code` names on `frame`, its memory reached
    /// through `pages`: the exit it took, or none where it misbehaved.
    fn run(
        &mut self,
        code: u64,
        frame: &mut Frame,
        pages: &PageMap,
        env: &mut Env<'_>,
    ) -> Option<Exit>;
    /// Takes the `len` bytes of code a block was compiled to, at `address`:
    /// the handle that names it, or none where it is not code this runs.
    fn adopt(&mut self, address: u64, len: u64) -> Option<u64>;
    /// Gives back the code `code` names: no block runs it again.
    fn release(&mut self, code: u64);
    /// Bytes of code it holds.
    fn held(&self) -> u64;
}

/// The machine a block runs in, as the dispatcher reaches it.
pub trait Host: Bus {
    /// The store the code is read from, which watches it.
    fn watched(&mut self) -> &mut dyn Watch;
    /// Calls `run` with the page map as the machine now stands and what a
    /// block running in it reaches.
    fn with_blocks(&mut self, run: &mut dyn FnMut(&PageMap, &mut Env<'_>));
}

/// A machine's translated code: the cache and what runs it.
pub struct Jit {
    /// The blocks, by where each begins.
    pub cache: Cache,
    executor: Box<dyn Execute>,
    /// A block answered no exit: none is entered again.
    broken: bool,
}

impl core::fmt::Debug for Jit {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Jit")
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl Jit {
    /// A cache for `cache`'s model, its blocks run by `executor`.
    #[must_use]
    pub fn new(cache: Cache, executor: Box<dyn Execute>) -> Self {
        Self {
            cache,
            executor,
            broken: false,
        }
    }

    /// Whether a block misbehaved, so the interpreter runs everything.
    #[must_use]
    pub const fn is_broken(&self) -> bool {
        self.broken
    }

    /// Hands the executor back the code of every block dropped.
    fn release(&mut self) {
        for code in self.cache.take_released() {
            self.executor.release(code);
        }
    }

    /// Drops everything translated from `host`'s memory.
    pub fn flush<H: Host>(&mut self, host: &mut H) {
        self.cache.flush(host.watched());
        self.release();
    }

    /// The oldest block waiting to be asked for, numbered now.
    pub fn next_request(&mut self) -> Option<Request> {
        self.cache.next_request()
    }

    /// The answer to request `number`, for the block at `key`: `code`, the
    /// `len` bytes at `address` the block was compiled to, or none. Code the
    /// executor takes is installed where the request still stands and given
    /// back where it does not; a block with none is left to the
    /// interpreter.
    pub fn answer<H: Host>(
        &mut self,
        (key, number): (Key, u32),
        code: Option<(u64, u64)>,
        host: &mut H,
    ) {
        match code.and_then(|(address, len)| self.executor.adopt(address, len)) {
            Some(handle) => {
                if !self.cache.install(key, number, handle) {
                    self.executor.release(handle);
                }
            }
            None => self.cache.refuse(key, number, host.watched()),
        }
    }

    /// Bytes of code installed.
    #[must_use]
    pub fn held(&self) -> u64 {
        self.executor.held()
    }

    /// Asks for no more blocks: those installed run on.
    pub fn stop_requesting<H: Host>(&mut self, host: &mut H) {
        self.cache.stop_requesting(host.watched());
    }
}

/// As [`Cpu::run`], with the blocks `jit` holds entered where they begin.
pub fn run<H: Host>(cpu: &mut Cpu, host: &mut H, limit: u64, jit: &mut Jit) -> Stop {
    while cpu.cycles() < limit {
        jit.cache.sweep(host.watched());
        jit.release();
        let interpret = if !jit.broken && cpu.can_enter_block(host) {
            let key = Key {
                cs: cpu.regs().sreg(Seg::Cs),
                ip: cpu.regs().ip,
            };
            if let Some(code) = jit.cache.lookup(key) {
                enter(cpu, host, limit, jit, code)
            } else {
                jit.cache.arrived(key, host.watched());
                true
            }
        } else {
            true
        };
        if interpret {
            if let Some(stop) = cpu.step(host, limit) {
                return stop;
            }
        }
        if host.schedule_changed() {
            return Stop::Rescheduled;
        }
    }
    Stop::Limit
}

/// Runs the block at `key`, whose code `code` names: whether the
/// interpreter takes the next instruction.
fn enter<H: Host>(cpu: &mut Cpu, host: &mut H, limit: u64, jit: &mut Jit, code: u64) -> bool {
    let mut frame = cpu.frame(host, limit);
    let mut exit = None;
    host.with_blocks(&mut |pages, env| exit = jit.executor.run(code, &mut frame, pages, env));
    let Some(exit) = exit else {
        // Generated code that answers no exit is a defect of the code
        // generator's, its effect on the frame unknown: the processor is
        // left as the block was entered, and no block runs again.
        jit.broken = true;
        jit.flush(host);
        return true;
    };
    cpu.adopt(host, &frame);
    exit == Exit::Step
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
