//! Unit tests for the machine report: every reading is the one the panel
//! draws, and nothing the sample lacks is claimed.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::net_ipc::{
    NetIfKind, NetInterfaceRatesRecord, NetInterfaceStateRecord, NET_IF_MAX_ADDRS,
};
use tairix_abi::switchboard_ipc::{
    MachineDeviceName, MachineReport, MachineScope, Permille, SeatReport, MACHINE_DEVICES_MAX,
    MACHINE_DEVICE_NAME_MAX, MACHINE_TASKS_MAX,
};
use tairix_abi::sysinfo::{
    KernelMemoryStats, LoadAverage, MemoryBand, MemoryPressureBand, MountAvailability,
    ProcessState, SystemIdentity, Uptime, MACHINE_ID_LEN, MOUNT_VOLUME_ID_LEN,
};
use tairix_abi::{Duration64, MemoryClass, SchedPriority, Time64, MEMORY_CLASS_COUNT};

use super::{fitted, machine_report};
use crate::derive::{derive_summary, Hysteresis};
use crate::model::{RollingMeters, SessionReport};
use crate::sample::{CoreBusy, DegradedField, MemoryPressureSample, Sample, ScopeVerdicts};
use crate::schedule::SAMPLE_PERIOD_NS;
use crate::test_host::{
    if_name, iface, mount_of, process_summary, process_summary_with, with_availability,
};

/// Every scope granted, so a reading's absence is the sample's, not a refusal.
const GRANTED: ScopeVerdicts = ScopeVerdicts {
    global_process_scope: true,
    memory_pressure: true,
    hardware_scope: true,
};

/// The report `samples` make, each folded into the meters in turn as the
/// service folds them, the last one projected.
fn report_after(samples: &[Sample], seat: &SeatReport) -> MachineReport {
    let mut meters = RollingMeters::new();
    let mut hysteresis = Hysteresis::new();
    let session = SessionReport {
        seat: *seat,
        frame: None,
    };
    for sample in samples {
        let _ = derive_summary(sample, &mut hysteresis);
        meters.record(sample, hysteresis, &session);
    }
    let last = samples.last().expect("at least one sample");
    machine_report(last, &meters, seat)
}

fn report_of(sample: Sample) -> MachineReport {
    report_after(&[sample], &SeatReport::HEALTHY)
}

fn granted() -> Sample {
    Sample {
        scopes: GRANTED,
        ..Sample::default()
    }
}

/// A sample in which every reading failed.
fn unread() -> Sample {
    Sample {
        degradations: vec![
            DegradedField::ProcessList,
            DegradedField::CpuTime,
            DegradedField::Mounts,
        ],
        ..granted()
    }
}

/// A volume identity distinct for each `index`.
fn volume(index: u8) -> [u8; MOUNT_VOLUME_ID_LEN] {
    [index.saturating_add(1); MOUNT_VOLUME_ID_LEN]
}

#[test]
fn a_report_claims_nothing_the_sample_did_not_read() {
    let report = report_of(unread());
    assert_eq!(
        report.tasks.count(),
        None,
        "an unread list is not an empty one"
    );
    assert_eq!(report.tasks.busiest().count(), 0);
    assert!(report.cpu.busy.is_none());
    assert!(report.cpu.cores.is_empty());
    assert!(report.cpu.history.points().is_empty());
    assert!(report.memory.committed.is_none());
    assert!(report.memory.composition.is_none());
    assert!(report.memory.band.is_none());
    assert!(report.host.is_none());
    assert!(report.uptime.is_none());
    assert!(report.storage.is_none());
    assert!(report.network.is_none());
}

#[test]
fn the_task_scope_follows_the_grant() {
    assert_eq!(report_of(granted()).scope, MachineScope::Machine);
    let own = Sample {
        scopes: ScopeVerdicts {
            global_process_scope: false,
            ..GRANTED
        },
        ..Sample::default()
    };
    assert_eq!(report_of(own).scope, MachineScope::Own);
}

#[test]
fn the_report_states_this_services_own_period() {
    assert_eq!(
        report_of(granted()).period.as_nanos(),
        SAMPLE_PERIOD_NS,
        "a reader times staleness against the period samples really come at"
    );
}

#[test]
fn the_machine_is_named_and_its_uptime_read() {
    let sample = Sample {
        identity: Some(
            SystemIdentity::new([3; MACHINE_ID_LEN], 1, 2, 3, b"rack-07").expect("an identity"),
        ),
        uptime: Some(Uptime {
            since_boot: Duration64::from_secs(93_784),
            boot_time: Time64::from_secs(0),
        }),
        ..granted()
    };
    let report = report_of(sample);
    assert_eq!(
        report.host.map(|host| String::from(host.as_str())),
        Some(String::from("rack-07"))
    );
    assert_eq!(report.uptime, Some(Duration64::from_secs(93_784)));
}

#[test]
fn the_processors_read_as_the_panel_reads_them() {
    let hot = Sample {
        cpu_busy_permille: Some(950),
        core_busy: vec![
            CoreBusy {
                cpu: 0,
                permille: Some(900),
            },
            CoreBusy {
                cpu: 1,
                permille: None,
            },
            CoreBusy {
                cpu: 2,
                permille: Some(1000),
            },
        ],
        load_average: Some(LoadAverage {
            load1: 3 << 11,
            load5: 2 << 11,
            load15: 1 << 11,
            runnable: 4,
            total_tasks: 90,
            users: 2,
        }),
        ..granted()
    };
    let report = report_after(&[hot.clone(), hot], &SeatReport::HEALTHY);
    assert_eq!(report.cpu.busy.map(Permille::as_u16), Some(950));
    assert!(
        report.cpu.pressured,
        "the latch the tray's rail is drawn from"
    );
    assert_eq!(report.cpu.load.map(|load| load.runnable), Some(4));
    let cores: Vec<Option<u16>> = report
        .cpu
        .cores
        .readings()
        .map(|core| core.map(Permille::as_u16))
        .collect();
    assert_eq!(cores, [Some(900), None, Some(1000)]);
    assert_eq!(report.cpu.history.points(), [950, 950]);
}

#[test]
fn memory_reads_its_share_its_band_and_where_it_went() {
    let mut class_bytes = [0u64; MEMORY_CLASS_COUNT];
    class_bytes[MemoryClass::UserAnon.index()] = 6 << 30;
    class_bytes[MemoryClass::UserFile.index()] = 2 << 30;
    let sample = Sample {
        memory_pressure: Some(MemoryPressureSample {
            band: 2,
            used_permille: 640,
            total_bytes: 16 << 30,
        }),
        pressure_band: Some(MemoryPressureBand {
            band: 2,
            reserved: [0; 7],
        }),
        kernel_memory: Some(KernelMemoryStats {
            total_bytes: 16 << 30,
            free_bytes: 8 << 30,
            kernel_heap_bytes: 0,
            user_resident_bytes: 0,
            page_size: 4_096,
            reserved: 0,
            class_bytes,
        }),
        ..granted()
    };
    let report = report_of(sample);
    let committed = report.memory.committed.expect("committed");
    assert_eq!(committed.total_bytes(), 16 << 30);
    assert_eq!(committed.used().as_u16(), 640);
    assert_eq!(report.memory.band.map(MemoryBand::name), Some("moderate"));
    assert!(report.memory.pressured);
    let composition = report.memory.composition.expect("a composition");
    assert_eq!(composition.class_bytes(), &class_bytes);
    assert_eq!(composition.free_bytes(), 8 << 30);
    assert_eq!(report.memory.history.points(), [640]);
}

#[test]
fn a_composition_the_kernel_could_not_have_made_is_left_unsaid() {
    let sample = Sample {
        kernel_memory: Some(KernelMemoryStats {
            total_bytes: 100,
            free_bytes: 90,
            kernel_heap_bytes: 0,
            user_resident_bytes: 0,
            page_size: 4_096,
            reserved: 0,
            class_bytes: [10; MEMORY_CLASS_COUNT],
        }),
        ..granted()
    };
    assert!(report_of(sample).memory.composition.is_none());
}

#[test]
fn the_census_counts_what_the_recovery_section_lists() {
    let sample = Sample {
        processes: vec![
            process_summary(10, ProcessState::Stopped, b"halted", None),
            process_summary(11, ProcessState::Running, b"wedged", None),
            process_summary(12, ProcessState::Running, b"fine", None),
        ],
        stopped_count: 1,
        ..granted()
    };
    let seat = SeatReport::new(1, &[11]).expect("a report");
    let report = report_after(&[sample], &seat);
    assert_eq!(report.tasks.count(), Some(3));
    assert_eq!(report.tasks.stopped(), 1);
    assert_eq!(
        report.tasks.recovery(),
        2,
        "stopped and no longer answering"
    );
}

#[test]
fn the_busiest_are_the_cpu_panes_busiest_in_its_order() {
    let processes: Vec<_> = (0..7u16)
        .map(|index| {
            process_summary_with(
                u64::from(index) + 1,
                ProcessState::Running,
                alloc::format!("task-{index}").as_bytes(),
                Some(100 * index),
                1_000,
                u64::from(index) << 20,
                SchedPriority::Normal,
            )
        })
        .collect();
    let report = report_of(Sample {
        processes,
        ..granted()
    });
    let named: Vec<(String, u16, u64)> = report
        .tasks
        .busiest()
        .map(|task| {
            (
                String::from(task.name.as_str()),
                task.cpu.as_u16(),
                task.memory_bytes,
            )
        })
        .collect();
    assert_eq!(named.len(), MACHINE_TASKS_MAX);
    assert_eq!(named[0], (String::from("task-6"), 600, 6 << 20));
    assert_eq!(named[4], (String::from("task-2"), 200, 2 << 20));
    assert_eq!(report.tasks.count(), Some(7));
}

#[test]
fn a_name_that_is_not_text_still_names_its_task() {
    let sample = Sample {
        processes: vec![process_summary(
            1,
            ProcessState::Running,
            b"bad\xff\x07name",
            Some(500),
        )],
        ..granted()
    };
    let report = report_of(sample);
    let name = report
        .tasks
        .busiest()
        .next()
        .map(|task| String::from(task.name.as_str()));
    assert_eq!(name, Some(String::from("bad\u{FFFD}\u{FFFD}name")));
}

#[test]
fn a_name_too_long_for_its_field_is_cut_with_a_mark_on_a_character_boundary() {
    let long: String = core::iter::repeat_n('x', MACHINE_DEVICE_NAME_MAX * 2).collect();
    let cut: MachineDeviceName = fitted(long.as_bytes()).expect("a name");
    assert_eq!(cut.as_str().len(), MACHINE_DEVICE_NAME_MAX);
    assert!(cut.as_str().ends_with('\u{2026}'));
    // Two-byte characters: the cut may not split one.
    let wide: String = core::iter::repeat_n('é', MACHINE_DEVICE_NAME_MAX).collect();
    let cut: MachineDeviceName = fitted(wide.as_bytes()).expect("a name");
    assert!(cut.as_str().len() <= MACHINE_DEVICE_NAME_MAX);
    assert!(cut
        .as_str()
        .trim_end_matches('\u{2026}')
        .chars()
        .all(|c| c == 'é'));
    // One that fits is left whole, and an empty one names nothing.
    let whole: MachineDeviceName = fitted(b"nvme0").expect("a name");
    assert_eq!(whole.as_str(), "nvme0");
    assert!(fitted::<MACHINE_DEVICE_NAME_MAX>(b"").is_none());
}

#[test]
fn storage_names_the_least_healthy_first_and_keeps_mount_order_otherwise() {
    let healthy = mount_of("sda", "/", volume(0), 4_096, 100, 40);
    let degraded = with_availability(
        &mount_of("sdb", "/Storage/b", volume(1), 4_096, 100, 90),
        MountAvailability::Degraded,
    );
    let other = mount_of("sdc", "/Storage/c", volume(2), 4_096, 100, 10);
    let report = report_of(Sample {
        mounts: Some(vec![healthy, degraded, other]),
        ..granted()
    });
    let storage = report.storage.expect("storage read");
    let order: Vec<String> = storage
        .devices()
        .map(|device| String::from(device.name.as_str()))
        .collect();
    assert_eq!(order, ["sdb", "sda", "sdc"]);
    assert_eq!(storage.total(), 3);
    let first = storage.devices().next().expect("a device");
    assert_eq!(first.availability, MountAvailability::Degraded);
    let capacity = first.capacity.expect("a capacity");
    assert_eq!(capacity.total_bytes(), 100 * 4_096);
    assert_eq!(capacity.used_bytes(), 10 * 4_096);
}

#[test]
fn more_devices_than_a_report_names_are_still_counted() {
    let mounts: Vec<_> = (0..10u8)
        .map(|index| {
            mount_of(
                &alloc::format!("disk{index}"),
                &alloc::format!("/Storage/{index}"),
                volume(index),
                4_096,
                100,
                50,
            )
        })
        .collect();
    let storage = report_of(Sample {
        mounts: Some(mounts),
        ..granted()
    })
    .storage
    .expect("storage read");
    assert_eq!(storage.devices().count(), MACHINE_DEVICES_MAX);
    assert_eq!(storage.total(), 10);
}

fn state(name: &[u8], link_up: bool) -> NetInterfaceStateRecord {
    NetInterfaceStateRecord {
        name: if_name(name),
        link_up,
        addr_count: 0,
        addrs: [NetInterfaceStateRecord::EMPTY_ADDR; NET_IF_MAX_ADDRS],
    }
}

#[test]
fn the_network_leaves_out_loopback_and_spells_rates_in_bytes() {
    let mut lo = iface("lo");
    lo.kind = NetIfKind::Loopback;
    let sample = Sample {
        net_facts: Some(vec![iface("eth0"), lo, iface("eth1")]),
        net_state: Some(vec![state(b"eth0", true), state(b"eth1", false)]),
        net_rates: Some(vec![NetInterfaceRatesRecord {
            name: if_name(b"eth0"),
            window: Duration64::from_secs(1),
            rx_pps: 1,
            rx_bps: 8_000,
            tx_pps: 1,
            tx_bps: 800,
        }]),
        ..granted()
    };
    let network = report_of(sample).network.expect("the stack answered");
    assert_eq!(network.total(), 2, "the loopback is not counted");
    let interfaces: Vec<_> = network.interfaces().collect();
    assert_eq!(interfaces[0].name.as_str(), "eth0");
    assert_eq!(interfaces[0].link_up, Some(true));
    assert_eq!(interfaces[0].receive_rate, Some(1_000));
    assert_eq!(interfaces[0].send_rate, Some(100));
    assert_eq!(interfaces[1].name.as_str(), "eth1");
    assert_eq!(interfaces[1].link_up, Some(false));
    assert_eq!(
        interfaces[1].receive_rate, None,
        "no rate was served for it"
    );
}

#[test]
fn a_projected_report_is_a_frame_the_session_accepts_whole() {
    let mut class_bytes = [0u64; MEMORY_CLASS_COUNT];
    class_bytes[MemoryClass::Kernel.index()] = 1 << 20;
    let sample = Sample {
        identity: Some(
            SystemIdentity::new([1; MACHINE_ID_LEN], 1, 0, 0, b"edge").expect("an identity"),
        ),
        cpu_busy_permille: Some(10),
        core_busy: vec![CoreBusy {
            cpu: 0,
            permille: Some(10),
        }],
        memory_pressure: Some(MemoryPressureSample {
            band: 0,
            used_permille: 100,
            total_bytes: 1 << 30,
        }),
        kernel_memory: Some(KernelMemoryStats {
            total_bytes: 1 << 30,
            free_bytes: 1 << 29,
            kernel_heap_bytes: 0,
            user_resident_bytes: 0,
            page_size: 4_096,
            reserved: 0,
            class_bytes,
        }),
        processes: vec![process_summary(1, ProcessState::Running, b"init", Some(10))],
        mounts: Some(vec![mount_of("vda", "/", volume(0), 4_096, 10, 5)]),
        net_facts: Some(vec![iface("eth0")]),
        ..granted()
    };
    let report = report_of(sample);
    assert_eq!(MachineReport::from_bytes(&report.to_le_bytes()), Ok(report));
}
