use super::Announced;

/// Neither a presented frame without the arm nor the arm without a presented
/// frame announces; both together announce once.
#[test]
fn an_announcement_waits_for_a_presented_frame_and_the_arm() {
    let mut announced = Announced::default();
    assert!(!announced.advance(false, Some(10), 20), "never presented");
    assert_eq!(announced.due(Some(10)), None, "nothing is held");
    assert!(
        !announced.advance(true, Some(10), 5),
        "presented, not armed"
    );
    assert_eq!(announced.due(Some(10)), Some(10));
    assert!(!announced.advance(false, None, 50), "not yet stamped");
    assert!(announced.advance(false, Some(10), 10));
    assert_eq!(announced, Announced::Shown);
    assert!(!announced.advance(true, Some(10), 99), "once");
    assert_eq!(announced.due(Some(10)), None);
}
