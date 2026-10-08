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

/// A unit whose tables cannot leave a write's read out passes too: the
/// suite holds the profile to what the tables do, not to one answer.
#[test]
fn a_unit_whose_tables_grant_no_write_alone_passes_the_conformance_suite() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::NoWriteOnly);
    unit.enable().unwrap();
    assert!(!unit.profile().write_only);
    conformance::run_all(
        &unit,
        &unit,
        &Fixture {
            streams: [0x0010, 0x0018],
            pages: [0x8000_0000, 0x8000_1000],
        },
    );
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
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    domain.attach(9).unwrap();
    let iova = domain
        .map(
            &[FrameRun {
                phys: 0x4000_0000,
                order: 9,
            }],
            0,
        )
        .unwrap();
    assert_eq!(iova, (1 << 39) - (PAGE << 9));
    let low = domain
        .map(
            &[FrameRun {
                phys: 0x4020_0000,
                order: 0,
            }],
            1 << 32,
        )
        .unwrap();
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

/// A carve of several runs, largest first, lands back to back at one IOVA
/// block of the next power of two, each run at its own alignment.
#[test]
fn runs_map_back_to_back_at_one_iova() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    domain.attach(9).unwrap();
    let runs = [
        FrameRun {
            phys: 0x4000_0000,
            order: 9,
        },
        FrameRun {
            phys: 0x7000_0000,
            order: 8,
        },
        FrameRun {
            phys: 0x5000_3000,
            order: 0,
        },
    ];
    let iova = domain.map(&runs, 0).unwrap();
    assert!(
        iova.is_multiple_of(PAGE << 10),
        "the block of the next power of two"
    );
    assert_eq!(unit.access(9, iova + 0x1_2345, true), Some(0x4001_2345));
    assert_eq!(
        unit.access(9, iova + (PAGE << 9) + 0x10, false),
        Some(0x7000_0010)
    );
    let last = iova + (PAGE << 9) + (PAGE << 8);
    assert_eq!(unit.access(9, last + 0x8, false), Some(0x5000_3008));
    assert_eq!(unit.access(9, last + PAGE, false), None, "past the runs");
    domain.unmap(iova).unwrap();
    assert_eq!(unit.access(9, iova, false), None);
    assert_eq!(domain.map(&runs, 0), Ok(iova), "the block came back whole");
    domain.destroy().unwrap();
    assert_eq!(frames.live(), 0);
}

/// Runs out of order, misaligned to their size or none at all are refused
/// before anything is mapped; a run the unit refuses takes the runs before it
/// back, and the IOVA is reused once that is confirmed.
#[test]
fn a_carve_of_runs_is_refused_whole() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    domain.attach(9).unwrap();
    let small = FrameRun {
        phys: 0x5000_0000,
        order: 0,
    };
    let large = FrameRun {
        phys: 0x4000_0000,
        order: 9,
    };
    assert_eq!(domain.map(&[small, large], 0), Err(IommuError::OutOfRange));
    let misaligned = FrameRun {
        phys: 0x4000_1000,
        order: 1,
    };
    assert_eq!(domain.map(&[misaligned], 0), Err(IommuError::OutOfRange));
    assert_eq!(domain.map(&[], 0), Err(IommuError::OutOfRange));
    assert_eq!(domain.mapped(), 0);

    // The block above keeps the tables the large run lands in, so only the
    // small run needs a table the frames can no longer give.
    let keeper = domain
        .map(
            &[FrameRun {
                phys: 0x3000_0000,
                order: 10,
            }],
            0,
        )
        .unwrap();
    let below = keeper - (PAGE << 10);
    frames.limit(0);
    assert_eq!(domain.map(&[large, small], 0), Err(IommuError::Exhausted));
    assert_eq!(
        unit.access(9, below, false),
        None,
        "the large run was taken back"
    );
    assert_eq!(domain.mapped(), 1);
    frames.limit(usize::MAX);
    assert_eq!(
        domain.map(&[large, small], 0),
        Ok(below),
        "its block came back"
    );
    domain.destroy().unwrap();
}

/// What a unit claims for a stream is never handed out, however the space
/// is filled, and a claim that is no page-aligned range is refused whole.
#[test]
fn a_streams_reserved_iova_is_never_handed_out() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let top = 1u64 << 39;
    let claimed = (top - (PAGE << 10))..(top - (PAGE << 8));
    let mut domain = Domain::new(&unit, &[], core::slice::from_ref(&claimed)).unwrap();
    let mut handed = std::vec::Vec::new();
    for _ in 0..8 {
        let iova = domain
            .map(
                &[FrameRun {
                    phys: 0x4000_0000,
                    order: 8,
                }],
                0,
            )
            .unwrap();
        assert!(
            iova + (PAGE << 8) <= claimed.start || claimed.end <= iova,
            "{iova:#x} lies in the claimed range"
        );
        handed.push(iova);
    }
    assert_eq!(handed[0], top - (PAGE << 8), "above the claim first");
    for iova in handed {
        domain.unmap(iova).unwrap();
    }
    for malformed in [PAGE..PAGE, (PAGE + 1)..(2 * PAGE)] {
        assert_eq!(
            Domain::new(&unit, &[], core::slice::from_ref(&malformed)).err(),
            Some(IommuError::OutOfRange)
        );
    }
}

#[test]
fn an_unmap_is_confirmed_before_the_iova_comes_back() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    domain.attach(3).unwrap();
    let first = domain
        .map(
            &[FrameRun {
                phys: 0x9000_0000,
                order: 0,
            }],
            0,
        )
        .unwrap();
    assert!(unit.access(3, first, true).is_some());
    domain.unmap(first).unwrap();
    assert_eq!(unit.access(3, first, true), None);
    assert_eq!(domain.mapped(), 0);
    assert_eq!(domain.unmap(first), Err(IommuError::NotMapped));
    let again = domain
        .map(
            &[FrameRun {
                phys: 0x9000_1000,
                order: 0,
            }],
            0,
        )
        .unwrap();
    assert_eq!(again, first, "a confirmed IOVA is reused");
}

/// A retried unmap must not take an IOVA for gone because the first attempt
/// already forgot it: only a confirmed sync frees it.
#[test]
fn an_unconfirmed_unmap_stays_recorded_until_a_later_sync_confirms_it() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::UnconfirmedSync);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    let first = domain
        .map(
            &[FrameRun {
                phys: 0x9000_0000,
                order: 0,
            }],
            0,
        )
        .unwrap();
    assert_eq!(domain.unmap(first), Err(IommuError::Unconfirmed));
    assert_eq!(
        domain.unmap(first),
        Err(IommuError::Unconfirmed),
        "a retry is still unconfirmed, never not-mapped"
    );
    assert_eq!(domain.mapped(), 1);
    let next = domain
        .map(
            &[FrameRun {
                phys: 0x9000_1000,
                order: 0,
            }],
            0,
        )
        .unwrap();
    assert_ne!(next, first, "an unconfirmed IOVA is not reused");
    unit.behave(Behaviour::Correct);
    domain.unmap(first).unwrap();
    assert_eq!(domain.mapped(), 1);
    assert_eq!(domain.unmap(first), Err(IommuError::NotMapped));
    assert_eq!(
        domain
            .map(
                &[FrameRun {
                    phys: 0x9000_2000,
                    order: 0
                }],
                0
            )
            .unwrap(),
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
    let mut domain = Domain::new(&unit, &windows, &[]).unwrap();
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
        Domain::new(&unit, &crossing, &[]).err(),
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
    let mut domain = Domain::new(&unit, &windows, &[]).unwrap();
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
    let mut domain = Domain::new(&unit, &[read_write(window.clone())], &[]).unwrap();
    domain.attach(0x00A0).unwrap();
    assert_eq!(unit.access(0x00A0, 0x7B80_0123, true), Some(0x7B80_0123));
    for _ in 0..64 {
        let iova = domain
            .map(
                &[FrameRun {
                    phys: 0x2_0000_0000,
                    order: 7,
                }],
                0x7C00_0000,
            )
            .unwrap();
        assert!(iova + (PAGE << 7) <= window.start || iova >= window.end);
    }
}

#[test]
fn a_block_the_unit_cannot_name_is_refused() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    assert_eq!(
        domain.map(
            &[FrameRun {
                phys: 1 << 46,
                order: 0
            }],
            0
        ),
        Err(IommuError::OutOfRange)
    );
    assert_eq!(
        domain.map(
            &[FrameRun {
                phys: 0x1001,
                order: 0
            }],
            0
        ),
        Err(IommuError::OutOfRange)
    );
    assert_eq!(
        domain.map(
            &[FrameRun {
                phys: 0x1000,
                order: 60
            }],
            0
        ),
        Err(IommuError::OutOfRange)
    );
    assert_eq!(
        domain.map(
            &[FrameRun {
                phys: 0x1000,
                order: 0
            }],
            PAGE
        ),
        Err(IommuError::Exhausted)
    );
    let past_reach = read_write((1 << 39)..(1 << 39) + PAGE);
    assert_eq!(
        Domain::new(&unit, core::slice::from_ref(&past_reach), &[]).err(),
        Some(IommuError::OutOfRange)
    );
}

#[test]
fn a_dropped_domain_blocks_its_streams() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let iova = {
        let mut domain = Domain::new(&unit, &[], &[]).unwrap();
        domain.attach(5).unwrap();
        domain
            .map(
                &[FrameRun {
                    phys: 0x7000_0000,
                    order: 0,
                }],
                0,
            )
            .unwrap()
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
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
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

fn page_carve(domain: &mut Domain<'_>, phys: u64) -> u64 {
    domain.map(&[FrameRun { phys, order: 0 }], 0).unwrap()
}

/// Mappings taken out of the tables leave the device at once, keep their
/// IOVAs until one invalidation confirms them all, and only then hand them
/// back.
#[test]
fn removed_mappings_are_confirmed_by_one_invalidation_before_their_iovas_return() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    domain.attach(3).unwrap();
    let carves: Vec<u64> = (0..3)
        .map(|i| page_carve(&mut domain, 0x9000_0000 + i * PAGE))
        .collect();
    let syncs = unit.syncs();
    for &iova in &carves[..2] {
        domain.remove(iova).unwrap();
        domain.remove(iova).unwrap();
        assert_eq!(unit.access(3, iova, true), None, "gone from the tables");
    }
    assert_eq!(unit.syncs(), syncs, "nothing confirmed yet");
    assert_eq!(domain.mapped(), 3);
    let held = page_carve(&mut domain, 0x9000_8000);
    assert!(!carves.contains(&held), "an unconfirmed IOVA is not reused");
    domain.confirm_removed().unwrap();
    assert_eq!(unit.syncs(), syncs + 1, "one invalidation for the batch");
    assert_eq!(domain.mapped(), 2);
    assert_eq!(domain.unmap(carves[0]), Err(IommuError::NotMapped));
    domain.confirm_removed().unwrap();
    assert_eq!(
        unit.syncs(),
        syncs + 1,
        "an empty batch asks nothing of the unit"
    );
    let reused: Vec<u64> = (0..2)
        .map(|i| page_carve(&mut domain, 0x9000_a000 + i * PAGE))
        .collect();
    assert!(
        reused.iter().all(|iova| carves[..2].contains(iova)),
        "confirmed IOVAs come back"
    );
}

#[test]
fn an_unconfirmed_batch_keeps_its_iovas_until_a_later_confirmation() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    let carve = page_carve(&mut domain, 0x9000_0000);
    domain.remove(carve).unwrap();
    unit.behave(Behaviour::UnconfirmedSync);
    assert_eq!(domain.confirm_removed(), Err(IommuError::Unconfirmed));
    assert_ne!(page_carve(&mut domain, 0x9000_1000), carve);
    unit.behave(Behaviour::Correct);
    domain.confirm_removed().unwrap();
    assert_eq!(page_carve(&mut domain, 0x9000_2000), carve);
}

/// A removed mapping confirmed alone leaves the batch, so its IOVA carrying a
/// new carve is not taken back when the batch is.
#[test]
fn a_removed_mapping_unmapped_alone_leaves_its_batch() {
    let frames = HostFrames::new(0x1000_0000);
    let unit = ModelUnit::new(&frames, Behaviour::Correct);
    unit.enable().unwrap();
    let mut domain = Domain::new(&unit, &[], &[]).unwrap();
    domain.attach(3).unwrap();
    let carve = page_carve(&mut domain, 0x9000_0000);
    domain.remove(carve).unwrap();
    domain.unmap(carve).unwrap();
    let again = page_carve(&mut domain, 0x9000_1000);
    assert_eq!(again, carve);
    domain.confirm_removed().unwrap();
    assert_eq!(domain.mapped(), 1, "the new carve stands");
    assert!(unit.access(3, again, true).is_some());
}
