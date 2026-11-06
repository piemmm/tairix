//! Unit tests for the follows' bookkeeping: what keeping a follow lets go of,
//! what a refusal and a lost wait let go, the lane a follow let go is ended
//! on, the follows the desk's sweeping and forgetting hand back, a follow
//! armed while its bundle went unfollowed, and an eject's.

use super::*;

use alloc::format;
use alloc::string::String;

use tairix_abi::driver::filesystem::MountFlags;
use tairix_abi::fs::FileId;
use tairix_abi::sysinfo::ViewBindingRecord;
use tairix_abi::time::Time64;
use tairix_icon::MAX_LISTED_BUNDLE_DIRS;

/// The version a listing named a bundle at as `n`.
fn stamp(n: u64) -> DocumentStamp {
    DocumentStamp {
        size: n,
        modified: Time64::UNIX_EPOCH,
        id: FileId {
            volume: [1; 16],
            node: n,
        },
        content_gen: n,
    }
}

/// The bundle at `dir`, as a listing named it at version 1.
fn bundle(dir: &str) -> ListedBundle {
    ListedBundle {
        dir: String::from(dir),
        listed: stamp(1),
    }
}

/// `bundle` followed through a wait reported under each of `tokens`, each
/// wait's handle its own token.
fn followed(bundle: &ListedBundle, tokens: &[u64]) -> FollowedBundle<u64> {
    FollowedBundle::new(
        bundle.clone(),
        tokens.iter().map(|&token| (token, token)).collect(),
    )
}

/// The tokens of every follow let go, ended by a reader of every lane.
fn ended(follows: &mut BundleFollows<u64>) -> Vec<u64> {
    let routes = Routes::default();
    let mut tokens = Vec::new();
    while let Some((_, followed)) = follows.next_ending(&routes, |_| true) {
        tokens.extend(followed.into_waits().into_iter().map(|(token, _)| token));
    }
    tokens.sort_unstable();
    tokens
}

/// The wait the follower made, which every follow here is armed through.
const SET: u64 = 9;

/// A desk following bundles, and the follows of a follower that made its
/// wait.
fn following() -> (ArtworkDesk, BundleFollows<u64>) {
    let mut desk: ArtworkDesk = ArtworkDesk::new(crate::tests::DESK_KEYS);
    desk.follow_bundles();
    let mut follows = BundleFollows::new();
    follows.joined(SET);
    (desk, follows)
}

/// `bundle` shown, and the read of its files about to arm their follow.
fn shown(desk: &mut ArtworkDesk, follows: &mut BundleFollows<u64>, bundle: &ListedBundle) {
    let _ = follows.version_of(desk, &bundle.dir, bundle.listed, 32);
    assert!(desk.follows(bundle), "its read does not arm its follow");
}

/// Keeping a follow lets go of what its bundle held before, which a reader
/// then ends, and the bundle's changes are reported under its new waits.
#[test]
fn a_kept_follow_lets_go_of_what_its_bundle_held() {
    let one = bundle("/Users/ann/Desktop/One.app");
    let (mut desk, mut follows) = following();
    shown(&mut desk, &mut follows, &one);
    follows.keep(&mut desk, (followed(&one, &[1, 2]), SET), false);
    assert_eq!(follows.holder(2), Some(&one));
    follows.keep(&mut desk, (followed(&one, &[3]), SET), false);
    assert_eq!(follows.holder(1), None, "a wait let go still reports");
    assert_eq!(follows.holder(3), Some(&one));
    assert_eq!(ended(&mut follows), [1, 2]);
}

/// A follow whose arming was refused is not held: its waits are let go at
/// once, with what its bundle held before, and its desk follows it no more.
#[test]
fn a_refused_follow_is_let_go_at_once() {
    let one = bundle("/Users/ann/Desktop/One.app");
    let (mut desk, mut follows) = following();
    shown(&mut desk, &mut follows, &one);
    follows.keep(&mut desk, (followed(&one, &[1]), SET), false);
    follows.keep(&mut desk, (followed(&one, &[2]), SET), true);
    assert_eq!(follows.holder(2), None);
    assert!(!desk.follows(&one), "its desk still follows it");
    assert_eq!(ended(&mut follows), [1, 2]);
}

/// Letting go answers whether a follow was held, so a reader is woken only
/// for one there is to end.
#[test]
fn letting_go_answers_only_for_a_follow_held() {
    let (one, two) = (bundle("/One.app"), bundle("/Two.app"));
    let (mut desk, mut follows) = following();
    assert!(!follows.let_go([&one]), "nothing was followed");
    shown(&mut desk, &mut follows, &one);
    follows.keep(&mut desk, (followed(&one, &[1]), SET), false);
    assert!(!follows.let_go([&two]));
    assert!(follows.let_go([&one, &two]));
    assert!(!follows.let_go([&one]), "a follow was let go twice");
    assert_eq!(ended(&mut follows), [1]);
}

/// A follow let go is handed only to a reader of its own volume's lane.
#[test]
fn a_follow_let_go_is_ended_on_its_own_lane() {
    let routes = Routes::default();
    let (mut desk, mut follows) = following();
    follows.keep(&mut desk, (followed(&bundle("/One.app"), &[1]), SET), true);
    let elsewhere = follows.next_ending(&routes, |lane| lane != Lane::Unnamed);
    assert!(elsewhere.is_none(), "another lane's reader was handed it");
    let taken = follows.next_ending(&routes, |_| true);
    assert!(matches!(taken, Some((Lane::Unnamed, _))), "{taken:?}");
    let again = follows.next_ending(&routes, |_| true);
    assert!(again.is_none(), "ended twice");
}

/// The follower's wait failing lets every follow go, and follows nothing
/// more.
#[test]
fn a_lost_wait_lets_every_follow_go() {
    let (mut desk, mut follows) = following();
    assert_eq!(follows.set(), Some(SET));
    for (token, one) in [(1, bundle("/One.app")), (2, bundle("/Two.app"))] {
        shown(&mut desk, &mut follows, &one);
        follows.keep(&mut desk, (followed(&one, &[token]), SET), false);
    }
    follows.lost();
    assert_eq!(follows.set(), None, "a lost wait is still armed through");
    assert_eq!((follows.holder(1), follows.holder(2)), (None, None));
    assert_eq!(ended(&mut follows), [1, 2]);
}

/// A sweep lets go of the follow of each bundle no surface asked about since
/// the last, and keeps the one still asked about.
#[test]
fn a_sweep_lets_go_of_the_bundles_no_surface_asked_about() {
    let (still, left) = (bundle("/Shown.app"), bundle("/Left.app"));
    let (mut desk, mut follows) = following();
    for (token, one) in [(1, &still), (2, &left)] {
        shown(&mut desk, &mut follows, one);
        follows.keep(&mut desk, (followed(one, &[token]), SET), false);
    }
    assert!(!follows.sweep(&mut desk), "both were just asked about");
    let (version, owed) = follows.version_of(&mut desk, &still.dir, still.listed, 32);
    assert_eq!(version, BundleVersion::Listed(still.listed));
    assert!(!owed);
    assert!(follows.sweep(&mut desk), "the bundle that left was kept");
    assert_eq!(follows.holder(1), Some(&still));
    assert_eq!(ended(&mut follows), [2]);
}

/// A bundle the desk forgets past its bound hands its follow back to be
/// ended.
#[test]
fn a_bundle_the_desk_forgets_hands_its_follow_back() {
    let oldest = bundle("/Oldest.app");
    let (mut desk, mut follows) = following();
    shown(&mut desk, &mut follows, &oldest);
    follows.keep(&mut desk, (followed(&oldest, &[1]), SET), false);
    for n in 1..MAX_LISTED_BUNDLE_DIRS {
        let (_, owed) = follows.version_of(&mut desk, &format!("/{n}.app"), stamp(1), 32);
        assert!(!owed, "a bundle was forgotten within the bound");
    }
    let (_, owed) = follows.version_of(&mut desk, "/Newest.app", stamp(1), 32);
    assert!(owed, "the oldest bundle's follow was kept");
    assert_eq!(ended(&mut follows), [1]);
}

/// A follow armed while its bundle stopped being followed — swept, forgotten
/// past the bound, or armed through a wait since lost — is let go as it is
/// kept: nothing else would ever let it go.
#[test]
fn a_follow_armed_while_its_bundle_went_unfollowed_is_let_go_as_it_is_kept() {
    let one = bundle("/Users/ann/Desktop/One.app");

    let (mut desk, mut follows) = following();
    shown(&mut desk, &mut follows, &one);
    assert!(!follows.sweep(&mut desk), "it was just asked about");
    assert!(
        !follows.sweep(&mut desk),
        "a follow not yet kept was let go"
    );
    follows.keep(&mut desk, (followed(&one, &[1]), SET), false);
    assert_eq!(follows.holder(1), None, "kept for a bundle swept meanwhile");
    assert_eq!(ended(&mut follows), [1]);

    let (mut desk, mut follows) = following();
    shown(&mut desk, &mut follows, &one);
    for n in 0..MAX_LISTED_BUNDLE_DIRS {
        let _ = follows.version_of(&mut desk, &format!("/{n}.app"), stamp(1), 32);
    }
    follows.keep(&mut desk, (followed(&one, &[2]), SET), false);
    assert_eq!(
        follows.holder(2),
        None,
        "kept for a bundle forgotten meanwhile"
    );
    assert_eq!(ended(&mut follows), [2]);

    let (mut desk, mut follows) = following();
    shown(&mut desk, &mut follows, &one);
    follows.lost();
    follows.keep(&mut desk, (followed(&one, &[3]), SET), false);
    assert_eq!(follows.holder(3), None, "kept through a wait since lost");
    assert_eq!(ended(&mut follows), [3]);
}

/// A bundle shown again before its read kept its follow holds it: its desk
/// follows it again, and a reader is woken to read its files anew.
#[test]
fn a_follow_kept_for_a_bundle_shown_again_meanwhile_is_held() {
    let one = bundle("/Users/ann/Desktop/One.app");
    let (mut desk, mut follows) = following();
    shown(&mut desk, &mut follows, &one);
    let _ = follows.sweep(&mut desk);
    let _ = follows.sweep(&mut desk);
    let (_, owed) = follows.version_of(&mut desk, &one.dir, one.listed, 32);
    assert!(owed, "no reader was woken to read it anew");
    follows.keep(&mut desk, (followed(&one, &[1]), SET), false);
    assert_eq!(follows.holder(1), Some(&one));
}

/// Every follow within a volume about to be ejected is let go, and one being
/// armed there as it goes is let go as it is kept; one elsewhere is held.
#[test]
fn a_volume_ejected_lets_go_of_every_follow_on_it() {
    let on = bundle("/Storage/usb1/Games.app");
    let arming = bundle("/Storage/usb1/More.app");
    let elsewhere = bundle("/Storage/usb10/Tools.app");
    let (mut desk, mut follows) = following();
    for (token, one) in [(1, &on), (2, &elsewhere)] {
        shown(&mut desk, &mut follows, one);
        follows.keep(&mut desk, (followed(one, &[token]), SET), false);
    }
    shown(&mut desk, &mut follows, &arming);
    let root = [String::from("Storage"), String::from("usb1")];
    assert!(follows.release_within(&mut desk, &[&root[..]]));
    follows.keep(&mut desk, (followed(&arming, &[3]), SET), false);
    assert_eq!(follows.holder(1), None, "a follow on the volume holds it");
    assert_eq!(
        follows.holder(3),
        None,
        "a follow armed as it went holds it"
    );
    assert_eq!(
        follows.holder(2),
        Some(&elsewhere),
        "a sibling whose name begins the same way was let go"
    );
    assert_eq!(ended(&mut follows), [1, 3]);
    assert!(
        !follows.release_within(&mut desk, &[&root[..]]),
        "a follow was let go twice"
    );
}

/// The reader carrying an eject out takes every follow on its volume, let go
/// within the places the volume is shown at, to end on the volume's lane
/// before the eject, and leaves one elsewhere held.
#[test]
fn an_eject_takes_every_follow_on_its_volume_to_end_on_its_lane() {
    let view = |target: &str, id: u8| {
        ViewBindingRecord::new(target.as_bytes(), b"/", MountFlags::default(), [id; 16])
            .expect("view")
    };
    let routes = Routes::new(alloc::vec![
        view("/Storage/usb1", 3),
        view("/Storage/usb10", 4),
    ]);
    let on = bundle("/Storage/usb1/Games.app");
    let elsewhere = bundle("/Storage/usb10/Tools.app");
    let (mut desk, mut follows) = following();
    for (token, one) in [(1, &on), (2, &elsewhere)] {
        shown(&mut desk, &mut follows, one);
        follows.keep(&mut desk, (followed(one, &[token]), SET), false);
    }
    let lane = Lane::of_volume([3; 16]);
    let root = [String::from("Storage"), String::from("usb1")];
    assert!(follows.release_within(&mut desk, &[&root[..]]));
    let mut taken = Vec::new();
    while let Some((at, followed)) = follows.next_ending(&routes, |at| at == lane) {
        assert_eq!(at, lane);
        taken.push(followed.bundle().clone());
    }
    assert_eq!(taken, [on]);
    assert_eq!(follows.holder(2), Some(&elsewhere));
}

/// Every reason the bundles go unfollowed says what that costs.
#[test]
fn every_unfollowed_reason_says_what_it_costs() {
    for reason in [
        Unfollowed::NoThread(Errno::LimitExceeded),
        Unfollowed::NoWake,
        Unfollowed::NoWait(Errno::LimitExceeded),
        Unfollowed::WakeRefused(Errno::NotFound),
        Unfollowed::WaitFailed(Errno::OutOfMemory),
    ] {
        let said = format!("{reason}");
        assert!(said.ends_with(BUNDLES_UNFOLLOWED), "{said}");
    }
}
