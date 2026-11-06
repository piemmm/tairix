//! Unit tests for what an image volume is derived from: its identity, the
//! names it is shown under, its reasons for refusal, its attach's wait and
//! which owed teardown is taken up next. Its lifecycle over the real service
//! is the volume service's scenarios'.

use tairix_abi::blkio::{BlkForm, BlkTransport};
use tairix_abi::imageattach::ImageRefusal;
use tairix_abi::{CapabilityId, CapabilityQuery, DriverError, Errno, FileId};

use tairix_abi::fsserve::SERVED_IDLE_NS;

use alloc::sync::Arc;

use tairix_kernel_core::fs::VolumeNodes;

use super::{
    first_settleable, identity, image_medium, last_component, lock_whole, reaches, refusal_of,
    stem, Custody, DriverWait, Holder, ImageVolumes, Owed, Refusal, RuntimeVolumeService,
    ShortOfMount, Step, Waited, IMAGE_DRIVER_START_NS, SETTLERS,
};

fn file(volume: u8, node: u64) -> FileId {
    FileId {
        volume: [volume; 16],
        node,
    }
}

/// An image's target is bound as its attacher short of the mount authority:
/// every other capability the attacher holds still holds.
#[test]
fn an_images_target_is_bound_without_the_mount_authority() {
    struct Every;
    impl CapabilityQuery for Every {
        fn holds(&self, _cap: CapabilityId) -> bool {
            true
        }
    }
    let bound = ShortOfMount(&Every);
    assert!(!bound.holds(CapabilityId::FS_MOUNT));
    assert!(bound.holds(CapabilityId::FS_ACCESS));
}

#[test]
fn an_image_volumes_identity_is_its_attachers_its_files_and_its_volumes() {
    let base = identity(1000, &file(1, 7), &[9; 16]);
    assert_eq!(identity(1000, &file(1, 7), &[9; 16]), base, "stable");
    for other in [
        identity(1001, &file(1, 7), &[9; 16]),
        identity(1000, &file(2, 7), &[9; 16]),
        identity(1000, &file(1, 8), &[9; 16]),
        identity(1000, &file(1, 7), &[8; 16]),
    ] {
        assert_ne!(other, base);
    }
    assert_ne!(base, [9; 16], "never the probed volume's own identity");
    assert_ne!(base, [0; 16]);
}

#[test]
fn a_volume_is_named_from_its_files_last_component_without_its_extension() {
    assert_eq!(
        last_component("/Users/ann/Home/Games/Lemmings.adf"),
        "Lemmings.adf"
    );
    assert_eq!(last_component("disk.iso"), "disk.iso");
    assert_eq!(stem("Lemmings.adf"), b"Lemmings");
    assert_eq!(stem("backup.tar.hdf"), b"backup.tar");
    assert_eq!(
        stem(".profile"),
        b".profile",
        "a leading dot is no extension"
    );
    assert_eq!(stem("Workbench"), b"Workbench");
}

#[test]
fn a_drivers_refusal_reads_as_a_reason() {
    assert_eq!(
        refusal_of(DriverError::Unsupported),
        ImageRefusal::UnsupportedVariant
    );
    assert_eq!(refusal_of(DriverError::BadMagic), ImageRefusal::Damaged);
    assert_eq!(
        refusal_of(DriverError::DeviceFault),
        ImageRefusal::Unreadable
    );
    assert_eq!(refusal_of(DriverError::Stale), ImageRefusal::Unreadable);
}

#[test]
fn an_image_is_a_virtual_medium_of_the_form_it_was_laid_out_for() {
    for form in BlkForm::ALL {
        let medium = image_medium(form);
        assert_eq!(medium.transport, BlkTransport::Virtual);
        assert_eq!(medium.form, form);
    }
}

#[test]
fn a_driver_that_never_starts_is_given_up_once_its_allowance_runs_out() {
    let mut wait = DriverWait::begin(100, Some(4));
    let due = 100 + IMAGE_DRIVER_START_NS;
    assert_eq!(wait.step(None, false, false, due - 1, Some(4)), Step::Wait);
    assert_eq!(
        wait.step(None, false, false, due, Some(4)),
        Step::Over(Waited::TimedOut)
    );
}

#[test]
fn a_driver_showing_life_is_judged_again_an_idle_interval_on() {
    let mut wait = DriverWait::begin(0, Some(4));
    let first = IMAGE_DRIVER_START_NS;
    assert_eq!(
        wait.step(None, false, false, first, None),
        Step::Rejudge(first + SERVED_IDLE_NS),
        "inside a transfer is working, however slow"
    );
    assert_eq!(
        wait.step(None, false, false, first + SERVED_IDLE_NS, Some(5)),
        Step::Rejudge(first + 2 * SERVED_IDLE_NS),
        "finished a transfer since"
    );
    assert_eq!(
        wait.step(None, false, false, first + 2 * SERVED_IDLE_NS, Some(5)),
        Step::Over(Waited::TimedOut),
        "then idle for a whole interval"
    );
}

#[test]
fn a_volume_an_operation_holds_is_never_given_up_on() {
    let mut wait = DriverWait::begin(0, Some(4));
    let due = IMAGE_DRIVER_START_NS;
    assert_eq!(
        wait.step(None, true, false, due, Some(4)),
        Step::Rejudge(due + SERVED_IDLE_NS)
    );
    assert_eq!(
        wait.step(None, false, false, due + SERVED_IDLE_NS, Some(4)),
        Step::Over(Waited::TimedOut),
        "judged again once it is let go"
    );
}

#[test]
fn a_death_owed_ends_the_wait_and_a_settled_stage_outranks_it() {
    let mut wait = DriverWait::begin(0, Some(4));
    assert_eq!(
        wait.step(None, true, true, 0, Some(4)),
        Step::Over(Waited::Interrupted),
        "killable before any deadline, held or not"
    );
    let refused = Waited::Refused(Refusal::Driver(DriverError::DeviceFault));
    for settled in [Waited::Mounted, refused] {
        assert_eq!(
            wait.step(Some(settled), false, true, 0, Some(4)),
            Step::Over(settled)
        );
    }
}

#[test]
fn a_volume_taken_out_before_its_attach_answered_is_gone_not_refused() {
    let service = RuntimeVolumeService::new();
    assert_eq!(
        service.stage_of(7),
        (Some(Waited::Gone), false),
        "never read as a driver's refusal"
    );
}

/// The custody of a volume owing `owed`.
fn owing(owed: Owed) -> Custody {
    Custody {
        owed,
        ..Custody::default()
    }
}

#[test]
fn only_an_owed_volume_nothing_holds_is_settled() {
    assert!(!Custody::default().settleable(0), "it owes nothing");
    for owed in [Owed::Abandoned, Owed::Orphaned] {
        assert!(owing(owed).settleable(0));
        for held in [
            Custody {
                holder: Holder::Operation,
                ..owing(owed)
            },
            Custody {
                holder: Holder::Settler,
                ..owing(owed)
            },
            Custody {
                answering: true,
                ..owing(owed)
            },
            Custody {
                stuck_at: Some(0),
                ..owing(owed)
            },
        ] {
            assert!(!held.settleable(0), "{held:?}");
        }
    }
}

#[test]
fn a_failed_teardown_waits_for_a_release_unless_one_came_while_it_tried() {
    let settling = |owed| Custody {
        holder: Holder::Settler,
        ..owing(owed)
    };
    let mut stuck = settling(Owed::Orphaned);
    assert!(
        !stuck.let_go(Holder::Settler, 4, 4),
        "it failed: no release"
    );
    assert!(!stuck.claimed());
    assert!(!stuck.settleable(4), "nothing has let go since");
    assert!(stuck.settleable(5), "something has let go since");

    let mut overtaken = settling(Owed::Orphaned);
    assert!(!overtaken.let_go(Holder::Settler, 4, 5));
    assert!(
        overtaken.settleable(5),
        "what it waited for let go while it tried, so it tries again"
    );

    let mut done = settling(Owed::Nothing);
    assert!(
        done.let_go(Holder::Settler, 4, 4),
        "it owes nothing now, so its letting go is a release"
    );
    let mut served = Custody {
        holder: Holder::Operation,
        ..owing(Owed::Orphaned)
    };
    assert!(
        served.let_go(Holder::Operation, 4, 4),
        "an operation never fails as a settler"
    );
    assert!(served.settleable(4));

    let mut beneath = Custody {
        holder: Holder::Clearing,
        stuck_at: Some(4),
        ..owing(Owed::Orphaned)
    };
    assert!(
        !beneath.let_go(Holder::Clearing, 4, 4),
        "a settling task's own hold beneath its volume is no release"
    );
    assert!(!beneath.settleable(4), "and leaves it as stuck as it was");
}

#[test]
fn a_settling_task_holds_what_lies_beneath_its_volume_as_its_own() {
    assert_eq!(Holder::Settler.beneath(), Holder::Clearing);
    assert_eq!(
        Holder::Clearing.beneath(),
        Holder::Clearing,
        "at every depth"
    );
    assert_eq!(Holder::Operation.beneath(), Holder::Operation);
}

#[test]
fn an_attach_whose_volume_left_is_answered_not_attached() {
    let images = ImageVolumes::new();
    assert_eq!(images.reply_for(&[7; 16]), Err(Errno::NotAttached));
}

#[test]
fn an_owners_teardowns_are_settled_one_at_a_time_and_others_go_on() {
    let settling = Custody {
        holder: Holder::Settler,
        ..owing(Owed::Orphaned)
    };
    let owed = owing(Owed::Orphaned);
    let volumes = [(1000, settling), (1000, owed), (1001, owed)];
    assert_eq!(
        first_settleable(volumes.iter().map(|(owner, custody)| (*owner, custody)), 0),
        Some(2),
        "the first owner's second waits for its first; the next owner's is taken up"
    );
    let alone = [(1000, settling), (1000, owed)];
    assert_eq!(
        first_settleable(alone.iter().map(|(owner, custody)| (*owner, custody)), 0),
        None
    );
    let every: [(u32, Custody); SETTLERS + 1] = core::array::from_fn(|at| {
        let owner = u32::try_from(at).expect("a few owners");
        (owner, if at < SETTLERS { settling } else { owed })
    });
    assert_eq!(
        first_settleable(every.iter().map(|(owner, custody)| (*owner, custody)), 0),
        Some(SETTLERS),
        "as many owners as settling tasks are served"
    );
}

#[test]
fn a_file_is_reached_only_through_its_own_registration_with_no_stand_in() {
    let nodes = Arc::new(VolumeNodes::new(1));
    assert!(reaches(Some((Arc::clone(&nodes), false)), &nodes));
    assert!(
        !reaches(Some((Arc::clone(&nodes), true)), &nodes),
        "a stand-in answers for it, a conflict kept or not"
    );
    assert!(
        !reaches(Some((Arc::new(VolumeNodes::new(2)), false)), &nodes),
        "registered afresh"
    );
    assert!(!reaches(None, &nodes), "unregistered");
}

/// An image's whole-file lock the kernel has no memory to record is refused
/// as want of memory, not as a holder in the way, and records nothing.
#[test]
fn a_lock_with_no_room_to_record_is_out_of_memory() {
    use tairix_kernel_core::filelock::{self, Held, LockedNode};
    let file = LockedNode {
        instance: 0x1A6E,
        pinned: 7,
    };
    let owner = filelock::mint_owner();
    let refused =
        tairix_fuzzseed::meter::refusing_after(0, || lock_whole(file, owner, Held::Exclusive, 1));
    assert_eq!(refused, Err(Errno::OutOfMemory));
    assert_eq!(lock_whole(file, owner, Held::Exclusive, 1), Ok(()));
    for wakes in filelock::release_owner(owner) {
        wakes.wake();
    }
}
