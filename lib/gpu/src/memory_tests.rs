use super::overlaps;

#[test]
fn spans_share_a_value_only_where_both_hold_one() {
    for (a, b, shared) in [
        (0..4, 2..6, true),
        (0..8, 2..4, true),
        (0..4, 4..8, false),
        (0..8, 4..4, false),
        (0..8, 0..0, false),
        (0..8, 8..8, false),
        (3..3, 3..3, false),
    ] {
        assert_eq!(overlaps(&a, &b), shared, "{a:?} and {b:?}");
        assert_eq!(overlaps(&b, &a), shared, "{b:?} and {a:?}");
    }
}
