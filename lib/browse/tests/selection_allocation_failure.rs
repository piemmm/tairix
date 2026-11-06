//! The selection under real allocation failure: a choice it has no memory to
//! hold selects nothing and never aborts its holder.

use tairix_browse::Selection;
use tairix_fuzzseed::meter::refusing_above;

#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

#[test]
fn a_choice_with_no_memory_to_hold_it_selects_nothing() {
    let mut selection = Selection::new();
    refusing_above(0, || {
        selection.single(5);
        selection.toggle(9);
        selection.select_all(100);
        selection.remap(100, Some);
    });
    assert!(selection.is_empty());
    assert_eq!(selection.anchor(), None);
    selection.single(5);
    assert_eq!(selection.iter().collect::<Vec<_>>(), [5]);
}

#[test]
fn a_selection_spanning_its_listing_chooses_without_allocating() {
    let mut selection = Selection::new();
    selection.reset(4096);
    refusing_above(0, || {
        selection.single(7);
        selection.range_to(4000);
        selection.toggle(3);
    });
    assert_eq!(selection.len(), 3995);
    assert!(selection.contains(3) && selection.contains(4000));
}
