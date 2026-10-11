//! A counted notice resumes past the board and announces each count once.

use tairix_abi::{Errno, Notice, NoticeTopic, SyscallNumber};
use tairix_abi_trap::seam::{capture, no_trap, refusal};

use super::{resumed_after, ChangeNotice};

const NOTICE_PUBLISH: u64 = SyscallNumber::NOTICE_PUBLISH.as_u16() as u64;

#[test]
fn a_count_resumes_one_past_the_board() {
    assert_eq!(resumed_after(None), 1, "nothing published yet");
    assert_eq!(
        resumed_after(Some(&Notice::Network { changes: 41 })),
        42,
        "never the count an earlier instance left"
    );
    assert_eq!(
        resumed_after(Some(&Notice::Network { changes: u64::MAX })),
        0
    );
    assert_eq!(
        resumed_after(Some(&Notice::Mounts)),
        1,
        "no count to resume"
    );
}

#[test]
fn an_unpublished_topic_resumes_from_one() {
    let (_, args) = capture(refusal(Errno::NotFound), || {
        let (_, from) = ChangeNotice::resume(NoticeTopic::Network);
        assert_eq!(from, 1);
    });
    assert_eq!(args[0], u64::from(NoticeTopic::Network.as_u32()));
}

#[test]
fn each_count_is_announced_once() {
    let mut notice = ChangeNotice {
        topic: NoticeTopic::AudioDevices,
        announced: None,
    };
    let (number, args) = capture(0, || assert_eq!(notice.announce(3), Ok(())));
    assert_eq!(number, NOTICE_PUBLISH);
    assert_eq!(args[0], u64::from(NoticeTopic::AudioDevices.as_u32()));
    no_trap(|| assert_eq!(notice.announce(3), Ok(())));
    let (number, _) = capture(0, || assert_eq!(notice.announce(4), Ok(())));
    assert_eq!(number, NOTICE_PUBLISH);
}

#[test]
fn a_refused_count_surfaces_and_is_not_offered_again() {
    let mut notice = ChangeNotice {
        topic: NoticeTopic::Network,
        announced: None,
    };
    let _ = capture(refusal(Errno::PermissionDenied), || {
        assert_eq!(notice.announce(9), Err(Errno::PermissionDenied));
    });
    no_trap(|| assert_eq!(notice.announce(9), Ok(())));
}

#[test]
fn a_topic_without_a_count_is_refused_without_a_trap() {
    let mut notice = ChangeNotice {
        topic: NoticeTopic::Mounts,
        announced: None,
    };
    no_trap(|| assert_eq!(notice.announce(1), Err(Errno::OutOfRange)));
}
