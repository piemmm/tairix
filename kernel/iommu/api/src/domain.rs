//! A domain as the kernel holds it: the unit's domain, its IOVA space, and
//! the ledger of what it maps.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;

use crate::iova::{IovaError, IovaSpace};
use crate::{Access, DomainId, IommuError, IommuUnit, IO_PAGE_SIZE};

/// The highest block order a mapping may take: every IOVA space holds it.
const MAX_ORDER: u32 = 51;

/// One domain of one unit.
///
/// Every translation it removes is confirmed gone before the IOVA it
/// occupied is reused, and an operation the unit cannot confirm answers
/// [`IommuError::Unconfirmed`]: the caller then keeps the memory it mapped
/// out of reuse for good.
pub struct Domain<'u> {
    unit: &'u dyn IommuUnit,
    id: DomainId,
    iova: IovaSpace,
    /// Exclusive physical address the unit's tables can name up to.
    output_limit: u64,
    /// IOVA of every live mapping, and its block order.
    mappings: HashMap<u64, u32, BuildFastHash>,
    streams: Vec<u32>,
    torn_down: bool,
}

impl<'u> Domain<'u> {
    /// A new domain on `unit`, keeping every range in `identity` mapped at
    /// its own address (firmware reserved windows its streams still need).
    ///
    /// # Errors
    ///
    /// The unit's refusal to create the domain or map a window, and
    /// [`IommuError::OutOfRange`] for a window outside the unit's reach.
    pub fn new(unit: &'u dyn IommuUnit, identity: &[Range<u64>]) -> Result<Self, IommuError> {
        let profile = unit.profile();
        let input_end = if profile.input_bits >= 64 {
            !(IO_PAGE_SIZE - 1)
        } else {
            1u64 << profile.input_bits
        };
        let output_limit = if profile.output_bits >= 64 {
            u64::MAX
        } else {
            1u64 << profile.output_bits
        };
        if identity
            .iter()
            .any(|window| window.end > input_end || window.end > output_limit)
        {
            return Err(IommuError::OutOfRange);
        }
        let mut holes = Vec::new();
        holes
            .try_reserve(profile.reserved.len() + identity.len())
            .map_err(|_| IommuError::Exhausted)?;
        holes.extend_from_slice(profile.reserved);
        holes.extend_from_slice(identity);
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
        for window in identity {
            unit.map(
                id,
                window.start,
                window.start,
                window.end - window.start,
                Access::READ_WRITE,
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
    /// record the stream.
    pub fn attach(&mut self, stream: u32) -> Result<(), IommuError> {
        self.streams
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        self.unit.attach(stream, self.id)?;
        self.streams.push(stream);
        Ok(())
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
            // The device may have walked part of the refused map; the IOVA is
            // reused only once that is confirmed gone.
            self.unit
                .sync(self.id)
                .map_err(|_| IommuError::Unconfirmed)?;
            let _ = self.iova.free(iova, order);
            return Err(err);
        }
        // Room was reserved above, so the insert cannot allocate.
        let _ = self.mappings.try_insert(iova, order);
        Ok(iova)
    }

    /// Remove the mapping at `iova` and confirm it gone.
    ///
    /// # Errors
    ///
    /// [`IommuError::NotMapped`] for an IOVA the domain did not hand out, or
    /// [`IommuError::Unconfirmed`] when the unit could not remove it or
    /// confirm it gone; the IOVA is then never reused.
    pub fn unmap(&mut self, iova: u64) -> Result<(), IommuError> {
        let order = *self.mappings.get(&iova).ok_or(IommuError::NotMapped)?;
        self.mappings.remove(&iova);
        self.unit
            .unmap(self.id, iova, IO_PAGE_SIZE << order)
            .and_then(|()| self.unit.sync(self.id))
            .map_err(|_| IommuError::Unconfirmed)?;
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

#[cfg(test)]
#[path = "domain_tests.rs"]
mod tests;
