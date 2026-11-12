//! The machine's digest, and the run the cross-target claim is staked on.
//!
//! The engine is written to compute alike everywhere — integers throughout
//! but for the sound chips' tables, which go through `lib/util`'s own maths
//! rather than a platform libm — but that is a claim, not evidence. Every
//! Tier-1 target boots the default machine, the processor interpreted, for
//! the same emulated time and asserts one constant, [`REFERENCE_DIGEST`]:
//! agreement between targets follows from each agreeing with it, so no
//! target passes by never having run. The digest folds the machine's whole
//! state at every checkpoint, so a divergence the boot later converges from
//! is caught too.

use core::hash::Hasher as _;

use alloc::boxed::Box;

use tairix_hash::FastHash;

use crate::bios::PowerError;
use crate::dos::setup::{default_disk, SetupError};
use crate::machine::{Machine, Template};
use crate::storage::hdc::AttachError;
use crate::time::{Nanos, NANOS_PER_SECOND};

/// The digest of [`reference_run`] on every Tier-1 target.
///
/// Any change to what the machine does moves it. The constant is the record
/// of what the machine does, written down deliberately when it changes and
/// never pasted out of a failure.
pub const REFERENCE_DIGEST: u64 = 0x4962_880E_3C2A_8CB8;

/// The machine the run boots: the one a new library is given.
const MACHINE: Template = Template::TurboXt;

/// The default disk's volume serial number in the run.
const SERIAL: u32 = 0x2026_1112;

/// What the snapshots the digest folds are taken under.
const FINGERPRINT: &[u8] = b"the reference run";

/// The emulated time between checkpoints, and the checkpoints: to four
/// seconds, past DOS's prompt.
const STEP: Nanos = NANOS_PER_SECOND / 2;
const CHECKPOINTS: u64 = 8;

/// Why the reference run could not be made.
#[derive(Debug, PartialEq, Eq)]
pub enum Unrun {
    /// The default disk could not be made.
    Disk(SetupError),
    /// The machine could not be built.
    Power(PowerError),
    /// The disk did not fit its drive.
    Attach(AttachError),
    /// A checkpoint's state did not fit the heap.
    Snapshot,
}

/// Boots the default machine on its default disk to the last checkpoint,
/// the processor interpreted, folding its whole state at each.
///
/// # Errors
///
/// [`Unrun`] where the machine could not be made or its state not kept.
pub fn reference_run() -> Result<u64, Unrun> {
    booted().map(|(digest, _)| digest)
}

/// The reference run's digest, and the machine as it left it.
fn booted() -> Result<(u64, Machine), Unrun> {
    let config = MACHINE.config();
    let disk = config
        .fixed
        .map(|adapter| default_disk(adapter.recording, SERIAL))
        .transpose()
        .map_err(Unrun::Disk)?;
    let mut machine = crate::bios::machine(&config, [disk.as_ref().map(|disk| disk.kind), None])
        .map_err(Unrun::Power)?;
    if let Some(disk) = disk {
        machine
            .board_mut()
            .attach_fixed(0, disk.kind, Box::new(disk.device), disk.low_level)
            .map_err(Unrun::Attach)?;
    }
    let mut hasher = FastHash::new();
    for checkpoint in 1..=CHECKPOINTS {
        crate::bios::run_until(&mut machine, checkpoint * STEP);
        let state = machine.snapshot(FINGERPRINT).ok_or(Unrun::Snapshot)?;
        hasher.write(&state);
    }
    Ok((hasher.finish(), machine))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_run_reaches_the_prompt_with_the_digest_every_target_asserts() {
        let (digest, machine) = booted().unwrap();
        let screen = machine.board().shown_text(0).unwrap();
        let text = crate::link::Text {
            columns: screen.columns,
            rows: screen.rows,
            codes: &screen.codes,
        };
        assert_eq!(
            text.last_line(),
            crate::dos::setup::ROOT_PROMPT,
            "at DOS's prompt"
        );
        assert_eq!(digest, REFERENCE_DIGEST);
    }
}
