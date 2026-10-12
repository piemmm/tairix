//! Per-process charges against the resource limits that bound the kernel
//! objects a process may create (`docs/src/architecture/resource-limits.md`).
//!
//! A [`Charge`] is taken when an object is created and travels with it, so
//! dropping the object returns it: no teardown path can forget to, and no
//! object outlives its accounting. The counters are atomics on the
//! process's own [`ResourceCharges`], which every copy of its capability
//! record shares, so a charge takes no lock and contends with no other
//! process.

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::{Errno, LimitKind};

/// The units one process holds against each of its limits.
#[derive(Debug)]
pub struct ResourceCharges {
    held: [AtomicU64; LimitKind::COUNT],
}

impl Default for ResourceCharges {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceCharges {
    /// Counters holding nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            held: [const { AtomicU64::new(0) }; LimitKind::COUNT],
        }
    }

    /// The units of `kind` currently held.
    #[must_use]
    pub fn held(&self, kind: LimitKind) -> u64 {
        self.counter(kind).load(Ordering::Relaxed)
    }

    /// Take `units` of `kind`, refusing if the total held would pass `limit`.
    ///
    /// # Errors
    ///
    /// [`Errno::LimitExceeded`] when the charge does not fit; nothing is
    /// taken.
    pub fn charge(
        self: &Arc<Self>,
        kind: LimitKind,
        units: u64,
        limit: u64,
    ) -> Result<Charge, Errno> {
        self.add(kind, units, limit)?;
        Ok(Charge {
            charges: Arc::clone(self),
            kind,
            units,
        })
    }

    /// A charge of `kind` holding nothing yet, for an object that starts
    /// empty and [grows](Charge::grow) — never refused, so lowering a limit
    /// below what is held cannot stop an empty object being made.
    pub fn empty(self: &Arc<Self>, kind: LimitKind) -> Charge {
        Charge {
            charges: Arc::clone(self),
            kind,
            units: 0,
        }
    }

    // Relaxed throughout: each counter is an independent total, and the
    // compare-and-swap alone makes check-and-add atomic.
    fn add(&self, kind: LimitKind, units: u64, limit: u64) -> Result<(), Errno> {
        self.counter(kind)
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                held.checked_add(units).filter(|&total| total <= limit)
            })
            .map(drop)
            .map_err(|_| Errno::LimitExceeded)
    }

    fn sub(&self, kind: LimitKind, units: u64) {
        self.counter(kind).fetch_sub(units, Ordering::Relaxed);
    }

    fn counter(&self, kind: LimitKind) -> &AtomicU64 {
        &self.held[kind as usize]
    }
}

/// Units of one limit a live object holds, returned when it is dropped.
#[derive(Debug)]
#[must_use = "dropping a charge returns it at once"]
pub struct Charge {
    charges: Arc<ResourceCharges>,
    kind: LimitKind,
    units: u64,
}

impl Charge {
    /// The units this charge holds.
    #[must_use]
    pub const fn units(&self) -> u64 {
        self.units
    }

    /// Take `units` more under `limit`, for an object that grows.
    ///
    /// # Errors
    ///
    /// [`Errno::LimitExceeded`] when the growth does not fit; the charge is
    /// unchanged.
    pub fn grow(&mut self, units: u64, limit: u64) -> Result<(), Errno> {
        self.charges.add(self.kind, units, limit)?;
        self.units += units;
        Ok(())
    }

    /// Return up to `units` of what this charge holds, for an object that
    /// shrinks.
    pub fn shrink(&mut self, units: u64) {
        let units = units.min(self.units);
        self.charges.sub(self.kind, units);
        self.units -= units;
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.charges.sub(self.kind, self.units);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::RLIMIT_INFINITY;

    fn charges() -> Arc<ResourceCharges> {
        Arc::new(ResourceCharges::new())
    }

    #[test]
    fn a_charge_is_held_until_it_drops() {
        let charges = charges();
        let charge = charges.charge(LimitKind::WaitSets, 2, 5).expect("fits");
        assert_eq!(charge.units(), 2);
        assert_eq!(charges.held(LimitKind::WaitSets), 2);
        assert_eq!(
            charges.held(LimitKind::IpcQueueBytes),
            0,
            "kinds are independent"
        );
        drop(charge);
        assert_eq!(charges.held(LimitKind::WaitSets), 0);
    }

    #[test]
    fn a_charge_past_the_limit_is_refused_and_takes_nothing() {
        let charges = charges();
        let held = charges
            .charge(LimitKind::IpcQueueBytes, 4, 5)
            .expect("fits");
        assert_eq!(
            charges
                .charge(LimitKind::IpcQueueBytes, 2, 5)
                .map(|c| c.units()),
            Err(Errno::LimitExceeded)
        );
        assert_eq!(charges.held(LimitKind::IpcQueueBytes), 4);
        let last = charges
            .charge(LimitKind::IpcQueueBytes, 1, 5)
            .expect("exactly fills");
        assert_eq!(charges.held(LimitKind::IpcQueueBytes), 5);
        drop((held, last));
    }

    #[test]
    fn an_overflowing_charge_is_refused_even_without_a_limit() {
        let charges = charges();
        let _held = charges
            .charge(LimitKind::IpcQueueBytes, u64::MAX, RLIMIT_INFINITY)
            .expect("an unlimited kind takes the whole range");
        assert!(charges
            .charge(LimitKind::IpcQueueBytes, 1, RLIMIT_INFINITY)
            .is_err());
        assert_eq!(charges.held(LimitKind::IpcQueueBytes), u64::MAX);
    }

    #[test]
    fn a_charge_grows_and_shrinks_with_its_object() {
        let charges = charges();
        let mut charge = charges.empty(LimitKind::WaitSetMembers);
        charge.grow(2, 3).expect("fits");
        assert_eq!(charge.grow(2, 3), Err(Errno::LimitExceeded));
        assert_eq!(
            charge.units(),
            2,
            "a refused growth leaves the charge alone"
        );
        charge.shrink(1);
        assert_eq!(charges.held(LimitKind::WaitSetMembers), 1);
        charge.shrink(5);
        assert_eq!(charge.units(), 0, "a shrink returns no more than it holds");
        assert_eq!(charges.held(LimitKind::WaitSetMembers), 0);
        charge.grow(3, 3).expect("refills");
        drop(charge);
        assert_eq!(charges.held(LimitKind::WaitSetMembers), 0);
    }

    #[test]
    fn a_lower_limit_refuses_growth_but_keeps_what_is_held() {
        let charges = charges();
        let held = charges.charge(LimitKind::WaitSets, 4, 4).expect("fits");
        assert!(charges.charge(LimitKind::WaitSets, 1, 2).is_err());
        let mut empty = charges.empty(LimitKind::WaitSets);
        assert_eq!(
            empty.units(),
            0,
            "an empty charge is made even over the limit"
        );
        assert!(empty.grow(1, 2).is_err());
        drop(held);
        assert!(charges.charge(LimitKind::WaitSets, 1, 2).is_ok());
    }

    #[test]
    fn concurrent_charges_never_pass_the_limit() {
        extern crate std;
        use std::thread;

        let charges = charges();
        let workers: alloc::vec::Vec<_> = (0..8)
            .map(|_| {
                let charges = Arc::clone(&charges);
                thread::spawn(move || {
                    (0..1000)
                        .filter_map(|_| charges.charge(LimitKind::WaitSets, 1, 100).ok())
                        .collect::<alloc::vec::Vec<_>>()
                })
            })
            .collect();
        let held: alloc::vec::Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("worker"))
            .collect();
        assert_eq!(held.len(), 100);
        assert_eq!(charges.held(LimitKind::WaitSets), 100);
        drop(held);
        assert_eq!(charges.held(LimitKind::WaitSets), 0);
    }
}
