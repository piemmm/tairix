//! Controlling-terminal (foreground) ownership: the one state machine that
//! decides which task may drain a terminal's input and receive its cooked-mode
//! `^C`/`^Z` job-control signals (`plans/DISPLAY.md` D5).
//!
//! A *terminal* here is either the kernel console device
//! ([`crate::console::ConsoleDevice`]) or a pseudo-terminal
//! ([`crate::pty::Pty`]); both carry exactly one [`ForegroundOwnership`] and
//! share **this** definition of the ownership rules rather than each keeping a
//! private copy. The `console_foreground` syscall routes to whichever backing a
//! caller's standard stream resolves to.
//!
//! # The rules
//!
//! Ownership only ever moves *down* a spawn chain — inherited and intersected,
//! never widened — because its caller authorises the new owner as a child of
//! the granter, or as the granter itself, before ever reaching here. Within
//! that, a transition is permitted only from a position of authority over the
//! slot: the terminal is unowned, or the caller is the recorded granter
//! (re-targeting between its own children, or taking the terminal back), or the
//! caller is the current owner (delegating onward to its own child). Anyone
//! else is refused, so a background task can never take the drain right, and
//! every path fails closed ([`Errno::NotForeground`]).
//!
//! Every party — owner, granter, caller — is a [`ProcessInstance`], so a
//! process that draws a dead party's number is a stranger to the terminal.
//!
//! An owner the terminal was handed to is its **job**: the cooked-mode `^C`/`^Z`
//! signals reach a job and nothing else. A process holding its own terminal —
//! the shell at its prompt — is no job, and reads those bytes.
//!
//! # Watching it change hands
//!
//! Every change of hands moves a generation, read without the lock, which a
//! `Foreground` wait-set member watches; the change itself reports that it
//! happened, so the terminal holding this can wake its parked readers to
//! re-check the gate.
//!
//! # Interrupt safety
//!
//! The console input filter reads the owner from the UART receive interrupt
//! handler, so the state sits behind an [`IrqSafeSpinLock`]: the hold masks the
//! holding CPU, and every critical section here is a handful of field accesses
//! that call nothing, so a handler can never find the lock held by the task it
//! interrupted and no lock order can invert through it.

use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::{Errno, ProcId};
use tairix_kernel_sec::ProcessId;
use tairix_sync::IrqSafeSpinLock;

/// A process, and *which instance of it*.
///
/// A pid alone does not name a process for longer than that process lives —
/// task ids are drawn at random, and an id whose task is gone may be drawn
/// again — so the ownership names each party by the instance that held the pid
/// when it acted, and a later holder of the number matches none of them.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct ProcessInstance {
    /// The process.
    pub process: ProcessId,
    /// The instance [`Self::process`] named. [`ProcId::KERNEL`] for a
    /// principal the capability table holds no distinct process instance for.
    pub instance: ProcId,
}

/// A held ownership: who owns the terminal, and who handed it to them.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct Ownership {
    owner: ProcessInstance,
    /// Only it or the owner itself may release or re-target, so a background
    /// task can never steal the drain right by clearing the slot.
    granter: ProcessInstance,
}

impl Ownership {
    /// Whether `caller` may hand this ownership on or clear it: it is the
    /// owner, or the task that granted it.
    fn authorises(self, caller: ProcessInstance) -> bool {
        self.owner == caller || self.granter == caller
    }
}

/// What one [`ForegroundOwnership::grant`] did: the ownership it made and the
/// one it replaced, so it can be undone exactly.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct HandOver {
    made: Ownership,
    replaced: Option<Ownership>,
}

impl HandOver {
    /// Whether the grant changed hands.
    #[must_use]
    pub fn moved(self) -> bool {
        self.replaced != Some(self.made)
    }
}

/// The controlling (foreground) ownership of one terminal.
pub struct ForegroundOwnership {
    state: IrqSafeSpinLock<Option<Ownership>>,
    generation: AtomicU64,
}

impl Default for ForegroundOwnership {
    fn default() -> Self {
        Self::new()
    }
}

impl ForegroundOwnership {
    /// A fresh, unowned ownership.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: IrqSafeSpinLock::new(None),
            generation: AtomicU64::new(0),
        }
    }

    /// Record a change of hands; `changed` passes through, so a transition
    /// reads as its own report.
    fn moved(&self, changed: bool) -> bool {
        if changed {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        changed
    }

    /// Whether `caller` may hand the terminal over now, as [`Self::grant`]
    /// would let it.
    ///
    /// The answer can be overtaken by a grant another authorised task makes
    /// before `caller` acts on it, so it only ever lets a caller refuse early;
    /// the grant decides.
    #[must_use]
    pub fn may_grant(&self, caller: ProcessInstance) -> bool {
        self.state.lock().is_none_or(|held| held.authorises(caller))
    }

    /// Hand the terminal's controlling ownership to `owner`, recording `caller`
    /// as the granter, and answer what changed. An `owner` that is `caller`
    /// itself holds the terminal as its own, not as a job.
    ///
    /// Permitted only from a position of authority over the slot: the terminal
    /// is unowned, `caller` is the current owner (delegating onward), or
    /// `caller` is the recorded granter (re-targeting between its own children,
    /// or taking the terminal back).
    ///
    /// # Errors
    ///
    /// [`Errno::NotForeground`] when another task's ownership is in place and
    /// `caller` is neither its granter nor the owner.
    pub fn grant(
        &self,
        caller: ProcessInstance,
        owner: ProcessInstance,
    ) -> Result<HandOver, Errno> {
        let mut state = self.state.lock();
        if state.is_some_and(|held| !held.authorises(caller)) {
            return Err(Errno::NotForeground);
        }
        let made = Ownership {
            owner,
            granter: caller,
        };
        let grant = HandOver {
            made,
            replaced: state.replace(made),
        };
        self.moved(grant.moved());
        Ok(grant)
    }

    /// Put back the ownership `grant` replaced, unless the terminal has changed
    /// hands since, and answer whether it changed hands now.
    pub fn undo(&self, grant: HandOver) -> bool {
        let mut state = self.state.lock();
        let untouched = *state == Some(grant.made);
        if untouched {
            *state = grant.replaced;
        }
        self.moved(untouched && grant.moved())
    }

    /// Release the terminal's foreground ownership, returning it to the open,
    /// unowned state.
    ///
    /// Only the recorded granter or the owner itself may release; anyone else
    /// is refused, so a background task cannot open the terminal by clearing the
    /// slot and then draining it. Releasing an already-unowned terminal is an
    /// idempotent success: the granter legitimately clears after its child
    /// exited (the exit path already cleared the slot), and there is nothing an
    /// unauthorised caller could gain from the no-op.
    ///
    /// Answers whether it changed hands.
    ///
    /// # Errors
    ///
    /// [`Errno::NotForeground`] when another task's ownership is in place and
    /// `caller` is neither its granter nor the owner.
    pub fn release(&self, caller: ProcessInstance) -> Result<bool, Errno> {
        let mut state = self.state.lock();
        let Some(held) = *state else {
            return Ok(false);
        };
        if !held.authorises(caller) {
            return Err(Errno::NotForeground);
        }
        *state = None;
        Ok(self.moved(true))
    }

    /// Clear the slot if `dead` owns it.
    ///
    /// The exit path calls this for every console when a task ends, and the
    /// read gate calls it when it proves a recorded owner dead, so a terminal is
    /// never wedged behind a task that can no longer read it. A slot naming any
    /// other party — a later holder of `dead`'s number included — is left
    /// untouched. Answers whether it changed hands.
    pub fn clear_dead(&self, dead: ProcessInstance) -> bool {
        let mut state = self.state.lock();
        let owned = state.is_some_and(|held| held.owner == dead);
        if owned {
            *state = None;
        }
        self.moved(owned)
    }

    /// The terminal's current controlling (foreground) owner, if any.
    #[must_use]
    pub fn current(&self) -> Option<ProcessInstance> {
        self.state.lock().map(|held| held.owner)
    }

    /// The job the terminal was handed to: its owner, unless the owner holds
    /// it as its own.
    #[must_use]
    pub fn job(&self) -> Option<ProcessInstance> {
        self.state
            .lock()
            .filter(|held| held.owner != held.granter)
            .map(|held| held.owner)
    }

    /// Whether `party` owns the terminal.
    #[must_use]
    pub fn holds(&self, party: ProcessInstance) -> bool {
        self.state.lock().is_some_and(|held| held.owner == party)
    }

    /// The count of changes of hands so far.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process `process` at instance `instance`, so a test can tell two
    /// occupants of one pid apart the way the ownership does.
    fn party(process: u64, instance: u8) -> ProcessInstance {
        ProcessInstance {
            process: ProcessId(process),
            instance: ProcId::from_raw([instance; 16]),
        }
    }

    /// Process `n` at the one instance a test gives it.
    fn p(n: u8) -> ProcessInstance {
        party(u64::from(n), n)
    }

    fn granted(fg: &ForegroundOwnership, caller: ProcessInstance, owner: ProcessInstance) -> bool {
        fg.grant(caller, owner).is_ok_and(HandOver::moved)
    }

    #[test]
    fn a_fresh_ownership_is_unowned() {
        let fg = ForegroundOwnership::new();
        assert_eq!(fg.current(), None);
    }

    #[test]
    fn granting_an_unowned_terminal_records_owner_and_granter() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(2)));
        assert_eq!(fg.current(), Some(p(2)));
        assert_eq!(fg.job(), Some(p(2)));
    }

    #[test]
    fn the_granter_can_retarget_between_its_children() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(2)));
        assert!(granted(&fg, p(1), p(3)));
        assert_eq!(fg.current(), Some(p(3)));
    }

    #[test]
    fn the_owner_can_delegate_onward() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(2)));
        // The owner (2) delegates to its own child (4); 2 becomes the granter.
        assert!(granted(&fg, p(2), p(4)));
        assert_eq!(fg.current(), Some(p(4)));
        // …and can re-target as the granter now.
        assert!(granted(&fg, p(2), p(5)));
        assert_eq!(fg.current(), Some(p(5)));
    }

    #[test]
    fn a_bystander_cannot_take_or_retarget_the_ownership() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(2)));
        assert_eq!(fg.grant(p(9), p(9)), Err(Errno::NotForeground));
        assert_eq!(fg.current(), Some(p(2)));
    }

    /// A process that drew a party's number after the party died is a
    /// stranger: it can neither act as the owner nor as the granter, nor
    /// read as holding the terminal.
    #[test]
    fn a_later_holder_of_a_partys_number_is_a_stranger() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, party(1, 0x11), party(2, 0x21)));
        let (owner_successor, granter_successor) = (party(2, 0x22), party(1, 0x12));
        for successor in [owner_successor, granter_successor] {
            assert!(!fg.may_grant(successor));
            assert_eq!(fg.grant(successor, successor), Err(Errno::NotForeground));
            assert_eq!(fg.release(successor), Err(Errno::NotForeground));
            assert!(!fg.holds(successor));
            assert!(!fg.clear_dead(successor));
        }
        assert_eq!(fg.current(), Some(party(2, 0x21)));
        assert!(fg.holds(party(2, 0x21)));
    }

    /// The early answer agrees with the grant it predicts, for every position
    /// a caller can stand in.
    #[test]
    fn may_grant_answers_as_the_grant_would() {
        let fg = ForegroundOwnership::new();
        assert!(fg.may_grant(p(9)), "anyone may take an unowned terminal");
        assert!(granted(&fg, p(1), p(2)));
        assert!(fg.may_grant(p(1)), "the granter");
        assert!(fg.may_grant(p(2)), "the owner");
        assert!(!fg.may_grant(p(9)), "a bystander");
        assert_eq!(fg.grant(p(9), p(9)), Err(Errno::NotForeground));
    }

    #[test]
    fn a_bystander_cannot_release_the_ownership() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(2)));
        assert_eq!(fg.release(p(9)), Err(Errno::NotForeground));
        assert_eq!(fg.current(), Some(p(2)));
    }

    #[test]
    fn the_owner_and_the_granter_can_each_release() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(2)));
        assert_eq!(fg.release(p(2)), Ok(true));
        assert_eq!(fg.current(), None);

        assert!(granted(&fg, p(1), p(2)));
        assert_eq!(fg.release(p(1)), Ok(true));
        assert_eq!(fg.current(), None);
    }

    #[test]
    fn releasing_an_unowned_terminal_is_an_idempotent_success() {
        let fg = ForegroundOwnership::new();
        assert_eq!(fg.release(p(9)), Ok(false));
        assert_eq!(fg.current(), None);
    }

    #[test]
    fn clear_dead_clears_only_the_matching_owner() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(7)));
        // A different task ending leaves the slot untouched.
        assert!(!fg.clear_dead(p(9)));
        assert_eq!(fg.current(), Some(p(7)));
        // The owner ending clears it.
        assert!(fg.clear_dead(p(7)));
        assert_eq!(fg.current(), None);
    }

    #[test]
    fn a_process_holding_its_own_terminal_is_no_job() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(1)));
        assert!(fg.holds(p(1)));
        assert_eq!(fg.job(), None, "the shell at its prompt takes no signals");
        assert!(granted(&fg, p(1), p(2)));
        assert_eq!(fg.job(), Some(p(2)), "a handed-over terminal has a job");
        assert!(!fg.holds(p(1)));
        assert!(granted(&fg, p(1), p(1)), "the granter takes it back");
        assert_eq!(fg.job(), None);
    }

    /// An undone grant puts back exactly what it replaced — an open terminal
    /// stays open, a held one returns to its holder — and moves the generation
    /// only when it changes hands.
    #[test]
    fn an_undone_grant_restores_what_it_replaced() {
        let fg = ForegroundOwnership::new();
        let opened = fg.grant(p(1), p(2)).expect("open terminal");
        let at = fg.generation();
        assert!(fg.undo(opened));
        assert_eq!(fg.current(), None, "an open terminal stays open");
        assert_eq!(fg.generation(), at + 1);

        assert!(granted(&fg, p(1), p(1)));
        let handed = fg.grant(p(1), p(3)).expect("the holder hands on");
        assert!(fg.undo(handed));
        assert_eq!(fg.current(), Some(p(1)));
        assert_eq!(fg.job(), None, "back with its holder, not a job");

        let same = fg.grant(p(1), p(1)).expect("the same hands");
        let at = fg.generation();
        assert!(!fg.undo(same), "nothing changed hands to undo");
        assert_eq!(fg.generation(), at);
        assert_eq!(fg.current(), Some(p(1)));
    }

    /// An undo never takes back a change of hands made after the grant it
    /// names: that ownership is its maker's to keep.
    #[test]
    fn an_undo_leaves_a_later_change_of_hands_alone() {
        let fg = ForegroundOwnership::new();
        assert!(granted(&fg, p(1), p(1)));
        let handed = fg.grant(p(1), p(2)).expect("the holder hands on");
        assert!(granted(&fg, p(1), p(3)), "the granter re-targets");
        let at = fg.generation();
        assert!(!fg.undo(handed));
        assert_eq!(fg.current(), Some(p(3)));
        assert_eq!(fg.generation(), at);
        assert_eq!(fg.release(p(1)), Ok(true));
        assert!(!fg.undo(handed), "nor an open terminal");
        assert_eq!(fg.current(), None);
    }

    #[test]
    fn every_change_of_hands_moves_the_generation_and_nothing_else_does() {
        let fg = ForegroundOwnership::new();
        let start = fg.generation();
        assert!(granted(&fg, p(1), p(2)));
        assert_eq!(fg.generation(), start + 1);
        assert_eq!(
            fg.grant(p(1), p(2)).map(HandOver::moved),
            Ok(false),
            "the same hands again"
        );
        assert_eq!(fg.generation(), start + 1);
        assert_eq!(fg.grant(p(9), p(9)), Err(Errno::NotForeground));
        assert!(!fg.clear_dead(p(9)));
        assert_eq!(
            fg.generation(),
            start + 1,
            "a refusal and a stranger's death move nothing"
        );
        assert!(fg.clear_dead(p(2)));
        assert_eq!(fg.generation(), start + 2);
        assert_eq!(fg.release(p(1)), Ok(false), "already open");
        assert_eq!(fg.generation(), start + 2);
        assert!(granted(&fg, p(1), p(1)));
        assert_eq!(fg.release(p(1)), Ok(true));
        assert_eq!(fg.generation(), start + 4);
    }
}
