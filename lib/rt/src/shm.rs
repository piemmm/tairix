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
//! [`SpawnShare`] is the region a drawing sandbox was handed at spawn, which
//! stays mapped for the life of the process; [`SpawnPart`]s are the disjoint
//! ranges its owner cuts it into and hands out.

use core::ptr::NonNull;

/// The address a kernel-reported mapping base can be read as, or `None`
/// when it is not one a slice may be built over.
///
/// Zero is refused rather than assumed unreachable: a slice over a null
/// pointer is undefined behaviour even when it is empty, and this is the one
/// place every mapping rules it out.
fn mapped_at(base: u64) -> Option<usize> {
    usize::try_from(base).ok().filter(|addr| *addr != 0)
}

/// The kernel's mapping at `addr`, which it made outside the abstract
/// machine, so its provenance is exposed by definition.
fn mapping(addr: usize) -> Option<NonNull<u8>> {
    NonNull::new(core::ptr::with_exposed_provenance_mut(addr))
}

/// `len` bytes the kernel mapped read/write at `base`, which stay mapped for
/// as long as this value lives: each owner below unmaps, if it ever does, only
/// in its own drop.
struct Mapping {
    base: NonNull<u8>,
    len: usize,
}

impl Mapping {
    fn bytes(&self) -> &[u8] {
        // SAFETY: `len` bytes are mapped read/write at `base` while `self`
        // lives, which is this type's invariant. `&self` excludes every mutable
        // reference to them on this side; a peer sharing the region writes it
        // under the owning protocol's serialisation, and at worst tears values
        // rather than breaking this borrow.
        unsafe { core::slice::from_raw_parts(self.base.as_ptr(), self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, with `&mut self` excluding every other reference
        // on this side.
        unsafe { core::slice::from_raw_parts_mut(self.base.as_ptr(), self.len) }
    }

    /// The bytes `range` covers, or `None` when it is not a range of them.
    fn range_mut(&mut self, range: core::ops::Range<usize>) -> Option<&mut [u8]> {
        let len = range.end.checked_sub(range.start)?;
        if range.end > self.len {
            return None;
        }
        // SAFETY: `range` lies within the `len` bytes mapped read/write at
        // `base` while `self` lives, and `&mut self` excludes every other
        // reference to them on this side.
        Some(unsafe { core::slice::from_raw_parts_mut(self.base.as_ptr().add(range.start), len) })
    }

    /// The base the kernel resolves an unmap by.
    fn base(&self) -> u64 {
        self.base.as_ptr().addr() as u64
    }
}

/// What a map call established, when the kernel did not refuse it.
enum Answer {
    /// A mapping a slice may be built over.
    Mapped(Mapping),
    /// A mapping at this base that this target cannot build a slice over.
    Unreachable(u64),
}

/// Read a map call's answer: `ret` is the base or `-errno`, and `raw_len` the
/// length the kernel wrote back, its own record of the region.
fn answer(ret: i64, raw_len: u64) -> Result<Answer, tairix_abi::Errno> {
    if ret < 0 {
        return Err(tairix_abi::Errno::from_syscall(ret));
    }
    #[allow(clippy::cast_sign_loss)] // `ret >= 0` checked above; it is a user VA.
    let base = ret as u64;
    Ok(
        match (mapped_at(base).and_then(mapping), usize::try_from(raw_len)) {
            (Some(addr), Ok(len)) => Answer::Mapped(Mapping { base: addr, len }),
            _ => Answer::Unreachable(base),
        },
    )
}

/// A shared-memory region this task created and mapped, unmapped on drop.
pub struct SharedRegion {
    mapping: Mapping,
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
            mapping: Mapping { base: addr, len },
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
                mapping: Mapping { base: addr, len },
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
        self.mapping.len
    }

    /// Whether the region maps nothing, which only a zero-length create
    /// produces.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.mapping.len == 0
    }

    /// The region's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.mapping.bytes()
    }

    /// The region as a mutable byte slice.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.mapping.bytes_mut()
    }
}

impl AsMut<[u8]> for SharedRegion {
    fn as_mut(&mut self) -> &mut [u8] {
        self.bytes_mut()
    }
}

impl Drop for SharedRegion {
    fn drop(&mut self) {
        let _ = crate::shm_unmap(self.mapping.base(), self.mapping.len);
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
    mapping: Mapping,
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
        let mapping = match answer(ret, raw_len)? {
            Answer::Mapped(mapping) => mapping,
            Answer::Unreachable(base) => {
                let _ = crate::shm_unmap(base, 0);
                return Err(tairix_abi::Errno::LengthOutOfRange);
            }
        };
        // Owned before the size check, so the refusal releases it.
        let grant = Self { mapping };
        if grant.len() < least {
            return Err(tairix_abi::Errno::LengthOutOfRange);
        }
        Ok(grant)
    }

    /// The mapping's byte length, as the kernel recorded it.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.mapping.len
    }

    /// Whether the mapping holds no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.mapping.len == 0
    }

    /// The mapped bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.mapping.bytes()
    }

    /// The mapped bytes, for writing.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.mapping.bytes_mut()
    }
}

impl Drop for MappedGrant {
    fn drop(&mut self) {
        let _ = crate::shm_unmap(self.mapping.base(), self.mapping.len);
    }
}

// SAFETY: as for `SharedRegion`: the mapping is the process's, and the unmap
// on drop is a process-wide call.
unsafe impl Send for MappedGrant {}

// SAFETY: as for `SharedRegion`: only `&mut self` writes.
unsafe impl Sync for MappedGrant {}

/// The region this task's spawner handed it at spawn, mapped read/write for
/// the rest of the process's life.
///
/// It is never unmapped: a drawing sandbox may not make the unmap call, the
/// kernel maps a share at most once, and the process's exit reclaims it.
pub struct SpawnShare {
    mapping: Mapping,
}

impl SpawnShare {
    /// Map this task's spawn share, requiring at least `least` bytes.
    ///
    /// # Errors
    ///
    /// * The kernel's own refusal: [`tairix_abi::Errno::NotFound`] for a task
    ///   handed no share, [`tairix_abi::Errno::AlreadyExists`] once an earlier
    ///   call claimed it.
    /// * [`tairix_abi::Errno::LengthOutOfRange`] for a share smaller than
    ///   `least`, or one whose base is not an address a slice may be built
    ///   over. The share stays mapped, as it always does.
    pub fn map(least: usize) -> Result<Self, tairix_abi::Errno> {
        let mut raw_len: u64 = 0;
        let ret = crate::share_map(&mut raw_len);
        let Answer::Mapped(mapping) = answer(ret, raw_len)? else {
            return Err(tairix_abi::Errno::LengthOutOfRange);
        };
        if mapping.len < least {
            return Err(tairix_abi::Errno::LengthOutOfRange);
        }
        Ok(Self { mapping })
    }

    /// The share's byte length, as the kernel recorded it.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.mapping.len
    }

    /// Whether the share holds no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.mapping.len == 0
    }

    /// The share cut `at` bytes in: the part before and the part after, each
    /// the one owner of its own range of the mapping.
    ///
    /// # Errors
    ///
    /// [`tairix_abi::Errno::LengthOutOfRange`] for a cut past the share's end.
    pub fn split(self, at: usize) -> Result<(SpawnPart, SpawnPart), tairix_abi::Errno> {
        let rest = self
            .mapping
            .len
            .checked_sub(at)
            .ok_or(tairix_abi::Errno::LengthOutOfRange)?;
        let after = NonNull::new(self.mapping.base.as_ptr().wrapping_add(at))
            .ok_or(tairix_abi::Errno::LengthOutOfRange)?;
        let part = |base, len| SpawnPart {
            mapping: Mapping { base, len },
        };
        Ok((part(self.mapping.base, at), part(after, rest)))
    }
}

// SAFETY: as for `SharedRegion`: the mapping is the process's, and nothing of
// it belongs to the thread that mapped it.
unsafe impl Send for SpawnShare {}

// SAFETY: as for `SharedRegion`: only `&mut self` writes.
unsafe impl Sync for SpawnShare {}

/// One range of a [`SpawnShare`], cut by [`SpawnShare::split`]: mapped for the
/// rest of the process's life, as the whole share is, and reached only through
/// this part.
pub struct SpawnPart {
    mapping: Mapping,
}

impl SpawnPart {
    /// The part's byte length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.mapping.len
    }

    /// Whether the part holds no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.mapping.len == 0
    }

    /// The part's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.mapping.bytes()
    }

    /// The part's bytes, for writing.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.mapping.bytes_mut()
    }
}

// SAFETY: as for `SpawnShare`: the range is the process's, and nothing of it
// belongs to the thread that cut it.
unsafe impl Send for SpawnPart {}

// SAFETY: as for `SharedRegion`: only `&mut self` writes.
unsafe impl Sync for SpawnPart {}

/// The data window the kernel mapped for a filesystem driver it accepted to
/// serve a volume (`volume_serve`), mapped for the rest of the process's life.
///
/// It is never unmapped: the attachment ends with the serving process, whose
/// exit reclaims it.
pub struct ServeWindow {
    mapping: Mapping,
}

impl ServeWindow {
    /// The window the kernel reported mapping at `base` for `len` bytes, or
    /// `None` when that is not one a slice may be built over.
    pub(crate) fn mapped(base: u64, len: u32) -> Option<Self> {
        let base = mapped_at(base).and_then(mapping)?;
        let len = usize::try_from(len).ok()?;
        Some(Self {
            mapping: Mapping { base, len },
        })
    }

    /// The window's byte length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.mapping.len
    }

    /// Whether the window holds no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.mapping.len == 0
    }

    /// The bytes `range` covers, for the call in service whose span it is,
    /// or `None` past the window's end. The kernel leaves that span alone
    /// until the call is answered while it stages and collects every other
    /// lane's, so no borrow of more than the span is made.
    pub fn span_mut(&mut self, range: core::ops::Range<usize>) -> Option<&mut [u8]> {
        self.mapping.range_mut(range)
    }
}

// SAFETY: as for `SpawnShare`: the mapping is the process's, and nothing of
// it belongs to the thread that received it.
unsafe impl Send for ServeWindow {}

// SAFETY: as for `SharedRegion`: only `&mut self` writes.
unsafe impl Sync for ServeWindow {}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;
    use std::vec;

    use super::{mapped_at, MappedGrant, Mapping, ServeWindow, SharedRegion, SpawnShare};

    /// A slice over a null pointer is undefined behaviour even when it is
    /// empty, so a zero base is not a mapping however the kernel reported
    /// it.
    #[test]
    fn a_zero_base_is_not_a_mapping() {
        assert_eq!(mapped_at(0), None);
        assert_eq!(mapped_at(0x1000), Some(0x1000));
    }

    /// On a host there is no kernel behind the trap, so no mapping is ever
    /// established, and no slice is built over an address this build
    /// fabricated.
    #[test]
    fn no_mapping_is_established_without_a_kernel() {
        let grantor = tairix_abi::ProcId::from_raw([1; tairix_abi::PROC_ID_LEN]);
        assert!(MappedGrant::map(grantor, 1, 0).is_err());
        assert!(SpawnShare::map(0).is_err());
        assert!(SharedRegion::create(4096).is_none());
        assert!(
            SharedRegion::create_dma(1, 4096, tairix_abi::driver::DmaReach::of::<32>()).is_err()
        );
    }

    /// The test's own memory standing in for a kernel mapping, so the
    /// interpreter checks every borrow the views make. Taken through the
    /// vector's own pointer, so no reference to the bytes outlives this call.
    fn over(memory: &mut std::vec::Vec<u8>) -> Mapping {
        Mapping {
            base: NonNull::new(memory.as_mut_ptr()).expect("a heap pointer"),
            len: memory.len(),
        }
    }

    /// A region's views are its mapping's own bytes, written and read in
    /// place.
    #[test]
    fn a_created_region_s_views_are_its_mapped_bytes() {
        let mut memory = vec![0u8; 64];
        let mut region = SharedRegion {
            mapping: over(&mut memory),
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
        let mut grant = MappedGrant {
            mapping: over(&mut memory),
        };
        grant.bytes_mut()[15] = 1;
        assert_eq!(grant.bytes()[15], 1);
        assert!(grant.bytes()[..15].iter().all(|&byte| byte == 0));
        drop(grant);
        assert_eq!(memory[15], 1);
    }

    /// A serve window's span is its mapping's bytes there, read through a
    /// shared reference on one thread and written in place on the thread that
    /// answers calls.
    #[test]
    fn a_serve_window_s_span_is_its_mapped_bytes_from_any_thread() {
        let mut memory = vec![0u8; 32];
        let mut window = ServeWindow {
            mapping: over(&mut memory),
        };
        let shared = &window;
        std::thread::scope(|scope| {
            scope.spawn(|| assert_eq!((shared.len(), shared.is_empty()), (32, false)));
        });
        let window = std::thread::spawn(move || {
            window.span_mut(30..32).expect("within the window")[1] = 5;
            window
        })
        .join()
        .expect("the answering thread returns its window");
        assert_eq!(window.len(), 32, "the same window comes back");
        assert_eq!(memory[31], 5, "written where it lies");
    }

    /// A span is the window's bytes only where it lies inside the window.
    #[test]
    fn a_span_past_the_window_or_reversed_is_none() {
        let mut memory = vec![0u8; 32];
        let mut window = ServeWindow {
            mapping: over(&mut memory),
        };
        assert!(window.span_mut(30..33).is_none(), "past the end");
        #[allow(clippy::reversed_empty_ranges)] // The reversed range is the case under test.
        let reversed = window.span_mut(5..3);
        assert!(reversed.is_none(), "reversed");
        assert_eq!(
            window.span_mut(32..32).as_deref().map(<[u8]>::len),
            Some(0),
            "empty, at the end"
        );
        assert_eq!(
            window.span_mut(0..32).as_deref().map(<[u8]>::len),
            Some(32),
            "the whole window"
        );
    }

    /// A window the kernel reported at address zero is refused before any
    /// pointer is made of it.
    #[test]
    fn a_serve_window_at_a_zero_base_is_refused() {
        assert!(ServeWindow::mapped(0, 4096).is_none());
    }

    /// A spawn share is cut into parts, each writing its own range where it
    /// lies.
    #[test]
    fn a_spawn_share_s_parts_are_its_mapped_bytes() {
        let mut memory = vec![0u8; 8];
        {
            let share = SpawnShare {
                mapping: over(&mut memory),
            };
            assert_eq!(share.len(), 8);
            assert!(!share.is_empty());
            let (mut head, mut tail) = share.split(5).expect("a cut inside");
            assert_eq!((head.len(), tail.len()), (5, 3));
            head.bytes_mut()[4] = 4;
            tail.bytes_mut()[0] = 6;
            assert_eq!(head.bytes()[4], 4);
            assert_eq!(tail.bytes(), &[6, 0, 0]);
        }
        assert_eq!(memory[4..6], [4, 6], "written where it lies");
    }

    /// A cut at either end leaves one part empty, and a cut past the end is
    /// refused.
    #[test]
    fn a_spawn_share_cut_at_its_ends_or_past_them() {
        let mut memory = vec![0u8; 8];
        let whole = |memory: &mut std::vec::Vec<u8>| SpawnShare {
            mapping: over(memory),
        };
        let (head, tail) = whole(&mut memory).split(8).expect("a cut at the end");
        assert_eq!((head.len(), tail.is_empty()), (8, true));
        assert!(tail.bytes().is_empty());
        let (head, tail) = whole(&mut memory).split(0).expect("a cut at the start");
        assert_eq!((head.is_empty(), tail.len()), (true, 8));
        assert!(whole(&mut memory).split(9).is_err());
    }

    /// A mapping belongs to the process, so it changes threads with its
    /// owner and is read from wherever it went, as the desktop hands a
    /// granted window frame to a worker.
    #[test]
    fn a_mapping_moves_to_another_thread_and_is_read_there() {
        fn shareable<T: Send + Sync>() {}
        shareable::<SharedRegion>();
        shareable::<MappedGrant>();
        shareable::<SpawnShare>();
        shareable::<super::SpawnPart>();
        let mut memory = vec![0u8; 16];
        memory[2] = 5;
        let grant = MappedGrant {
            mapping: over(&mut memory),
        };
        let read = std::thread::spawn(move || grant.bytes()[2])
            .join()
            .expect("the worker ran");
        assert_eq!(read, 5);
    }
}
