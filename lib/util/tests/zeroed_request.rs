//! `zeroed` under the metered allocator: its bytes are asked for zeroed, so
//! an allocator handing out fresh pages writes none of them, and a refusal is
//! an answer, never an abort.

use tairix_fuzzseed::meter::{metered, refusing_above, Metered};
use tairix_util::fallible::zeroed;

#[global_allocator]
static ALLOCATOR: Metered = Metered;

#[test]
fn zeroed_bytes_are_asked_for_zeroed_and_a_refusal_is_an_answer() {
    let (bytes, metering) = metered(|| zeroed(1 << 20));
    assert_eq!(bytes.map(|bytes| bytes.len()), Some(1 << 20));
    assert_eq!((metering.requested, metering.zeroed), (1 << 20, 1 << 20));
    assert_eq!(refusing_above(4096, || zeroed(8192)), None);
}
