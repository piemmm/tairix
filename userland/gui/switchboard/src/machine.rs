//! The machine report (`plans/NEW-SWITCHBOARD.md` S14): the sample this
//! service already took, projected into the fixed frame the desktop's System
//! Monitor screensaver draws.
//!
//! Nothing here samples, derives a second verdict, or spells a figure: each
//! reading is one the overview panel draws — the same busy share and pressure
//! latches, the same composition, the same busiest tasks, the same storage
//! devices with the same health — so the board on the wall and the panel on
//! the desk cannot disagree. A reading the sample lacks is absent from the
//! report, never a zero.

use core::cmp::Reverse;

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::net_ipc::{NetIfKind, NetInterfaceFactsRecord, NetInterfaceRatesRecord};
use tairix_abi::switchboard_ipc::{
    DeviceCapacity, MachineCommitted, MachineComposition, MachineCores, MachineCpu, MachineDevice,
    MachineHistory, MachineInterface, MachineMemory, MachineNetwork, MachineReport, MachineScope,
    MachineStorage, MachineTask, MachineTasks, Permille, ReportPeriod, SeatReport,
    MACHINE_CORES_MAX, MACHINE_DEVICES_MAX, MACHINE_HISTORY_MAX, MACHINE_INTERFACES_MAX,
    MACHINE_TASKS_MAX,
};
use tairix_abi::sysinfo::MemoryBand;
use tairix_controls::RecoveryState;
use tairix_font::ELLIPSIS;

use crate::model::{process_count, process_recovery, RollingMeters};
use crate::resource_report::{busiest_by_cpu, served_rates, storage_subjects, trim_nul};
use crate::sample::Sample;
use crate::schedule::SAMPLE_PERIOD_NS;

/// The sample period in whole milliseconds, the unit a report states it in.
const PERIOD_MS: u64 = SAMPLE_PERIOD_NS / 1_000_000;
const _: () = assert!(PERIOD_MS <= 0xFFFF_FFFF);

/// How often this service samples, as a report states it.
#[allow(clippy::cast_possible_truncation)] // Guarded by the assert above.
const REPORT_PERIOD: ReportPeriod = match ReportPeriod::from_millis(PERIOD_MS as u32) {
    Ok(period) => period,
    Err(_) => panic!("the sample period is one a report can state"),
};

/// The machine as `sample` saw it, with the histories and pressure latches
/// `meters` folded from it and the unresponsive owners `seat` reports.
#[must_use]
pub fn machine_report(sample: &Sample, meters: &RollingMeters, seat: &SeatReport) -> MachineReport {
    MachineReport {
        period: REPORT_PERIOD,
        scope: if sample.scopes.global_process_scope {
            MachineScope::Machine
        } else {
            MachineScope::Own
        },
        host: sample
            .identity
            .as_ref()
            .and_then(|identity| fitted(identity.hostname_bytes())),
        uptime: sample.uptime.map(|uptime| uptime.since_boot),
        cpu: cpu(sample, meters),
        memory: memory(sample, meters),
        tasks: tasks(sample, seat),
        storage: storage(sample, meters),
        network: network(sample),
    }
}

/// The processors: the busy share and its latch, the load, every core, and
/// the trace.
fn cpu(sample: &Sample, meters: &RollingMeters) -> MachineCpu {
    let cores = sample
        .core_busy
        .iter()
        .take(MACHINE_CORES_MAX)
        .map(|core| core.permille.and_then(share));
    MachineCpu {
        busy: sample.cpu_busy_permille.and_then(share),
        pressured: meters.system.cpu_pressured(),
        load: sample.load_average,
        cores: MachineCores::new(cores).unwrap_or(MachineCores::EMPTY),
        history: history(meters.system.cpu_history()),
    }
}

/// Memory: the committed share and its latch, the band, where the RAM went,
/// and the trace.
fn memory(sample: &Sample, meters: &RollingMeters) -> MachineMemory {
    MachineMemory {
        committed: sample.memory_pressure.and_then(|memory| {
            MachineCommitted::new(memory.total_bytes, share(memory.used_permille)?).ok()
        }),
        band: sample
            .pressure_band
            .and_then(|band| MemoryBand::new(band.band).ok()),
        pressured: meters.system.memory_pressured(),
        // The kernel's own classes partition the RAM in use, so they never sum
        // past its whole; a sample in which they did is left unsaid.
        composition: sample.kernel_memory.and_then(|kernel| {
            MachineComposition::new(kernel.class_bytes, kernel.free_bytes, kernel.total_bytes).ok()
        }),
        history: history(meters.system.memory_history()),
    }
}

/// The census, read through the one classifier the Recovery section lists by,
/// and the tasks the CPU pane names as costing most.
fn tasks(sample: &Sample, seat: &SeatReport) -> MachineTasks {
    let Some(count) = process_count(sample) else {
        return MachineTasks::UNREAD;
    };
    let awaiting = sample
        .processes
        .iter()
        .filter(|process| process_recovery(process, seat) != RecoveryState::None)
        .count();
    let busiest: Vec<MachineTask> = busiest_by_cpu(sample)
        .into_iter()
        .take(MACHINE_TASKS_MAX)
        .filter_map(|(process, cpu)| {
            Some(MachineTask {
                name: fitted(&process.name)?,
                cpu: share(u16::try_from(cpu).ok()?)?,
                memory_bytes: process.mem_bytes,
            })
        })
        .collect();
    let awaiting = u16::try_from(awaiting).unwrap_or(u16::MAX).min(count);
    MachineTasks::new(
        Some(u32::from(count)),
        sample.stopped_count.min(count),
        awaiting,
        &busiest,
    )
    .unwrap_or(MachineTasks::UNREAD)
}

/// Every storage device the panel lists, the least healthy first.
///
/// The sort is stable, so devices of one health keep the mount table's own
/// order and the board does not reshuffle while nothing changes.
fn storage(sample: &Sample, meters: &RollingMeters) -> Option<MachineStorage> {
    sample.mounts.as_ref()?;
    let subjects = storage_subjects(sample);
    let mut devices: Vec<MachineDevice> = subjects
        .iter()
        .filter_map(|subject| {
            let service = meters.devices.volume_service(subject.device_id());
            Some(MachineDevice {
                name: fitted(subject.name(sample).as_bytes())?,
                availability: subject.availability(sample),
                capacity: subject
                    .held()
                    .and_then(|held| DeviceCapacity::new(held.total, held.used()).ok()),
                busy: service.utilisation_permille.and_then(share),
                read_rate: service.read_bps,
                write_rate: service.write_bps,
            })
        })
        .collect();
    devices.sort_by_key(|device| Reverse(device.availability.health()));
    devices.truncate(MACHINE_DEVICES_MAX);
    MachineStorage::new(u16::try_from(subjects.len()).unwrap_or(u16::MAX), &devices).ok()
}

/// Every interface the stack manages but its loopback, which carries only the
/// machine's traffic to itself, in the stack's own order.
fn network(sample: &Sample) -> Option<MachineNetwork> {
    let facts = sample.net_facts.as_ref()?;
    let managed: Vec<&NetInterfaceFactsRecord> = facts
        .iter()
        .filter(|iface| iface.kind != NetIfKind::Loopback)
        .collect();
    let shown: Vec<MachineInterface> = managed
        .iter()
        .take(MACHINE_INTERFACES_MAX)
        .filter_map(|iface| {
            let rates = served_rates(sample, &iface.name);
            Some(MachineInterface {
                name: fitted(trim_nul(&iface.name))?,
                link_up: sample
                    .net_state
                    .as_ref()
                    .and_then(|states| {
                        states
                            .iter()
                            .find(|state| trim_nul(&state.name) == trim_nul(&iface.name))
                    })
                    .map(|state| state.link_up),
                receive_rate: rates.map(NetInterfaceRatesRecord::rx_bytes_per_sec),
                send_rate: rates.map(NetInterfaceRatesRecord::tx_bytes_per_sec),
            })
        })
        .collect();
    MachineNetwork::new(u16::try_from(managed.len()).unwrap_or(u16::MAX), &shown).ok()
}

/// A permille reading as the frame carries it.
fn share(permille: u16) -> Option<Permille> {
    Permille::new(permille).ok()
}

/// The newest readings of `points` the frame carries.
fn history(points: &[u16]) -> MachineHistory {
    let newest = &points[points.len().saturating_sub(MACHINE_HISTORY_MAX)..];
    MachineHistory::new(newest).unwrap_or(MachineHistory::EMPTY)
}

/// `raw` as a bounded display name, or `None` for an empty one.
///
/// A name is display text whoever chose it, so bytes that are not text and
/// control characters stand as U+FFFD rather than refusing the name, and one
/// too long for the field is cut on a character boundary with a mark saying
/// it was cut.
fn fitted<const MAX: usize>(raw: &[u8]) -> Option<BoundedText<1, MAX>> {
    let text: String = String::from_utf8_lossy(raw)
        .chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    if text.is_empty() {
        return None;
    }
    if let Ok(name) = BoundedText::new(&text) {
        return Some(name);
    }
    let mut end = MAX.checked_sub(ELLIPSIS.len())?.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = String::from(&text[..end]);
    cut.push_str(ELLIPSIS);
    BoundedText::new(&cut).ok()
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
