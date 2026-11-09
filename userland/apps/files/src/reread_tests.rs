use super::*;

#[test]
fn each_publication_is_read_once_and_a_refusal_waits_ever_longer() {
    let mut reread = Reread::new(1);
    assert!(!reread.wants(1, 0), "the publication read");
    assert!(reread.wants(2, 0));
    assert!(!reread.wants(2, 0), "under way");
    reread.landed(2, true, 0);
    assert_eq!(reread.retry_at(), Some(FIRST_RETRY_NS));
    assert!(!reread.wants(2, FIRST_RETRY_NS - 1));
    assert!(reread.wants(2, FIRST_RETRY_NS));
    reread.landed(2, true, FIRST_RETRY_NS);
    assert_eq!(reread.retry_at(), Some(FIRST_RETRY_NS * 3));
    assert!(reread.wants(2, FIRST_RETRY_NS * 3));
    reread.landed(2, false, FIRST_RETRY_NS * 3);
    assert_eq!(reread.retry_at(), None);
    assert!(!reread.wants(2, u64::MAX), "read, it is not asked again");
}

/// A refused read that lands while a later publication's read is under way
/// leaves no time for the loop to wake at: that read's landing is the next
/// turn, and a past time would have the loop turn without parking until then.
///
/// Regression: the refusal armed its wait regardless, the wait passed while
/// the later read was out, and the loop's deadline stayed in the past for as
/// long as that read took — under memory pressure, a long spin.
#[test]
fn a_refusal_landing_behind_a_later_read_names_no_retry() {
    let mut reread = Reread::new(1);
    assert!(reread.wants(2, 0));
    assert!(reread.wants(3, 0), "a later publication is read at once");
    reread.landed(2, true, 0);
    assert_eq!(reread.retry_at(), None, "the read of 3 is under way");
    assert!(!reread.wants(3, FIRST_RETRY_NS * 10), "still under way");
    reread.landed(3, true, FIRST_RETRY_NS * 10);
    assert!(
        reread.retry_at().is_some_and(|at| at > FIRST_RETRY_NS * 10),
        "its own refusal waits from its landing"
    );
}
