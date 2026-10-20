//! What a kernel port decides for the framebuffer console it hosts, and the
//! surface the console paints. Allocator-free, so a port crate states its
//! half without the heap the console's grids need (`hosted`).

use core::marker::PhantomData;
use core::ptr::NonNull;

use tairix_abi::driver::display::DisplayMode;
use tairix_sync::InterruptControl;

/// What a port decides for the console it hosts.
pub trait ConsolePort: 'static {
    /// The interrupt mask the render lock holds, so a handler logging on the
    /// CPU it fired on cannot spin on a lock its own mainline holds.
    type Irq: InterruptControl;

    /// Whether [`Self::publish`] reaches a scan-out that does not snoop the
    /// caches, so the console may be hosted for one.
    const CLEANS: bool;

    /// Whether every page of the `bytes` at `base` translates for a kernel
    /// write under the active regime.
    fn reachable(base: NonNull<u32>, bytes: usize) -> bool;

    /// Where the kernel reaches the `len` bytes at physical `phys`, when the
    /// direct map covers all of them.
    fn direct_map(phys: u64, len: u64) -> Option<NonNull<u32>>;

    /// Make the `bytes` written at `first` visible to the scan-out.
    fn publish(first: NonNull<u32>, bytes: usize);
}

/// A linear map of physical memory: physical `p` lies at kernel address
/// `base + p` for every `p < len`. What a port with such a direct map reaches
/// its console's memory through.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct LinearMap {
    /// The kernel address of physical `0`.
    pub base: u64,
    /// Bytes of physical memory the map covers.
    pub len: u64,
}

impl LinearMap {
    /// Whether the `bytes` at `at` lie inside the map.
    #[must_use]
    pub fn covers(&self, at: NonNull<u32>, bytes: usize) -> bool {
        let start = at.as_ptr().addr() as u64;
        let end = start.checked_add(bytes as u64);
        let top = self.base.checked_add(self.len);
        matches!((end, top), (Some(end), Some(top)) if start >= self.base && end <= top)
    }

    /// Where the map puts the `len` bytes at physical `phys`, when it covers
    /// them all.
    #[must_use]
    pub fn of(&self, phys: u64, len: u64) -> Option<NonNull<u32>> {
        if phys.checked_add(len)? > self.len {
            return None;
        }
        let address = usize::try_from(self.base.checked_add(phys)?).ok()?;
        NonNull::new(core::ptr::with_exposed_provenance_mut(address))
    }
}

/// What a port whose direct map is linear supplies.
pub trait LinearDirectMap: 'static {
    /// The interrupt mask the console's render lock holds.
    type Irq: InterruptControl;

    /// The direct map as it stands.
    fn map() -> LinearMap;
}

/// The console port of a linear direct map whose scan-outs snoop the
/// caches: every reachable byte is the map's, and a written pixel needs no
/// maintenance to reach the device.
pub struct CoherentPort<M>(PhantomData<fn() -> M>);

impl<M: LinearDirectMap> ConsolePort for CoherentPort<M> {
    type Irq = M::Irq;

    const CLEANS: bool = false;

    fn reachable(base: NonNull<u32>, bytes: usize) -> bool {
        M::map().covers(base, bytes)
    }

    fn direct_map(phys: u64, len: u64) -> Option<NonNull<u32>> {
        M::map().of(phys, len)
    }

    fn publish(_first: NonNull<u32>, _bytes: usize) {}
}

/// A framebuffer console's surface: where it lies, its shape, and whose
/// memory it is.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ConsoleSurface {
    /// CPU-physical base.
    pub phys: u64,
    /// Its shape.
    pub mode: DisplayMode,
    /// The kernel owns the memory, rather than the firmware that lit the
    /// screen.
    pub kernel_owned: bool,
}

#[cfg(test)]
mod tests {
    use tairix_sync::NopInterruptControl;

    use super::*;

    struct Map;

    impl LinearDirectMap for Map {
        type Irq = NopInterruptControl;

        fn map() -> LinearMap {
            LinearMap {
                base: 0x1000_0000,
                len: 0x4000,
            }
        }
    }

    type Port = CoherentPort<Map>;

    #[test]
    fn a_coherent_port_reaches_exactly_its_direct_map() {
        let at = Port::direct_map(0x1000, 0x3000).expect("inside the map");
        assert_eq!(at.as_ptr().addr(), 0x1000_1000);
        assert!(Port::reachable(at, 0x3000));
        assert!(!Port::reachable(at, 0x3001), "one byte past its top");
        assert_eq!(Port::direct_map(0x1000, 0x3001), None);
        assert_eq!(Port::direct_map(u64::MAX, 2), None);
        const { assert!(!Port::CLEANS, "a coherent scan-out needs no clean") };
    }

    #[test]
    fn a_linear_map_covers_only_what_lies_inside_it() {
        let map = LinearMap {
            base: 0x1000,
            len: 0x2000,
        };
        let at = |address: usize| NonNull::new(core::ptr::without_provenance_mut::<u32>(address));
        let inside = at(0x1800).expect("non-null");
        assert!(map.covers(inside, 0x1800));
        assert!(!map.covers(inside, 0x1801), "past the top");
        assert!(
            !map.covers(at(0x0ffc).expect("non-null"), 4),
            "below the base"
        );
        assert!(
            !map.covers(at(usize::MAX - 3).expect("non-null"), 8),
            "wrapping"
        );
        assert_eq!(map.of(0x2000, 0x1000), None, "the last byte lies outside");
        assert_eq!(map.of(u64::MAX, 2), None);
        assert_eq!(map.of(0x10, 0x10).map(|p| p.as_ptr().addr()), Some(0x1010));
    }
}
