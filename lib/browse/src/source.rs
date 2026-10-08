//! The directory-read seam the browser is built on.
//!
//! [`DirectorySource`] is the one thing the browser needs from the outside
//! world: the children of an absolute path. Keeping it a trait means the
//! navigation and rendering logic is exhaustively testable against an
//! in-memory tree without a kernel, exactly as `appmgr`'s `BundleStore` and
//! `ps`'s transport are injected seams.
//!
//! On a running system the source is backed by the VFS: a `list` call is a
//! capability-checked directory read, so the permission decision and the
//! path policy live in the VFS, not here. The browser shows exactly the
//! entries the source returns — it never fabricates a `/proc`/`/sys`-style
//! synthetic entry.
//!
//! # A listing may not be ready yet
//!
//! A source that reads the directory on the calling thread answers
//! [`Listing::Ready`] and is the simple case. A source that reads it
//! *elsewhere* — the desktop session, whose event loop must not stall on a
//! directory a slow disk is still walking — answers [`Listing::Pending`], and
//! the embedder asks again when its own wake says the answer has landed
//! (`Browser::resume`). Nothing here ever polls or waits: the trait reports
//! what it has, and the party that owns the wake decides when to ask again.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::Errno;
use tairix_icon::FolderSample;

use crate::entry::{Entry, Occupancy};

/// What a source has for a directory right now.
///
/// Two answers, both of them normal: the children, or "not yet". A refusal is
/// the `Err` half of the enclosing [`Result`] and is a third thing entirely —
/// pending is never an error, and an error is never retried by waiting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Listing {
    /// The children, in the source's own stable order.
    Ready(Vec<Entry>),
    /// The read is under way somewhere else and the answer will arrive later.
    ///
    /// The source has taken note of the request; the *embedder* is what asks
    /// again, when whatever it parks on says the answer has landed. A source
    /// that returns this and never becomes ready simply leaves the view where
    /// it was — it can never make the browser spin.
    Pending,
}

/// What a source knows about a directory's occupancy right now.
///
/// The [`Listing`] shape, for the one question a listing cannot answer. A
/// refusal is the `Err` half of the enclosing [`Result`]: pending is never an
/// error, and an error is never retried by waiting.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Probe {
    /// The directory holds nothing.
    Empty,
    /// The directory holds something; the sample is what its first batch of
    /// entries shows of it.
    Holds(FolderSample),
    /// The probe is under way somewhere else and the answer will arrive later.
    ///
    /// The source has taken note; the *embedder* asks again when whatever it
    /// parks on says the answer has landed. A source that returns this forever
    /// simply leaves the folder drawn without its cue.
    Pending,
}

/// A read-only view of the filesystem's directory structure.
pub trait DirectorySource {
    /// List the children of the directory named by `components`
    /// (root-first; an empty slice is the root directory `/`).
    ///
    /// The returned entries are taken as authoritative and shown verbatim;
    /// iteration order is the source's own stable order. The browser does not
    /// sort, filter, or add to them.
    ///
    /// A source that reads the directory itself always answers
    /// [`Listing::Ready`]. One that reads it elsewhere answers
    /// [`Listing::Pending`] and is asked again later; asking with the same
    /// `components` must not start a second read.
    ///
    /// # Errors
    ///
    /// Returns the kernel boundary's [`Errno`] when the directory cannot be
    /// listed — for example [`Errno::PermissionDenied`] when the caller lacks
    /// the capability to read it or [`Errno::NotFound`]
    /// when it does not exist.
    fn list(&mut self, components: &[String]) -> Result<Listing, Errno>;

    /// List `components` afresh: answered only by a read that begins after
    /// this call.
    ///
    /// What an embedder asks when it knows the directory may just have changed
    /// — after its own write, or when the user asks to look again — where
    /// [`list`](Self::list) may be answered by a read already under way. A
    /// source that reads on the calling thread is fresh by construction, which
    /// is the default; one that reads elsewhere must not serve an answer, or a
    /// read, that predates the call.
    ///
    /// # Errors
    ///
    /// As [`list`](Self::list).
    fn refresh(&mut self, components: &[String]) -> Result<Listing, Errno> {
        self.list(components)
    }

    /// Whether the listing this source serves for `components` follows the
    /// directory, reporting each change made there: an embedder that has just
    /// changed it shows the change as it is reported rather than reading the
    /// directory again.
    ///
    /// The default is that it does not, which is right for a source that only
    /// reads.
    fn follows(&self, _components: &[String]) -> bool {
        false
    }

    /// Whether the directory named by `components` holds anything, and what
    /// its first batch of entries shows of it.
    ///
    /// This answers the one question a listing cannot: no VFS surface reports
    /// a child count, so the cue the browser draws is only knowable by reading
    /// the directory. An implementation reads one bounded batch and classifies
    /// its names ([`crate::folder_sample`]); it never reads further, opens a
    /// child, or reads a byte of content.
    ///
    /// A source that probes on the caller's own thread answers
    /// [`Probe::Holds`] or [`Probe::Empty`]. One that probes elsewhere answers
    /// [`Probe::Pending`] and is asked again on the next resolve; asking again
    /// with the same `components` must not start a second probe.
    ///
    /// Probing exercises the caller's directory-read authority on a child the
    /// caller is only *displaying*, so a source is free not to offer it: the
    /// default answers [`Errno::NotImplemented`], which the browser records as
    /// [`Occupancy::Indeterminate`] and draws
    /// as a plain folder. The trusted file picker takes that default
    /// deliberately — the cue adds nothing to choosing a file, so the picker
    /// exercises no authority it does not need.
    ///
    /// # Errors
    ///
    /// Returns the kernel boundary's [`Errno`] when the directory cannot be
    /// read — for example [`Errno::PermissionDenied`] when the caller lacks
    /// the capability, or [`Errno::NotImplemented`] when the source does not
    /// probe at all.
    fn has_children(&mut self, components: &[String]) -> Result<Probe, Errno> {
        let _ = components;
        Err(Errno::NotImplemented)
    }
}

/// Latch what `source` now knows of each plain directory in `range` of
/// `entries`, listed out of the directory `parent` names, calling `moved` with
/// the index of each whose picture changed.
///
/// Only an entry that [needs one](Entry::needs_occupancy_probe) is asked
/// about, so a file, a bundle, and an answered folder cost nothing — a refused
/// probe included, which latches
/// [`Indeterminate`](Occupancy::Indeterminate) rather than becoming a
/// per-frame ask. An answer still [`Pending`](Probe::Pending) leaves the entry
/// to be asked on the next resolve. Indices past the end are ignored.
///
/// Every surface that draws folder cues resolves them here, so the file
/// manager and the desktop cannot read the same answer differently. `parent`
/// is borrowed only to spell each child's components in place, and is as it
/// was when this returns.
pub fn resolve_occupancy<S: DirectorySource + ?Sized>(
    source: &mut S,
    parent: &mut Vec<String>,
    entries: &mut [Entry],
    range: Range<usize>,
    mut moved: impl FnMut(usize),
) {
    let end = range.end.min(entries.len());
    let Some(asked) = entries.get_mut(range.start.min(end)..end) else {
        return;
    };
    // One leaf buffer, rewritten per child, rather than an allocation per ask.
    parent.push(String::new());
    for (index, entry) in (range.start..).zip(asked) {
        if !entry.needs_occupancy_probe() {
            continue;
        }
        if let Some(leaf) = parent.last_mut() {
            leaf.clear();
            leaf.push_str(entry.name());
        }
        let occupancy = match source.has_children(parent) {
            Ok(Probe::Holds(sample)) => Occupancy::NonEmpty(sample),
            Ok(Probe::Empty) => Occupancy::Empty,
            Ok(Probe::Pending) => continue,
            Err(_) => Occupancy::Indeterminate,
        };
        // Only a folder with contents draws them: an empty one, an unknown one
        // and an unprobed one are the same plain picture.
        if entry.occupancy().pictured() != occupancy.pictured() {
            moved(index);
        }
        entry.set_occupancy(occupancy);
    }
    parent.pop();
}
