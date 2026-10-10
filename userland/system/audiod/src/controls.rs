//! Whose device controls are in force: the room's tenant's, over the machine's
//! baseline (`plans/SOUND.md` §Desktop integration).
//!
//! Each tenant keeps what it set — levels, mutes and the endpoint it prefers
//! as each direction's default — keyed by location, so a session's controls
//! stand aside while another holds the room and are back, unchanged, when it
//! returns.

use alloc::vec::Vec;

use tairix_abi::audio::{AudioBaseline, AudioGain, AudioLocation};
use tairix_abi::driver::audio::StreamDirection;
use tairix_abi::{Errno, ProcId};
use tairix_audio::route::Room;
use tairix_util::fallible;

/// Who a room's controls belong to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Tenant {
    /// Nobody holds the seat, so the room's controls are anybody's.
    Unclaimed,
    /// This login session holds the seat.
    Session(ProcId),
}

impl Tenant {
    /// The tenant of `room`. A withheld room has none, so nothing may change
    /// its controls and the baseline is in force.
    pub(crate) const fn of(room: Room) -> Option<Self> {
        match room {
            Room::Unclaimed => Some(Self::Unclaimed),
            Room::Session(session) => Some(Self::Session(session)),
            Room::Withheld => None,
        }
    }
}

/// What one tenant set.
struct Tenancy {
    tenant: Tenant,
    levels: Vec<(AudioLocation, AudioGain)>,
    muted: Vec<AudioLocation>,
    /// The endpoint preferred as each direction's default, playback first.
    prefer: [Option<AudioLocation>; 2],
}

/// Every tenant's controls, over the machine's baseline.
pub(crate) struct Controls {
    baseline: AudioBaseline,
    tenancies: Vec<Tenancy>,
}

/// The index of `direction` in a per-direction pair, playback first.
pub(crate) const fn direction_slot(direction: StreamDirection) -> usize {
    match direction {
        StreamDirection::Playback => 0,
        StreamDirection::Capture => 1,
    }
}

impl Controls {
    /// No tenant's controls, over the machine nobody configured.
    pub(crate) const fn new() -> Self {
        Self {
            baseline: AudioBaseline::DEFAULT,
            tenancies: Vec::new(),
        }
    }

    /// Adopt the machine's baseline, answering whether it moved.
    pub(crate) fn set_baseline(&mut self, baseline: AudioBaseline) -> bool {
        let moved = self.baseline != baseline;
        self.baseline = baseline;
        moved
    }

    /// The level in force `at` for `tenant`: its own, else the baseline's.
    pub(crate) fn level(&self, tenant: Option<Tenant>, at: AudioLocation) -> AudioGain {
        self.tenancy(tenant)
            .and_then(|held| held.levels.iter().find(|(place, _)| *place == at))
            .map_or(self.baseline.level, |(_, level)| *level)
    }

    /// Whether `tenant` set a level of its own `at`, rather than taking the
    /// baseline's.
    pub(crate) fn own_level(&self, tenant: Option<Tenant>, at: AudioLocation) -> bool {
        self.tenancy(tenant)
            .is_some_and(|held| held.levels.iter().any(|(place, _)| *place == at))
    }

    /// Whether `tenant` has the endpoint `at` muted.
    pub(crate) fn muted(&self, tenant: Option<Tenant>, at: AudioLocation) -> bool {
        self.tenancy(tenant)
            .is_some_and(|held| held.muted.contains(&at))
    }

    /// The preferences a `direction` default is chosen by, the tenant's
    /// first and the machine's after it.
    pub(crate) fn preferences(
        &self,
        tenant: Option<Tenant>,
        direction: StreamDirection,
    ) -> [Option<AudioLocation>; 2] {
        let machine = match direction {
            StreamDirection::Playback => self.baseline.output,
            StreamDirection::Capture => self.baseline.input,
        };
        let own = self
            .tenancy(tenant)
            .and_then(|held| held.prefer[direction_slot(direction)]);
        [own, machine]
    }

    /// Set `tenant`'s level `at`, answering whether the level in force moved.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the tenancy cannot grow.
    pub(crate) fn set_level(
        &mut self,
        tenant: Tenant,
        at: AudioLocation,
        level: AudioGain,
    ) -> Result<bool, Errno> {
        let before = self.level(Some(tenant), at);
        let held = self.tenancy_mut(tenant)?;
        if let Some(entry) = held.levels.iter_mut().find(|(place, _)| *place == at) {
            entry.1 = level;
        } else {
            if !fallible::reserve(&mut held.levels, 1) {
                return Err(Errno::OutOfMemory);
            }
            held.levels.push((at, level));
        }
        Ok(before != level)
    }

    /// Mute or unmute `at` for `tenant`, answering whether that moved.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the tenancy cannot grow.
    pub(crate) fn set_muted(
        &mut self,
        tenant: Tenant,
        at: AudioLocation,
        muted: bool,
    ) -> Result<bool, Errno> {
        if self.muted(Some(tenant), at) == muted {
            return Ok(false);
        }
        let held = self.tenancy_mut(tenant)?;
        if muted {
            if !fallible::reserve(&mut held.muted, 1) {
                return Err(Errno::OutOfMemory);
            }
            held.muted.push(at);
        } else {
            held.muted.retain(|place| *place != at);
        }
        Ok(true)
    }

    /// Make `at` the endpoint `tenant` prefers as `direction`'s default,
    /// answering whether its preference moved.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the tenancy cannot be recorded.
    pub(crate) fn prefer(
        &mut self,
        tenant: Tenant,
        direction: StreamDirection,
        at: AudioLocation,
    ) -> Result<bool, Errno> {
        let held = self.tenancy_mut(tenant)?;
        let preference = &mut held.prefer[direction_slot(direction)];
        let moved = *preference != Some(at);
        *preference = Some(at);
        Ok(moved)
    }

    /// Forget every session's controls `keep` refuses: nothing could be heard
    /// at them. The unclaimed room's are the machine's and are always kept.
    pub(crate) fn retain(&mut self, keep: impl Fn(ProcId) -> bool) {
        self.tenancies.retain(|held| match held.tenant {
            Tenant::Unclaimed => true,
            Tenant::Session(session) => keep(session),
        });
    }

    fn tenancy(&self, tenant: Option<Tenant>) -> Option<&Tenancy> {
        let tenant = tenant?;
        self.tenancies.iter().find(|held| held.tenant == tenant)
    }

    fn tenancy_mut(&mut self, tenant: Tenant) -> Result<&mut Tenancy, Errno> {
        if let Some(index) = self.tenancies.iter().position(|held| held.tenant == tenant) {
            return Ok(&mut self.tenancies[index]);
        }
        if !fallible::reserve(&mut self.tenancies, 1) {
            return Err(Errno::OutOfMemory);
        }
        self.tenancies.push(Tenancy {
            tenant,
            levels: Vec::new(),
            muted: Vec::new(),
            prefer: [None; 2],
        });
        self.tenancies.last_mut().ok_or(Errno::OutOfMemory)
    }
}

#[cfg(test)]
#[path = "controls_tests.rs"]
mod tests;
