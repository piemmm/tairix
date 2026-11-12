//! Deterministic fuzz harness for the machine snapshot. For any change to
//! the bytes of a real snapshot:
//!
//! 1. Restoring them into a machine built as the snapshot's was either takes
//!    or is refused; it never panics.
//! 2. A machine they were restored into runs on without panicking: every
//!    value a restore takes is one the machine can hold.
//!
//! The snapshots are of each template partway through booting its firmware
//! and TAIRiX DOS. Changes land on the processor and the board's head, on
//! the chips, cards and drives past the memory, and anywhere at all.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::bios;
use tairix_pcemu::dos::setup::{default_disk, DefaultDisk};
use tairix_pcemu::machine::{Machine, Template};
use tairix_pcemu::time::{Nanos, NANOS_PER_MILLISECOND as MS, NANOS_PER_SECOND};

/// Snapshots changed and restored by a plain `cargo test`, per template.
const SMOKE_ITERATIONS: u64 = 24;

/// How long a machine restored from a changed snapshot runs.
const RUN_ON: Nanos = 30 * MS;

const FINGERPRINT: &[u8] = b"the snapshot fuzz";

const TEMPLATES: [Template; 4] = [
    Template::IbmPc,
    Template::IbmXt,
    Template::TurboXt,
    Template::Workstation186,
];

/// A machine of `template`, its default disk attached where it has a
/// fixed-disk controller.
fn machine(template: Template, disk: Option<&DefaultDisk>) -> Machine {
    let config = template.config();
    let mut m = bios::machine(&config, [disk.map(|disk| disk.kind), None]).unwrap();
    if let Some(disk) = disk {
        m.board_mut()
            .attach_fixed(
                0,
                disk.kind,
                Box::new(disk.device.clone()),
                disk.low_level.clone(),
            )
            .unwrap();
    }
    m
}

/// `pristine` with a few of its bytes changed where they stand: nothing
/// moved, so every field after a change still reads as a field.
fn changed(rng: &mut Prng, pristine: &[u8]) -> Vec<u8> {
    let mut bytes = pristine.to_vec();
    let len = bytes.len();
    for _ in 0..=rng.below(3) {
        let at = match rng.below(3) {
            0 => rng.below(len.min(4096)),
            1 => len - 1 - rng.below(len.min(128 * 1024)),
            _ => rng.below(len),
        };
        match rng.below(4) {
            0 => bytes[at] ^= 1 << rng.below(8),
            1 => bytes[at] = *rng.pick(&[0, 1, 2, 3, 7, 8, 0x0F, 0x10, 0x7F, 0x80, 0xFE, 0xFF]),
            2 => bytes[at] = rng.next_u8(),
            _ => {
                let end = (at + 8).min(len);
                bytes[at..end].fill(0xFF);
            }
        }
    }
    bytes
}

#[test]
fn a_changed_snapshot_is_refused_or_restores_a_machine_that_runs() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_changed_snapshot_is_refused_or_restores_a_machine_that_runs",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let taken: Vec<(Template, Option<DefaultDisk>, Vec<u8>)> = TEMPLATES
        .into_iter()
        .map(|template| {
            let disk = template
                .config()
                .fixed
                .map(|adapter| default_disk(adapter.recording, 0x2026_1111).unwrap());
            let mut m = machine(template, disk.as_ref());
            bios::run_until(&mut m, 3 * NANOS_PER_SECOND);
            let snapshot = m.snapshot(FINGERPRINT).unwrap();
            (template, disk, snapshot)
        })
        .collect();
    let mut restored = 0u64;
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            for (template, disk, snapshot) in &taken {
                let mut m = machine(*template, disk.as_ref());
                if m.restore(&changed(&mut rng, snapshot), FINGERPRINT)
                    .is_err()
                {
                    continue;
                }
                restored += 1;
                let now = m.now();
                bios::run_until(&mut m, now.saturating_add(RUN_ON));
            }
        }
        assert!(restored > 0, "some changes are ones a machine can hold");
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
