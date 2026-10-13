//! Side-channel mitigation surface of the Arch HAL.
//!
//! Microarchitectural side channels (Meltdown, Spectre, MDS, L1TF, MMIO
//! stale data) are defeated by mechanisms only an architecture port can
//! emit, each at one privilege boundary: kernel/user address-space
//! separation, speculation hardening on every user→kernel entry, a buffer
//! clear and a speculation barrier on every return to user mode, and an
//! indirect-branch-predictor barrier on context switch.
//!
//! The mechanisms live in each port's entry and return code, not behind
//! a callable method: their position relative to the transition
//! instruction is what makes them a defence (a buffer clear must be the
//! last kernel work before the return, a straight-line-speculation barrier
//! must follow it), and no Rust call can stand there. What this module
//! holds is the port's honest declaration of which mechanisms it applies,
//! [`MitigationProfile`], and the [`conformance`] check that every
//! omission is justified. Each port's own tests pin every mechanism it
//! declares [`Mitigation::Applied`] onto every entry and return path in
//! its sources, so a declaration cannot outlive its code.

/// One mitigation's status on a given architecture port.
///
/// A port takes exactly one honest position per mitigation. A no-op
/// ([`Mitigation::NotVulnerable`]) is permitted only where the silicon is
/// provably not exposed to the class, and carries the justification the
/// port also records in its `README.md`. [`Mitigation::Pending`] is a
/// mitigation the silicon requires but the port does not yet build; it is
/// honest but not release-ready.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mitigation {
    /// The port applies this mitigation on every path it covers.
    Applied,
    /// Deliberately omitted because the silicon is provably not exposed to
    /// the class it defends against. The payload is the justification; it
    /// must be non-empty.
    NotVulnerable(&'static str),
    /// Required by the silicon but not yet built. The payload names what
    /// it waits on; it must be non-empty.
    Pending(&'static str),
}

impl Mitigation {
    /// `true` if this mitigation is [`Mitigation::Applied`].
    #[must_use]
    pub const fn is_applied(self) -> bool {
        matches!(self, Self::Applied)
    }

    /// `true` if this mitigation is a tracked [`Mitigation::Pending`].
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Pending(_))
    }

    /// `true` if this mitigation is applied or a justified
    /// [`Mitigation::NotVulnerable`].
    #[must_use]
    pub const fn is_release_ready(self) -> bool {
        matches!(self, Self::Applied | Self::NotVulnerable(_))
    }

    /// The justification or tracking note of a non-applied decision, or
    /// `None` when applied.
    #[must_use]
    pub const fn detail(self) -> Option<&'static str> {
        match self {
            Self::Applied => None,
            Self::NotVulnerable(reason) | Self::Pending(reason) => Some(reason),
        }
    }
}

/// A port's honest declaration of the mitigations it applies.
///
/// Every field takes a position; there is no "undecided".
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MitigationProfile {
    /// The kernel is not mapped in the translation root active in user
    /// mode (KPTI-equivalent), defeating Meltdown-class cross-privilege
    /// reads.
    pub address_space_isolation: Mitigation,
    /// Speculation hardening at every user→kernel entry — syscall,
    /// exception and interrupt alike: no register still holds a value the
    /// user chose once the entry has saved it, so a speculatively executed
    /// kernel gadget cannot consume one, and any privilege-dependent switch
    /// of kernel state the entry performs is fenced against a mispredicted
    /// direction.
    pub user_entry_hardening: Mitigation,
    /// Microarchitectural buffers (MDS, TAA, MMIO stale data) are
    /// overwritten as the last kernel work before every return to user
    /// mode. Every context switch to a user task ends in such a return, so
    /// neither the kernel's data nor a previous task's can be sampled.
    pub user_return_buffer_clear: Mitigation,
    /// A speculation barrier follows every return-to-user instruction, so
    /// straight-line speculation past the return cannot execute the kernel
    /// bytes behind it with the user's registers loaded.
    pub user_return_speculation_barrier: Mitigation,
    /// Indirect-branch-predictor barrier (IBPB-equivalent) on context
    /// switch, defeating cross-task branch-target injection.
    pub context_switch_indirect_branch_barrier: Mitigation,
}

/// A single named slot of a [`MitigationProfile`], yielded by
/// [`MitigationProfile::entries`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MitigationEntry {
    /// Stable name of the slot.
    pub name: &'static str,
    /// The port's decision for this slot.
    pub mitigation: Mitigation,
}

/// Reason a [`MitigationProfile`] failed [`MitigationProfile::validate`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProfileError {
    /// A non-applied decision carried an empty (or whitespace-only)
    /// explanation.
    EmptyJustification {
        /// The [`MitigationEntry::name`] of the unjustified slot.
        field: &'static str,
    },
}

impl MitigationProfile {
    /// The five slots, in a stable order, each paired with its name.
    #[must_use]
    pub const fn entries(&self) -> [MitigationEntry; 5] {
        [
            MitigationEntry {
                name: "address_space_isolation",
                mitigation: self.address_space_isolation,
            },
            MitigationEntry {
                name: "user_entry_hardening",
                mitigation: self.user_entry_hardening,
            },
            MitigationEntry {
                name: "user_return_buffer_clear",
                mitigation: self.user_return_buffer_clear,
            },
            MitigationEntry {
                name: "user_return_speculation_barrier",
                mitigation: self.user_return_speculation_barrier,
            },
            MitigationEntry {
                name: "context_switch_indirect_branch_barrier",
                mitigation: self.context_switch_indirect_branch_barrier,
            },
        ]
    }

    /// Check that every non-applied slot carries a non-empty explanation.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::EmptyJustification`] naming the first slot
    /// whose [`Mitigation::detail`] is empty or whitespace-only.
    pub fn validate(&self) -> Result<(), ProfileError> {
        for entry in self.entries() {
            if let Some(reason) = entry.mitigation.detail() {
                if reason.trim().is_empty() {
                    return Err(ProfileError::EmptyJustification { field: entry.name });
                }
            }
        }
        Ok(())
    }

    /// `true` if no slot is [`Mitigation::Pending`]: the release gate a
    /// port must pass before it ships, stricter than [`Self::validate`].
    #[must_use]
    pub fn is_release_ready(&self) -> bool {
        self.entries()
            .iter()
            .all(|entry| entry.mitigation.is_release_ready())
    }
}

/// The side-channel declaration an architecture port exposes.
pub trait SideChannelMitigation: Send + Sync {
    /// The port's honest declaration. Must satisfy
    /// [`MitigationProfile::validate`].
    fn profile(&self) -> MitigationProfile;
}

/// The side-channel conformance vertical every port runs over its handle.
pub mod conformance {
    use super::SideChannelMitigation;

    /// Run the side-channel conformance suite against `port`.
    ///
    /// # Panics
    ///
    /// Panics (failing the test) if the profile leaves a non-applied slot
    /// unexplained.
    pub fn run_all<M: SideChannelMitigation + ?Sized>(port: &M) {
        let profile = port.profile();
        assert!(
            profile.validate().is_ok(),
            "side-channel profile must justify every omitted mitigation (AGENTS.md §19.1): {:?}",
            profile.validate()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape a port on fully exposed silicon declares.
    fn all_applied() -> MitigationProfile {
        MitigationProfile {
            address_space_isolation: Mitigation::Applied,
            user_entry_hardening: Mitigation::Applied,
            user_return_buffer_clear: Mitigation::Applied,
            user_return_speculation_barrier: Mitigation::Applied,
            context_switch_indirect_branch_barrier: Mitigation::Applied,
        }
    }

    struct StubPort {
        profile: MitigationProfile,
    }

    impl SideChannelMitigation for StubPort {
        fn profile(&self) -> MitigationProfile {
            self.profile
        }
    }

    #[test]
    fn applied_profile_validates() {
        assert_eq!(all_applied().validate(), Ok(()));
    }

    #[test]
    fn justified_omission_validates() {
        let mut p = all_applied();
        p.user_entry_hardening = Mitigation::NotVulnerable("no speculative execution on this core");
        assert_eq!(p.validate(), Ok(()));
    }

    #[test]
    fn empty_justification_is_rejected() {
        let mut p = all_applied();
        p.user_return_buffer_clear = Mitigation::NotVulnerable("   ");
        assert_eq!(
            p.validate(),
            Err(ProfileError::EmptyJustification {
                field: "user_return_buffer_clear"
            })
        );
    }

    #[test]
    fn validate_reports_the_first_offending_slot() {
        let mut p = all_applied();
        p.address_space_isolation = Mitigation::NotVulnerable("");
        p.user_return_speculation_barrier = Mitigation::NotVulnerable("");
        assert_eq!(
            p.validate(),
            Err(ProfileError::EmptyJustification {
                field: "address_space_isolation"
            })
        );
    }

    #[test]
    fn entries_round_trip_the_named_slots() {
        let p = all_applied();
        let names: [&str; 5] = core::array::from_fn(|i| p.entries()[i].name);
        assert_eq!(
            names,
            [
                "address_space_isolation",
                "user_entry_hardening",
                "user_return_buffer_clear",
                "user_return_speculation_barrier",
                "context_switch_indirect_branch_barrier",
            ]
        );
    }

    #[test]
    fn entries_carry_each_slots_own_decision() {
        let p = MitigationProfile {
            address_space_isolation: Mitigation::Pending("a"),
            user_entry_hardening: Mitigation::NotVulnerable("b"),
            user_return_buffer_clear: Mitigation::Pending("c"),
            user_return_speculation_barrier: Mitigation::NotVulnerable("d"),
            context_switch_indirect_branch_barrier: Mitigation::Pending("e"),
        };
        let details: [Option<&str>; 5] =
            core::array::from_fn(|i| p.entries()[i].mitigation.detail());
        assert_eq!(
            details,
            [Some("a"), Some("b"), Some("c"), Some("d"), Some("e")]
        );
    }

    #[test]
    fn mitigation_helpers() {
        assert!(Mitigation::Applied.is_applied());
        assert!(!Mitigation::NotVulnerable("x").is_applied());
        assert!(Mitigation::Pending("later").is_pending());
        assert!(!Mitigation::Applied.is_pending());
        assert_eq!(Mitigation::Applied.detail(), None);
        assert_eq!(Mitigation::NotVulnerable("why").detail(), Some("why"));
        assert_eq!(Mitigation::Pending("kpti").detail(), Some("kpti"));
        assert!(Mitigation::Applied.is_release_ready());
        assert!(Mitigation::NotVulnerable("why").is_release_ready());
        assert!(!Mitigation::Pending("kpti").is_release_ready());
    }

    #[test]
    fn pending_is_honest_but_not_release_ready() {
        let mut p = all_applied();
        p.address_space_isolation = Mitigation::Pending("per-privilege translation roots");
        assert_eq!(p.validate(), Ok(()));
        assert!(!p.is_release_ready());
        assert!(all_applied().is_release_ready());
    }

    #[test]
    fn empty_pending_note_is_rejected() {
        let mut p = all_applied();
        p.address_space_isolation = Mitigation::Pending("  ");
        assert_eq!(
            p.validate(),
            Err(ProfileError::EmptyJustification {
                field: "address_space_isolation"
            })
        );
    }

    #[test]
    fn conformance_accepts_a_tracked_pending_port() {
        let mut profile = all_applied();
        profile.address_space_isolation = Mitigation::Pending("per-privilege translation roots");
        conformance::run_all(&StubPort { profile });
    }

    #[test]
    fn conformance_accepts_an_honest_port() {
        let port = StubPort {
            profile: all_applied(),
        };
        conformance::run_all(&port);
        // Object-safe: the kernel reaches the handle through `&dyn`.
        let dynamic: &dyn SideChannelMitigation = &port;
        conformance::run_all(dynamic);
    }

    #[test]
    #[should_panic(expected = "must justify every omitted mitigation")]
    fn conformance_rejects_an_unjustified_omission() {
        let mut profile = all_applied();
        profile.user_entry_hardening = Mitigation::NotVulnerable("");
        conformance::run_all(&StubPort { profile });
    }
}
