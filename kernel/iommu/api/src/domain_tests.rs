extern crate std;

use super::*;
use crate::conformance::{self, Fixture, TranslationProbe};
use crate::hostmem::HostFrames;
use crate::model::{Behaviour, ModelUnit};

const PAGE: u64 = IO_PAGE_SIZE;

#[test]
fn the_reference_model_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    conformance::run_all(
        &unit,
        &unit,
        &Fixture {
            streams: [0x0010, 0x0018],
            pages: [0x8000_0000, 0x8000_1000],
        },
    );
    assert_eq!(unit.domains(), 0);
    assert_eq!(frames.live(), 0);
}

#[test]
#[should_panic(expected = "a translation survived a confirmed sync")]
fn the_suite_fails_a_unit_whose_sync_leaves_the_cache() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::StaleSync);
    unit.enable().unwrap();
    conformance::run_all(
        &unit,
        &unit,
        &Fixture {
            streams: [1, 2],
            pages: [0x8000_0000, 0x8000_1000],
        },
    );
}

#[test]
fn a_block_maps_top_down_at_its_own_alignment_below_the_reach() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    domain.attach(9).unwrap();
    let iova = domain.map(0x4000_0000, 9, 0).unwrap();
    assert_eq!(iova, (1 << 39) - (PAGE << 9));
    let low = domain.map(0x4020_0000, 0, 1 << 32).unwrap();
    assert!(low + PAGE <= 1 << 32);
    assert!(
        !(0xFEE0_0000..0xFEF0_0000).contains(&low),
        "the interrupt window stays reserved"
    );
    assert_eq!(unit.access(9, iova + 0x1234, true), Some(0x4000_1234));
    assert_eq!(unit.access(9, low, false), Some(0x4020_0000));
    domain.destroy().unwrap();
    assert_eq!(frames.live(), 0);
}

#[test]
fn an_unmap_is_confirmed_before_the_iova_comes_back() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    domain.attach(3).unwrap();
    let first = domain.map(0x9000_0000, 0, 0).unwrap();
    assert!(unit.access(3, first, true).is_some());
    domain.unmap(first).unwrap();
    assert_eq!(unit.access(3, first, true), None);
    assert_eq!(domain.mapped(), 0);
    assert_eq!(domain.unmap(first), Err(IommuError::NotMapped));
    let again = domain.map(0x9000_1000, 0, 0).unwrap();
    assert_eq!(again, first, "a confirmed IOVA is reused");
}

/// A retried unmap must not take an IOVA for gone because the first attempt
/// already forgot it: only a confirmed sync frees it.
#[test]
fn an_unconfirmed_unmap_stays_recorded_until_a_later_sync_confirms_it() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::UnconfirmedSync);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    let first = domain.map(0x9000_0000, 0, 0).unwrap();
    assert_eq!(domain.unmap(first), Err(IommuError::Unconfirmed));
    assert_eq!(
        domain.unmap(first),
        Err(IommuError::Unconfirmed),
        "a retry is still unconfirmed, never not-mapped"
    );
    assert_eq!(domain.mapped(), 1);
    let next = domain.map(0x9000_1000, 0, 0).unwrap();
    assert_ne!(next, first, "an unconfirmed IOVA is not reused");
    unit.behave(Behaviour::Correct);
    domain.unmap(first).unwrap();
    assert_eq!(domain.mapped(), 1);
    assert_eq!(domain.unmap(first), Err(IommuError::NotMapped));
    assert_eq!(
        domain.map(0x9000_2000, 0, 0).unwrap(),
        first,
        "a confirmed IOVA comes back"
    );
    domain.destroy().unwrap();
}

#[test]
fn windows_firmware_names_twice_or_overlapping_are_mapped_once() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let windows = [
        0x7B80_0000..0x7B90_0000,
        0x7B80_0000..0x7B90_0000,
        0x7B88_0000..0x7BA0_0000,
        0x7BA0_0000..0x7BA0_1000,
        0x7C00_0000..0x7C00_0000,
    ];
    let mut domain = Domain::new(&unit, &windows).unwrap();
    domain.attach(0x00A0).unwrap();
    for inside in [0x7B80_0000, 0x7B9F_F000, 0x7BA0_0FFF] {
        assert_eq!(unit.access(0x00A0, inside, true), Some(inside));
    }
    assert_eq!(unit.access(0x00A0, 0x7BA0_1000, true), None);
    domain.destroy().unwrap();
    assert_eq!(frames.live(), 0);
}

#[test]
fn identity_windows_are_kept_mapped_and_out_of_the_iova_space() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let window = 0x7B80_0000..0x7C00_0000;
    let mut domain = Domain::new(&unit, core::slice::from_ref(&window)).unwrap();
    domain.attach(0x00A0).unwrap();
    assert_eq!(unit.access(0x00A0, 0x7B80_0123, true), Some(0x7B80_0123));
    for _ in 0..64 {
        let iova = domain.map(0x2_0000_0000, 7, 0x7C00_0000).unwrap();
        assert!(iova + (PAGE << 7) <= window.start || iova >= window.end);
    }
}

#[test]
fn a_block_the_unit_cannot_name_is_refused() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    assert_eq!(domain.map(1 << 46, 0, 0), Err(IommuError::OutOfRange));
    assert_eq!(domain.map(0x1001, 0, 0), Err(IommuError::OutOfRange));
    assert_eq!(domain.map(0x1000, 60, 0), Err(IommuError::OutOfRange));
    assert_eq!(domain.map(0x1000, 0, PAGE), Err(IommuError::Exhausted));
    let past_reach = (1 << 39)..(1 << 39) + PAGE;
    assert_eq!(
        Domain::new(&unit, core::slice::from_ref(&past_reach)).err(),
        Some(IommuError::OutOfRange)
    );
}

#[test]
fn a_dropped_domain_blocks_its_streams() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let iova = {
        let mut domain = Domain::new(&unit, &[]).unwrap();
        domain.attach(5).unwrap();
        domain.map(0x7000_0000, 0, 0).unwrap()
    };
    assert_eq!(unit.access(5, iova, false), None);
    assert_eq!(unit.domains(), 0);
}
