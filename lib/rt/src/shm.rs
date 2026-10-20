//! A mapped shared-memory region, owned.
//!
//! [`shm_create`](crate::shm_create) hands back a raw base address and a
//! length, and every consumer that wants the bytes has to build the same
//! slice over them and unmap on the same paths. This is that, once: one
//! `unsafe` with one proof, and a `Drop` that cannot be forgotten.
//!
//! It is deliberately not window-, driver-, or protocol-specific: what a
//! region is *for* — which endpoint it is granted to, what is written into it
//! — belongs to the crate that knows, and this owns only the mapping.
//!
//! [`SharedRegion`] is a region this task **created**; [`MappedGrant`] is one
//! another task granted it. Both own a mapping and unmap on drop; they differ
//! only in where the mapping came from, and a grant carries no region id
//! because the holder is not the owner and may not re-grant it.

use core::ptr::NonNull;

/// The address a kernel-reported mapping base can be read as, or `None`
/// when it is not one a slice may be built over.
///
/// Zero is refused rather than assumed unreachable: a slice over a null
/// pointer is undefined behaviour even when it is empty, and this is the one
/// place both mappings rule it out.
fn mapped_at(base: u64) -> Option<usize> {
    usize::try_from(base).ok().filter(|addr| *addr != 0)
}

/// The kernel's mapping at `addr`, which it made outside the abstract
/// machine, so its provenance is exposed by definition.
fn mapping(addr: usize) -> Option<NonNull<u8>> {
    NonNull::new(core::ptr::with_exposed_provenance_mut(addr))
}

/// A shared-memory region this task created and mapped, unmapped on drop.
pub struct SharedRegion {
    base: NonNull<u8>,
    len: usize,
    id: u64,
}

impl SharedRegion {
    /// Create and map a `len`-byte region.
    ///
    /// `None` if the kernel refused it, or if the base it returned is not
    /// one this target can build a slice over — never a partially-
    /// established region.
    #[must_use]
    pub fn create(len: usize) -> Option<Self> {
        let mut id: u64 = 0;
        let base = crate::shm_create(len, &mut id);
        if base < 0 {
            return None;
        }
        #[allow(clippy::cast_sign_loss)] // `base >= 0` checked above; it is a user VA.
        let addr = mapping(mapped_at(base as u64)?)?;
        Some(Self {
            base: addr,
            len,
            id,
        })
    }

    /// Carve and map a `len`-byte region a DMA master driving `reach` address
    /// bits may reach, under this driver's `Dma` grant `handle`
    /// ([`shm_create_dma`](crate::shm_create_dma)), with the address the
    /// device reaches it at.
    ///
    /// The mapping's memory type is the device's coherence, for every task
    /// the region is granted to: no cache maintenance is owed to it.
    /// Dropping the region unmaps it, which tells the kernel the device no
    /// longer reaches it, so it is dropped only once the device has stopped.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, or [`tairix_abi::Errno::LengthOutOfRange`] for
    /// a base no slice may be built over.
    pub fn create_dma(
        handle: u64,
        len: usize,
        reach: tairix_abi::driver::DmaReach,
    ) -> Result<(Self, u64), tairix_abi::Errno> {
        let (mut id, mut device) = (0u64, 0u64);
        let base = crate::shm_create_dma(handle, len, reach, &mut id, &mut device);
        if base < 0 {
            return Err(tairix_abi::Errno::from_syscall(base));
        }
        #[allow(clippy::cast_sign_loss)] // `base >= 0` checked above; it is a user VA.
        let Some(addr) = mapped_at(base as u64).and_then(mapping) else {
            let _ = crate::shm_unmap(base as u64, len);
            return Err(tairix_abi::Errno::LengthOutOfRange);
        };
        Ok((
            Self {
                base: addr,
                len,
                id,
            },
            device,
        ))
    }

    /// The region's kernel id, for granting it to an endpoint
    /// ([`shm_grant`](crate::shm_grant)).
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Bytes of the mapping.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the region maps nothing, which only a zero-length create
    /// produces.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The region's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: as `bytes_mut`, with `&self` excluding every mutable
        // reference on this side; a peer granted the region writes it under
        // the owning protocol's serialisation, and at worst tears values.
        unsafe { core::slice::from_raw_parts(self.base.as_ptr(), self.len) }
    }

    /// The region as a mutable byte slice.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: the kernel mapped exactly `len` zeroed bytes read/write at
        // `base` — `shm_create` maps the length it was asked for — and the
        // mapping lives exactly as long as `self`, which owns it and unmaps
        // only on drop. `&mut self` is what excludes every other reference to
        // these bytes on this side; a peer that was *granted* the region
        // reads it under the owning protocol's own serialisation.
        unsafe { core::slice::from_raw_parts_mut(self.base.as_ptr(), self.len) }
    }
}

impl AsMut<[u8]> for SharedRegion {
    fn as_mut(&mut self) -> &mut [u8] {
        self.bytes_mut()
    }
}

impl Drop for SharedRegion {
    fn drop(&mut self) {
        let _ = crate::shm_unmap(self.base.as_ptr().addr() as u64, self.len);
    }
}

// SAFETY: the mapping is the process's, not a thread's: every thread reaches
// the same bytes at `base`, and the unmap on drop is a process-wide call, so
// the region may change threads with its owner.
unsafe impl Send for SharedRegion {}

// SAFETY: `&self` hands out shared byte slices alone and writing takes
// `&mut self`, so a reference shared between threads permits only reads.
unsafe impl Sync for SharedRegion {}

/// A shared-memory region another task granted to this one, mapped
/// read/write and unmapped on drop.
///
/// The length is the **kernel's** own record of the region, reported by
/// `shm_map` — never the granting task's claim about it — so a server
/// writing into a client's region writes inside what the kernel actually
/// mapped.
pub struct MappedGrant {
    base: NonNull<u8>,
    len: usize,
}

impl MappedGrant {
    /// Map the region `grantor` delegated this task as `handle`, requiring at
    /// least `least` bytes.
    ///
    /// `grantor` is the attested process a request naming `handle` came from,
    /// so one client can never have this task map a region another delegated.
    ///
    /// # Errors
    ///
    /// * The kernel's own refusal — [`tairix_abi::Errno::NotFound`] for a
    ///   handle naming no grant `grantor` delegated this task.
    /// * [`tairix_abi::Errno::LengthOutOfRange`] for a region smaller than
    ///   `least`, or one whose base is not an address a slice may be built
    ///   over.
    ///
    /// A refusal maps nothing: a region established and then found too
    /// small is released before this returns.
    pub fn map(
        grantor: tairix_abi::ProcId,
        handle: u64,
        least: usize,
    ) -> Result<Self, tairix_abi::Errno> {
        let mut raw_len: u64 = 0;
        let ret = crate::shm_map_from(handle, grantor, &mut raw_len);
        if ret < 0 {
            return Err(tairix_abi::Errno::from_syscall(ret));
        }
        #[allow(clippy::cast_sign_loss)] // `ret >= 0` checked above; it is a user VA.
        let base = ret as u64;
        let (Some(addr), Ok(len)) = (mapped_at(base).and_then(mapping), usize::try_from(raw_len))
        else {
            // A base or length this target cannot build a slice over names
            // no reachable mapping; release it (the kernel resolves an unmap
            // by its base) and refuse rather than truncate.
            let _ = crate::shm_unmap(base, 0);
            return Err(tairix_abi::Errno::LengthOutOfRange);
        };
        // Constructed before the size check so every refusal below — and any
        // later drop — releases the mapping.
        let region = Self { base: addr, len };
        if len < least {
            return Err(tairix_abi::Errno::LengthOutOfRange);
        }
        Ok(region)
    }

    /// The mapping's byte length, as the kernel recorded it.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping holds no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The mapped bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the kernel mapped exactly `len` bytes read/write at `base`
        // — its own record of the region size — and the mapping lives
        // exactly as long as `self`, which owns it and unmaps only on drop.
        // `&self` excludes every mutable reference to these bytes on this
        // side; the granting task's own access is the owning protocol's to
        // serialise, and at worst tears pixel values rather than breaking
        // this borrow.
        unsafe { core::slice::from_raw_parts(self.base.as_ptr(), self.len) }
    }

    /// The mapped bytes, for writing.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, with `&mut self` excluding every other
        // reference on this side.
        unsafe { core::slice::from_raw_parts_mut(self.base.as_ptr(), self.len) }
    }
}

impl Drop for MappedGrant {
    fn drop(&mut self) {
        let _ = crate::shm_unmap(self.base.as_ptr().addr() as u64, self.len);
    }
}

// SAFETY: as for `SharedRegion`: the mapping is the process's, and the unmap
// on drop is a process-wide call.
unsafe impl Send for MappedGrant {}

// SAFETY: as for `SharedRegion`: only `&mut self` writes.
unsafe impl Sync for MappedGrant {}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;
    use std::vec;

    use super::{mapped_at, MappedGrant, SharedRegion};

    /// A slice over a null pointer is undefined behaviour even when it is
    /// empty, so a zero base is not a mapping however the kernel reported
    /// it.
    #[test]
    fn a_zero_base_is_not_a_mapping() {
        assert_eq!(mapped_at(0), None);
        assert_eq!(mapped_at(0x1000), Some(0x1000));
    }

    /// On a host there is no kernel behind the trap, so neither mapping is
    /// ever established, and no slice is built over an address this build
    /// fabricated.
    #[test]
    fn neither_mapping_is_established_without_a_kernel() {
        let grantor = tairix_abi::ProcId::from_raw([1; tairix_abi::PROC_ID_LEN]);
        assert!(MappedGrant::map(grantor, 1, 0).is_err());
        assert!(SharedRegion::create(4096).is_none());
        assert!(
            SharedRegion::create_dma(1, 4096, tairix_abi::driver::DmaReach::of::<32>()).is_err()
        );
    }

    /// A region's views are its mapping's own bytes, written and read in
    /// place: over memory the test owns, so the interpreter checks every
    /// borrow the views make.
    #[test]
    fn a_created_region_s_views_are_its_mapped_bytes() {
        let mut memory = vec![0u8; 64];
        let base = NonNull::new(memory.as_mut_ptr()).expect("a heap pointer");
        let mut region = SharedRegion {
            base,
            len: memory.len(),
            id: 7,
        };
        region.bytes_mut()[3] = 9;
        assert_eq!(region.bytes()[3], 9);
        assert_eq!(region.bytes().len(), 64);
        assert_eq!(region.as_mut().len(), 64);
        drop(region);
        assert_eq!(memory[3], 9, "written where it lies");
    }

    #[test]
    fn a_granted_region_s_views_are_its_mapped_bytes() {
        let mut memory = vec![0u8; 16];
        let base = NonNull::new(memory.as_mut_ptr()).expect("a heap pointer");
        let mut grant = MappedGrant {
            base,
            len: memory.len(),
        };
        grant.bytes_mut()[15] = 1;
        assert_eq!(grant.bytes()[15], 1);
        assert!(grant.bytes()[..15].iter().all(|&byte| byte == 0));
        drop(grant);
        assert_eq!(memory[15], 1);
    }

    /// A mapping belongs to the process, so it changes threads with its
    /// owner and is read from wherever it went, as the desktop hands a
    /// granted window frame to a worker.
    #[test]
    fn a_mapping_moves_to_another_thread_and_is_read_there() {
        fn shareable<T: Send + Sync>() {}
        shareable::<SharedRegion>();
        shareable::<MappedGrant>();
        let mut memory = vec![0u8; 16];
        memory[2] = 5;
        let base = NonNull::new(memory.as_mut_ptr()).expect("a heap pointer");
        let grant = MappedGrant {
            base,
            len: memory.len(),
        };
        let read = std::thread::spawn(move || grant.bytes()[2])
            .join()
            .expect("the worker ran");
        assert_eq!(read, 5);
    }
}
