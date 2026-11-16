//! The rule every reference harness holds its recordings to: no fixture
//! carries a run of [`ROM_RUN`] bytes of an original ROM (`plans/BEEB.md`
//! §7).

use std::collections::HashSet;

/// The shortest run of an original ROM's bytes a fixture may not carry.
pub const ROM_RUN: usize = 12;

/// A set of runs of [`ROM_RUN`] bytes.
pub type Runs = HashSet<[u8; ROM_RUN]>;

/// Every run of [`ROM_RUN`] bytes in `bytes`, in order.
pub fn windows(bytes: &[u8]) -> impl Iterator<Item = [u8; ROM_RUN]> + '_ {
    bytes
        .windows(ROM_RUN)
        .filter_map(|run| <[u8; ROM_RUN]>::try_from(run).ok())
}

/// Every run of [`ROM_RUN`] bytes in `streams`.
pub fn runs<I: AsRef<[u8]>>(streams: &[I]) -> Runs {
    streams
        .iter()
        .flat_map(|stream| windows(stream.as_ref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_run_is_taken_and_none_spans_two_streams() {
        let streams = [&b"0123456789AB"[..], &b"CDEFGHIJKLMNO"[..]];
        let found = runs(&streams);
        assert_eq!(found.len(), 3);
        assert!(found.contains(b"0123456789AB"));
        assert!(found.contains(b"DEFGHIJKLMNO"));
        assert!(!found.contains(b"123456789ABC"), "across the streams' join");
        assert_eq!(windows(b"short").count(), 0);
    }
}
