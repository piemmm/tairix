//! The disk images and archives this file manager attached because the user
//! opened them, and when each is to be unmounted: once no window shows it, if
//! nothing else is using it then.
//!
//! The kernel decides whether anything else is using a volume, by refusing an
//! idle detach of one still held (`DetachMode::Idle`); this decides only when
//! to ask. A volume found in use is asked about again as soon as something this
//! file manager started ends, or a window shows it and leaves it again. Nothing
//! says when a holder elsewhere lets go, so failing those it is asked again
//! after a wait that doubles each time it is still in use, from
//! [`FIRST_RETRY_NS`] to [`LONGEST_RETRY_NS`] — a volume held all day costs a
//! question every few minutes, never one a turn.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::VOLUME_ID_LEN;
use tairix_abi::volume::DetachMode;
use tairix_abi::{Errno, STORAGE_CATALOG};
use tairix_util::fallible;

/// The first wait before a volume found in use is asked about again.
pub const FIRST_RETRY_NS: u64 = 5_000_000_000;

/// The longest wait before a volume found in use is asked about again.
pub const LONGEST_RETRY_NS: u64 = 300_000_000_000;

/// Where the unmount of one volume stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Unmount {
    /// Nothing is known to hold it: it is asked for once no window shows it.
    Due,
    /// Asked for; its answer is on its way.
    Asked,
    /// Asked for and found in use: asked again at `retry_ns`, or sooner once
    /// something this file manager started ends, or a window shows it and
    /// leaves it again.
    InUse { retry_ns: u64 },
}

/// One volume this file manager attached for an open.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Opened {
    volume_id: [u8; VOLUME_ID_LEN],
    /// Every place it is known to be shown at, root-first: its catalog entry
    /// first, whose last component is what the user calls it, then any other
    /// it was found shown through.
    roots: Vec<Vec<String>>,
    unmount: Unmount,
    /// The last wait before it was asked about again while in use; `0` until
    /// it is found in use.
    backoff_ns: u64,
    /// The mount table has listed it, so its leaving that table means it was
    /// unmounted some other way.
    seen: bool,
}

impl Opened {
    /// What the user calls it: its name in the storage catalog.
    fn name(&self) -> &str {
        self.roots
            .first()
            .and_then(|root| root.last())
            .map_or("", String::as_str)
    }
}

/// Add a copy of `root` to `roots`, or answer `false` where there is no room
/// for it.
fn note_root(roots: &mut Vec<Vec<String>>, root: &[String]) -> bool {
    let mut copy = Vec::new();
    if copy.try_reserve_exact(root.len()).is_err() {
        return false;
    }
    for part in root {
        let Some(part) = fallible::concatenated(&[part]) else {
            return false;
        };
        copy.push(part);
    }
    fallible::push(roots, copy)
}

/// The volumes this file manager attached for an open, and which of them to
/// unmount now.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpenedVolumes {
    volumes: Vec<Opened>,
}

/// An unmount to ask for: of the volume published as `volume_id`, called
/// `name`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Finished<'a> {
    /// The identity a detach names it by.
    pub volume_id: [u8; VOLUME_ID_LEN],
    /// What the user calls it.
    pub name: &'a str,
}

impl OpenedVolumes {
    /// No volume opened.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            volumes: Vec::new(),
        }
    }

    /// Note that opening an image attached the volume published as
    /// `volume_id` under `name`; one already noted is left as it is.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] where there is no room to note it: the volume
    /// is then never unmounted for being closed, which loses nothing.
    pub fn opened(&mut self, volume_id: [u8; VOLUME_ID_LEN], name: &str) -> Result<(), Errno> {
        if self.volumes.iter().any(|held| held.volume_id == volume_id) {
            return Ok(());
        }
        let text =
            |part: &str| tairix_util::fallible::concatenated(&[part]).ok_or(Errno::OutOfMemory);
        let mut catalog = Vec::new();
        catalog
            .try_reserve_exact(2)
            .map_err(|_| Errno::OutOfMemory)?;
        catalog.push(text(STORAGE_CATALOG)?);
        catalog.push(text(name)?);
        let mut roots = Vec::new();
        roots.try_reserve_exact(1).map_err(|_| Errno::OutOfMemory)?;
        roots.push(catalog);
        self.volumes
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        self.volumes.push(Opened {
            volume_id,
            roots,
            unmount: Unmount::Due,
            backoff_ns: 0,
            seen: false,
        });
        Ok(())
    }

    /// The unmount owed at `now_ns`, where `enabled`: of the first volume no
    /// window shows at any place it is known at — `on_show` answering whether
    /// one shows a place at or below a root — that nothing is known to hold, or
    /// whose wait since it was found in use is over, and that is not one `lost`
    /// names, which only the user may let go.
    ///
    /// One is asked for at a time, and not again until its answer lands, so
    /// unmounts the user did not ask for never crowd out an eject they did.
    pub fn sweep(
        &mut self,
        (now_ns, enabled): (u64, bool),
        on_show: impl Fn(&[String]) -> bool,
        lost: impl Fn(&[u8; VOLUME_ID_LEN]) -> bool,
    ) -> Option<Finished<'_>> {
        let mut asking = self
            .volumes
            .iter()
            .any(|volume| volume.unmount == Unmount::Asked);
        let mut owed = None;
        for (at, volume) in self.volumes.iter_mut().enumerate() {
            if volume.unmount == Unmount::Asked {
                continue;
            }
            if volume.roots.iter().any(|root| on_show(root)) {
                // Shown, leaving it again is a new chance it is free.
                volume.unmount = Unmount::Due;
                volume.backoff_ns = 0;
                continue;
            }
            let waiting =
                matches!(volume.unmount, Unmount::InUse { retry_ns } if retry_ns > now_ns);
            if asking || !enabled || waiting || lost(&volume.volume_id) {
                continue;
            }
            volume.unmount = Unmount::Asked;
            asking = true;
            owed = Some(at);
        }
        let volume = self.volumes.get(owed?)?;
        Some(Finished {
            volume_id: volume.volume_id,
            name: volume.name(),
        })
    }

    /// Note that the volume published as `volume_id`, asked to be unmounted,
    /// was found shown at `roots` once its places were known: it is due again
    /// once no window shows any of them. A place it cannot note leaves it
    /// waiting as one in use rather than asked about again every turn.
    pub fn shown_at(&mut self, volume_id: [u8; VOLUME_ID_LEN], roots: &[&[String]]) {
        let Some(volume) = self
            .volumes
            .iter_mut()
            .find(|volume| volume.volume_id == volume_id)
        else {
            return;
        };
        volume.unmount = Unmount::Due;
        for root in roots {
            if volume.roots.iter().any(|known| known.as_slice() == *root) {
                continue;
            }
            if !note_root(&mut volume.roots, root) {
                // Not asked again every turn while that window stays.
                volume.unmount = Unmount::InUse { retry_ns: u64::MAX };
                return;
            }
        }
    }

    /// Whether an unmount is on its way, whose answer has not landed.
    #[must_use]
    pub fn unmounting(&self) -> bool {
        self.volumes
            .iter()
            .any(|volume| volume.unmount == Unmount::Asked)
    }

    /// Note that the unmount asked for the volume published as `volume_id`
    /// could not be sent: it is due again.
    pub fn deferred(&mut self, volume_id: [u8; VOLUME_ID_LEN]) {
        for volume in &mut self.volumes {
            if volume.volume_id == volume_id && volume.unmount == Unmount::Asked {
                volume.unmount = Unmount::Due;
            }
        }
    }

    /// Note that a child of this file manager ended, `launched` when it was
    /// an application it started: whatever that held is let go, so each
    /// volume found in use is worth asking about again. Any other child — one
    /// of its own decoders — held none of them, so its end asks nothing.
    pub fn child_ended(&mut self, launched: bool) {
        if !launched {
            return;
        }
        for volume in &mut self.volumes {
            if matches!(volume.unmount, Unmount::InUse { .. }) {
                volume.unmount = Unmount::Due;
            }
        }
    }

    /// When a volume found in use is next due to be asked about, if the
    /// [`sweep`](Self::sweep) given `enabled` and `lost` would then ask: none
    /// while unmounting is not `enabled` or one is on its way, whose landing
    /// is a turn of its own, and none for a volume `lost` names.
    ///
    /// A time the sweep would pass over stays in the past, and a loop parking
    /// until it would turn without waiting.
    #[must_use]
    pub fn retry_at(
        &self,
        enabled: bool,
        lost: impl Fn(&[u8; VOLUME_ID_LEN]) -> bool,
    ) -> Option<u64> {
        if !enabled || self.unmounting() {
            return None;
        }
        self.volumes
            .iter()
            .filter(|volume| !lost(&volume.volume_id))
            .filter_map(|volume| match volume.unmount {
                Unmount::InUse { retry_ns } => Some(retry_ns),
                Unmount::Due | Unmount::Asked => None,
            })
            .min()
    }

    /// Note what the unmount of the volume published as `volume_id` came to,
    /// at `now_ns`. Found in use, it waits for something to let it go, or for
    /// a wait twice the last; unmounted, gone already, or refused for a reason
    /// the user was told, it is not asked about again.
    pub fn landed(
        &mut self,
        volume_id: [u8; VOLUME_ID_LEN],
        answer: Result<(), Errno>,
        now_ns: u64,
    ) {
        let Some(at) = self
            .volumes
            .iter()
            .position(|volume| volume.volume_id == volume_id)
        else {
            return;
        };
        match answer {
            Err(err) if DetachMode::Idle.left_in_use(err) => {
                if let Some(volume) = self.volumes.get_mut(at) {
                    volume.backoff_ns = if volume.backoff_ns == 0 {
                        FIRST_RETRY_NS
                    } else {
                        volume.backoff_ns.saturating_mul(2).min(LONGEST_RETRY_NS)
                    };
                    volume.unmount = Unmount::InUse {
                        retry_ns: now_ns.saturating_add(volume.backoff_ns),
                    };
                }
            }
            Ok(()) | Err(_) => {
                self.volumes.remove(at);
            }
        }
    }

    /// Note that the mount table lists the volumes `mounted` names: one listed
    /// before and missing now was unmounted some other way, and is forgotten.
    pub fn mounted(&mut self, mounted: impl Fn(&[u8; VOLUME_ID_LEN]) -> bool) {
        self.volumes.retain_mut(|volume| {
            let listed = mounted(&volume.volume_id);
            let keep = listed || !volume.seen;
            volume.seen |= listed;
            keep
        });
    }
}

#[cfg(test)]
#[path = "opened_tests.rs"]
mod tests;
