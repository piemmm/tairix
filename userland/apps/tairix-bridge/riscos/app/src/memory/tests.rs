use super::*;

const MIB: u64 = 1 << 20;

/// A plan's guest is the largest whose keeping fits: one page more would
/// not.
fn largest(plan: &Plan) {
    let guest = plan.guest;
    assert!(guest.bytes() + kept(guest) <= plan.worker_bytes);
    if let Some(more) = Ceiling::of_pages(guest.pages() + 1) {
        assert!(
            more.bytes() + kept(more) > plan.worker_bytes,
            "{} pages fit",
            more.pages()
        );
    }
}

/// The plan for a machine of `usable` bytes, nothing else limiting it.
fn machine(usable: u64) -> Option<Plan> {
    Plan::of(Some(usable), None, None)
}

#[test]
fn a_session_is_a_quarter_of_the_machine_and_its_guest_what_that_holds() {
    let plan = machine(1 << 30).unwrap();
    assert_eq!(plan.worker_bytes, 256 * MIB);
    largest(&plan);
    assert!(plan.guest.bytes() > 128 * MIB, "{}", plan.guest.bytes());
    assert!(plan.guest.bytes() < plan.worker_bytes - FIXED_BYTES);
}

#[test]
fn the_users_limit_and_the_setting_only_lower_it() {
    let quarter = machine(8 << 30).unwrap();
    assert_eq!(quarter.worker_bytes, 2 << 30);
    let limited = Plan::of(Some(8 << 30), Some(512 * MIB), None).unwrap();
    assert_eq!(limited.worker_bytes, 512 * MIB);
    largest(&limited);
    let set = Plan::of(Some(8 << 30), Some(512 * MIB), Some(256 * MIB)).unwrap();
    assert_eq!(set.worker_bytes, 256 * MIB);
    let raised = Plan::of(Some(8 << 30), None, Some(64 << 30)).unwrap();
    assert_eq!(raised, quarter, "nothing raises it");
}

#[test]
fn a_machine_of_unknown_size_is_bounded_by_what_is_known() {
    assert_eq!(Plan::of(None, None, None), None, "nothing bounds it");
    let limited = Plan::of(None, Some(512 * MIB), None).unwrap();
    assert_eq!(limited.worker_bytes, 512 * MIB);
    let set = Plan::of(None, Some(512 * MIB), Some(256 * MIB)).unwrap();
    assert_eq!(set.worker_bytes, 256 * MIB);
}

#[test]
fn a_machine_too_small_for_a_slot_has_no_session() {
    assert_eq!(machine(0), None);
    assert_eq!(machine(64 * MIB), None, "its frames alone");
    assert_eq!(Plan::of(Some(8 << 30), Some(FIXED_BYTES), None), None);
    let least = Plan::of(Some(8 << 30), Some(FIXED_BYTES + 2 * MIB), None).unwrap();
    assert_eq!(least.slot_bytes, SLOT_LEAST);
    assert!(least.guest.bytes() >= u64::from(SLOT_LEAST));
}

#[test]
fn a_slot_is_an_eighth_of_the_guest_within_a_26_bit_slot_in_whole_pages() {
    let most = SHARED.top_26bit - APPLICATION;
    for usable in [512 * MIB, 1 << 30, 3 << 30, 8 << 30, 64 << 30] {
        let plan = machine(usable).unwrap();
        assert_eq!(plan.slot_bytes % PAGE_SIZE, 0, "{usable}");
        assert!(plan.slot_bytes >= SLOT_LEAST);
        assert!(plan.slot_bytes <= most);
        let share = plan.guest.bytes() / SLOT_SHARE;
        if share > u64::from(SLOT_LEAST) && share < u64::from(most) {
            assert!(share - u64::from(plan.slot_bytes) < u64::from(PAGE_SIZE));
        }
    }
    assert_eq!(machine(64 << 30).unwrap().slot_bytes, most);
}

#[test]
fn a_large_machine_s_guest_is_held_to_32_bits_and_its_decodings_to_the_bound() {
    let plan = machine(1 << 40).unwrap();
    assert_eq!(u64::from(plan.guest.pages()), MOST_PAGES);
    assert!(plan.memo_pages <= MEMO_MOST);
    largest(&plan);
}

#[test]
fn the_decoded_pages_are_a_cache_s_share_of_the_guest_one_at_least() {
    let small = machine(256 * MIB).unwrap();
    assert!(small.memo_pages >= 1);
    let larger = machine(4 << 30).unwrap();
    assert!(larger.memo_pages > small.memo_pages);
    assert!(
        memo_bytes(larger.memo_pages) <= cache_budget(larger.guest.bytes()).hard() as u64,
        "within one cache's share"
    );
}
