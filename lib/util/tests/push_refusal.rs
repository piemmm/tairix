//! `push` and `try_push` under real allocation failure: a push the allocator
//! refuses leaves its list as it was, and says so.

use tairix_fuzzseed::meter::{refusing_after, Metered};
use tairix_util::fallible::{push, try_push};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

#[test]
fn a_push_the_allocator_refuses_leaves_its_list_whole() {
    let mut words: Vec<u32> = Vec::new();
    assert!(push(&mut words, 7));
    let full = words.capacity();
    words.extend(core::iter::repeat_n(7, full - words.len()));
    let (at, len) = (words.as_ptr(), words.len());
    assert!(!refusing_after(0, || push(&mut words, 8)));
    assert!(refusing_after(0, || try_push(&mut words, 8)).is_err());
    assert_eq!(
        (words.as_ptr(), words.len(), words.capacity()),
        (at, len, full)
    );
    assert!(words.iter().all(|&word| word == 7));
    assert_eq!(try_push(&mut words, 8), Ok(()));
    assert_eq!(words.last(), Some(&8));
}
