extern crate alloc;

use alloc::vec::Vec;

use tairix_abi::ProcId;

use super::{Placement, PlacementError, SessionTree, ROOT_SESSION, SESSION_DEPTH_MAX};
use crate::captable::ProcessId;

fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; 16])
}

/// A new session asked for by the anchor of `within`, which it nests in.
fn found_in(within: ProcId) -> Placement {
    Placement::Found {
        anchor: within,
        parent: within,
    }
}

/// Found a session anchored at process `pid` (instance `byte`) inside `within`.
fn found(tree: &mut SessionTree, pid: u64, byte: u8, within: ProcId) -> ProcId {
    tree.place(ProcessId(pid), instance(byte), found_in(within))
        .expect("founded")
}

fn members(tree: &SessionTree, session: ProcId) -> Vec<u64> {
    tree.members_after(session, None)
        .map(|member| member.0)
        .collect()
}

/// Whether the tree has let go of `session`: nothing can join it, and it is
/// not merely ending.
fn released(tree: &SessionTree, session: ProcId) -> bool {
    tree.check(Placement::Join(session)) == Err(PlacementError::Ending) && !tree.is_ending(session)
}

#[test]
fn a_founded_session_holds_its_anchor_and_the_root_is_never_indexed() {
    let mut tree = SessionTree::new();
    let login = found(&mut tree, 10, 1, ROOT_SESSION);
    assert_eq!(login, instance(1));
    assert_eq!(members(&tree, login), [10]);
    assert_eq!(members(&tree, ROOT_SESSION), Vec::<u64>::new());
    assert!(tree.contains(ROOT_SESSION, ProcessId(10)));
    assert_eq!(tree.check(Placement::Join(login)), Ok(()));
}

#[test]
fn a_nested_member_lies_within_every_enclosing_session() {
    let mut tree = SessionTree::new();
    let login = found(&mut tree, 10, 1, ROOT_SESSION);
    let desktop = found(&mut tree, 20, 2, login);
    tree.place(ProcessId(30), instance(3), Placement::Join(desktop))
        .expect("joined");
    assert!(tree.contains(desktop, ProcessId(30)));
    assert!(tree.contains(login, ProcessId(30)));
    assert!(!tree.contains(desktop, ProcessId(10)));
    assert_eq!(members(&tree, login), [10, 20, 30]);
    assert_eq!(members(&tree, desktop), [20, 30]);
}

#[test]
fn nothing_joins_an_ending_session_or_one_nested_in_it() {
    let mut tree = SessionTree::new();
    let login = found(&mut tree, 10, 1, ROOT_SESSION);
    let desktop = found(&mut tree, 20, 2, login);
    tree.depart(ProcessId(10), instance(1), login);
    assert!(tree.is_ending(login));
    for placement in [
        Placement::Join(login),
        Placement::Join(desktop),
        found_in(desktop),
        Placement::Anchored {
            anchor: instance(2),
            parent: login,
        },
        Placement::Found {
            anchor: instance(9),
            parent: desktop,
        },
    ] {
        assert_eq!(
            tree.place(ProcessId(40), instance(4), placement),
            Err(PlacementError::Ending)
        );
    }
    assert!(!tree.contains(login, ProcessId(40)));
}

#[test]
fn an_anchor_leaving_ends_its_session_only_while_members_remain() {
    let mut tree = SessionTree::new();
    let lone = found(&mut tree, 10, 1, ROOT_SESSION);
    tree.depart(ProcessId(10), instance(1), lone);
    assert!(released(&tree, lone), "an emptied session is dropped");

    let desktop = found(&mut tree, 20, 2, ROOT_SESSION);
    tree.place(ProcessId(30), instance(3), Placement::Join(desktop))
        .expect("joined");
    tree.depart(ProcessId(30), instance(3), desktop);
    assert!(!tree.is_ending(desktop), "a member leaving ends nothing");
    tree.place(ProcessId(31), instance(4), Placement::Join(desktop))
        .expect("joined");
    tree.depart(ProcessId(20), instance(2), desktop);
    assert!(tree.is_ending(desktop));
    assert_eq!(members(&tree, desktop), [31]);
    tree.depart(ProcessId(31), instance(4), desktop);
    assert!(released(&tree, desktop), "the last member frees it");
}

#[test]
fn an_anchored_session_is_founded_on_first_use_and_ends_with_its_anchor() {
    let mut tree = SessionTree::new();
    let shell = found(&mut tree, 10, 1, ROOT_SESSION);
    tree.place(ProcessId(20), instance(2), Placement::Join(shell))
        .expect("the desktop joins the shell's session");
    let anchored = Placement::Anchored {
        anchor: instance(2),
        parent: shell,
    };
    let apps = tree
        .place(ProcessId(30), instance(3), anchored)
        .expect("founded");
    assert_eq!(apps, instance(2));
    assert_eq!(
        tree.place(ProcessId(31), instance(4), anchored),
        Ok(apps),
        "the second joins the one already founded"
    );
    assert_eq!(members(&tree, apps), [30, 31]);
    assert!(tree.contains(shell, ProcessId(31)));
    // The desktop is not a member of what it anchors, yet its leaving ends it.
    tree.depart(ProcessId(20), instance(2), shell);
    assert!(tree.is_ending(apps));
    assert!(!tree.is_ending(shell));
}

#[test]
fn an_anchored_request_from_a_session_anchor_joins_its_own_session() {
    let mut tree = SessionTree::new();
    let desktop = found(&mut tree, 20, 2, ROOT_SESSION);
    let placed = tree.place(
        ProcessId(30),
        instance(3),
        Placement::Anchored {
            anchor: instance(2),
            parent: ROOT_SESSION,
        },
    );
    assert_eq!(placed, Ok(desktop));
    assert_eq!(members(&tree, desktop), [20, 30]);
}

#[test]
fn nesting_stops_at_the_depth_bound() {
    let mut tree = SessionTree::new();
    let mut parent = ROOT_SESSION;
    for level in 1..=SESSION_DEPTH_MAX {
        parent = found(&mut tree, u64::from(level), level, parent);
    }
    assert_eq!(
        tree.place(ProcessId(99), instance(99), found_in(parent)),
        Err(PlacementError::TooDeep)
    );
    assert_eq!(
        tree.place(ProcessId(99), instance(99), Placement::Join(parent)),
        Ok(parent),
        "joining the deepest session adds no level"
    );
}

#[test]
fn a_session_founded_around_its_spawner_counts_toward_the_bound() {
    let mut tree = SessionTree::new();
    let mut parent = ROOT_SESSION;
    for level in 1..SESSION_DEPTH_MAX {
        parent = found(&mut tree, u64::from(level), level, parent);
    }
    tree.place(ProcessId(90), instance(90), Placement::Join(parent))
        .expect("a member that anchors nothing yet");
    let around = |anchor| Placement::Found { anchor, parent };
    assert_eq!(
        tree.place(ProcessId(91), instance(91), around(instance(90))),
        Err(PlacementError::TooDeep),
        "its own session would sit one level past the bound"
    );
    assert!(released(&tree, instance(90)), "a refusal founds nothing");
    assert_eq!(
        tree.place(
            ProcessId(92),
            instance(92),
            Placement::Anchored {
                anchor: instance(90),
                parent,
            }
        ),
        Ok(instance(90)),
        "the spawner's own session still fits"
    );
}

#[test]
fn a_new_session_stays_inside_its_spawner_even_when_the_spawner_anchors_nothing() {
    let mut tree = SessionTree::new();
    let desktop = found(&mut tree, 20, 2, ROOT_SESSION);
    tree.place(ProcessId(30), instance(3), Placement::Join(desktop))
        .expect("a terminal joins the desktop");
    let shell = Placement::Found {
        anchor: instance(3),
        parent: desktop,
    };
    assert_eq!(
        tree.place(ProcessId(40), instance(4), shell),
        Ok(instance(4))
    );
    assert_eq!(
        tree.place(ProcessId(50), instance(5), shell),
        Ok(instance(5))
    );
    let terminal = instance(3);
    assert_eq!(
        members(&tree, terminal),
        [40, 50],
        "one session around the terminal"
    );
    assert_eq!(members(&tree, desktop), [20, 30, 40, 50]);
    tree.depart(ProcessId(40), instance(4), instance(4));
    assert!(
        !tree.is_ending(terminal),
        "a shell leaving ends nothing around it"
    );
    tree.depart(ProcessId(30), instance(3), desktop);
    assert!(
        tree.is_ending(terminal),
        "the terminal leaving ends every shell it started"
    );
    assert!(tree.enclosing_ending(instance(5)));
    assert!(!tree.is_ending(desktop));
    assert_eq!(
        tree.place(ProcessId(60), instance(6), shell),
        Err(PlacementError::Ending)
    );
}

#[test]
fn the_kernel_sentinel_founds_and_anchors_nothing() {
    let mut tree = SessionTree::new();
    assert_eq!(
        tree.place(ProcessId(1), ProcId::KERNEL, found_in(ROOT_SESSION)),
        Err(PlacementError::NotFound)
    );
    assert_eq!(
        tree.place(ProcessId(3), instance(3), found_in(ROOT_SESSION)),
        Ok(instance(3)),
        "the kernel's new session nests in the root"
    );
    assert_eq!(members(&tree, instance(3)), [3]);
    assert_eq!(
        tree.place(
            ProcessId(2),
            instance(2),
            Placement::Anchored {
                anchor: ProcId::KERNEL,
                parent: ROOT_SESSION,
            }
        ),
        Err(PlacementError::NotFound)
    );
}

#[test]
fn only_an_enclosing_ending_session_is_reported_as_enclosing() {
    let mut tree = SessionTree::new();
    let login = found(&mut tree, 10, 1, ROOT_SESSION);
    let desktop = found(&mut tree, 20, 2, login);
    tree.place(ProcessId(30), instance(3), Placement::Join(desktop))
        .expect("joined");
    tree.depart(ProcessId(20), instance(2), desktop);
    assert!(tree.is_ending(desktop));
    assert!(
        !tree.enclosing_ending(desktop),
        "its own end is not enclosing"
    );
    let mut tree = SessionTree::new();
    let login = found(&mut tree, 10, 1, ROOT_SESSION);
    let desktop = found(&mut tree, 20, 2, login);
    tree.depart(ProcessId(10), instance(1), login);
    assert!(tree.enclosing_ending(desktop));
}

#[test]
fn a_walk_resumes_after_its_cursor_and_skips_what_left() {
    let mut tree = SessionTree::new();
    let desktop = found(&mut tree, 20, 2, ROOT_SESSION);
    for pid in [21, 22, 23, 24] {
        tree.place(
            ProcessId(pid),
            instance(u8::try_from(pid).expect("small")),
            Placement::Join(desktop),
        )
        .expect("joined");
    }
    let first: Vec<u64> = tree
        .members_after(desktop, None)
        .take(2)
        .map(|member| member.0)
        .collect();
    assert_eq!(first, [20, 21]);
    tree.depart(ProcessId(22), instance(22), desktop);
    let rest: Vec<u64> = tree
        .members_after(desktop, Some(ProcessId(21)))
        .map(|member| member.0)
        .collect();
    assert_eq!(rest, [23, 24]);
}
