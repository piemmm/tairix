//! A unit's domains by id, each behind a lock of its own, so one domain's
//! edits and confirmations never wait on another's.

use core::sync::atomic::{AtomicU64, Ordering};

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_sync::{RwLock, SpinLock};

use crate::pagetable::{IoPageTable, PteFormat};
use crate::queue::{Command, Invalidator};
use crate::IommuError;

/// A unit's domains by id. A domain moves only while the map is held
/// exclusively, which only adding or removing one does, so a domain is
/// reached under the map's shared hold and edited under its own lock.
pub struct DomainMap<V> {
    domains: RwLock<HashMap<u32, SpinLock<V>, BuildFastHash>>,
    /// The next batch a confirmation tags its tables with: unique across the
    /// map, so a batch outliving its domain never names a successor's tables.
    batches: AtomicU64,
}

impl<V> Default for DomainMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> DomainMap<V> {
    /// No domains.
    #[must_use]
    pub fn new() -> Self {
        Self {
            domains: RwLock::new(HashMap::with_hasher(BuildFastHash::new())),
            batches: AtomicU64::new(0),
        }
    }

    /// Run `edit` on domain `id`.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for no such domain, or `edit`'s own.
    pub fn with<T>(
        &self,
        id: u32,
        edit: impl FnOnce(&mut V) -> Result<T, IommuError>,
    ) -> Result<T, IommuError> {
        let domains = self.domains.read();
        let domain = domains.get(&id).ok_or(IommuError::OutOfRange)?;
        let mut domain = domain.lock();
        edit(&mut domain)
    }

    /// Whether domain `id` exists.
    #[must_use]
    pub fn contains(&self, id: u32) -> bool {
        self.domains.read().contains_key(&id)
    }

    /// Add `domain` as `id`.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when the map cannot grow, or
    /// [`IommuError::OutOfRange`] for an id already taken, handing `domain`
    /// back so what it holds can be let go.
    pub fn insert(&self, id: u32, domain: V) -> Result<(), (IommuError, V)> {
        let mut domains = self.domains.write();
        if domains.contains_key(&id) {
            return Err((IommuError::OutOfRange, domain));
        }
        if domains.try_reserve(1).is_err() {
            return Err((IommuError::Exhausted, domain));
        }
        // Room was reserved above.
        let _ = domains.try_insert(id, SpinLock::new(domain));
        Ok(())
    }

    /// Take domain `id` out, if it exists.
    pub fn remove(&self, id: u32) -> Option<V> {
        self.domains.write().remove(&id).map(SpinLock::into_inner)
    }

    /// Install into domain `id`'s tables what `install` maps from `iova`,
    /// then make it visible through `publish`, handed the bytes installed. A
    /// refusal takes them back, since the caller frees the frames once this
    /// fails.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for no such domain, `install`'s or
    /// `publish`'s own, or [`IommuError::Unconfirmed`] where what was
    /// installed could not all be taken back.
    pub fn map_published<'f, F>(
        &self,
        id: u32,
        iova: u64,
        tables: impl Fn(&mut V) -> &mut IoPageTable<'f, F>,
        install: impl FnOnce(&mut IoPageTable<'f, F>) -> Result<u64, IommuError>,
        publish: impl FnOnce(u64) -> Result<(), IommuError>,
    ) -> Result<(), IommuError>
    where
        F: PteFormat + 'f,
    {
        let mapped = self.with(id, |domain| install(tables(domain)))?;
        publish(mapped).map_err(|err| {
            self.with(id, |domain| Ok(tables(domain).take_back(iova, mapped, err)))
                .unwrap_or(IommuError::Unconfirmed)
        })
    }

    /// Confirm gone the tables domain `id` retired, through the batch `plan`
    /// makes from them: its commands, and the range they reach or [`None`]
    /// for the whole domain. The batch is handed over and waited on holding
    /// nothing, since a full ring keeps a submit waiting as long as a
    /// completion; the tables it reaches are freed once it is done, or handed
    /// to the next sync where it failed.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for no such domain, `plan`'s own, or
    /// [`IommuError::Unconfirmed`] where the batch failed.
    pub fn confirm<'f, F, C>(
        &self,
        id: u32,
        tables: impl Fn(&mut V) -> &mut IoPageTable<'f, F>,
        unit: &Invalidator<'_>,
        plan: impl FnOnce(&IoPageTable<'f, F>) -> Result<(C, Option<(u64, u64)>), IommuError>,
    ) -> Result<(), IommuError>
    where
        F: PteFormat + 'f,
        C: IntoIterator<Item = Command>,
    {
        let batch = self.batches.fetch_add(1, Ordering::Relaxed);
        let commands = self.with(id, |domain| {
            let table = tables(domain);
            let (commands, range) = plan(table)?;
            table.tag_retired(range, batch);
            Ok(commands)
        })?;
        let done = unit
            .submit(commands)
            .and_then(|ticket| unit.wait(ticket))
            .map_err(|_| IommuError::Unconfirmed);
        // A domain removed meanwhile took its tables with it.
        let _ = self.with(id, |domain| {
            let table = tables(domain);
            if done.is_ok() {
                table.release_tagged(batch);
            } else {
                table.untag(batch);
            }
            Ok(())
        });
        done
    }
}

#[cfg(test)]
#[path = "domains_tests.rs"]
mod tests;
