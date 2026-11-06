//! The answer for the writes a re-inserted volume holds while its device
//! cannot prove the medium the one they were made on
//! (`docs/src/filesystem/drives.md` §8).
//!
//! The volume came back read-only, its content proven unmutated and its
//! writes held ([`Availability::ReplayUnconfirmed`]). The mount authority
//! answers for any such volume, wherever it runs, so a machine with no seat is
//! never left to a forced detach; the process holding the seat answers for the
//! user at it, and for a removable device's volume alone, so no user at the
//! desktop gives up writes other principals made on a fixed disk. *Write*
//! replays the writes once the content still proves the medium unmutated,
//! *Discard* gives them up. Either way the driver serving the read-only volume
//! is let go first, so no driver serves a medium changed under it, and the
//! volume is served again in place, writable unless the medium no longer
//! proves itself.

use alloc::boxed::Box;
use alloc::sync::Arc;

use tairix_abi::driver::block::Block;
use tairix_abi::volume::{ReplayAnswer, VolumeReplayRequest};
use tairix_abi::Errno;
use tairix_kernel_core::fs::Principal;
use tairix_log::{EventId, Sink};

use super::{
    answering_authority, audit_recovery_outcome, held_path, held_text, judged_level,
    read_blocks_at, record, shown_name, Availability, DeviceAuthority, Mount, Place, Proof,
    RecoveryOutcome, RecoveryTarget, RuntimeVolumeService, UnavailableFs, VolumeClaim, Wiring,
    NO_MOUNT_AUTHORITY,
};
use crate::system_mount::LATE_FILESYSTEM;

/// Audit event: an answer for a volume's held writes was refused, and nothing
/// was written or given up (drives.md `fs.hotplug.replay_refused`); `cause`
/// says why.
const VOLUME_REPLAY_REFUSED: EventId = EventId(4239);

/// The cause and refusal of an answer for a volume awaiting none.
const NOT_AWAITING: (&str, Errno) = ("not_awaiting_answer", Errno::Stale);

/// A volume claimed for an answer: the claim held while it is acted on, the
/// recovery it holds, and the extent and publisher it is served again over.
struct Unconfirmed<'a> {
    _claim: VolumeClaim<'a>,
    target: RecoveryTarget,
    extent: (u64, u64),
    publisher: u32,
}

impl RuntimeVolumeService {
    /// Answer `request` for `caller`: the held writes it names written to the
    /// volume's medium or discarded, and the volume served again.
    ///
    /// # Errors
    ///
    /// As [`VolumeService::replay`](tairix_kernel_core::VolumeService::replay),
    /// each refusal audited.
    pub(super) fn answer_held(
        &self,
        wiring: &Wiring,
        caller: &Principal<'_>,
        request: &VolumeReplayRequest,
    ) -> Result<(), Errno> {
        let audit = wiring.audit;
        let id = request.volume_id;
        let Some(by) = answering_authority(caller) else {
            return Err(refuse_answer(audit, &shown_name(&id), NO_MOUNT_AUTHORITY));
        };
        let Some((name, device)) = self.attached_device(&id) else {
            let unknown = ("identity_not_attached", Errno::NotFound);
            return Err(refuse_answer(audit, &shown_name(&id), unknown));
        };
        let refused = |refusal| refuse_answer(audit, &name, refusal);
        // The claim decides; this refuses short of the device's lock an answer
        // the claim would refuse.
        if by == DeviceAuthority::Seat && !self.removable(&id) {
            return Err(refused(NO_MOUNT_AUTHORITY));
        }
        let (endpoint, window) = device.ok_or_else(|| refused(NOT_AWAITING))?;
        let pin = self
            .pin_device(endpoint, window)
            .map_err(|errno| refused(("registry_full", errno)))?;
        let _op = pin.op.lock();
        let Unconfirmed {
            _claim,
            target,
            extent,
            publisher,
        } = self
            .claim_unconfirmed(&id, (endpoint, window), by)
            .map_err(refused)?;
        let device = self
            .device_for(endpoint, window)
            .ok_or_else(|| refused(("device_gone", Errno::DeviceOffline)))?;
        self.let_driver_go(&target);
        // Judged by the medium as it stands now, never by what the disk cache
        // last read of it; the journal's spin lock is let go before the
        // device's sleeping lock is taken.
        let evidence = target.journal.lock().evidence_window();
        if let Some((lba, len)) = evidence {
            device.device.with_device(|cache| cache.forget(lba, len));
        }
        let outcome = match request.answer {
            ReplayAnswer::Write => Self::attempt_replay(
                extent,
                &mut device.window(),
                device.block_size(),
                &target,
                (device.read_only, Proof::Answer(by)),
            ),
            ReplayAnswer::Discard => discard_held(&mut device.window(), &target, by),
        };
        audit_recovery_outcome(
            audit,
            &target.name,
            &outcome,
            &target.journal,
            Proof::Answer(by),
        );
        let place = Place {
            device: &device,
            at: (endpoint, window),
            extent,
            publisher,
        };
        self.serve_afresh(wiring, &place, &target, &outcome)
            .map_err(refused)?;
        // Kept, as the audited conflict records, and the asker told so.
        match (request.answer, outcome) {
            (ReplayAnswer::Write, RecoveryOutcome::Conflict(_)) => Err(Errno::ReadOnly),
            _ => Ok(()),
        }
    }

    /// Claim volume `id` for an answer on `by`'s authority: the authority must
    /// reach it, and it must await one on the device `held`, with no other
    /// operation acting on it.
    ///
    /// # Errors
    ///
    /// The refusal's cause and errno; the volume is left unclaimed.
    fn claim_unconfirmed(
        &self,
        id: &[u8; 16],
        held: (u64, u64),
        by: DeviceAuthority,
    ) -> Result<Unconfirmed<'_>, (&'static str, Errno)> {
        let mut state = self.state.lock();
        let volume = state
            .iter_mut()
            .find(|v| v.id == *id)
            .ok_or(("identity_not_attached", Errno::NotFound))?;
        if !by.reaches(volume.class) {
            return Err(NO_MOUNT_AUTHORITY);
        }
        if volume.availability != Availability::ReplayUnconfirmed
            || (volume.endpoint, volume.window) != held
        {
            return Err(NOT_AWAITING);
        }
        if volume.claimed {
            return Err(("volume_busy", Errno::Busy));
        }
        let format = volume.mount.format().ok_or(NOT_AWAITING)?;
        let target = RecoveryTarget {
            name: held_text(&volume.name)?,
            path: held_path(&volume.path)?,
            handle: volume.handle,
            format,
            fstype: volume.fstype,
            id: volume.id,
            identity: volume.identity,
            title: volume.title,
            journal: Arc::clone(&volume.journal),
            first_lba: volume.first_lba,
            blocks: volume.blocks,
            lost: false,
        };
        volume.claimed = true;
        let claim = VolumeClaim {
            service: self,
            id: *id,
        };
        Ok(Unconfirmed {
            _claim: claim,
            target,
            extent: volume.served_extent,
            publisher: volume.publisher,
        })
    }

    /// Let the driver serving `target` read-only go before its medium is
    /// written or its writes given up: the fail-closed stand-in takes the
    /// driver's place under its lock, so nothing reaches a driver whose view
    /// of the medium is about to go stale, and its served volume and node
    /// retire so it is unloaded. Its endpoint's end then finds no volume it
    /// serves.
    fn let_driver_go(&self, target: &RecoveryTarget) {
        let (served, node) = {
            let mut state = self.state.lock();
            state
                .iter_mut()
                .find(|v| v.id == target.id)
                .map_or((None, None), |v| {
                    v.mount = Mount::Mounted {
                        format: target.format,
                        server: None,
                    };
                    (v.served.take(), v.node.take())
                })
        };
        if let Ok(driver) = LATE_FILESYSTEM.driver(target.handle) {
            let mut lanes = driver.lock_all();
            for fs in lanes.each() {
                *fs = Box::new(UnavailableFs);
            }
        }
        self.retire_served(served.as_deref(), node);
    }
}

/// Give the writes `target`'s journal holds up on `by`'s authority, its
/// evidence re-read from the medium as it now stands — what a later re-insert
/// is judged against — and none kept where that window cannot be read.
fn discard_held<B: Block>(
    client: &mut B,
    target: &RecoveryTarget,
    by: DeviceAuthority,
) -> RecoveryOutcome {
    let (given_up, window) = {
        let journal = target.journal.lock();
        (journal.retained_bytes(), journal.evidence_window())
    };
    let current = window.and_then(|(lba, len)| Some((lba, read_blocks_at(client, lba, len)?)));
    let mut journal = target.journal.lock();
    journal.discard_all();
    match current {
        Some((lba, content)) => journal.set_evidence(lba, &content),
        // An empty window leaves the journal holding no evidence.
        None => journal.set_evidence(0, &[]),
    }
    RecoveryOutcome::Discarded(given_up, by)
}

/// Audit a refused answer for the volume shown as `name` and hand back its
/// errno.
fn refuse_answer(
    audit: &'static (dyn Sink + Sync),
    name: &str,
    (cause, errno): (&'static str, Errno),
) -> Errno {
    record(
        audit,
        judged_level(errno),
        VOLUME_REPLAY_REFUSED,
        "volume-service: an answer for held writes was refused",
        name,
        cause,
    );
    errno
}
