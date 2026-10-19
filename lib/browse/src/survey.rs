//! Totalling what a long file operation will touch before it begins: every
//! item beneath its sources and the bytes of every file, so its progress is a
//! share of a counted whole rather than a count of what has gone by
//! (`plans/NEW-FILEMANAGER.md` `FM7b`).
//!
//! The survey does no I/O. The app looks at each item whose kind it does not
//! yet know and lists each directory it names, handing back what it found one
//! step at a time, so preparing is as cancellable as the walk it comes before. A directory that cannot be listed, or a tree
//! deeper than any walk goes, leaves the whole uncounted: the operation then
//! states what it has done and no share of a whole it could not count.

use alloc::string::String;
use alloc::vec::Vec;

use crate::progress::Tally;
use crate::MAX_WALK_DEPTH;

/// One child of a directory listing, as a survey counts it: its name, whether
/// it is a directory the operation goes into, and its own bytes.
pub type Counted = (String, bool, u64);

/// What a survey needs looked at next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SurveyStep<'a> {
    /// This item, whose kind and size are not yet known: answer with
    /// [`Survey::stated`].
    Stat(&'a [String]),
    /// This directory: answer with [`Survey::listed`].
    List(&'a [String]),
}

impl<'a> SurveyStep<'a> {
    /// The path the step names.
    #[must_use]
    pub const fn path(self) -> &'a [String] {
        match self {
            Self::Stat(path) | Self::List(path) => path,
        }
    }
}

/// A directory listed, and the names of the directories beneath it still to
/// list: its path is held once, however many of them there are.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Frame {
    path: Vec<String>,
    below: Vec<String>,
}

/// A survey of the trees beneath an operation's sources.
///
/// A directory is listed only where its children lie within the depth the
/// walks go to, counted on the whole path as they count it; one deeper leaves
/// the whole uncounted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Survey {
    /// The sources whose kind and size are still to be looked at, last first.
    unstated: Vec<Vec<String>>,
    /// The source directories still to list, last first.
    roots: Vec<Vec<String>>,
    /// The directories listed on the way to the one in hand, deepest last.
    frames: Vec<Frame>,
    /// The directory [`next`](Self::next) names to list.
    listing: Option<Vec<String>>,
    found: Tally,
    /// Part of the trees could not be counted.
    partial: bool,
}

impl Survey {
    /// A survey of `roots`: each a path, whether it is a directory the
    /// operation goes into, and its own bytes.
    pub fn new(roots: impl IntoIterator<Item = (Vec<String>, bool, u64)>) -> Self {
        let mut survey = Self::default();
        for (path, descend, bytes) in roots {
            survey.count_root(path, descend, bytes);
        }
        survey.roots.reverse();
        survey.advance();
        survey
    }

    /// A survey of `roots` whose kinds and sizes are still to be looked at,
    /// first to last.
    pub fn of(roots: impl IntoIterator<Item = Vec<String>>) -> Self {
        let mut unstated: Vec<Vec<String>> = roots.into_iter().collect();
        unstated.reverse();
        let mut survey = Self {
            unstated,
            ..Self::default()
        };
        survey.advance();
        survey
    }

    /// Count one item, and whether a directory at `len` components has its
    /// children within the walks' depth, so that it may be listed.
    fn count(&mut self, descend: bool, bytes: u64, len: usize) -> bool {
        self.found.items = self.found.items.saturating_add(1);
        self.found.bytes = self.found.bytes.saturating_add(bytes);
        let listable = descend && len < MAX_WALK_DEPTH;
        self.partial |= descend && !listable;
        listable
    }

    /// Count the source `path`, keeping it to list.
    fn count_root(&mut self, path: Vec<String>, descend: bool, bytes: u64) {
        if self.count(descend, bytes, path.len()) {
            self.roots.push(path);
        }
    }

    /// Settle on the next directory to list: the deepest one waiting beneath
    /// what was listed last, then the next source.
    fn advance(&mut self) {
        if !self.unstated.is_empty() {
            self.listing = None;
            return;
        }
        while let Some(frame) = self.frames.last_mut() {
            if let Some(name) = frame.below.pop() {
                let mut path = Vec::with_capacity(frame.path.len() + 1);
                path.extend_from_slice(&frame.path);
                path.push(name);
                self.listing = Some(path);
                return;
            }
            self.frames.pop();
        }
        self.listing = self.roots.pop();
    }

    /// What to look at next: a source to state, then a directory to list;
    /// `None` once there is nothing left.
    #[must_use]
    pub fn next(&self) -> Option<SurveyStep<'_>> {
        if let Some(path) = self.unstated.last() {
            return Some(SurveyStep::Stat(path));
        }
        self.listing.as_deref().map(SurveyStep::List)
    }

    /// The source [`next`](Self::next) named to state is a directory the
    /// operation goes into, or not, of `bytes` of its own.
    pub fn stated(&mut self, descend: bool, bytes: u64) {
        if let Some(path) = self.unstated.pop() {
            self.count_root(path, descend, bytes);
            if self.unstated.is_empty() {
                self.roots.reverse();
                self.advance();
            }
        }
    }

    /// Count `children`, the listing of the directory [`next`](Self::next)
    /// named to list.
    pub fn listed(&mut self, children: &[Counted]) {
        if !self.unstated.is_empty() {
            return;
        }
        let Some(path) = self.listing.take() else {
            return;
        };
        let len = path.len() + 1;
        let mut below = Vec::new();
        for (name, descend, bytes) in children {
            if self.count(*descend, *bytes, len) {
                below.push(name.clone());
            }
        }
        // Popped from the end, so the listing's own order is kept.
        below.reverse();
        self.frames.push(Frame { path, below });
        self.advance();
    }

    /// What [`next`](Self::next) named could not be looked at: it, or what is
    /// beneath it, goes uncounted.
    pub fn unreadable(&mut self) {
        if self.unstated.pop().is_some() {
            self.partial = true;
            if self.unstated.is_empty() {
                self.roots.reverse();
            }
        } else if self.listing.take().is_some() {
            self.partial = true;
        } else {
            return;
        }
        self.advance();
    }

    /// What has been counted so far.
    #[must_use]
    pub const fn found(&self) -> Tally {
        self.found
    }

    /// Whether everything there is to look at has been.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.unstated.is_empty() && self.listing.is_none()
    }

    /// The whole, once counted in full: `None` while anything remains to look
    /// at, and for good once part could not be counted.
    #[must_use]
    pub fn total(&self) -> Option<Tally> {
        (self.is_done() && !self.partial).then_some(self.found)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec::Vec;

    use super::{Counted, Survey, SurveyStep};
    use crate::progress::Tally;
    use crate::MAX_WALK_DEPTH;

    fn path(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| String::from(*part)).collect()
    }

    fn file(name: &str, bytes: u64) -> Counted {
        (String::from(name), false, bytes)
    }

    fn folder(name: &str) -> Counted {
        (String::from(name), true, 0)
    }

    #[test]
    fn a_tree_is_counted_whole_one_listing_at_a_time() {
        let mut survey = Survey::new([
            (path(&["Home", "a.txt"]), false, 10),
            (path(&["Home", "Photos"]), true, 0),
        ]);
        assert_eq!(survey.total(), None, "a folder is still to list");
        assert_eq!(
            survey.next().map(SurveyStep::path),
            Some(path(&["Home", "Photos"]).as_slice())
        );
        survey.listed(&[file("one.jpg", 100), folder("2024"), file("two.jpg", 200)]);
        assert_eq!(
            survey.next().map(SurveyStep::path),
            Some(path(&["Home", "Photos", "2024"]).as_slice())
        );
        survey.listed(&[file("three.jpg", 300)]);
        assert!(survey.is_done());
        assert_eq!(
            survey.total(),
            Some(Tally {
                items: 6,
                bytes: 610
            })
        );
    }

    #[test]
    fn a_folder_that_cannot_be_listed_leaves_the_whole_uncounted() {
        let mut survey = Survey::new([(path(&["Home", "Locked"]), true, 0)]);
        survey.unreadable();
        assert!(survey.is_done());
        assert_eq!(survey.total(), None);
        assert_eq!(survey.found().items, 1, "what was counted stays counted");
        survey.unreadable();
        survey.listed(&[file("stray", 1)]);
        assert_eq!(survey.found().items, 1, "nothing is listed past the end");
    }

    /// The depth is the walks' own, counted on the whole path: a folder whose
    /// children a walk would refuse is not listed.
    #[test]
    fn a_tree_deeper_than_any_walk_goes_is_not_counted_whole() {
        let mut survey = Survey::new([(path(&["deep"]), true, 0)]);
        for level in 1..MAX_WALK_DEPTH {
            let step = survey.next().expect("a folder to list");
            assert_eq!(step.path().len(), level);
            survey.listed(&[folder("next")]);
        }
        assert!(
            survey.is_done(),
            "the folder whose children lie past the deepest is not listed"
        );
        assert_eq!(survey.total(), None);
        let found = survey.found().items;
        assert_eq!(found, u64::try_from(MAX_WALK_DEPTH).expect("small"));

        let mut shallow = Survey::new([(path(&["Users", "me", "Home"]), true, 0)]);
        while let Some(step) = shallow.next() {
            assert!(step.path().len() < MAX_WALK_DEPTH);
            shallow.listed(&[folder("next")]);
        }
        assert_eq!(
            shallow.found().items,
            u64::try_from(MAX_WALK_DEPTH - 2).expect("small"),
            "a source already deep leaves its tree less room"
        );
    }

    /// A wide folder holds its path once, and its folders are listed in the
    /// listing's own order, each with the whole of the path to it.
    #[test]
    fn the_folders_of_a_wide_listing_are_listed_in_order() {
        let mut survey = Survey::new([(path(&["Home"]), true, 0)]);
        survey.listed(&[folder("a"), file("x", 1), folder("b"), folder("c")]);
        let mut seen = Vec::new();
        while let Some(step) = survey.next() {
            seen.push(step.path().to_vec());
            survey.listed(&[]);
        }
        assert_eq!(
            seen,
            [
                path(&["Home", "a"]),
                path(&["Home", "b"]),
                path(&["Home", "c"])
            ]
        );
        assert_eq!(survey.total(), Some(Tally { items: 5, bytes: 1 }));
    }

    #[test]
    fn sources_of_unknown_kind_are_stated_first_then_their_trees_listed() {
        let mut survey = Survey::of([path(&["a.txt"]), path(&["Photos"]), path(&["gone"])]);
        assert_eq!(
            survey.next(),
            Some(SurveyStep::Stat(path(&["a.txt"]).as_slice()))
        );
        survey.listed(&[file("ignored", 9)]);
        assert_eq!(
            survey.found(),
            Tally::default(),
            "a listing waits for the stating"
        );
        survey.stated(false, 10);
        assert_eq!(
            survey.next(),
            Some(SurveyStep::Stat(path(&["Photos"]).as_slice()))
        );
        survey.stated(true, 0);
        assert_eq!(
            survey.next(),
            Some(SurveyStep::Stat(path(&["gone"]).as_slice()))
        );
        survey.unreadable();
        assert_eq!(
            survey.next(),
            Some(SurveyStep::List(path(&["Photos"]).as_slice()))
        );
        survey.listed(&[file("one.jpg", 100)]);
        assert!(survey.is_done());
        assert_eq!(survey.total(), None, "a source that could not be looked at");
        assert_eq!(
            survey.found(),
            Tally {
                items: 3,
                bytes: 110
            }
        );
    }
}
