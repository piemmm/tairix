//! What the System Monitor's tests are drawn from.

use tairix_abi::switchboard_ipc::{
    DeviceCapacity, MachineCommitted, MachineCores, MachineCpu, MachineDevice, MachineDeviceName,
    MachineHistory, MachineInterface, MachineInterfaceName, MachineMemory, MachineNetwork,
    MachineReport, MachineScope, MachineStorage, MachineTask, MachineTasks, Permille, ReportPeriod,
    TrayTaskName,
};
use tairix_abi::sysinfo::{LoadAverage, MemoryBand, MountAvailability};
use tairix_abi::Duration64;

/// The period every fixture report states.
pub(in crate::saver) const PERIOD_NS: u64 = 2_000_000_000;

pub(in crate::saver) fn share(value: u16) -> Permille {
    Permille::new(value).expect("a fraction")
}

/// A report carrying every reading, so every part has something to draw.
pub(in crate::saver) fn report() -> MachineReport {
    MachineReport {
        period: ReportPeriod::from_millis(2_000).expect("a period"),
        scope: MachineScope::Machine,
        host: None,
        uptime: Some(Duration64::from_secs(93_784)),
        cpu: MachineCpu {
            busy: Some(share(370)),
            pressured: false,
            load: Some(LoadAverage {
                load1: 3 << 11,
                load5: 2 << 11,
                load15: 1 << 11,
                runnable: 4,
                total_tasks: 412,
                users: 2,
            }),
            cores: MachineCores::new((0..16).map(|core| Some(share(core * 60)))).expect("cores"),
            history: MachineHistory::new(&[100, 300, 370]).expect("history"),
        },
        memory: MachineMemory {
            committed: Some(MachineCommitted::new(16 << 30, share(610)).expect("committed")),
            band: MemoryBand::new(0).ok(),
            pressured: false,
            composition: None,
            history: MachineHistory::new(&[600, 610]).expect("history"),
        },
        tasks: MachineTasks::new(
            Some(214),
            1,
            1,
            &[MachineTask {
                name: TrayTaskName::new("postgres").expect("a name"),
                cpu: share(340),
                memory_bytes: 2 << 30,
            }],
        )
        .expect("tasks"),
        storage: Some(
            MachineStorage::new(
                12,
                &[MachineDevice {
                    name: MachineDeviceName::new("nvme0").expect("a name"),
                    availability: MountAvailability::Degraded,
                    capacity: Some(DeviceCapacity::new(1 << 40, 1 << 39).expect("capacity")),
                    busy: Some(share(20)),
                    read_rate: Some(1 << 20),
                    write_rate: Some(1 << 18),
                }],
            )
            .expect("storage"),
        ),
        network: Some(
            MachineNetwork::new(
                1,
                &[MachineInterface {
                    name: MachineInterfaceName::new("eth0").expect("a name"),
                    link_up: Some(true),
                    receive_rate: Some(1 << 20),
                    send_rate: Some(1 << 16),
                }],
            )
            .expect("network"),
        ),
    }
}

/// A report that read nothing at all.
pub(crate) fn unread() -> MachineReport {
    MachineReport {
        period: ReportPeriod::from_millis(2_000).expect("a period"),
        scope: MachineScope::Machine,
        host: None,
        uptime: None,
        cpu: MachineCpu::UNREAD,
        memory: MachineMemory::UNREAD,
        tasks: MachineTasks::UNREAD,
        storage: None,
        network: None,
    }
}
