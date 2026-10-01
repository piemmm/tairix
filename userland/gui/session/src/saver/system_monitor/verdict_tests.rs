//! The board's verdict: read from the monitor's own verdicts, worst first.

use tairix_abi::switchboard_ipc::{MachineDevice, MachineDeviceName, MachineStorage, MachineTasks};
use tairix_abi::sysinfo::{MemoryBand, MountAvailability};
use tairix_theme::SignalRole;

use super::super::fixtures::unread;
use super::{Concern, Verdict};

fn device(availability: MountAvailability) -> MachineDevice {
    MachineDevice {
        name: MachineDeviceName::new("disk").expect("a name"),
        availability,
        capacity: None,
        busy: None,
        read_rate: None,
        write_rate: None,
    }
}

#[test]
fn a_machine_with_nothing_to_report_reads_calm() {
    let verdict = Verdict::of(&unread());
    assert_eq!(verdict, Verdict::Calm);
    assert_eq!(verdict.text(), "All readings normal");
    assert_eq!(verdict.tone(), Some(SignalRole::Success));
}

#[test]
fn the_pressure_latches_are_the_monitors_own() {
    let mut report = unread();
    report.cpu.busy = tairix_abi::switchboard_ipc::Permille::new(1000).ok();
    assert_eq!(
        Verdict::of(&report),
        Verdict::Calm,
        "a busy share is not pressure until the monitor latches it"
    );
    report.cpu.pressured = true;
    assert_eq!(
        Verdict::of(&report),
        Verdict::Attention {
            worst: Concern::CpuPressured,
            others: 0
        }
    );
}

#[test]
fn the_worst_concern_leads_and_the_rest_are_counted() {
    let mut report = unread();
    report.cpu.pressured = true;
    report.memory.pressured = true;
    report.memory.band = MemoryBand::new(3).ok();
    report.tasks = MachineTasks::new(Some(10), 1, 2, &[]).expect("tasks");
    let verdict = Verdict::of(&report);
    assert_eq!(
        verdict,
        Verdict::Attention {
            worst: Concern::MemoryPressured(MemoryBand::new(3).ok()),
            others: 2
        }
    );
    assert_eq!(verdict.text(), "Memory under pressure (severe) · 2 more");
    assert_eq!(verdict.tone(), Some(SignalRole::Warning));
}

#[test]
fn a_failing_device_outranks_everything_and_is_drawn_as_the_storage_pill_draws_it() {
    let mut report = unread();
    report.cpu.pressured = true;
    report.storage = Some(
        MachineStorage::new(
            3,
            &[
                device(MountAvailability::UnavailableLost),
                device(MountAvailability::Degraded),
                device(MountAvailability::Available),
            ],
        )
        .expect("storage"),
    );
    let verdict = Verdict::of(&report);
    assert_eq!(
        verdict,
        Verdict::Attention {
            worst: Concern::StorageFailing(1),
            others: 2
        }
    );
    assert_eq!(verdict.text(), "A storage device is failing · 2 more");
    assert_eq!(
        verdict.tone(),
        Some(SignalRole::for_volume_health(
            tairix_abi::sysinfo::VolumeHealth::Failing
        ))
    );
}

#[test]
fn counts_are_spelled_in_words_a_reader_reads() {
    assert_eq!(
        Concern::StorageDegraded(3).text(),
        "3 storage devices are degraded"
    );
    assert_eq!(Concern::AwaitingRecovery(1).text(), "A task needs recovery");
    assert_eq!(Concern::AwaitingRecovery(4).text(), "4 tasks need recovery");
    assert_eq!(
        Concern::MemoryPressured(None).text(),
        "Memory under pressure"
    );
}

#[test]
fn a_board_without_live_readings_says_so() {
    assert_eq!(Verdict::Waiting.tone(), None);
    assert_eq!(Verdict::Waiting.text(), "Waiting for readings");
    assert_eq!(Verdict::Unmonitored.tone(), Some(SignalRole::Warning));
    assert_eq!(Verdict::Silent.tone(), Some(SignalRole::Warning));
    assert_eq!(
        Verdict::Silent.text(),
        "The system monitor is not answering"
    );
    let stale = Verdict::Stale {
        at: alloc::string::String::from("14:02"),
    };
    assert_eq!(stale.text(), "Readings stopped at 14:02");
    assert_eq!(stale.tone(), Some(SignalRole::Warning));
}
