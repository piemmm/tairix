use super::*;

const SIZE: (u32, u32) = (768, 600);

#[test]
fn a_view_refused_at_a_size_is_not_asked_for_again_at_it() {
    let mut refusal = ViewRefusal::default();
    assert!(refusal.allows(SIZE));
    refusal.refused(SIZE);
    assert!(!refusal.allows(SIZE));
    refusal.resized(SIZE);
    assert!(!refusal.allows(SIZE), "the size the desktop kept");
    assert!(refusal.allows((1920, 1080)), "another size is asked");
}

#[test]
fn a_new_size_forgets_the_refusal() {
    let mut refusal = ViewRefusal::default();
    refusal.refused(SIZE);
    refusal.resized((1920, 1080));
    assert!(refusal.allows((1920, 1080)));
    assert!(refusal.allows(SIZE), "asked once more when it comes back");
}
