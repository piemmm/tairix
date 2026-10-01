//! Unit tests for the machine-report frame.

extern crate alloc;

use super::{
    is_machine_report, DeviceCapacity, MachineCommitted, MachineComposition, MachineCores,
    MachineCpu, MachineDevice, MachineDeviceName, MachineHistory, MachineHost, MachineInterface,
    MachineInterfaceName, MachineMemory, MachineNetwork, MachineReport, MachineScope,
    MachineStorage, MachineTask, MachineTasks, ReportPeriod, CPU_OFFSET, HOST_LEN_OFFSET,
    HOST_OFFSET, MACHINE_CORES_MAX, MACHINE_DEVICES_MAX, MACHINE_HISTORY_MAX,
    MACHINE_INTERFACES_MAX, MACHINE_PERIOD_MAX_MS, MACHINE_PERIOD_MIN_MS, MACHINE_TASKS_MAX,
    MEMORY_OFFSET, NETWORK_OFFSET, PERIOD_OFFSET, SCOPE_OFFSET, STORAGE_OFFSET, TASKS_OFFSET,
    UPTIME_OFFSET,
};
use crate::memory::MEMORY_CLASS_COUNT;
use crate::switchboard_ipc::{Permille, SwitchboardRequest, TraySummary, TrayTaskName};
use crate::sysinfo::{LoadAverage, MemoryBand, MountAvailability, PRESSURE_BAND_COUNT};
use crate::time::Duration64;
use crate::Errno;

fn share(value: u16) -> Permille {
    Permille::new(value).expect("a fraction")
}

fn task(name: &str, cpu: u16, memory_bytes: u64) -> MachineTask {
    MachineTask {
        name: TrayTaskName::new(name).expect("a name"),
        cpu: share(cpu),
        memory_bytes,
    }
}

fn device(name: &str, availability: MountAvailability) -> MachineDevice {
    MachineDevice {
        name: MachineDeviceName::new(name).expect("a name"),
        availability,
        capacity: Some(DeviceCapacity::new(4_000, 3_000).expect("a capacity")),
        busy: Some(share(250)),
        read_rate: Some(12_345),
        write_rate: Some(678),
    }
}

fn interface(name: &str, link_up: Option<bool>) -> MachineInterface {
    MachineInterface {
        name: MachineInterfaceName::new(name).expect("a name"),
        link_up,
        receive_rate: Some(1_000_000),
        send_rate: Some(250_000),
    }
}

/// A report carrying every reading, each at a value no other field shares, so
/// a field read from the wrong offset cannot round-trip by coincidence.
fn full() -> MachineReport {
    let cores = (0..48u16).map(|core| (core % 7 != 3).then(|| share(core * 20)));
    MachineReport {
        period: ReportPeriod::from_millis(2_000).expect("a period"),
        scope: MachineScope::Machine,
        host: Some(MachineHost::new("rack-07.example").expect("a name")),
        uptime: Some(Duration64::new(1_234_567, 890).expect("a span")),
        cpu: MachineCpu {
            busy: Some(share(371)),
            pressured: true,
            load: Some(LoadAverage {
                load1: 0x1_8000,
                load5: 0x2_4000,
                load15: 0x0_c000,
                runnable: 9,
                total_tasks: 412,
                users: 3,
            }),
            cores: MachineCores::new(cores).expect("cores"),
            history: MachineHistory::new(&[0, 100, 999, 1000, 42]).expect("history"),
        },
        memory: MachineMemory {
            committed: Some(MachineCommitted::new(16 << 30, share(612)).expect("committed")),
            band: Some(MemoryBand::new(2).expect("a band")),
            pressured: true,
            composition: Some(
                MachineComposition::new(
                    [1 << 30, 2 << 30, 1 << 20, 3 << 28, 1 << 24, 5],
                    4 << 30,
                    16 << 30,
                )
                .expect("a composition"),
            ),
            history: MachineHistory::new(&[600, 605, 612]).expect("history"),
        },
        tasks: MachineTasks::new(
            Some(214),
            3,
            4,
            &[
                task("postgres", 340, 2 << 30),
                task("nginx", 120, 300 << 20),
                task("backup", 90, 64 << 20),
            ],
        )
        .expect("tasks"),
        storage: Some(
            MachineStorage::new(
                11,
                &[
                    device("nvme0 · root", MountAvailability::Degraded),
                    device("sda · backup", MountAvailability::Available),
                ],
            )
            .expect("storage"),
        ),
        network: Some(
            MachineNetwork::new(
                3,
                &[
                    interface("eth0", Some(true)),
                    interface("eth1", Some(false)),
                ],
            )
            .expect("network"),
        ),
    }
}

/// A report with nothing read: every optional reading absent.
fn unread() -> MachineReport {
    MachineReport {
        period: ReportPeriod::from_millis(MACHINE_PERIOD_MIN_MS).expect("a period"),
        scope: MachineScope::Own,
        host: None,
        uptime: None,
        cpu: MachineCpu::UNREAD,
        memory: MachineMemory::UNREAD,
        tasks: MachineTasks::UNREAD,
        storage: None,
        network: None,
    }
}

#[test]
fn a_full_report_round_trips_exactly() {
    let report = full();
    let bytes = report.to_le_bytes();
    assert_eq!(MachineReport::from_bytes(&bytes), Ok(report));
}

#[test]
fn an_unread_report_round_trips_with_every_reading_absent() {
    let report = unread();
    let decoded = MachineReport::from_bytes(&report.to_le_bytes()).expect("decodes");
    assert_eq!(decoded, report);
    assert!(decoded.cpu.busy.is_none());
    assert!(decoded.memory.committed.is_none());
    assert!(decoded.tasks.count().is_none());
    assert!(decoded.storage.is_none());
    assert!(decoded.network.is_none());
}

#[test]
fn a_machine_with_no_devices_is_told_apart_from_one_not_read() {
    let mut report = unread();
    report.storage = Some(MachineStorage::new(0, &[]).expect("storage"));
    report.network = Some(MachineNetwork::new(0, &[]).expect("network"));
    let decoded = MachineReport::from_bytes(&report.to_le_bytes()).expect("decodes");
    assert_eq!(decoded.storage.map(|storage| storage.total()), Some(0));
    assert_eq!(decoded.network.map(|network| network.total()), Some(0));
}

#[test]
fn the_layout_is_the_one_documented() {
    assert_eq!(HOST_OFFSET, 16);
    assert_eq!(UPTIME_OFFSET, 80);
    assert_eq!(CPU_OFFSET, 92);
    assert_eq!(MEMORY_OFFSET, CPU_OFFSET + 1_184);
    assert_eq!(TASKS_OFFSET, MEMORY_OFFSET + 216);
    assert_eq!(STORAGE_OFFSET, TASKS_OFFSET + 256);
    assert_eq!(NETWORK_OFFSET, STORAGE_OFFSET + 840);
    assert_eq!(MachineReport::WIRE_LEN, NETWORK_OFFSET + 328);
}

#[test]
fn a_report_is_told_apart_from_a_request_by_its_magic() {
    assert!(is_machine_report(&full().to_le_bytes()));
    let request = SwitchboardRequest::PublishSummary {
        summary: TraySummary {
            jobs: 0,
            recovery: 0,
            cpu_busy_permille: share(10),
            pressure: None,
            top_task: None,
            power_capable: false,
        },
    };
    assert!(!is_machine_report(&request.to_le_bytes()));
    assert!(!is_machine_report(&[]));
    assert!(!is_machine_report(b"SWM"));
}

/// Every single-byte change either refuses or decodes to a report whose own
/// encoding is the changed frame: no byte is read loosely or ignored, so a
/// frame the session draws is exactly the one its sender wrote.
#[test]
fn every_accepted_frame_is_the_encoding_of_what_it_decodes_to() {
    for base in [full().to_le_bytes(), unread().to_le_bytes()] {
        for offset in 0..base.len() {
            for mask in [0x01u8, 0x80, 0xff] {
                let mut bytes = base;
                bytes[offset] ^= mask;
                if let Ok(report) = MachineReport::from_bytes(&bytes) {
                    assert_eq!(
                        report.to_le_bytes(),
                        bytes,
                        "byte {offset} ^ {mask:#04x} decoded loosely"
                    );
                }
            }
        }
    }
}

#[test]
fn a_frame_shorter_than_a_report_is_refused() {
    let bytes = full().to_le_bytes();
    assert_eq!(
        MachineReport::from_bytes(&bytes[..MachineReport::WIRE_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn the_wrong_magic_or_version_is_refused() {
    let mut bytes = full().to_le_bytes();
    bytes[0] ^= 1;
    assert_eq!(MachineReport::from_bytes(&bytes), Err(Errno::BadMagic));
    let mut bytes = full().to_le_bytes();
    bytes[4] = 2;
    assert_eq!(
        MachineReport::from_bytes(&bytes),
        Err(Errno::AbiVersionUnsupported)
    );
}

#[test]
fn an_undefined_flag_is_refused() {
    let mut bytes = unread().to_le_bytes();
    bytes[7] = 0x80;
    assert_eq!(MachineReport::from_bytes(&bytes), Err(Errno::BadMagic));
}

#[test]
fn a_period_outside_its_bounds_is_refused() {
    for millis in [0, MACHINE_PERIOD_MIN_MS - 1, MACHINE_PERIOD_MAX_MS + 1] {
        let mut bytes = unread().to_le_bytes();
        bytes[PERIOD_OFFSET..PERIOD_OFFSET + 4].copy_from_slice(&millis.to_le_bytes());
        assert_eq!(
            MachineReport::from_bytes(&bytes),
            Err(Errno::OutOfRange),
            "{millis}"
        );
    }
    assert!(ReportPeriod::from_millis(MACHINE_PERIOD_MAX_MS).is_ok());
    assert_eq!(
        ReportPeriod::from_millis(2_000).map(ReportPeriod::as_nanos),
        Ok(2_000_000_000)
    );
}

#[test]
fn a_scope_outside_the_closed_set_is_refused() {
    for scope in [0u8, 3, 0xff] {
        let mut bytes = unread().to_le_bytes();
        bytes[SCOPE_OFFSET] = scope;
        assert_eq!(MachineReport::from_bytes(&bytes), Err(Errno::OutOfRange));
    }
}

#[test]
fn a_malformed_host_name_is_refused() {
    let mut bytes = unread().to_le_bytes();
    bytes[HOST_OFFSET] = b'x';
    assert_eq!(
        MachineReport::from_bytes(&bytes),
        Err(Errno::BadMagic),
        "name bytes with no length"
    );
    let mut bytes = unread().to_le_bytes();
    bytes[HOST_LEN_OFFSET] = 65;
    assert_eq!(
        MachineReport::from_bytes(&bytes),
        Err(Errno::LengthOutOfRange)
    );
    let mut bytes = unread().to_le_bytes();
    bytes[HOST_LEN_OFFSET] = 1;
    bytes[HOST_OFFSET] = 0x07;
    assert_eq!(MachineReport::from_bytes(&bytes), Err(Errno::OutOfRange));
}

#[test]
fn an_uptime_with_unsettled_nanoseconds_is_refused() {
    let mut report = unread();
    report.uptime = Some(Duration64::from_secs(5));
    let mut bytes = report.to_le_bytes();
    bytes[UPTIME_OFFSET + 8..UPTIME_OFFSET + 12].copy_from_slice(&1_000_000_000u32.to_le_bytes());
    assert_eq!(
        MachineReport::from_bytes(&bytes),
        Err(Errno::TimestampOutOfRange)
    );
}

#[test]
fn an_absent_reading_must_be_zero_on_the_wire() {
    for offset in [
        UPTIME_OFFSET,
        CPU_OFFSET,
        MEMORY_OFFSET,
        STORAGE_OFFSET,
        NETWORK_OFFSET,
    ] {
        let mut bytes = unread().to_le_bytes();
        bytes[offset] = 1;
        assert!(
            MachineReport::from_bytes(&bytes).is_err(),
            "a stray byte at {offset}"
        );
    }
}

#[test]
fn a_fraction_above_full_is_refused_wherever_it_stands() {
    assert_eq!(MachineHistory::new(&[1001]), Err(Errno::OutOfRange));
    let mut report = unread();
    report.cpu.busy = Some(share(1000));
    let mut bytes = report.to_le_bytes();
    bytes[CPU_OFFSET..CPU_OFFSET + 2].copy_from_slice(&1001u16.to_le_bytes());
    assert_eq!(MachineReport::from_bytes(&bytes), Err(Errno::OutOfRange));
}

#[test]
fn a_history_is_bounded() {
    let longest = [500u16; MACHINE_HISTORY_MAX];
    assert_eq!(
        MachineHistory::new(&longest).map(|history| history.points().len()),
        Ok(MACHINE_HISTORY_MAX)
    );
    let mut longer = longest.to_vec();
    longer.push(500);
    assert_eq!(MachineHistory::new(&longer), Err(Errno::LengthOutOfRange));
    assert!(MachineHistory::EMPTY.points().is_empty());
}

#[test]
fn every_core_up_to_the_bound_is_named_and_unmeasured_ones_stay_unmeasured() {
    let readings = (0..MACHINE_CORES_MAX).map(|core| (core % 2 == 0).then(|| share(999)));
    let cores = MachineCores::new(readings).expect("every core");
    assert_eq!(cores.len(), MACHINE_CORES_MAX);
    let mut report = unread();
    report.cpu.cores = cores;
    let decoded = MachineReport::from_bytes(&report.to_le_bytes()).expect("decodes");
    let read: alloc::vec::Vec<_> = decoded.cpu.cores.readings().collect();
    assert_eq!(read.first(), Some(&Some(share(999))));
    assert_eq!(read.get(1), Some(&None));
    assert_eq!(
        MachineCores::new(core::iter::repeat_n(None, MACHINE_CORES_MAX + 1)),
        Err(Errno::LengthOutOfRange)
    );
    assert!(MachineCores::EMPTY.is_empty());
}

#[test]
fn a_core_count_past_the_bound_or_a_core_above_full_is_refused() {
    let mut report = unread();
    report.cpu.cores = MachineCores::new([Some(share(10))]).expect("one core");
    let bytes = report.to_le_bytes();
    let mut longer = bytes;
    let count = u16::try_from(MACHINE_CORES_MAX + 1).expect("fits");
    longer[CPU_OFFSET + 2..CPU_OFFSET + 4].copy_from_slice(&count.to_le_bytes());
    assert_eq!(
        MachineReport::from_bytes(&longer),
        Err(Errno::LengthOutOfRange)
    );
    let cores_at = CPU_OFFSET + 4 + LoadAverage::WIRE_LEN + 4 + 2 * MACHINE_HISTORY_MAX;
    let mut hot = bytes;
    hot[cores_at..cores_at + 2].copy_from_slice(&1001u16.to_le_bytes());
    assert_eq!(MachineReport::from_bytes(&hot), Err(Errno::OutOfRange));
}

#[test]
fn a_composition_that_outgrows_its_whole_is_refused() {
    assert_eq!(
        MachineComposition::new([10; MEMORY_CLASS_COUNT], 41, 100),
        Err(Errno::OutOfRange)
    );
    assert!(MachineComposition::new([10; MEMORY_CLASS_COUNT], 40, 100).is_ok());
    assert_eq!(
        MachineComposition::new([u64::MAX; MEMORY_CLASS_COUNT], 1, u64::MAX),
        Err(Errno::OutOfRange),
        "parts that overflow their sum"
    );
    assert_eq!(
        MachineComposition::new([0; MEMORY_CLASS_COUNT], 0, 0),
        Err(Errno::OutOfRange)
    );
    assert_eq!(MachineCommitted::new(0, share(1)), Err(Errno::OutOfRange));
}

#[test]
fn a_band_deeper_than_the_deepest_is_refused() {
    let mut report = unread();
    report.memory.band = Some(MemoryBand::new(0).expect("a band"));
    let mut bytes = report.to_le_bytes();
    bytes[MEMORY_OFFSET + 10] = u8::try_from(PRESSURE_BAND_COUNT).expect("fits");
    assert_eq!(MachineReport::from_bytes(&bytes), Err(Errno::OutOfRange));
}

#[test]
fn a_census_cannot_name_more_than_it_counts() {
    let three = [task("a", 1, 1), task("b", 1, 1), task("c", 1, 1)];
    assert_eq!(
        MachineTasks::new(Some(2), 0, 0, &three),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        MachineTasks::new(Some(2), 3, 0, &[]),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        MachineTasks::new(None, 1, 0, &[]),
        Err(Errno::OutOfRange),
        "stopped tasks of an unread list"
    );
    assert_eq!(
        MachineTasks::new(None, 0, 0, &three[..1]),
        Err(Errno::OutOfRange),
        "a busiest task of an unread list"
    );
    let six = [task("t", 1, 1); MACHINE_TASKS_MAX + 1];
    assert_eq!(
        MachineTasks::new(Some(100), 0, 0, &six),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        MachineTasks::new(None, 0, 1, &[]),
        Err(Errno::OutOfRange),
        "a recovery of an unread list"
    );
    assert_eq!(
        MachineTasks::new(Some(2), 0, 3, &[]),
        Err(Errno::OutOfRange)
    );
    let named = MachineTasks::new(Some(3), 1, 2, &three).expect("tasks");
    assert_eq!(named.recovery(), 2);
    assert_eq!(
        named
            .busiest()
            .map(|task| task.name.as_str())
            .collect::<alloc::vec::Vec<_>>(),
        ["a", "b", "c"]
    );
}

#[test]
fn a_corrupt_named_task_is_refused() {
    let report = full();
    let bytes = report.to_le_bytes();
    let first = TASKS_OFFSET + 16;
    let mut nameless = bytes;
    nameless[first] = 0;
    assert_eq!(
        MachineReport::from_bytes(&nameless),
        Err(Errno::LengthOutOfRange)
    );
    let mut hot = bytes;
    hot[first + 34..first + 36].copy_from_slice(&1001u16.to_le_bytes());
    assert_eq!(MachineReport::from_bytes(&hot), Err(Errno::OutOfRange));
    let mut overcounted = bytes;
    overcounted[TASKS_OFFSET + 8] = u8::try_from(MACHINE_TASKS_MAX + 1).expect("fits");
    assert_eq!(
        MachineReport::from_bytes(&overcounted),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_device_list_cannot_name_more_than_it_counts() {
    let one = device("disk", MountAvailability::Available);
    assert_eq!(MachineStorage::new(0, &[one]), Err(Errno::OutOfRange));
    let nine = [one; MACHINE_DEVICES_MAX + 1];
    assert_eq!(
        MachineStorage::new(100, &nine),
        Err(Errno::LengthOutOfRange)
    );
    let storage = MachineStorage::new(20, &[one, one]).expect("storage");
    assert_eq!(storage.devices().count(), 2);
    assert_eq!(storage.total(), 20);
}

#[test]
fn a_corrupt_device_is_refused() {
    let bytes = full().to_le_bytes();
    let first = STORAGE_OFFSET + 8;
    let mut availability = bytes;
    availability[first + 65] = 6;
    assert_eq!(
        MachineReport::from_bytes(&availability),
        Err(Errno::OutOfRange)
    );
    let mut flag = bytes;
    flag[first + 66] |= 0x10;
    assert_eq!(MachineReport::from_bytes(&flag), Err(Errno::BadMagic));
    let mut overfull = bytes;
    overfull[first + 80..first + 88].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(MachineReport::from_bytes(&overfull), Err(Errno::OutOfRange));
    let mut undercounted = bytes;
    undercounted[STORAGE_OFFSET..STORAGE_OFFSET + 2].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(
        MachineReport::from_bytes(&undercounted),
        Err(Errno::OutOfRange)
    );
    assert_eq!(DeviceCapacity::new(0, 0), Err(Errno::OutOfRange));
    assert_eq!(DeviceCapacity::new(10, 11), Err(Errno::OutOfRange));
}

#[test]
fn an_interface_list_cannot_name_more_than_it_counts() {
    let one = interface("eth0", None);
    assert_eq!(MachineNetwork::new(0, &[one]), Err(Errno::OutOfRange));
    let nine = [one; MACHINE_INTERFACES_MAX + 1];
    assert_eq!(
        MachineNetwork::new(100, &nine),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_corrupt_interface_is_refused() {
    let bytes = full().to_le_bytes();
    let first = NETWORK_OFFSET + 8;
    let mut link = bytes;
    link[first + 17] = 3;
    assert_eq!(MachineReport::from_bytes(&link), Err(Errno::OutOfRange));
    let mut flag = bytes;
    flag[first + 18] |= 0x04;
    assert_eq!(MachineReport::from_bytes(&flag), Err(Errno::BadMagic));
    let mut reserved = bytes;
    reserved[first + 20] = 1;
    assert_eq!(MachineReport::from_bytes(&reserved), Err(Errno::BadMagic));
}

#[test]
fn an_unknown_link_is_carried_as_unknown() {
    let mut report = unread();
    report.network = Some(MachineNetwork::new(1, &[interface("eth0", None)]).expect("network"));
    let decoded = MachineReport::from_bytes(&report.to_le_bytes()).expect("decodes");
    let link = decoded.network.and_then(|network| {
        network
            .interfaces()
            .next()
            .map(|interface| interface.link_up)
    });
    assert_eq!(link, Some(None));
}
