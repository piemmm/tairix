//! A resolver that asks each listed bundle's version once a turn of its
//! embedder's loop (`plans/ICONS.md` I22).
//!
//! A draw walks a tile's tiers for its paint, its prefetch and its note that
//! it is still shown, and each walk asks the version of a listed bundle it
//! draws ([`ArtworkResolver::bundle_version`]). A resolver answering from
//! behind its readers' lock takes that lock once a walk; [`ByTurn`] answers
//! every ask a turn makes after its first, so the file manager's grid and the
//! desktop take the lock once a bundle a turn.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::DocumentStamp;
use tairix_collections::OrderedMap;
use tairix_util::fallible;

use crate::artwork::{ArtworkKey, ArtworkResolver, BundleVersion, Resolved};
use crate::desk::MAX_LISTED_BUNDLE_DIRS;

/// The most answers a turn keeps: as many as a desk remembers directories.
/// One past it is asked of the wrapped resolver each time.
const ANSWERS_HELD: usize = MAX_LISTED_BUNDLE_DIRS;

/// A resolver asking the one it wraps each listed bundle's version once a
/// turn, from the turn's first ask.
///
/// Its embedder begins a turn at the head of each turn of its loop
/// ([`begin_turn`](Self::begin_turn)). An ask answered here reaches no desk,
/// so an embedder sweeping its desk's follows does so at most once a turn,
/// after every surface has asked.
pub struct ByTurn {
    resolver: Box<dyn ArtworkResolver>,
    /// The turn's answers, by bundle directory.
    answered: OrderedMap<String, Vec<Answer>>,
    /// How many answers `answered` holds.
    held: usize,
    /// The turn under way.
    turn: u64,
}

/// One answer: the version listed and the side asked at, the version the
/// picture is keyed by, and the turn that asked.
#[derive(Copy, Clone)]
struct Answer {
    listed: DocumentStamp,
    side: u32,
    version: BundleVersion,
    turn: u64,
}

impl ByTurn {
    /// `resolver`, asked each listed bundle's version once a turn.
    #[must_use]
    pub fn new(resolver: Box<dyn ArtworkResolver>) -> Self {
        Self {
            resolver,
            answered: OrderedMap::new(),
            held: 0,
            turn: 0,
        }
    }

    /// A turn of the embedder's loop begins: each listed bundle's version is
    /// asked once more, the first time the turn needs it.
    pub fn begin_turn(&mut self) {
        self.turn = self.turn.wrapping_add(1);
    }

    /// The version the bundle at `dir`, listed as `listed`, was answered at
    /// `side` this turn.
    fn this_turn(&self, dir: &str, listed: DocumentStamp, side: u32) -> Option<BundleVersion> {
        self.answered
            .get(dir)?
            .iter()
            .find(|answer| answer.listed == listed && answer.side == side)
            .filter(|answer| answer.turn == self.turn)
            .map(|answer| answer.version)
    }

    /// Keep `version`, answered this turn for the bundle at `dir` listed and
    /// asked at as `asked` says, where an earlier answer for it stood. Past
    /// the bound the earlier turns' answers go first; one the bound or the
    /// heap leaves no room for is asked again when next needed.
    fn note(&mut self, dir: &str, asked: (DocumentStamp, u32), version: BundleVersion) {
        let (listed, side) = asked;
        let answer = Answer {
            listed,
            side,
            version,
            turn: self.turn,
        };
        let stood = self.answered.get_mut(dir).and_then(|answers| {
            answers
                .iter_mut()
                .find(|held| held.listed == listed && held.side == side)
        });
        if let Some(stood) = stood {
            *stood = answer;
            return;
        }
        if self.held >= ANSWERS_HELD {
            self.forget_earlier_turns();
            if self.held >= ANSWERS_HELD {
                return;
            }
        }
        if let Some(answers) = self.answered.get_mut(dir) {
            if fallible::push(answers, answer) {
                self.held += 1;
            }
            return;
        }
        let Some(key) = fallible::concatenated(&[dir]) else {
            return;
        };
        let mut answers = Vec::new();
        if fallible::push(&mut answers, answer) && self.answered.try_insert(key, answers).is_ok() {
            self.held += 1;
        }
    }

    /// Drop every answer an earlier turn gave.
    fn forget_earlier_turns(&mut self) {
        let turn = self.turn;
        let mut held = 0;
        self.answered.retain(|_, answers| {
            answers.retain(|answer| answer.turn == turn);
            held += answers.len();
            !answers.is_empty()
        });
        self.held = held;
    }
}

impl ArtworkResolver for ByTurn {
    fn resolve(&mut self, key: &ArtworkKey, side: u32) -> Resolved {
        self.resolver.resolve(key, side)
    }

    fn prefetch(&mut self, key: &ArtworkKey, side: u32) {
        self.resolver.prefetch(key, side);
    }

    fn declined(&mut self, key: &ArtworkKey, side: u32, bytes: usize) {
        self.resolver.declined(key, side, bytes);
    }

    fn still_shown(&mut self, key: &ArtworkKey, side: u32) {
        self.resolver.still_shown(key, side);
    }

    fn bundle_version(&mut self, dir: &str, listed: DocumentStamp, side: u32) -> BundleVersion {
        if let Some(version) = self.this_turn(dir, listed, side) {
            return version;
        }
        let version = self.resolver.bundle_version(dir, listed, side);
        self.note(dir, (listed, side), version);
        version
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use core::cell::Cell;

    use super::*;

    /// Counts the asks for a listed bundle's version that reach it.
    struct Counting(Rc<Cell<usize>>);

    impl ArtworkResolver for Counting {
        fn resolve(&mut self, _key: &ArtworkKey, _side: u32) -> Resolved {
            Resolved::Pending
        }

        fn bundle_version(
            &mut self,
            _dir: &str,
            listed: DocumentStamp,
            _side: u32,
        ) -> BundleVersion {
            self.0.set(self.0.get() + 1);
            BundleVersion::Listed(listed)
        }
    }

    /// The version a listing named a bundle at as `n`.
    fn stamp(n: u64) -> DocumentStamp {
        DocumentStamp {
            size: n,
            modified: tairix_abi::time::Time64::UNIX_EPOCH,
            id: tairix_abi::fs::FileId {
                volume: [1; 16],
                node: 7,
            },
            content_gen: n,
        }
    }

    /// A memo over a counting resolver, and its count.
    fn counted() -> (ByTurn, Rc<Cell<usize>>) {
        let asked = Rc::new(Cell::new(0));
        let turns = ByTurn::new(Box::new(Counting(Rc::clone(&asked))));
        (turns, asked)
    }

    /// A turn asks each bundle's version at each side once, however often it
    /// is asked, and the next turn asks again.
    #[test]
    fn a_turn_asks_each_bundles_version_once() {
        let (mut turns, asked) = counted();
        turns.begin_turn();
        for _ in 0..3 {
            assert_eq!(
                turns.bundle_version("/One.app", stamp(1), 32),
                BundleVersion::Listed(stamp(1))
            );
        }
        assert_eq!(asked.get(), 1, "a turn asked more than once");
        let _ = turns.bundle_version("/One.app", stamp(1), 48);
        let _ = turns.bundle_version("/One.app", stamp(2), 32);
        let _ = turns.bundle_version("/Two.app", stamp(1), 32);
        assert_eq!(
            asked.get(),
            4,
            "another side, version or bundle is its own ask"
        );
        turns.begin_turn();
        let _ = turns.bundle_version("/One.app", stamp(1), 32);
        assert_eq!(asked.get(), 5, "a new turn did not ask again");
    }

    /// An answer the heap has no room to keep is asked again, and nothing
    /// aborts: the memo once filed its answers with an insert that could not
    /// refuse. However far the heap ran out, the memo keeps the answer once
    /// it has room.
    #[test]
    fn an_exhausted_heap_asks_the_resolver_each_time() {
        use tairix_fuzzseed::meter::{refusing_above, refusing_after};
        let (mut turns, asked) = counted();
        turns.begin_turn();
        refusing_above(0, || {
            for _ in 0..3 {
                assert_eq!(
                    turns.bundle_version("/One.app", stamp(1), 32),
                    BundleVersion::Listed(stamp(1))
                );
            }
        });
        assert_eq!(asked.get(), 3, "an answer the heap refused was kept");
        for _ in 0..2 {
            let _ = turns.bundle_version("/One.app", stamp(1), 32);
        }
        assert_eq!(asked.get(), 4, "the answer kept with room was asked again");
        for granted in 0..8 {
            let (mut turns, asked) = counted();
            turns.begin_turn();
            let version =
                refusing_after(granted, || turns.bundle_version("/One.app", stamp(1), 32));
            assert_eq!(version, BundleVersion::Listed(stamp(1)), "{granted}");
            let _ = turns.bundle_version("/One.app", stamp(1), 32);
            let kept = asked.get();
            let _ = turns.bundle_version("/One.app", stamp(1), 32);
            assert_eq!(asked.get(), kept, "{granted}");
        }
    }
}
