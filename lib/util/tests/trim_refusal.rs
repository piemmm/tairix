//! `trim` under real allocation failure: where the allocator refuses the
//! exact copy, the buffer stays as it was and the process carries on.

use tairix_fuzzseed::meter::{refusing_above, Metered};
use tairix_util::fallible::trim;

#[global_allocator]
static ALLOCATOR: Metered = Metered;

#[test]
fn a_trim_the_allocator_refuses_leaves_its_buffer_whole() {
    let mut words: Vec<u32> = Vec::with_capacity(1024);
    words.extend([3, 1, 4]);
    let at = words.as_ptr();
    refusing_above(8, || trim(&mut words));
    assert_eq!(words, [3, 1, 4]);
    assert_eq!((words.capacity(), words.as_ptr()), (1024, at), "untouched");
    trim(&mut words);
    assert_eq!(
        (words.as_slice(), words.capacity()),
        ([3, 1, 4].as_slice(), 3)
    );
}
