//! Unit tests for the VESA driver: the VBE `ModeInfoBlock` decoder, and the
//! surface it opens through the shared engine against an in-process mock
//! [`MmioMapper`].

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ptr::NonNull;

use super::*;
use core::cell::Cell;
use tairix_abi::driver::display::{DamageRect, Display, DisplayMode, Presented, SeatGate};
use tairix_abi::driver::DriverKind;
use tairix_abi::{CapabilityId, MmioMapError, MmioMapper, RegisterWindow};
use tairix_display::testkit::HeapMemory;

/// Build a well-formed VBE `ModeInfoBlock` describing a 32-bpp
/// direct-colour linear-framebuffer mode with the `Bgra8888` layout
/// (red at bit 16, green at bit 8, blue at bit 0). Tests mutate single
/// fields to exercise each rejection path.
fn bgra_block(width: u16, height: u16, stride: u16, phys_base: u32) -> Vec<u8> {
    let mut b = vec![0u8; VBE_MODE_INFO_BLOCK_LEN];
    // ModeAttributes: supported (bit 0) + linear framebuffer (bit 7).
    let attrs: u16 = MODE_ATTR_SUPPORTED | MODE_ATTR_LINEAR_FRAMEBUFFER;
    b[field::MODE_ATTRIBUTES..field::MODE_ATTRIBUTES + 2].copy_from_slice(&attrs.to_le_bytes());
    b[field::BYTES_PER_SCAN_LINE..field::BYTES_PER_SCAN_LINE + 2]
        .copy_from_slice(&stride.to_le_bytes());
    b[field::X_RESOLUTION..field::X_RESOLUTION + 2].copy_from_slice(&width.to_le_bytes());
    b[field::Y_RESOLUTION..field::Y_RESOLUTION + 2].copy_from_slice(&height.to_le_bytes());
    b[field::BITS_PER_PIXEL] = SUPPORTED_BITS_PER_PIXEL;
    b[field::MEMORY_MODEL] = MEMORY_MODEL_DIRECT_COLOUR;
    b[field::RED_MASK_SIZE] = SUPPORTED_CHANNEL_MASK_BITS;
    b[field::GREEN_MASK_SIZE] = SUPPORTED_CHANNEL_MASK_BITS;
    b[field::BLUE_MASK_SIZE] = SUPPORTED_CHANNEL_MASK_BITS;
    b[field::RED_FIELD_POSITION] = 16;
    b[field::GREEN_FIELD_POSITION] = 8;
    b[field::BLUE_FIELD_POSITION] = 0;
    b[field::PHYS_BASE_PTR..field::PHYS_BASE_PTR + 4].copy_from_slice(&phys_base.to_le_bytes());
    b
}

/// Stand-in for the kernel's MMIO-map facility, backing every minted
/// [`RegisterWindow`] with a fixed `u32` buffer (≥ 4-byte aligned) shared
/// with the test so it can read scan-out bytes back. `granted` models the
/// mapper-side `CAP_MMIO_MAP` check.
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
        // never during — the driver's writes.
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

/// Mock driver host. `drv_load` / `mmio_map` model the load-time
/// capability grants; `mapper` is `None` to model a host with no
/// MMIO-map facility; `gate` is `None` to model a host with no seat
/// wired (present then proceeds ungated).
struct MockHost {
    drv_load: bool,
    mmio_map: bool,
    mapper: Option<MockMapper>,
    gate: Option<MockGate>,
}

impl MockHost {
    fn full(words: usize) -> Self {
        Self {
            drv_load: true,
            mmio_map: true,
            mapper: Some(MockMapper::new(words, true)),
            gate: None,
        }
    }
}

impl DriverHost for MockHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        match cap {
            CapabilityId::DRV_LOAD => self.drv_load,
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

#[test]
fn register_requires_drv_load() {
    let granted = MockHost {
        drv_load: true,
        mmio_map: false,
        mapper: None,
        gate: None,
    };
    assert!(register(&granted).is_ok());
    let denied = MockHost {
        drv_load: false,
        mmio_map: false,
        mapper: None,
        gate: None,
    };
    assert_eq!(register(&denied), Err(DriverError::PermissionDenied));
}

#[test]
fn parse_decodes_bgra_geometry() {
    let block = bgra_block(4, 2, 16, 0xFD00_0000);
    assert_eq!(
        parse_mode_info(&block),
        Ok(FramebufferConfig {
            phys_base: 0xFD00_0000,
            width_px: 4,
            height_px: 2,
            stride_bytes: 16,
            format: DisplayFormat::Bgra8888,
        })
    );
}

#[test]
fn parse_decodes_rgba_layout() {
    let mut block = bgra_block(4, 2, 16, 0xFD00_0000);
    block[field::RED_FIELD_POSITION] = 0;
    block[field::BLUE_FIELD_POSITION] = 16;
    assert_eq!(
        parse_mode_info(&block).map(|config| config.format),
        Ok(DisplayFormat::Rgba8888)
    );
}

#[test]
fn parse_rejects_short_block() {
    let short = vec![0u8; VBE_MODE_INFO_BLOCK_LEN - 1];
    assert_eq!(parse_mode_info(&short), Err(DriverError::BufferTooSmall));
}

#[test]
fn parse_rejects_every_mode_it_cannot_present() {
    let set = |block: &mut Vec<u8>, at: usize, value: u8| block[at] = value;
    let mut cases: Vec<Vec<u8>> = Vec::new();
    for attrs in [MODE_ATTR_LINEAR_FRAMEBUFFER, MODE_ATTR_SUPPORTED] {
        let mut block = bgra_block(4, 2, 16, 0xFD00_0000);
        block[field::MODE_ATTRIBUTES..field::MODE_ATTRIBUTES + 2]
            .copy_from_slice(&attrs.to_le_bytes());
        cases.push(block);
    }
    for (at, value) in [
        (field::MEMORY_MODEL, 4),
        (field::BITS_PER_PIXEL, 24),
        (field::GREEN_MASK_SIZE, 6),
    ] {
        let mut block = bgra_block(4, 2, 16, 0xFD00_0000);
        set(&mut block, at, value);
        cases.push(block);
    }
    let mut layout = bgra_block(4, 2, 16, 0xFD00_0000);
    set(&mut layout, field::RED_FIELD_POSITION, 8);
    set(&mut layout, field::GREEN_FIELD_POSITION, 0);
    set(&mut layout, field::BLUE_FIELD_POSITION, 16);
    cases.push(layout);
    for block in cases {
        assert_eq!(parse_mode_info(&block), Err(DriverError::Unsupported));
    }
    assert_eq!(
        parse_mode_info(&bgra_block(4, 2, 16, 0)),
        Err(DriverError::DeviceFault),
        "a block naming no surface"
    );
}

#[test]
fn open_refuses_a_degenerate_geometry_the_block_carries() {
    let host = MockHost::full(8);
    for block in [
        bgra_block(0, 2, 16, 0xFD00_0000),
        bgra_block(4, 0, 16, 0xFD00_0000),
        bgra_block(4, 2, 12, 0xFD00_0000),
    ] {
        assert_eq!(
            open(&host, &block, HeapMemory::default()).err(),
            Some(DriverError::LengthOutOfRange)
        );
    }
}

#[test]
fn open_reports_the_decoded_mode_and_presents_through_the_engine() {
    let host = MockHost::full(8);
    let frame: Vec<u8> = (0..32u8).collect();
    let memory = HeapMemory::default();
    let mut fb = open(&host, &bgra_block(4, 2, 16, 0xFD00_0000), memory.clone()).expect("open");
    assert_eq!(
        fb.output(),
        Some(DisplayMode {
            width_px: 4,
            height_px: 2,
            stride_bytes: 16,
            format: DisplayFormat::Bgra8888,
        })
    );
    let buffer = fb.alloc_buffer(4, 2).expect("a buffer");
    memory.fill(buffer.region, &frame).expect("drawn");
    let whole = DamageRect {
        x: 0,
        y: 0,
        width_px: 4,
        height_px: 2,
    };
    assert_eq!(fb.present(buffer.id, &[whole]), Ok(Presented::Shown));
    let mapper = host.mapper.as_ref().expect("mapper");
    for (off, expected) in frame.iter().enumerate() {
        assert_eq!(mapper.byte(off), *expected, "byte {off}");
    }
}

#[test]
fn open_surfaces_every_refusal() {
    let block = bgra_block(4, 2, 16, 0xFD00_0000);
    let no_cap = MockHost {
        drv_load: true,
        mmio_map: false,
        mapper: Some(MockMapper::new(8, true)),
        gate: None,
    };
    assert_eq!(
        open(&no_cap, &block, HeapMemory::default()).err(),
        Some(DriverError::PermissionDenied)
    );
    let refused = MockHost {
        drv_load: true,
        mmio_map: true,
        mapper: Some(MockMapper::new(8, false)),
        gate: None,
    };
    assert_eq!(
        open(&refused, &block, HeapMemory::default()).err(),
        Some(DriverError::PermissionDenied)
    );
    let no_mapper = MockHost {
        drv_load: true,
        mmio_map: true,
        mapper: None,
        gate: None,
    };
    assert_eq!(
        open(&no_mapper, &block, HeapMemory::default()).err(),
        Some(DriverError::Unsupported)
    );
    assert_eq!(
        open(&MockHost::full(4), &block, HeapMemory::default()).err(),
        Some(DriverError::Unsupported),
        "a surface larger than the platform maps"
    );
    assert_eq!(
        open(&MockHost::full(8), &block[..8], HeapMemory::default()).err(),
        Some(DriverError::BufferTooSmall)
    );
}

#[test]
fn a_present_is_gated_on_the_live_seat_lease() {
    let mut host = MockHost::full(8);
    host.gate = Some(MockGate {
        verdict: Cell::new(Err(DriverError::SeatRevoked)),
    });
    let memory = HeapMemory::default();
    let mut fb = open(&host, &bgra_block(4, 2, 16, 0xFD00_0000), memory.clone()).expect("open");
    let allocated = fb.alloc_buffer(4, 2).expect("a buffer");
    memory.fill(allocated.region, &[0x11; 32]).expect("drawn");
    let buffer = allocated.id;
    let whole = DamageRect {
        x: 0,
        y: 0,
        width_px: 4,
        height_px: 2,
    };
    assert_eq!(fb.present(buffer, &[whole]), Err(DriverError::SeatRevoked));
    let mapper = host.mapper.as_ref().expect("mapper");
    assert_eq!(mapper.byte(0), 0x00);
    if let Some(gate) = host.gate.as_ref() {
        gate.verdict.set(Ok(()));
    }
    assert_eq!(fb.present(buffer, &[whole]), Ok(Presented::Shown));
    assert_eq!(mapper.byte(0), 0x11);
}
