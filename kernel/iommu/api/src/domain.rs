//! A domain as the kernel holds it: the unit's domain, its IOVA space, and
//! the ledger of what it maps.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;

use crate::iova::{IovaBlock, IovaError, IovaSpace};
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
    /// The IOVAs [`Self::remove`] took out of the tables, awaiting the one
    /// invalidation [`Self::confirm_removed`] runs for them all.
    removed: Vec<u64>,
    /// The bytes those mappings map.
    mapped_bytes: u64,
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

/// One naturally aligned block of physical frames a carve is made of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FrameRun {
    /// Its first byte, aligned to its own size.
    pub phys: u64,
    /// Its size: `IO_PAGE_SIZE << order` bytes.
    pub order: u32,
}

impl FrameRun {
    pub(crate) const fn bytes(self) -> u64 {
        IO_PAGE_SIZE << self.order
    }
}

struct Mapping {
    /// The IOVA block it was handed.
    block: IovaBlock,
    /// Bytes mapped from the block's start.
    len: u64,
    /// The tables no longer hold it; only the unit's caches may.
    removed: bool,
    /// Listed in [`Domain::removed`].
    awaiting: bool,
}

impl<'u> Domain<'u> {
    /// A new domain on `unit`, keeping every window in `identity` mapped at
    /// its own address (firmware reserved windows its streams still need),
    /// and handing out no IOVA in `reserved` (what the unit claims for the
    /// streams it will translate, [`IommuUnit::reserved_iova`]). Windows of
    /// one access that overlap or touch are mapped as one, since firmware may
    /// name a window twice.
    ///
    /// # Errors
    ///
    /// The unit's refusal to create the domain or map a window, and
    /// [`IommuError::OutOfRange`] for a window outside the unit's reach, two
    /// of different access that overlap, or a reserved range that is empty
    /// or not page-aligned.
    pub fn new(
        unit: &'u dyn IommuUnit,
        identity: &[IdentityWindow],
        reserved: &[Range<u64>],
    ) -> Result<Self, IommuError> {
        let profile = unit.profile();
        let input_end = if profile.reach.input_bits >= 64 {
            !(IO_PAGE_SIZE - 1)
        } else {
            1u64 << profile.reach.input_bits
        };
        let output_limit = profile.reach.output_limit();
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
            .and_then(|()| {
                holes.try_reserve(identity.len() + profile.reserved.len() + reserved.len())
            })
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
        holes.extend_from_slice(reserved);
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
            removed: Vec::new(),
            mapped_bytes: 0,
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

    /// The bytes the live mappings map.
    #[must_use]
    pub const fn mapped_bytes(&self) -> u64 {
        self.mapped_bytes
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

    /// Map `runs`, largest first, back to back at one IOVA ending at most at
    /// `limit` (`0` for the domain's whole reach), returning the IOVA. Each
    /// run lands aligned to its own size, so the unit maps it with the
    /// largest leaves it has.
    ///
    /// # Errors
    ///
    /// [`IommuError::OutOfRange`] for no runs, runs out of order, or a run the
    /// unit cannot name, [`IommuError::Exhausted`] when no IOVA below `limit`
    /// is free, the unit's own refusal, or [`IommuError::Unconfirmed`] when a
    /// refused map could not be confirmed gone.
    pub fn map(&mut self, runs: &[FrameRun], limit: u64) -> Result<u64, IommuError> {
        let len = self.check_runs(runs)?;
        let order = len
            .div_ceil(IO_PAGE_SIZE)
            .next_power_of_two()
            .trailing_zeros();
        if order > MAX_ORDER {
            return Err(IommuError::OutOfRange);
        }
        self.mappings
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        let block = self.iova.alloc(order, limit).ok_or(IommuError::Exhausted)?;
        let iova = block.base();
        if let Err(err) = self.unit.map_runs(self.id, iova, runs, Access::READ_WRITE) {
            return Err(self.abandon(block, err));
        }
        // Room was reserved above, so the insert cannot allocate.
        let _ = self.mappings.try_insert(
            iova,
            Mapping {
                block,
                len,
                removed: false,
                awaiting: false,
            },
        );
        self.mapped_bytes = self.mapped_bytes.saturating_add(len);
        Ok(iova)
    }

    /// The bytes `runs` span, refusing what [`Self::map`] cannot place.
    fn check_runs(&self, runs: &[FrameRun]) -> Result<u64, IommuError> {
        let mut len: u64 = 0;
        let mut previous = None;
        for run in runs {
            let size = 1u64
                .checked_shl(run.order)
                .and_then(|pages| pages.checked_mul(IO_PAGE_SIZE))
                .ok_or(IommuError::OutOfRange)?;
            let end = run.phys.checked_add(size).ok_or(IommuError::OutOfRange)?;
            if !run.phys.is_multiple_of(size)
                || end > self.output_limit
                || previous.is_some_and(|larger| run.order > larger)
            {
                return Err(IommuError::OutOfRange);
            }
            previous = Some(run.order);
            len = len.checked_add(size).ok_or(IommuError::OutOfRange)?;
        }
        if len == 0 {
            return Err(IommuError::OutOfRange);
        }
        Ok(len)
    }

    /// Answer `err` for a refused map into `block`, which took back what it
    /// installed. The device may have walked part of it, so the block is
    /// reused only once all of it is confirmed gone.
    fn abandon(&mut self, block: IovaBlock, err: IommuError) -> IommuError {
        if err == IommuError::Unconfirmed
            || self
                .unit
                .sync_range(self.id, block.base(), block.bytes())
                .is_err()
        {
            return IommuError::Unconfirmed;
        }
        let _ = self.iova.free(block);
        err
    }

    /// Remove the mapping at `iova` and confirm it gone.
    ///
    /// # Errors
    ///
    /// [`IommuError::NotMapped`] for an IOVA the domain did not hand out, or
    /// [`IommuError::Unconfirmed`] when the unit could not remove it or
    /// confirm it gone. The IOVA then stays out of reuse, and a later call
    /// for the same IOVA confirms it, its sync naming the mapping's range.
    pub fn unmap(&mut self, iova: u64) -> Result<(), IommuError> {
        let mapping = self.mappings.get_mut(&iova).ok_or(IommuError::NotMapped)?;
        let (len, awaiting) = (mapping.len, mapping.awaiting);
        if !mapping.removed {
            self.unit
                .unmap(self.id, iova, len)
                .map_err(|_| IommuError::Unconfirmed)?;
            mapping.removed = true;
        }
        self.unit
            .sync_range(self.id, iova, len)
            .map_err(|_| IommuError::Unconfirmed)?;
        if awaiting {
            // Its IOVA may carry another carve before the batch is confirmed.
            self.removed.retain(|&at| at != iova);
        }
        self.mapped_bytes = self.mapped_bytes.saturating_sub(len);
        if let Some(mapping) = self.mappings.remove(&iova) {
            let _ = self.iova.free(mapping.block);
        }
        Ok(())
    }

    /// Take the mapping at `iova` out of the tables, its IOVA kept out of
    /// reuse until [`Self::confirm_removed`] confirms no cached translation of
    /// it survives.
    ///
    /// # Errors
    ///
    /// [`IommuError::NotMapped`] for an IOVA the domain did not hand out,
    /// [`IommuError::Exhausted`] when the removal cannot be recorded, or
    /// [`IommuError::Unconfirmed`] when the unit could not remove it.
    pub fn remove(&mut self, iova: u64) -> Result<(), IommuError> {
        let mapping = self.mappings.get_mut(&iova).ok_or(IommuError::NotMapped)?;
        if mapping.awaiting {
            return Ok(());
        }
        self.removed
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        if !mapping.removed {
            self.unit
                .unmap(self.id, iova, mapping.len)
                .map_err(|_| IommuError::Unconfirmed)?;
            mapping.removed = true;
        }
        mapping.awaiting = true;
        self.removed.push(iova);
        Ok(())
    }

    /// Confirm every mapping [`Self::remove`] took out gone with one
    /// invalidation of the domain, and hand their IOVAs back.
    ///
    /// # Errors
    ///
    /// [`IommuError::Unconfirmed`] when the unit could not confirm it: the
    /// mappings keep their IOVAs, and a later call confirms them.
    pub fn confirm_removed(&mut self) -> Result<(), IommuError> {
        if self.removed.is_empty() {
            return Ok(());
        }
        self.unit
            .sync(self.id)
            .map_err(|_| IommuError::Unconfirmed)?;
        for iova in self.removed.drain(..) {
            if let Some(mapping) = self.mappings.remove(&iova) {
                self.mapped_bytes = self.mapped_bytes.saturating_sub(mapping.len);
                let _ = self.iova.free(mapping.block);
            }
        }
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
