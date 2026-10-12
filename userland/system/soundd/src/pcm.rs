//! A decoded cue, and the reclaimable cache that keeps one decode per boot
//! per asset.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, Frames, Rate, SampleFormat};
use tairix_abi::origin::ProcId;
use tairix_abi::time::Duration64;
use tairix_hash::BuildSipHash13;
use tairix_log::Sink;
use tairix_reclaim::{
    CacheBudget, CacheCandidate, CachedBytes, InvalidationSource, PressureGauge, RebuildCost,
    ReclaimCache, ReclaimClass, ReclaimOwner, ReclaimRule, Sensitivity,
};
use tairix_soundtheme::{SoundAsset, SoundEvent};

use crate::store::FileIdentity;

/// A cue's samples, interleaved in the format the decoder wrote them, zeroed
/// when the last holder lets them go: they may be decoded from a user's file.
#[derive(Debug, Eq, PartialEq)]
pub struct CuePcm {
    /// How each sample is encoded.
    pub format: SampleFormat,
    /// Frames a second.
    pub rate: Rate,
    /// The channels each frame interleaves.
    pub channel_map: ChannelMap,
    /// The samples.
    pub bytes: Vec<u8>,
}

impl CuePcm {
    /// Bytes one frame occupies.
    #[must_use]
    pub const fn frame_bytes(&self) -> usize {
        self.format.bytes_per_sample() * self.channel_map.channels() as usize
    }

    /// Frames it holds.
    #[must_use]
    pub fn frames(&self) -> u64 {
        u64::try_from(self.bytes.len() / self.frame_bytes().max(1)).unwrap_or(u64::MAX)
    }

    /// How long it plays for.
    #[must_use]
    pub fn duration(&self) -> Duration64 {
        Frames::new(self.frames()).duration(self.rate)
    }
}

impl Drop for CuePcm {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

/// Where a cached decode's file came from.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum CueOrigin {
    /// A theme's asset.
    Theme {
        /// The theme.
        theme: String,
        /// The asset.
        asset: SoundAsset,
    },
    /// The file the machine's document names for an event.
    Machine(SoundEvent),
    /// The file a session handed over for an event.
    Handed {
        /// The login session the file came from.
        session: ProcId,
        /// The event.
        event: SoundEvent,
    },
}

/// Which file, at which version, a cached decode is of.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CueKey {
    /// Where the file came from.
    pub origin: CueOrigin,
    /// The version of the file it was decoded from.
    pub identity: FileIdentity,
}

/// A decode the cache keeps, shared with every cue playing it.
#[derive(Clone, Debug)]
pub struct CachedCue(pub Arc<CuePcm>);

impl CachedBytes for CachedCue {
    fn payload_bytes(&self) -> usize {
        self.0.bytes.capacity()
    }

    fn wipe(&mut self) {
        // A cue still sounding holds them, and they zero themselves when it
        // lets them go.
        if let Some(pcm) = Arc::get_mut(&mut self.0) {
            pcm.bytes.fill(0);
        }
    }
}

/// The decoded-cue cache.
///
/// Its generation is `()`: a changed file is a different [`CueKey`], so
/// nothing invalidates the cache wholesale.
pub type CueCache = ReclaimCache<CueKey, CachedCue, (), BuildSipHash13>;

/// The audit label the cache is named by in reclaim records.
const CACHE_LABEL: &str = "soundd.cues";

/// Total machine RAM per byte the cache may hold: a mebibyte a gibibyte, so
/// the whole shipped set stays decoded on a machine of four.
const CUE_CACHE_RAM_DIVISOR: u64 = 1024;

/// A key's own bytes plus a map entry's and the recency index's, with
/// headroom: a theme name is a leaf name, at most 255 bytes, and every other
/// origin is smaller.
const CUE_ENTRY_METADATA_BYTES: usize = 384;

/// The classification the cache declares: a decoded sound is a sandbox round
/// trip to rebuild, is replaced when its file changes, and is simply dropped
/// on reclaim — nothing about a cue is pinned, because a cue re-decodes in
/// milliseconds and losing one under pressure costs nothing audible. A user's
/// own file is theirs, so the cache is private and wiped as it gives an entry
/// up.
#[must_use]
pub const fn cue_cache_candidate() -> CacheCandidate {
    CacheCandidate {
        class: Some(ReclaimClass::DisposableUi),
        owner: Some(ReclaimOwner::UserlandProcess("soundd")),
        rebuild_cost: RebuildCost::Moderate,
        sensitivity: Some(Sensitivity::UserData),
        invalidation: Some(InvalidationSource::SourceMutation),
        rule: Some(ReclaimRule::Drop),
        entry_metadata_bytes: CUE_ENTRY_METADATA_BYTES,
    }
}

/// The cache, budgeted from the machine's total usable RAM.
///
/// A reading of zero — the System Information service unreachable — admits
/// nothing: every cue then decodes afresh, correct and merely slower.
#[must_use]
pub fn cue_cache(
    total_ram_bytes: u64,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
    hasher: BuildSipHash13,
) -> CueCache {
    let ceiling = usize::try_from(total_ram_bytes / CUE_CACHE_RAM_DIVISOR).unwrap_or(usize::MAX);
    ReclaimCache::new(
        CACHE_LABEL,
        cue_cache_candidate(),
        CacheBudget::from_ceiling(ceiling),
        pressure,
        sink,
        hasher,
    )
}
