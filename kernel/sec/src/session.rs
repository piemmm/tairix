//! Sessions: the sets of processes that end together
//! (`docs/src/architecture/sessions.md`).
//!
//! A session is anchored at one process and ends when that process dies:
//! the kernel then ends every member of it and of every session nested in
//! it. A new session nests inside the one anchored at the process that asked
//! for it, founded around that spawner on first use, so a spawner contains
//! everything it starts and founding a session needs no authority. The root
//! session holds the processes the kernel admits itself; it is anchored at
//! nothing and never ends.
//!
//! [`SessionTree`] is the bookkeeping behind that. Each member is indexed
//! under its own session *and every ancestor of it*, so ending a session is
//! an ordered walk of one range, a containment check is one lookup, and a
//! join or a departure costs a chain walk bounded by [`SESSION_DEPTH_MAX`].
//! No process ever changes session: membership is fixed at admission and
//! released at teardown.

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::Bound;

use tairix_abi::ProcId;

use crate::captable::ProcessId;

/// The deepest a session may nest below the root.
///
/// A containment bound, not a capacity: a real machine nests a handful of
/// levels (init's services, a login, a desktop, a terminal, its shell), and
/// every join, departure and ending check walks the chain, so this is what
/// keeps each of them constant-cost however a principal nests its children.
pub const SESSION_DEPTH_MAX: u8 = 16;

/// The session the kernel's own admissions belong to — PID 1 and the drivers
/// it loads. Anchored at no process, never ending, and not indexed.
pub const ROOT_SESSION: ProcId = ProcId::KERNEL;

/// Where an admission places its process, resolved against the spawner by
/// [`crate::CapTable::resolve_placement`].
///
/// `anchor` names the spawner and `parent` its own session: the session
/// anchored at the spawner is founded inside `parent` the first time either
/// form below needs it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Placement {
    /// Join this existing session (the root included).
    Join(ProcId),
    /// Join the session anchored at `anchor`.
    Anchored {
        /// The spawner.
        anchor: ProcId,
        /// The spawner's own session.
        parent: ProcId,
    },
    /// Found a session anchored at the admitted process inside the one
    /// anchored at `anchor` — or inside the root, for the kernel, which
    /// anchors nothing.
    Found {
        /// The spawner.
        anchor: ProcId,
        /// The spawner's own session.
        parent: ProcId,
    },
}

/// What a placement changes, decided before anything does.
#[derive(Copy, Clone, Debug)]
struct Plan {
    /// The spawner's session, `(anchor, parent)`, when it must be founded
    /// first.
    container: Option<(ProcId, ProcId)>,
    /// The session joined, or the one a session of the process's own nests in.
    session: ProcId,
    /// Whether the process founds a session of its own.
    founds: bool,
}

/// Why a process could not be placed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PlacementError {
    /// The session the process would join, or one enclosing it, is ending.
    Ending,
    /// A join named no live process within the spawner's own session, or a
    /// session was asked to be anchored at no process.
    NotFound,
    /// The session would nest deeper than [`SESSION_DEPTH_MAX`].
    TooDeep,
}

/// One live, non-root session.
#[derive(Copy, Clone, Debug)]
struct Node {
    parent: ProcId,
    depth: u8,
    /// Set once the anchor has died; nothing joins an ending session.
    ending: bool,
}

/// The session tree.
#[derive(Debug, Default)]
pub struct SessionTree {
    nodes: BTreeMap<ProcId, Node>,
    /// `(session, member)` for every member of every non-root session and each
    /// of its ancestors, ordered so one session's members are one range.
    members: BTreeSet<(ProcId, ProcessId)>,
}

impl SessionTree {
    /// An empty tree: every process is in the root session.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
            members: BTreeSet::new(),
        }
    }

    fn depth(&self, session: ProcId) -> Option<u8> {
        if session == ROOT_SESSION {
            Some(0)
        } else {
            self.nodes.get(&session).map(|node| node.depth)
        }
    }

    /// Whether `session` or any session enclosing it is ending. A session the
    /// tree no longer holds has no members left to join.
    fn closed(&self, session: ProcId) -> bool {
        let mut at = session;
        while at != ROOT_SESSION {
            match self.nodes.get(&at) {
                Some(node) if !node.ending => at = node.parent,
                _ => return true,
            }
        }
        false
    }

    /// Whether `member` lies within `session`: in it, or in a session nested
    /// in it. Everything lies within the root.
    #[must_use]
    pub fn contains(&self, session: ProcId, member: ProcessId) -> bool {
        session == ROOT_SESSION || self.members.contains(&(session, member))
    }

    /// Whether a session enclosing `session` — not `session` itself — is
    /// ending, so its end reaches every member of this one.
    #[must_use]
    pub fn enclosing_ending(&self, session: ProcId) -> bool {
        let Some(node) = self.nodes.get(&session) else {
            return false;
        };
        let mut at = node.parent;
        while at != ROOT_SESSION {
            match self.nodes.get(&at) {
                Some(node) if node.ending => return true,
                Some(node) => at = node.parent,
                None => return false,
            }
        }
        false
    }

    /// Whether `session` is ending with members still in it.
    #[must_use]
    pub fn is_ending(&self, session: ProcId) -> bool {
        self.nodes.get(&session).is_some_and(|node| node.ending)
    }

    /// Check that a process could be placed as `placement` asks now.
    ///
    /// # Errors
    ///
    /// The [`PlacementError`] placing it would raise.
    pub fn check(&self, placement: Placement) -> Result<(), PlacementError> {
        self.plan(placement).map(|_| ())
    }

    /// Place `member`, instance `instance`, as `placement` asks, returning the
    /// session it is now in.
    ///
    /// # Errors
    ///
    /// The [`PlacementError`] the checks name; nothing changes on a refusal.
    pub fn place(
        &mut self,
        member: ProcessId,
        instance: ProcId,
        placement: Placement,
    ) -> Result<ProcId, PlacementError> {
        let plan = self.plan(placement)?;
        if plan.founds && instance.is_kernel() {
            return Err(PlacementError::NotFound);
        }
        if let Some((anchor, parent)) = plan.container {
            self.insert_node(anchor, parent);
        }
        let session = if plan.founds {
            self.insert_node(instance, plan.session);
            instance
        } else {
            plan.session
        };
        let mut at = session;
        while let Some(node) = self.nodes.get(&at) {
            self.members.insert((at, member));
            at = node.parent;
        }
        Ok(session)
    }

    fn plan(&self, placement: Placement) -> Result<Plan, PlacementError> {
        match placement {
            Placement::Join(session) => {
                self.check_join(session)?;
                Ok(Plan {
                    container: None,
                    session,
                    founds: false,
                })
            }
            Placement::Anchored { anchor, parent } => {
                if anchor.is_kernel() {
                    return Err(PlacementError::NotFound);
                }
                let (_, container) = self.container(anchor, parent)?;
                Ok(Plan {
                    container,
                    session: anchor,
                    founds: false,
                })
            }
            Placement::Found { anchor, parent } => {
                let (session, depth, container) = if anchor.is_kernel() {
                    (ROOT_SESSION, 0, None)
                } else {
                    let (depth, container) = self.container(anchor, parent)?;
                    (anchor, depth, container)
                };
                if depth >= SESSION_DEPTH_MAX {
                    return Err(PlacementError::TooDeep);
                }
                Ok(Plan {
                    container,
                    session,
                    founds: true,
                })
            }
        }
    }

    /// The depth of the session anchored at `anchor`, and `(anchor, parent)`
    /// when it must first be founded inside `parent`.
    fn container(
        &self,
        anchor: ProcId,
        parent: ProcId,
    ) -> Result<(u8, Option<(ProcId, ProcId)>), PlacementError> {
        if let Some(node) = self.nodes.get(&anchor) {
            self.check_join(anchor)?;
            return Ok((node.depth, None));
        }
        self.check_join(parent)?;
        match self.depth(parent) {
            Some(depth) if depth < SESSION_DEPTH_MAX => Ok((depth + 1, Some((anchor, parent)))),
            _ => Err(PlacementError::TooDeep),
        }
    }

    fn check_join(&self, session: ProcId) -> Result<(), PlacementError> {
        if self.closed(session) {
            Err(PlacementError::Ending)
        } else {
            Ok(())
        }
    }

    fn insert_node(&mut self, session: ProcId, parent: ProcId) {
        let depth = self.depth(parent).map_or(1, |depth| depth + 1);
        self.nodes.insert(
            session,
            Node {
                parent,
                depth,
                ending: false,
            },
        );
    }

    /// Release `member` from `session` and every session enclosing it,
    /// dropping each session left with no member, then mark the session
    /// anchored at `instance` ending if members remain in it.
    ///
    /// The one departure every death takes, so an anchor's end can never be
    /// missed and no emptied session is left behind.
    pub fn depart(&mut self, member: ProcessId, instance: ProcId, session: ProcId) {
        let mut at = session;
        while let Some(node) = self.nodes.get(&at).copied() {
            self.members.remove(&(at, member));
            if self.members_after(at, None).next().is_none() {
                self.nodes.remove(&at);
            }
            at = node.parent;
        }
        if let Some(node) = self.nodes.get_mut(&instance) {
            node.ending = true;
        }
    }

    /// The members of `session` and of every session nested in it, in
    /// ascending order, after `after` when one is given — a cursor, so a walk
    /// can release the table between batches and resume where it stopped.
    pub fn members_after(
        &self,
        session: ProcId,
        after: Option<ProcessId>,
    ) -> impl Iterator<Item = ProcessId> + '_ {
        let lower = after.map_or(Bound::Included((session, ProcessId(0))), |member| {
            Bound::Excluded((session, member))
        });
        self.members
            .range((lower, Bound::Included((session, ProcessId(u64::MAX)))))
            .map(|&(_, member)| member)
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
