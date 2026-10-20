//! Tests for the linear-framebuffer surface engine against an
//! in-process mock [`MmioMapper`] (mirrors the bus drivers' `MockMapper`).
//!
//! These live in the crate's `tests/` directory rather than beside the
//! code: minting a mock [`RegisterWindow`] requires its `unsafe`
//! constructor, and the library itself forbids `unsafe` outright — a
//! separate test crate keeps that guarantee intact.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;

use tairix_abi::driver::display::{
    BufferId, DamageRect, Display, DisplayFormat, DisplayMode, DisplayPower, Presented, SeatGate,
};
use tairix_abi::driver::DriverKind;
use tairix_abi::{CapabilityId, DriverError, DriverHost, MmioMapError, MmioMapper, RegisterWindow};
use tairix_display::testkit::HeapMemory;
use tairix_display::{Framebuffer, FramebufferConfig};

/// Stand-in for the kernel's MMIO-map facility. Backs every minted
/// [`RegisterWindow`] with a fixed `u32` buffer (≥ 4-byte aligned,
/// matching the page-aligned mapping the real mapper produces) shared
/// with the test so it can read scan-out bytes back. `granted` models
/// the mapper-side `CAP_MMIO_MAP` check.
struct MockMapper {
    backing: Rc<RefCell<Vec<u32>>>,
    granted: bool,
}

impl MockMapper {
    fn new(words: usize, granted: bool) -> Self {
        Self {
            backing: Rc::new(RefCell::new(vec![0u32; words])),
            granted,
        }
    }

    /// Read a single scan-out byte back from the shared backing. The
    /// host runs little-endian, so byte `off` of word `off / 4` sits
    /// in lane `off % 4`.
    fn byte(&self, off: usize) -> u8 {
        self.backing.borrow()[off / 4].to_le_bytes()[off % 4]
    }
}

impl MmioMapper for MockMapper {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        if !self.granted {
            return Err(MmioMapError::CapabilityMissing);
        }
        if len == 0 {
            return Err(MmioMapError::InvalidRegion);
        }
        let mut backing = self.backing.borrow_mut();
        if len > backing.len() * 4 {
            return Err(MmioMapError::Unsupported);
        }
        let base = NonNull::new(backing.as_mut_ptr().cast::<u8>()).expect("non-null heap buffer");
        // SAFETY: `base` covers `backing.len() * 4 >= len` bytes and is
        // 4-byte aligned (the `Vec<u32>` allocation guarantee); the
        // backing is held in an `Rc` that outlives every window minted
        // here, and the tests touch it through `byte()` only between —
        // never during — the engine's writes.
        Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
    }
}

/// Settable stand-in for the kernel's live seat-lease check: the test
/// flips the verdict between frames to model an administrative revoke
/// (and a fresh grant) without a kernel registry.
struct MockGate {
    verdict: Cell<Result<(), DriverError>>,
}

impl SeatGate for MockGate {
    fn check_present(&self) -> Result<(), DriverError> {
        self.verdict.get()
    }
}

/// Mock driver host. `mmio_map` models the load-time capability grant;
/// `mapper` is `None` to model a host with no MMIO-map facility; `gate`
/// is `None` to model a host with no seat wired (present then proceeds
/// ungated).
struct MockHost {
    mmio_map: bool,
    mapper: Option<MockMapper>,
    gate: Option<MockGate>,
}

impl MockHost {
    fn full(words: usize) -> Self {
        Self {
            mmio_map: true,
            mapper: Some(MockMapper::new(words, true)),
            gate: None,
        }
    }
}

impl DriverHost for MockHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        match cap {
            CapabilityId::MMIO_MAP => self.mmio_map,
            _ => false,
        }
    }
    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
    fn mmio_mapper(&self) -> Option<&dyn MmioMapper> {
        self.mapper.as_ref().map(|m| m as &dyn MmioMapper)
    }
    fn seat_gate(&self) -> Option<&dyn SeatGate> {
        self.gate.as_ref().map(|g| g as &dyn SeatGate)
    }
}

fn config(width: u32, height: u32, stride: u32) -> FramebufferConfig {
    FramebufferConfig {
        phys_base: 0xFD00_0000,
        width_px: width,
        height_px: height,
        stride_bytes: stride,
        format: DisplayFormat::Bgra8888,
    }
}

type Engine<'h> = Framebuffer<'h, HeapMemory>;

fn open(
    host: &MockHost,
    config: FramebufferConfig,
) -> Result<(Engine<'_>, HeapMemory), DriverError> {
    let memory = HeapMemory::default();
    Framebuffer::open(host, config, memory.clone()).map(|fb| (fb, memory))
}

/// Allocate a whole-output buffer holding `content`.
fn drawn(fb: &mut Engine<'_>, memory: &HeapMemory, content: &[u8]) -> BufferId {
    let mode = fb.output().expect("an output");
    let buffer = fb
        .alloc_buffer(mode.width_px, mode.height_px)
        .expect("allocated");
    memory.fill(buffer.region, content).expect("drawn");
    buffer.id
}

fn rect(x: u32, y: u32, width_px: u32, height_px: u32) -> DamageRect {
    DamageRect {
        x,
        y,
        width_px,
        height_px,
    }
}

#[test]
fn open_reports_the_configured_mode_and_presents_from_one_buffer() {
    let host = MockHost::full(8);
    let (fb, _) = open(&host, config(4, 2, 16)).expect("open");
    assert_eq!(
        fb.output(),
        Some(DisplayMode {
            width_px: 4,
            height_px: 2,
            stride_bytes: 16,
            format: DisplayFormat::Bgra8888,
        })
    );
    assert_eq!(
        fb.frame_count(),
        1,
        "a present has copied its buffer on return"
    );
}

#[test]
fn a_presented_buffer_reaches_the_surface() {
    let host = MockHost::full(8);
    let (mut fb, memory) = open(&host, config(4, 2, 16)).expect("open");
    let frame: Vec<u8> = (0..32u8).collect();
    let buffer = drawn(&mut fb, &memory, &frame.clone());
    assert_eq!(
        fb.present(buffer, &[rect(0, 0, 4, 2)]),
        Ok(Presented::Shown)
    );
    let mapper = host.mapper.as_ref().expect("mapper");
    for (off, expected) in frame.iter().enumerate() {
        assert_eq!(mapper.byte(off), *expected, "byte {off}");
    }
}

#[test]
fn a_padded_surface_takes_each_row_at_its_own_stride() {
    // Three 4-byte pixels a row, the surface padding each row to 13 bytes.
    let host = MockHost::full(8);
    let (mut fb, memory) = open(&host, config(3, 2, 13)).expect("open");
    let frame: Vec<u8> = (0..24u8).map(|b| b.wrapping_mul(7)).collect();
    let buffer = drawn(&mut fb, &memory, &frame.clone());
    fb.present(buffer, &[rect(0, 0, 3, 2)]).expect("present");
    let mapper = host.mapper.as_ref().expect("mapper");
    for row in 0..2 {
        for column in 0..12 {
            assert_eq!(
                mapper.byte(row * 13 + column),
                frame[row * 12 + column],
                "row {row} byte {column}"
            );
        }
    }
}

#[test]
fn only_the_damaged_spans_are_copied() {
    let host = MockHost::full(8);
    let (mut fb, memory) = open(&host, config(4, 2, 16)).expect("open");
    let buffer = drawn(&mut fb, &memory, &[0xEEu8; 32]);
    fb.present(buffer, &[rect(1, 1, 2, 1)])
        .expect("region present");
    let mapper = host.mapper.as_ref().expect("mapper");
    for off in 0..32 {
        let expected = if (20..28).contains(&off) { 0xEE } else { 0x00 };
        assert_eq!(mapper.byte(off), expected, "byte {off}");
    }
}

#[test]
fn a_whole_damage_list_is_copied_or_none_of_it() {
    let host = MockHost::full(8);
    let (mut fb, memory) = open(&host, config(4, 2, 16)).expect("open");
    let buffer = drawn(&mut fb, &memory, &[0xEEu8; 32]);
    let mapper = host.mapper.as_ref().expect("mapper");
    for bad in [
        [rect(0, 0, 1, 1), rect(3, 0, 2, 1)],
        [rect(0, 0, 1, 1), rect(0, 0, 0, 1)],
    ] {
        assert_eq!(fb.present(buffer, &bad), Err(DriverError::LengthOutOfRange));
        for off in 0..32 {
            assert_eq!(mapper.byte(off), 0x00, "byte {off} was written anyway");
        }
    }
    fb.present(buffer, &[rect(0, 0, 1, 1), rect(3, 1, 1, 1)])
        .expect("both corners");
    for off in 0..32 {
        let expected = if (4..28).contains(&off) { 0x00 } else { 0xEE };
        assert_eq!(mapper.byte(off), expected, "byte {off}");
    }
}

#[test]
fn only_a_live_buffer_of_the_outputs_size_is_shown() {
    let host = MockHost::full(8);
    let (mut fb, memory) = open(&host, config(4, 2, 16)).expect("open");
    let whole = rect(0, 0, 4, 2);
    let small = fb.alloc_buffer(2, 2).expect("a smaller buffer").id;
    assert_eq!(fb.present(small, &[whole]), Err(DriverError::Stale));
    let buffer = drawn(&mut fb, &memory, &[0x11u8; 32]);
    assert_eq!(fb.free_buffer(buffer), Ok(()));
    assert_eq!(fb.present(buffer, &[whole]), Err(DriverError::NotFound));
    assert_eq!(fb.free_buffer(buffer), Err(DriverError::NotFound));
    let mapper = host.mapper.as_ref().expect("mapper");
    assert_eq!(mapper.byte(0), 0x00);
}

#[test]
fn allocation_is_bounded_by_the_output_and_a_few_surfaces_of_memory() {
    let host = MockHost::full(8);
    let (mut fb, _) = open(&host, config(4, 2, 16)).expect("open");
    for (width, height) in [(0, 2), (4, 0), (5, 2), (4, 3)] {
        assert_eq!(
            fb.alloc_buffer(width, height).err(),
            Some(DriverError::LengthOutOfRange)
        );
    }
    let held: Vec<_> = (0..4)
        .map(|_| fb.alloc_buffer(4, 2).expect("within the budget").id)
        .collect();
    assert_eq!(fb.alloc_buffer(1, 1).err(), Some(DriverError::NoSpace));
    fb.free_buffer(held[0]).expect("freed");
    assert!(fb.alloc_buffer(4, 2).is_ok(), "freeing returns its bytes");
    let ids: Vec<_> = held.iter().map(|id| id.get()).collect();
    assert_eq!(
        ids,
        [1, 2, 3, 4],
        "ids are the engine's, never reused within a load"
    );
}

#[test]
fn shared_memory_that_cannot_be_had_is_refused() {
    let host = MockHost::full(8);
    let memory = HeapMemory::refusing();
    let mut fb = Framebuffer::open(&host, config(4, 2, 16), memory).expect("open");
    assert_eq!(fb.alloc_buffer(4, 2).err(), Some(DriverError::OutOfMemory));
}

#[test]
fn a_present_is_seat_gated_before_any_write() {
    let mut host = MockHost::full(8);
    host.gate = Some(MockGate {
        verdict: Cell::new(Ok(())),
    });
    let (mut fb, memory) = open(&host, config(4, 2, 16)).expect("open");
    let whole = rect(0, 0, 4, 2);
    let first = drawn(&mut fb, &memory, &[0x11u8; 32]);
    fb.present(first, &[whole]).expect("live lease presents");
    let gate = host.gate.as_ref().expect("gate installed");
    let mapper = host.mapper.as_ref().expect("mapper installed");
    let overwrite = drawn(&mut fb, &memory, &[0x22u8; 32]);
    for refusal in [DriverError::SeatRevoked, DriverError::PermissionDenied] {
        gate.verdict.set(Err(refusal));
        assert_eq!(fb.present(overwrite, &[whole]), Err(refusal));
        assert_eq!(mapper.byte(0), 0x11, "a refused present writes nothing");
    }
    gate.verdict.set(Ok(()));
    fb.present(overwrite, &[whole])
        .expect("live lease presents again");
    assert_eq!(mapper.byte(31), 0x22);
}

#[test]
fn open_requires_mmio_map_capability_on_host() {
    let host = MockHost {
        mmio_map: false,
        mapper: Some(MockMapper::new(8, true)),
        gate: None,
    };
    assert_eq!(
        open(&host, config(4, 2, 16)).err(),
        Some(DriverError::PermissionDenied)
    );
}

#[test]
fn open_surfaces_mapper_capability_denial() {
    let host = MockHost {
        mmio_map: true,
        mapper: Some(MockMapper::new(8, false)),
        gate: None,
    };
    assert_eq!(
        open(&host, config(4, 2, 16)).err(),
        Some(DriverError::PermissionDenied)
    );
}

#[test]
fn open_without_mapper_is_unsupported() {
    let host = MockHost {
        mmio_map: true,
        mapper: None,
        gate: None,
    };
    assert_eq!(
        open(&host, config(4, 2, 16)).err(),
        Some(DriverError::Unsupported)
    );
}

#[test]
fn open_rejects_region_larger_than_platform_can_map() {
    let host = MockHost::full(4);
    assert_eq!(
        open(&host, config(4, 2, 16)).err(),
        Some(DriverError::Unsupported)
    );
}

#[test]
fn open_rejects_degenerate_geometry() {
    let host = MockHost::full(8);
    for geometry in [config(0, 2, 16), config(4, 0, 16), config(4, 2, 12)] {
        assert_eq!(
            open(&host, geometry).err(),
            Some(DriverError::LengthOutOfRange)
        );
    }
}

#[test]
fn unload_then_reload_presents_again() {
    let host = MockHost::full(8);
    let whole = rect(0, 0, 4, 2);
    {
        let (mut fb, memory) = open(&host, config(4, 2, 16)).expect("first load");
        let buffer = drawn(&mut fb, &memory, &[0x11u8; 32]);
        fb.present(buffer, &[whole]).expect("present");
    }
    let (mut reloaded, memory) = open(&host, config(4, 2, 16)).expect("reload");
    let buffer = drawn(&mut reloaded, &memory, &[0x22u8; 32]);
    reloaded.present(buffer, &[whole]).expect("present");
    let mapper = host.mapper.as_ref().expect("mapper");
    for off in 0..32 {
        assert_eq!(mapper.byte(off), 0x22, "byte {off}");
    }
}

#[test]
fn a_linear_surface_has_no_power_control_to_switch() {
    let host = MockHost::full(8);
    let (mut fb, _) = open(&host, config(4, 2, 16)).expect("load");
    for power in [DisplayPower::Off, DisplayPower::On] {
        assert_eq!(fb.set_power(power), Err(DriverError::Unsupported));
    }
}
