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
fn the_reference_model_passes_the_interrupt_suite() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    conformance::run_interrupts(&unit, &unit, [0x0010, 0x0208]);
}

#[test]
#[should_panic(expected = "a released entry still delivered")]
fn the_interrupt_suite_fails_a_unit_whose_release_leaves_the_cache() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::StaleRelease);
    unit.enable().unwrap();
    conformance::run_interrupts(&unit, &unit, [0x0010, 0x0208]);
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

fn read_write(range: Range<u64>) -> IdentityWindow {
    IdentityWindow {
        range,
        access: Access::READ_WRITE,
    }
}

/// A window firmware keeps for reading alone is mapped for reading alone,
/// beside one it lets the device write; two of different access that overlap
/// leave no one answer for what they share, so the domain is refused.
#[test]
fn a_window_is_mapped_for_the_access_firmware_allows_there() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let windows = [
        IdentityWindow {
            range: 0x7B80_0000..0x7B90_0000,
            access: Access::READ,
        },
        read_write(0x7B90_0000..0x7BA0_0000),
    ];
    let mut domain = Domain::new(&unit, &windows).unwrap();
    domain.attach(0x00A0).unwrap();
    assert_eq!(unit.access(0x00A0, 0x7B80_0000, false), Some(0x7B80_0000));
    assert_eq!(unit.access(0x00A0, 0x7B80_0000, true), None, "read-only");
    assert_eq!(unit.access(0x00A0, 0x7B90_0000, true), Some(0x7B90_0000));
    domain.destroy().unwrap();
    let crossing = [
        IdentityWindow {
            range: 0x7B80_0000..0x7B90_0000,
            access: Access::READ,
        },
        read_write(0x7B8F_0000..0x7BA0_0000),
    ];
    assert_eq!(
        Domain::new(&unit, &crossing).err(),
        Some(IommuError::OutOfRange)
    );
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
    ]
    .map(read_write);
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
    let mut domain = Domain::new(&unit, &[read_write(window.clone())]).unwrap();
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
    let past_reach = read_write((1 << 39)..(1 << 39) + PAGE);
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

/// An attach the unit could not confirm may still translate the stream, so
/// the teardown blocks it before the domain's tables go.
#[test]
fn a_stream_whose_attach_went_unconfirmed_is_blocked_by_the_teardown() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::UnconfirmedAttach);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[]).unwrap();
    let id = domain.id();
    assert_eq!(domain.attach(4), Err(IommuError::Unconfirmed));
    assert_eq!(domain.streams(), &[4]);
    unit.behave(Behaviour::Correct);
    domain.attach(4).unwrap();
    assert_eq!(domain.streams(), &[4], "recorded once");
    assert_eq!(unit.attached(4), Some(id));
    domain.destroy().unwrap();
    assert_eq!(unit.attached(4), None);
    assert_eq!(frames.live(), 0);
}

/// Blocking a silenced stream leaves it silent: only an attach ends silence.
#[test]
fn the_model_keeps_a_blocked_stream_silent() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    unit.silence(6).unwrap();
    unit.block(6).unwrap();
    assert!(unit.silenced(6));
}
