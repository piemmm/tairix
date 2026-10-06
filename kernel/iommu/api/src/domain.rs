//! A domain as the kernel holds it: the unit's domain, its IOVA space, and
//! the ledger of what it maps.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;

use crate::iova::{IovaError, IovaSpace};
use crate::pagetable::output_limit;
use crate::{Access, DomainId, IommuError, IommuUnit, IO_PAGE_SIZE};

/// The highest block order a mapping may take: every IOVA space holds it.
const MAX_ORDER: u32 = 51;

/// One domain of one unit.
///
/// Every translation it removes is confirmed gone before the IOVA it
/// occupied is reused, and an operation the unit cannot confirm answers
/// [`IommuError::Unconfirmed`]: the caller then keeps the memory it mapped
/// out of reuse until a later attempt confirms it.
pub struct Domain<'u> {
    unit: &'u dyn IommuUnit,
    id: DomainId,
    iova: IovaSpace,
    /// Exclusive physical address the unit's tables can name up to.
    output_limit: u64,
    /// Every IOVA handed out and not yet confirmed gone.
    mappings: HashMap<u64, Mapping, BuildFastHash>,
    streams: Vec<u32>,
    torn_down: bool,
}

/// Memory a domain keeps mapped at its own address — a window firmware
/// reserved for its streams — for the access firmware allows there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityWindow {
    /// The addresses.
    pub range: Range<u64>,
    /// What the domain's streams may do there.
    pub access: Access,
}

#[derive(Copy, Clone)]
struct Mapping {
    order: u32,
    /// The tables no longer hold it; only the unit's caches may.
    removed: bool,
}

impl<'u> Domain<'u> {
    /// A new domain on `unit`, keeping every window in `identity` mapped at
    /// its own address (firmware reserved windows its streams still need).
    /// Windows of one access that overlap or touch are mapped as one, since
    /// firmware may name a window twice.
    ///
    /// # Errors
    ///
    /// The unit's refusal to create the domain or map a window, and
    /// [`IommuError::OutOfRange`] for a window outside the unit's reach, or
    /// two of different access that overlap.
    pub fn new(unit: &'u dyn IommuUnit, identity: &[IdentityWindow]) -> Result<Self, IommuError> {
        let profile = unit.profile();
        let input_end = if profile.reach.input_bits >= 64 {
            !(IO_PAGE_SIZE - 1)
        } else {
            1u64 << profile.reach.input_bits
        };
        let output_limit = output_limit(profile.reach);
        if identity
            .iter()
            .any(|window| window.range.end > input_end || window.range.end > output_limit)
        {
            return Err(IommuError::OutOfRange);
        }
        let mut windows = Vec::new();
        let mut holes = Vec::new();
        windows
            .try_reserve(identity.len())
            .and_then(|()| holes.try_reserve(identity.len() + profile.reserved.len()))
            .map_err(|_| IommuError::Exhausted)?;
        windows.extend(
            identity
                .iter()
                .filter(|window| window.range.start < window.range.end)
                .cloned(),
        );
        coalesce_windows(&mut windows)?;
        holes.extend(windows.iter().map(|window| window.range.clone()));
        coalesce(&mut holes);
        holes.extend_from_slice(profile.reserved);
        let iova = IovaSpace::new(IO_PAGE_SIZE..input_end, &holes).map_err(|err| match err {
            IovaError::Exhausted => IommuError::Exhausted,
            _ => IommuError::OutOfRange,
        })?;
        let id = unit.create_domain()?;
        let domain = Self {
            unit,
            id,
            iova,
            output_limit,
            mappings: HashMap::with_hasher(BuildFastHash::new()),
            streams: Vec::new(),
            torn_down: false,
        };
        for window in &windows {
            let IdentityWindow { range, access } = window;
            unit.map(
                id,
                range.start,
                range.start,
                range.end - range.start,
                *access,
            )?;
        }
        Ok(domain)
    }

    /// The unit's handle for the domain.
    #[must_use]
    pub const fn id(&self) -> DomainId {
        self.id
    }

    /// The streams translated through the domain.
    #[must_use]
    pub fn streams(&self) -> &[u32] {
        &self.streams
    }

    /// Live mappings, identity windows aside.
    #[must_use]
    pub fn mapped(&self) -> usize {
        self.mappings.len()
    }

    /// Translate `stream` through the domain.
    ///
    /// # Errors
    ///
    /// The unit's refusal, or [`IommuError::Exhausted`] when the domain cannot
    /// record the stream. An [`IommuError::Unconfirmed`] attach leaves the
    /// stream recorded, since the unit may walk the tables for it still: the
    /// teardown blocks it like any other.
    pub fn attach(&mut self, stream: u32) -> Result<(), IommuError> {
        self.streams
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let attached = self.unit.attach(stream, self.id);
        if matches!(attached, Ok(()) | Err(IommuError::Unconfirmed))
            && !self.streams.contains(&stream)
        {
            self.streams.push(stream);
        }
        attached
    }

    /// Map the naturally aligned block of `IO_PAGE_SIZE << order` bytes at
    /// physical `phys` at an IOVA ending at most at `limit` (`0` for the
    /// domain's whole reach), returning the IOVA.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for a block the unit cannot name,
    /// [`IommuError::Exhausted`] when no IOVA below `limit` is free, the
    /// unit's own refusal, or [`IommuError::Unconfirmed`] when a refused map
    /// could not be confirmed gone.
    pub fn map(&mut self, phys: u64, order: u32, limit: u64) -> Result<u64, IommuError> {
        if order > MAX_ORDER {
            return Err(IommuError::OutOfRange);
        }
        let len = IO_PAGE_SIZE << order;
        let end = phys.checked_add(len).ok_or(IommuError::OutOfRange)?;
        if !phys.is_multiple_of(IO_PAGE_SIZE) || end > self.output_limit {
            return Err(IommuError::OutOfRange);
        }
        self.mappings
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let iova = self.iova.alloc(order, limit).ok_or(IommuError::Exhausted)?;
        if let Err(err) = self.unit.map(self.id, iova, phys, len, Access::READ_WRITE) {
            if err == IommuError::Unconfirmed {
                return Err(err);
            }
            // The device may have walked part of the refused map; the IOVA is
            // reused only once that is confirmed gone.
            self.unit
                .sync(self.id)
                .map_err(|_| IommuError::Unconfirmed)?;
            let _ = self.iova.free(iova, order);
            return Err(err);
        }
        // Room was reserved above, so the insert cannot allocate.
        let _ = self.mappings.try_insert(
            iova,
            Mapping {
                order,
                removed: false,
            },
        );
        Ok(iova)
    }

    /// Remove the mapping at `iova` and confirm it gone.
    ///
    /// # Errors
    ///
    /// [`IommuError::NotMapped`] for an IOVA the domain did not hand out, or
    /// [`IommuError::Unconfirmed`] when the unit could not remove it or
    /// confirm it gone. The IOVA then stays out of reuse, and a later call
    /// confirms it: a confirmed sync covers every removal before it.
    pub fn unmap(&mut self, iova: u64) -> Result<(), IommuError> {
        let mapping = self.mappings.get_mut(&iova).ok_or(IommuError::NotMapped)?;
        let order = mapping.order;
        if !mapping.removed {
            self.unit
                .unmap(self.id, iova, IO_PAGE_SIZE << order)
                .map_err(|_| IommuError::Unconfirmed)?;
            mapping.removed = true;
        }
        self.unit
            .sync(self.id)
            .map_err(|_| IommuError::Unconfirmed)?;
        self.mappings.remove(&iova);
        let _ = self.iova.free(iova, order);
        Ok(())
    }

    /// Block every stream, then release the unit's domain once no cached
    /// translation of it survives.
    ///
    /// # Errors
    ///
    /// [`IommuError::Unconfirmed`] when a stream could not be blocked or the
    /// domain released: its tables are then kept, and nothing it mapped may
    /// be reused.
    pub fn destroy(mut self) -> Result<(), IommuError> {
        self.teardown()
    }

    fn teardown(&mut self) -> Result<(), IommuError> {
        if core::mem::replace(&mut self.torn_down, true) {
            return Ok(());
        }
        let mut blocked = true;
        for &stream in &self.streams {
            blocked &= self.unit.block(stream).is_ok();
        }
        // A stream that may still walk the tables keeps them alive.
        if !blocked {
            return Err(IommuError::Unconfirmed);
        }
        self.streams.clear();
        self.unit
            .destroy_domain(self.id)
            .map_err(|_| IommuError::Unconfirmed)
    }
}

impl Drop for Domain<'_> {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// Sort `windows` and merge each run of one access that overlaps or touches,
/// in place.
///
/// # Errors
///
/// [`IommuError::OutOfRange`] where two of different access overlap: no one
/// access is right for the bytes they share.
fn coalesce_windows(windows: &mut Vec<IdentityWindow>) -> Result<(), IommuError> {
    windows.sort_unstable_by_key(|window| window.range.start);
    let mut kept = 0_usize;
    for index in 0..windows.len() {
        let window = windows[index].clone();
        match kept.checked_sub(1).map(|last| &mut windows[last]) {
            Some(last) if window.range.start < last.range.end && window.access != last.access => {
                return Err(IommuError::OutOfRange);
            }
            Some(last) if window.range.start <= last.range.end && window.access == last.access => {
                last.range.end = last.range.end.max(window.range.end);
            }
            _ => {
                windows[kept] = window;
                kept += 1;
            }
        }
    }
    windows.truncate(kept);
    Ok(())
}

/// Sort `windows` and merge each run that overlaps or touches, in place.
fn coalesce(windows: &mut Vec<Range<u64>>) {
    windows.sort_unstable_by_key(|window| window.start);
    let mut kept = 0_usize;
    for index in 0..windows.len() {
        let window = windows[index].clone();
        match kept.checked_sub(1).map(|last| &mut windows[last]) {
            Some(last) if window.start <= last.end => last.end = last.end.max(window.end),
            _ => {
                windows[kept] = window;
                kept += 1;
            }
        }
    }
    windows.truncate(kept);
}

#[cfg(test)]
#[path = "domain_tests.rs"]
mod tests;
