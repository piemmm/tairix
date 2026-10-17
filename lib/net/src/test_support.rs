//! Test-only deterministic [`TempAddrSource`] for the engine tests.
//!
//! The RFC 8981 temporary-address path draws its randomised interface
//! identifiers and desync jitter from an injected [`TempAddrSource`]
//! (entropy stays at the service seam, the engine is pure). The tests
//! inject this reproducible `SplitMix64` stream so the engine's behaviour
//! is fully deterministic while still yielding distinct, non-reserved
//! identifiers.

use alloc::boxed::Box;

use tairix_mix::{SplitMix64, GOLDEN_GAMMA};

use crate::iface::TempAddrSource;

/// A deterministic randomness source: a `SplitMix64` stream, so
/// temporary identifiers are distinct across draws yet reproducible
/// run to run.
#[derive(Debug)]
pub(crate) struct SeqTempSource {
    draws: SplitMix64,
}

impl SeqTempSource {
    /// A source seeded with `SplitMix64`'s own increment.
    pub(crate) fn new() -> Self {
        Self::seeded(GOLDEN_GAMMA)
    }

    /// A source with a caller-chosen seed, so a test can force a
    /// distinct identifier stream.
    pub(crate) fn seeded(seed: u64) -> Self {
        Self {
            draws: SplitMix64::new(seed),
        }
    }
}

impl TempAddrSource for SeqTempSource {
    fn fill_random(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(8) {
            let bytes = self.draws.next_u64().to_le_bytes();
            let len = chunk.len();
            chunk.copy_from_slice(&bytes[..len]);
        }
    }
}

/// A boxed deterministic source for `Iface::new` / `Stack::new` in
/// tests that do not exercise privacy addresses.
pub(crate) fn temp_source() -> Box<dyn TempAddrSource> {
    Box::new(SeqTempSource::new())
}
