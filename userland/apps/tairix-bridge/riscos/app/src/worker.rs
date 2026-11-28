//! The worker's half of a session (`plans/RISCOS-EMULATOR.md` §15.1): the
//! kernel made from what the front end started it with — every module the
//! session has, the VDU drivers, the zone, the desktop font, the pressure
//! gauge — and the machine's pressure handed on as it moves.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_hash::HashSeed;
use tairix_log::DiscardSink;
use tairix_reclaim::{PressureBand, PressureGauge};
use tairix_riscos_kernel::memory::layout::SHARED;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Config, Host};
use tairix_riscos_session::worker::{Session, Started, Unconfigured};
use tairix_tz::Zone;

/// Every crate's built-in modules, in the order the session has them.
static MODULES: [&[&Builtin]; 6] = [
    &tairix_riscos_filing::MODULES,
    &tairix_riscos_vdu::MODULES,
    &tairix_riscos_draw::MODULES,
    &tairix_riscos_font::MODULES,
    &tairix_riscos_basic::MODULES,
    &tairix_riscos_clib::MODULES,
];

/// Where the session's caches record what they decide: nowhere, as a
/// canonical sandbox has no log of its own.
static DISCARD: DiscardSink = DiscardSink;

/// The machine's memory pressure as the worker's process reads and reports
/// it.
pub trait Memory {
    /// The gauge the session's caches read.
    fn gauge(&self) -> &'static (dyn PressureGauge + 'static);

    /// The machine's pressure is now `band`.
    fn report(&self, band: PressureBand);
}

/// The worker's session.
pub struct Worker<M> {
    memory: M,
    cache_key: HashSeed,
}

impl<M: Memory> Worker<M> {
    /// The session of a worker whose pressure is `memory`'s, its caches
    /// keyed by `cache_key`, drawn from the process's own CSPRNG.
    pub const fn new(memory: M, cache_key: HashSeed) -> Self {
        Self { memory, cache_key }
    }
}

/// Every module of [`MODULES`], kept for the worker's life, which is its
/// session's: none where there is no memory for the list.
fn modules() -> Option<&'static [&'static Builtin]> {
    let mut all = Vec::new();
    all.try_reserve_exact(MODULES.iter().map(|modules| modules.len()).sum())
        .ok()?;
    all.extend(MODULES.iter().flat_map(|modules| modules.iter().copied()));
    Some(Vec::leak(all))
}

/// The zone `name` of `store`, the store kept for the worker's life.
fn zone(store: Vec<u8>, name: &str) -> Result<Zone<'static>, Unconfigured> {
    let info = tairix_tz::keep(store)
        .map_err(|_| Unconfigured::Refused("a zone store the zone reader refuses"))?;
    info.zone(name)
        .ok_or(Unconfigured::Refused("a zone its store does not hold"))
}

impl<M: Memory> Session for Worker<M> {
    fn config(&mut self, host: Box<dyn Host>, started: Started) -> Result<Config, Unconfigured> {
        let zone = match started.zone {
            Some((store, name)) => zone(store, &name)?,
            None => Zone::UTC,
        };
        self.memory.report(started.pressure);
        Ok(Config {
            layout: &SHARED,
            ceiling: started.ceiling,
            memo_pages: started.memo_pages,
            host,
            vdu: Some(tairix_riscos_vdu::DRIVERS),
            modules: modules().ok_or(Unconfigured::NoMemory)?,
            zone,
            desktop_font: started.font,
            pressure: self.memory.gauge(),
            log: &DISCARD,
            cache_key: self.cache_key,
            show: None,
        })
    }

    fn pressure(&mut self, band: PressureBand) {
        self.memory.report(band);
    }
}

#[cfg(test)]
mod tests;
