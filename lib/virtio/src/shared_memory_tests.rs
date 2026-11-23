use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ptr::NonNull;

use super::*;

const PAGE: u64 = PAGE_SIZE as u64;

/// A mapper over memory of its own standing for the region, recording every
/// part it maps and unmaps.
struct Mapper {
    memory: RefCell<Vec<u8>>,
    maps: RefCell<Vec<(u64, usize, RegionCaching)>>,
    unmaps: RefCell<Vec<u64>>,
    refuse_unmap: bool,
}

impl Mapper {
    fn over(len: u64) -> Self {
        Self {
            memory: RefCell::new(vec![0; usize::try_from(len).expect("fits")]),
            maps: RefCell::new(Vec::new()),
            unmaps: RefCell::new(Vec::new()),
            refuse_unmap: false,
        }
    }
}

impl MemoryMapper for Mapper {
    fn map_memory(
        &self,
        _region: &HwResource,
        offset: u64,
        len: usize,
        caching: RegionCaching,
    ) -> Result<MemoryWindow, DriverError> {
        let start = usize::try_from(offset).map_err(|_| DriverError::OutOfRange)?;
        let mut memory = self.memory.borrow_mut();
        let part = memory
            .get_mut(start..start + len)
            .ok_or(DriverError::OutOfRange)?;
        self.maps.borrow_mut().push((offset, len, caching));
        let base = NonNull::new(part.as_mut_ptr()).ok_or(DriverError::Unsupported)?;
        // SAFETY: the vector outlives every window a test makes and is never
        // resized, and the windows are the only way any test reaches it.
        Ok(unsafe { MemoryWindow::from_mapping(offset, base, len) })
    }

    fn unmap_memory(&self, window: MemoryWindow) -> Result<(), (MemoryWindow, DriverError)> {
        if self.refuse_unmap {
            return Err((window, DriverError::DeviceFault));
        }
        self.unmaps.borrow_mut().push(window.offset());
        Ok(())
    }
}

fn region(pages: u64) -> SharedMemory {
    region_at(0x1_0000_0000, pages)
}

fn region_at(base: u64, pages: u64) -> SharedMemory {
    SharedMemory::new(HwResource::memory_region(base, pages * PAGE, 1)).expect("a region")
}

#[test]
fn a_region_is_a_granted_memory_region_of_whole_pages() {
    let held = region(16);
    assert_eq!((held.id(), held.size()), (1, 16 * PAGE));
    for refused in [
        HwResource::mmio(0x1_0000_0000, PAGE),
        HwResource::memory_region(0x1_0000_0000, PAGE + 8, 1),
    ] {
        assert!(matches!(
            SharedMemory::new(refused),
            Err(VirtioError::DeviceFault)
        ));
    }
}

#[test]
fn parts_are_whole_pages_placed_first_fit_until_the_region_is_full() {
    let mut held = region(4);
    let one = held.reserve(1).expect("a page");
    let two = held.reserve(PAGE + 1).expect("two pages");
    assert_eq!((one.offset(), one.size()), (0, PAGE));
    assert_eq!((two.offset(), two.size()), (PAGE, 2 * PAGE));
    assert!(matches!(
        held.reserve(2 * PAGE),
        Err(VirtioError::Host(DriverError::NoSpace))
    ));
    assert!(matches!(
        held.reserve(0),
        Err(VirtioError::Host(DriverError::LengthOutOfRange))
    ));
    held.release(one).expect("unmapped");
    let back = held.reserve(PAGE).expect("the freed page");
    assert_eq!(back.offset(), 0, "first fit takes the run handed back");
}

#[test]
fn a_mapped_part_is_never_released_and_its_pages_are_not_reused() {
    let mapper = Mapper::over(4 * PAGE);
    let mut held = region(4);
    let mut part = held.reserve(4 * PAGE).expect("the region");
    held.map(&mut part, &mapper, RegionCaching::WriteBack)
        .expect("maps");
    let part = held.release(part).expect_err("still mapped");
    assert!(matches!(
        held.reserve(PAGE),
        Err(VirtioError::Host(DriverError::NoSpace))
    ));
    let mut part = part;
    held.unmap(&mut part, &mapper).expect("unmaps");
    assert_eq!(*mapper.unmaps.borrow(), [0]);
    held.release(part).expect("unmapped");
    held.reserve(PAGE).expect("free again");
}

#[test]
fn a_part_maps_once_and_what_its_window_writes_it_reads() {
    let mapper = Mapper::over(4 * PAGE);
    let mut held = region(4);
    let _first = held.reserve(PAGE).expect("a page");
    let mut part = held.reserve(2 * PAGE).expect("two pages");
    let window = held
        .map(&mut part, &mapper, RegionCaching::WriteCombine)
        .expect("maps");
    assert_eq!((window.offset(), window.len()), (PAGE, 2 * PAGE_SIZE));
    window.write(5, b"blob").expect("in bounds");
    held.map(&mut part, &mapper, RegionCaching::WriteCombine)
        .expect("already mapped");
    assert!(
        matches!(
            held.map(&mut part, &mapper, RegionCaching::WriteBack),
            Err(VirtioError::Host(DriverError::Busy))
        ),
        "a part keeps the caching it was mapped with"
    );
    assert_eq!(
        *mapper.maps.borrow(),
        [(PAGE, 2 * PAGE_SIZE, RegionCaching::WriteCombine)],
        "the window it has, not a second mapping"
    );
    let mut back = [0u8; 4];
    part.window()
        .expect("mapped")
        .read(5, &mut back)
        .expect("in bounds");
    assert_eq!(&back, b"blob");
    assert_eq!(&mapper.memory.borrow()[PAGE_SIZE + 5..][..4], b"blob");
}

#[test]
fn a_refused_unmap_leaves_the_part_mapped_and_held() {
    let mut mapper = Mapper::over(PAGE);
    mapper.refuse_unmap = true;
    let mut held = region(1);
    let mut part = held.reserve(PAGE).expect("a page");
    held.map(&mut part, &mapper, RegionCaching::Uncached)
        .expect("maps");
    assert!(matches!(
        held.unmap(&mut part, &mapper),
        Err(VirtioError::Host(DriverError::DeviceFault))
    ));
    assert!(part.window().is_some(), "still mapped");
    held.release(part).expect_err("still mapped");
}

#[test]
fn a_part_of_another_region_is_refused() {
    let mapper = Mapper::over(4 * PAGE);
    let mut held = region(4);
    let mut other = region_at(0x2_0000_0000, 4);
    let _taken = held.reserve(PAGE).expect("a page");
    let mut stray = other.reserve(PAGE).expect("a page");
    assert!(matches!(
        held.map(&mut stray, &mapper, RegionCaching::WriteBack),
        Err(VirtioError::Host(DriverError::NotFound))
    ));
    let stray = held.release(stray).expect_err("not this region's");
    let mut longer = other.reserve(2 * PAGE).expect("two pages");
    assert!(matches!(
        held.unmap(&mut longer, &mapper),
        Err(VirtioError::Host(DriverError::NotFound))
    ));
    other.release(stray).expect("its own region's");
    assert!(mapper.maps.borrow().is_empty());
}
