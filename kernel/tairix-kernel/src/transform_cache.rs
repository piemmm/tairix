//! The `ARXFS` transform cache: reclaimable decompressed-cluster
//! plaintext (`plans/SMARTRAM.md` SMART3, section 6.2).
//!
//! [`TransformClusterCache`] is the production implementation of the
//! [`ClusterCache`] seam the `ARXFS` driver exposes: it retains the
//! verified, decrypted, decompressed plaintext of compressed clusters
//! between reads, so the driver pays the transform pipeline (per-block
//! AEAD + integrity checks, then a whole-frame decompression) once per
//! cluster instead of once per read. It complements the clean file
//! cache (`kernel/core::fs::CachedFs`, `plans/SMARTRAM.md` section 6.1)
//! rather than duplicating it: `CachedFs` retains page-sized chunks of
//! *served* plaintext for small reads, while this cache sits below the
//! driver's own read path and also covers the large sequential reads
//! (bundle loads) that bypass `CachedFs` entirely and `CachedFs`'s misses,
//! both of which otherwise re-run the whole transform per call. The
//! `/System` mount and the driver-store server's own window onto the same
//! volume share one cache ([`SharedClusterCache`]), so a bundle devmgr's
//! scan or a driver load decompressed is not decompressed again by the
//! other.
//!
//! # Classification, budget, pressure
//!
//! At construction the cache declares its [`CacheCandidate`] — class
//! [`ReclaimClass::TransformCache`], owned by the mounted volume,
//! expensive to rebuild, decrypted user data, precisely invalidated by
//! the volume's single writer, droppable on demand — and classifies it
//! through the `kernel/mem::reclaim` admission gate. A refusal starts
//! the cache poisoned: every lookup misses, every offer is refused, and
//! the driver keeps serving through the full pipeline (fail closed). A
//! cache the heap cannot build at all — its ledger, or the cell its mounts
//! share it through — is refused for memory, and the mount it was for with
//! it.
//!
//! The cache is bounded by a [`CacheBudget`] and accounted in a
//! [`CacheAccounting`] ledger. Every operation first applies the
//! current pressure band's forced-shrink target for the transform-cache
//! class ([`shrink_target`]): the class is preserved at mild pressure
//! and drained to zero from moderate pressure on, before anonymous
//! pages are handed to `ramzip` (`plans/SWAPSWAPSWAP.md` section 6).
//! Growth is admitted only at normal pressure and never into the
//! reserve ([`tairix_reclaim::PressureGauge::growth_permitted`]). Inserts
//! over the hard limit first evict least-recently-used entries down to the
//! low watermark (hysteresis). A cluster the heap cannot index is not
//! retained: its copy and its room in both indexes are had before anything
//! is evicted for it. Room a shrink or an invalidation frees goes back to
//! the heap at the first operation above normal pressure.
//!
//! # Coherence and secret hygiene
//!
//! Entries are keyed by the cluster's first stored physical block; the
//! driver invalidates on every block free and purges on transaction
//! rollback (the `tairix_drv_fs_arxfs::ClusterCache` contract), so an
//! entry can never outlive the bytes it was derived from. The plaintext
//! is user data, decrypted when the volume is encrypted: every buffer
//! erases itself as its entry is released, on whichever path releases it.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_collections::OrderedMap;
use tairix_drv_fs_arxfs::{ClusterCache, MAX_CLUSTER_PLAINTEXT};
use tairix_kernel_core::{CacheClass, CacheControl, SleepLock, CACHE_CONTROL};
use tairix_log::Sink;
use tairix_reclaim::{
    log_cache_poisoned, log_cache_refused, shrink_target, CacheAccounting, CacheBudget,
    CacheCandidate, CacheLedger, CachePolicy, InvalidationSource, MemoryPressure, PressureBand,
    RebuildCost, ReclaimClass, ReclaimOwner, ReclaimRule, Sensitivity, MAP_ENTRY_OVERHEAD,
};
use tairix_util::secret::WipedBuf;

/// The fixed `cache` label this cache's audit records carry.
const CACHE_LABEL: &str = "transform";

/// One retained cluster: its stored-run length in physical blocks (for
/// run-covering invalidation), its plaintext, and its recency tick.
struct Entry {
    stored: u64,
    plain: WipedBuf,
    tick: u64,
}

/// The reclaimable decompressed-cluster cache one mounted volume
/// installs into its `ARXFS` driver. See the module docs.
pub struct TransformClusterCache {
    budget: CacheBudget,
    /// The live cache-admission control (the operator's `cache.transform`
    /// / `cache.all` switch). Sampled at the head of every operation: when
    /// the transform class is disabled the cache admits nothing and drops
    /// (wiping) what it holds, a real bypass. Defaults to the process-
    /// global [`CACHE_CONTROL`]; a test binds its own.
    control: &'static CacheControl,
    /// The system memory-pressure gauge, sampled at the head of every
    /// operation: the band's forced-shrink target is applied before the
    /// cache is read or grown, and admission is refused outside normal
    /// pressure or when growth would dip into the reserve.
    pressure: &'static MemoryPressure,
    /// The audit sink a classification refusal or detected ledger
    /// defect reports through (`kernel/mem::reclaim_audit`).
    sink: &'static (dyn Sink + Sync),
    accounting: Arc<CacheAccounting>,
    /// The classified admission policy; `None` when classification
    /// refused, which poisons the cache from birth.
    policy: Option<CachePolicy>,
    /// The cache admits nothing further (a classification refusal or a
    /// detected ledger defect): every lookup misses and the driver
    /// serves through the full transform pipeline (fail closed).
    poisoned: bool,
    /// Monotonic recency counter; every touch assigns a fresh tick, so
    /// ticks are unique and the LRU index is keyed by them.
    tick: u64,
    /// Retained clusters, keyed by the run's first stored block.
    entries: OrderedMap<u64, Entry>,
    /// LRU index: tick (oldest first) to entry key.
    lru: OrderedMap<u64, u64>,
    /// A cluster left the indexes outside a forced shrink, so they may hold
    /// room they no longer need.
    slack: bool,
}

impl TransformClusterCache {
    /// Build a cache bounded by `budget`, charged to `owner`, and
    /// governed by the system `pressure` gauge.
    ///
    /// The candidate declaration passes the `kernel/mem::reclaim`
    /// classification gate; a refusal poisons the cache from birth, so
    /// the volume still works and every read runs the full pipeline —
    /// fail closed, never an unclassified cache.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the cache's ledger cannot be
    /// allocated; nothing is built or reported.
    pub fn new(
        budget: CacheBudget,
        owner: ReclaimOwner,
        pressure: &'static MemoryPressure,
        sink: &'static (dyn Sink + Sync),
    ) -> Result<Self, DriverError> {
        let accounting =
            Arc::try_new(CacheAccounting::new()).map_err(|_| DriverError::OutOfMemory)?;
        let candidate = CacheCandidate {
            class: Some(ReclaimClass::TransformCache),
            owner: Some(owner),
            rebuild_cost: RebuildCost::Expensive,
            sensitivity: Some(Sensitivity::UserData),
            invalidation: Some(InvalidationSource::SourceMutation),
            rule: Some(ReclaimRule::Drop),
            entry_metadata_bytes: MAP_ENTRY_OVERHEAD,
        };
        let policy = match candidate.classify() {
            Ok(policy) => Some(policy),
            Err(refusal) => {
                log_cache_refused(sink, CACHE_LABEL, Some(owner), refusal);
                None
            }
        };
        Ok(Self {
            budget,
            control: &CACHE_CONTROL,
            pressure,
            sink,
            accounting,
            poisoned: policy.is_none(),
            policy,
            tick: 0,
            entries: OrderedMap::new(),
            lru: OrderedMap::new(),
            slack: false,
        })
    }

    /// Bind a specific [`CacheControl`] instead of the process-global
    /// [`CACHE_CONTROL`] this cache consults by default. Production uses
    /// the shared global the unlock path applies the operator's
    /// configuration to; a host test drives the disable path against its
    /// own control through this builder.
    #[must_use]
    pub fn with_cache_control(mut self, control: &'static CacheControl) -> Self {
        self.control = control;
        self
    }

    /// The cache a mounted volume installs into its driver, budgeted from
    /// discovered RAM exactly like the volume's clean filesystem cache and
    /// charged to the volume identified by its stable per-boot mount handle.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn for_volume(
        volume: u64,
        pressure: &'static MemoryPressure,
        sink: &'static (dyn Sink + Sync),
    ) -> Result<Self, DriverError> {
        let cache = Self::new(
            // Budget from discovered physical RAM (the growable kernel heap's
            // bootstrap size is no longer the memory to size a cache against);
            // falls back to the bootstrap size before RAM is published.
            CacheBudget::from_backing(tairix_kernel_core::memstats::cache_backing_bytes()),
            ReclaimOwner::FilesystemVolume { volume },
            pressure,
            sink,
        )?;
        // A cache the registry cannot list would spend memory reclaim cannot
        // see. `None` is a cache classification refused at birth, which admits
        // nothing and is already audited.
        if let Some(ledger) = cache.ledger() {
            tairix_kernel_core::memstats::MEM_STATS
                .register_ledger(ledger)
                .map_err(|_| DriverError::OutOfMemory)?;
        }
        Ok(cache)
    }

    /// The cache's byte ledger and event counters.
    #[must_use]
    pub fn accounting(&self) -> &CacheAccounting {
        &self.accounting
    }

    /// The owner the cache's memory is charged to, or `None` when
    /// classification refused the cache (it is then poisoned and admits
    /// nothing).
    #[must_use]
    pub fn owner(&self) -> Option<ReclaimOwner> {
        self.policy.map(CachePolicy::owner)
    }

    /// This cache described for the System Information memory-statistics
    /// registry: its label, owner, and class, plus a shared handle to the
    /// ledger above.
    ///
    /// `None` when classification refused the cache (it is then poisoned
    /// and admits nothing, so there is no footprint to attribute — the
    /// refusal is already in the audit log with its reason).
    #[must_use]
    pub fn ledger(&self) -> Option<CacheLedger> {
        let policy = self.policy?;
        Some(CacheLedger::new(
            CACHE_LABEL,
            policy.owner(),
            policy.class(),
            Arc::clone(&self.accounting),
        ))
    }

    /// The next unique recency tick.
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// The accounted `(payload, metadata)` byte cost of an entry
    /// holding `payload` plaintext bytes plus the fixed per-entry
    /// bookkeeping.
    const fn cost_of(payload: usize) -> (usize, usize) {
        (payload, MAP_ENTRY_OVERHEAD)
    }

    /// Remove the entry keyed by `start`, its plaintext erasing itself,
    /// dropping its LRU slot and discharging its cost. `false` when no such
    /// entry exists.
    fn remove_start(&mut self, start: u64) -> bool {
        let Some(entry) = self.entries.remove(&start) else {
            return false;
        };
        self.lru.remove(&entry.tick);
        self.slack = true;
        let (payload, metadata) = Self::cost_of(entry.plain.len());
        if self
            .accounting
            .discharge(ReclaimClass::TransformCache, payload, metadata)
            .is_err()
        {
            self.poison("ledger_imbalance");
        }
        true
    }

    /// Drop every entry (wiped) and admit nothing further: the
    /// fail-closed response to the internal defect named by `cause`.
    /// The driver keeps serving; only the cache is disabled. The
    /// defect is counted and reported once through the audit sink.
    fn poison(&mut self, cause: &'static str) {
        if !self.poisoned {
            self.accounting.record_failure(ReclaimClass::TransformCache);
            log_cache_poisoned(self.sink, CACHE_LABEL, self.owner(), cause);
        }
        self.poisoned = true;
        self.drop_all();
    }

    /// Drop every entry, each plaintext erasing itself, and rebalance the
    /// ledger to empty, both indexes giving their room back. Every
    /// whole-cache drain is counted as a teardown.
    fn drop_all(&mut self) {
        self.accounting
            .record_teardown(ReclaimClass::TransformCache);
        self.entries.clear();
        self.lru.clear();
        self.accounting.zero_ledger();
        self.slack = false;
    }

    /// Evict least-recently-used entries until the ledger total is at
    /// most `target`.
    fn evict_until(&mut self, target: usize) {
        while self.accounting.total_bytes() > target {
            let Some((_, &start)) = self.lru.first_key_value() else {
                return;
            };
            if !self.remove_start(start) {
                // An index slot with no backing entry is a ledger
                // defect; fail closed rather than loop.
                self.poison("orphan_index_slot");
                return;
            }
            self.accounting.record_eviction();
        }
    }

    /// Apply the current pressure band's forced-shrink target for the
    /// transform-cache class, called at the head of every operation
    /// before the cache is read or grown (`plans/SMARTRAM.md` section
    /// 7): preserved at mild pressure, drained to zero from moderate
    /// on. Every evicted buffer erases itself on the way out, and at any band
    /// above normal the room an earlier removal left in the indexes goes back
    /// to the heap.
    fn enforce_pressure(&mut self) {
        if self.poisoned {
            return;
        }
        // The operator disabled the transform cache (`cache.transform` or
        // the master `cache.all` off): drop (wiping) everything and admit
        // nothing further — a real bypass, every read runs the full
        // transform pipeline. Re-enabling lets the next read refill it.
        if !self.control.admits(CacheClass::Transform) {
            if self.accounting.total_bytes() > 0 {
                self.drop_all();
            }
            return;
        }
        let band = self.pressure.sample();
        let target = shrink_target(band, ReclaimClass::TransformCache, self.budget);
        if self.accounting.total_bytes() > target {
            self.accounting
                .record_pressure_shrink(ReclaimClass::TransformCache);
            self.evict_until(target);
            self.release_index_room();
        } else if self.slack && band > PressureBand::Normal {
            self.release_index_room();
        }
    }

    /// Give the heap back the room the indexes hold beyond their entries:
    /// all of an index emptied, and what a fitted arena spares of the rest.
    fn release_index_room(&mut self) {
        // A refused fit keeps the room it had, every entry in place.
        let _ = [
            self.entries.try_shrink_to_fit(),
            self.lru.try_shrink_to_fit(),
        ];
        self.slack = false;
    }

    /// The retained plaintext of the cluster whose stored run starts at
    /// `phys`, marked most recently used, or `None` when it is not retained.
    pub fn get(&mut self, phys: u64) -> Option<&[u8]> {
        if self.poisoned {
            return None;
        }
        self.enforce_pressure();
        if !self.entries.contains_key(&phys) {
            self.accounting.record_miss(ReclaimClass::TransformCache);
            return None;
        }
        let tick = self.next_tick();
        let mut indexed = true;
        // Recency is a hint: an entry whose slot has no room to move is
        // served from where it stands.
        if let Some(entry) = self.entries.get_mut(&phys) {
            if self.lru.try_reserve(1).is_ok() {
                self.lru.remove(&entry.tick);
                entry.tick = tick;
                indexed = self.lru.try_insert(tick, phys).is_ok();
            }
        }
        if !indexed {
            self.poison("index_divergence");
            return None;
        }
        self.accounting.record_hit(ReclaimClass::TransformCache);
        self.entries.get(&phys).map(|entry| &entry.plain[..])
    }
}

impl ClusterCache for TransformClusterCache {
    fn lend(&mut self, phys: u64, read: &mut dyn FnMut(&[u8]) -> usize) -> Option<usize> {
        self.get(phys).map(read)
    }

    fn put(&mut self, phys: u64, stored: u64, plaintext: &[u8]) {
        if self.poisoned {
            return;
        }
        self.enforce_pressure();
        if !self.control.admits(CacheClass::Transform) {
            self.accounting.record_refusal(ReclaimClass::TransformCache);
            return;
        }
        // A shapeless offer (no run to invalidate against, an empty or
        // over-bound plaintext) is refused: entries stay individually
        // bounded and coherently invalidatable.
        if stored == 0 || plaintext.is_empty() || plaintext.len() > MAX_CLUSTER_PLAINTEXT {
            self.accounting.record_refusal(ReclaimClass::TransformCache);
            return;
        }
        // Replace, never shadow: a stale entry under the same key would
        // desynchronise the ledger.
        if self.remove_start(phys) {
            self.accounting.record_invalidation();
        }
        if self.poisoned {
            return;
        }
        let (payload, metadata) = Self::cost_of(plaintext.len());
        let cost = payload.saturating_add(metadata);
        let class = ReclaimClass::TransformCache;
        // One reading: the ceiling and the reserve draw must agree on the
        // band, so both come from the same fold.
        let mut allowance = self.pressure.growth_allowance();
        let ceiling = shrink_target(allowance.band(), class, self.budget);
        if !allowance.take(class, self.budget, cost) {
            self.accounting.record_refusal(class);
            return;
        }
        // The copy and room in both indexes come before anything is evicted
        // for the entry, since an eviction only adds to that room, so heap
        // exhaustion refuses the entry having evicted nothing for it.
        let mut plain = Vec::new();
        if plain.try_reserve_exact(plaintext.len()).is_err()
            || self.entries.try_reserve(1).is_err()
            || self.lru.try_reserve(1).is_err()
        {
            self.accounting.record_refusal(class);
            return;
        }
        plain.extend_from_slice(plaintext);
        let plain = WipedBuf::new(plain);
        if self.accounting.total_bytes().saturating_add(cost) > ceiling {
            let headroom = self.budget.low().min(ceiling - cost);
            self.evict_until(headroom);
            if self.poisoned {
                return;
            }
        }
        if self.accounting.charge(class, payload, metadata).is_err() {
            self.accounting.record_refusal(class);
            return;
        }
        let tick = self.next_tick();
        let entry = Entry {
            stored,
            plain,
            tick,
        };
        if self.lru.try_insert(tick, phys).is_err() || self.entries.try_insert(phys, entry).is_err()
        {
            self.poison("index_divergence");
        }
    }

    fn invalidate_run(&mut self, phys: u64, len: u64) {
        let Some(last) = len.checked_sub(1).map(|last| phys.saturating_add(last)) else {
            return;
        };
        // An entry starting below the run can still reach into it, and every
        // entry starting inside it overlaps by definition. Both are dropped by
        // start address, so the walk costs the entries it touches and nothing
        // per block of the run.
        let covering = self
            .entries
            .range(..phys)
            .next_back()
            .filter(|(start, entry)| phys - *start < entry.stored)
            .map(|(start, _)| *start);
        if covering.is_some_and(|start| self.remove_start(start)) {
            self.accounting.record_invalidation();
        }
        while let Some(start) = self.entries.range(phys..=last).next().map(|(&s, _)| s) {
            if self.remove_start(start) {
                self.accounting.record_invalidation();
            }
        }
    }

    fn purge(&mut self) {
        self.drop_all();
    }
}

/// One [`TransformClusterCache`] that several mounts of a volume share, so
/// plaintext one of them decompressed serves the others' reads under a
/// single budget and ledger.
///
/// Each clone is a handle installed into one mount's driver. The lock is held
/// for one lookup, offer or invalidation and never across a device read; its
/// contenders park rather than spin, since a pressure shrink can wipe the
/// whole cache under it.
#[derive(Clone)]
pub struct SharedClusterCache(Arc<SleepLock<TransformClusterCache>>);

impl SharedClusterCache {
    /// Share `cache` between the mounts given clones of the result.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] when the shared cell cannot be
    /// allocated; `cache` goes with the refusal.
    pub fn new(cache: TransformClusterCache) -> Result<Self, DriverError> {
        Arc::try_new(SleepLock::new(cache))
            .map(Self)
            .map_err(|_| DriverError::OutOfMemory)
    }
}

impl ClusterCache for SharedClusterCache {
    fn lend(&mut self, phys: u64, read: &mut dyn FnMut(&[u8]) -> usize) -> Option<usize> {
        self.0.lock().lend(phys, read)
    }

    fn put(&mut self, phys: u64, stored: u64, plaintext: &[u8]) {
        self.0.lock().put(phys, stored, plaintext);
    }

    fn invalidate_run(&mut self, phys: u64, len: u64) {
        self.0.lock().invalidate_run(phys, len);
    }

    fn purge(&mut self) {
        self.0.lock().purge();
    }
}

#[cfg(test)]
#[path = "transform_cache_tests.rs"]
mod tests;
