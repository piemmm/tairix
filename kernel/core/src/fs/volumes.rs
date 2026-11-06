//! The kernel volume forest: durable `id::` volume-root publication and
//! resolution (`docs/src/filesystem/drives.md` §8, `plans/DEVICES.md` D3a).
//!
//! TAIRiX storage is a forest of independently addressable named roots. The
//! durable machine form of a root is `id::<volume-id>/path`, where the
//! volume id is the key the volume is published under: the filesystem's own
//! stable identity (the `ARXFS` per-volume UUID), or a key of its own for a
//! twin of a volume published under that (`docs/src/filesystem/drives.md`
//! §8). This registry is the kernel-side map from that key to the live root:
//! a boot or hotplug path **publishes** each mounted volume's key together
//! with the `/`-view location its root resolves to, and the single kernel
//! path-resolution entry point resolves an `id::`-rooted path against it.
//!
//! Resolving through the forest grants nothing: the resolved location is
//! then authorised by the secured VFS exactly as the equivalent view path
//! would be (per-inode owner/mode/ACL/`required_cap` and mount flags), so
//! the durable spelling is never a policy bypass. An identity that is not
//! published fails closed with "not found" — never a guessed root.
//!
//! Every mounted volume backs a region of the one `/` view, so a
//! publication records the volume root's view prefix: `[]` for the
//! writable root volume, `["System"]` for the read-only system volume,
//! and `["Storage", <name>]` for a runtime-attached volume projected
//! into the `Storage:` catalog (`plans/DEVICES.md` D3b). A hotplug
//! detach withdraws its publication with [`VolumeForest::unpublish`];
//! the boot volumes are published once and never withdrawn.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_sync::RwLock;

/// One published volume root: the volume's stable 16-byte identity and the
/// `/`-view components its root directory resolves to.
struct VolumeRoot {
    id: [u8; 16],
    view_prefix: ViewPrefix,
}

/// A published root's `/`-view components, one shared text the forest holds
/// with every resolution of the root, so resolving copies nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewPrefix(Arc<Spelled>);

/// The components laid end to end, and where each ends.
#[derive(Debug, Eq, PartialEq)]
struct Spelled {
    text: String,
    ends: Vec<usize>,
}

impl ViewPrefix {
    /// `components` copied, or `None` when the heap cannot hold them.
    fn copied(components: &[&str]) -> Option<Self> {
        let total = components
            .iter()
            .try_fold(0usize, |total, part| total.checked_add(part.len()))?;
        let mut text = String::new();
        text.try_reserve_exact(total).ok()?;
        let mut ends = Vec::new();
        ends.try_reserve_exact(components.len()).ok()?;
        for part in components {
            text.push_str(part);
            ends.push(text.len());
        }
        Arc::try_new(Spelled { text, ends }).ok().map(Self)
    }

    /// The components, root-most first.
    pub fn components(&self) -> impl Iterator<Item = &str> + '_ {
        let Spelled { text, ends } = &*self.0;
        let starts = core::iter::once(0).chain(ends.iter().copied());
        starts
            .zip(ends.iter().copied())
            .filter_map(move |(start, end)| text.get(start..end))
    }

    /// The last component: the name the root is shown under.
    fn last(&self) -> Option<&str> {
        let Spelled { text, ends } = &*self.0;
        let end = *ends.last()?;
        let start = ends.len().checked_sub(2).map_or(0, |before| ends[before]);
        text.get(start..end)
    }
}

/// Why a volume-root publication was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VolumePublishError {
    /// The identity is the reserved all-zero value ("no identity"); a
    /// volume with no stable identity is never published, so a forged
    /// all-zero spelling can never resolve.
    NilIdentity,
    /// The key is already published. A key binds to exactly one root; a
    /// duplicate is refused, never silently re-bound, and a twin is published
    /// under a key of its own.
    AlreadyPublished,
    /// The heap cannot hold the publication; nothing is published.
    OutOfMemory,
}

impl core::fmt::Display for VolumePublishError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Self::NilIdentity => "volume has the reserved all-zero identity",
            Self::AlreadyPublished => "volume identity is already published",
            Self::OutOfMemory => "no memory to publish the volume identity",
        };
        f.write_str(msg)
    }
}

/// The registry of published volume roots the `id::` resolver reads.
///
/// Held as a `&'static` borrow by the syscall handlers, exactly like the
/// other boot-installed seams: the boot or runtime-attach path that mounts
/// a volume publishes its identity here, a runtime detach withdraws it,
/// and an identity that is not published fails closed. Reads take the
/// shared lock only for the tiny lookup, never across a filesystem
/// operation.
pub struct VolumeForest {
    roots: RwLock<Vec<VolumeRoot>>,
}

impl VolumeForest {
    /// Construct an empty forest. `const` so a boot path can place it in a
    /// `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            roots: RwLock::new(Vec::new()),
        }
    }

    /// Publish a mounted volume's stable identity, binding it to the
    /// `/`-view location its root directory resolves to.
    ///
    /// # Errors
    ///
    /// * [`VolumePublishError::NilIdentity`] for the reserved all-zero
    ///   identity.
    /// * [`VolumePublishError::AlreadyPublished`] when the identity is
    ///   already bound — fail closed, never a silent re-bind.
    /// * [`VolumePublishError::OutOfMemory`] when the heap cannot hold the
    ///   publication; nothing is published.
    pub fn publish(&self, id: [u8; 16], view_prefix: &[&str]) -> Result<(), VolumePublishError> {
        if id == [0u8; 16] {
            return Err(VolumePublishError::NilIdentity);
        }
        // Copied before the guard is taken, which is then held over no copy.
        let view_prefix = ViewPrefix::copied(view_prefix).ok_or(VolumePublishError::OutOfMemory)?;
        let mut roots = self.roots.write();
        if roots.iter().any(|root| root.id == id) {
            return Err(VolumePublishError::AlreadyPublished);
        }
        roots
            .try_reserve(1)
            .map_err(|_| VolumePublishError::OutOfMemory)?;
        roots.push(VolumeRoot { id, view_prefix });
        Ok(())
    }

    /// Withdraw a published identity, returning the `/`-view components
    /// its root resolved to so the caller can retract the matching mount.
    ///
    /// Fails closed: an identity that is not published returns `None` and
    /// removes nothing, so a forged or repeated detach can never withdraw
    /// another volume's root.
    pub fn unpublish(&self, id: &[u8; 16]) -> Option<ViewPrefix> {
        let mut roots = self.roots.write();
        let pos = roots.iter().position(|root| &root.id == id)?;
        Some(roots.remove(pos).view_prefix)
    }

    /// Resolve a published identity to its root's `/`-view components, or
    /// `None` when the identity is not published (fail closed — the caller
    /// reports "not found", never a guessed root).
    ///
    /// The components are the publication's own, shared, so a resolution
    /// allocates nothing and no registry borrow is held across the filesystem
    /// operation the caller goes on to perform.
    #[must_use]
    pub fn resolve(&self, id: &[u8; 16]) -> Option<ViewPrefix> {
        self.roots
            .read()
            .iter()
            .find(|root| &root.id == id)
            .map(|root| root.view_prefix.clone())
    }

    /// A copy of the name a published identity's root is shown under, the
    /// last component of its `/`-view location: `None` when it is not
    /// published, its root is the view's own, or the copy cannot be held.
    #[must_use]
    pub fn shown_name(&self, id: &[u8; 16]) -> Option<String> {
        let roots = self.roots.read();
        let root = roots.iter().find(|held| &held.id == id)?;
        tairix_util::fallible::concatenated(&[root.view_prefix.last()?])
    }

    /// Whether `id` is published, asked without copying where its root
    /// resolves.
    #[must_use]
    pub fn publishes(&self, id: &[u8; 16]) -> bool {
        self.roots.read().iter().any(|root| &root.id == id)
    }
}

impl Default for VolumeForest {
    fn default() -> Self {
        Self::new()
    }
}

/// The fail-closed default forest the syscall handlers hold before a boot
/// path installs the real one: nothing is ever published into it, so every
/// `id::` resolution against it reports "not found".
pub static NULL_VOLUME_FOREST: VolumeForest = VolumeForest::new();

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    const ID_A: [u8; 16] = [1; 16];
    const ID_B: [u8; 16] = [2; 16];

    /// `prefix`'s components as text.
    fn spelled(prefix: Option<ViewPrefix>) -> Option<Vec<String>> {
        prefix.map(|prefix| prefix.components().map(ToString::to_string).collect())
    }

    #[test]
    fn publish_then_resolve_returns_the_prefix() {
        let forest = VolumeForest::new();
        forest.publish(ID_A, &["System"]).expect("publish");
        forest.publish(ID_B, &[]).expect("publish");
        assert_eq!(
            spelled(forest.resolve(&ID_A)),
            Some(vec!["System".to_string()])
        );
        assert_eq!(spelled(forest.resolve(&ID_B)), Some(Vec::new()));
    }

    #[test]
    fn unpublished_identity_fails_closed() {
        let forest = VolumeForest::new();
        forest.publish(ID_A, &[]).expect("publish");
        assert_eq!(forest.resolve(&ID_B), None);
    }

    #[test]
    fn publishes_answers_whether_a_key_is_bound() {
        let forest = VolumeForest::new();
        forest.publish(ID_A, &["Storage", "usb1"]).expect("publish");
        assert!(forest.publishes(&ID_A));
        assert!(!forest.publishes(&ID_B));
        assert!(forest.unpublish(&ID_A).is_some());
        assert!(!forest.publishes(&ID_A));
    }

    #[test]
    fn nil_identity_is_refused() {
        let forest = VolumeForest::new();
        assert_eq!(
            forest.publish([0u8; 16], &[]),
            Err(VolumePublishError::NilIdentity)
        );
        assert_eq!(forest.resolve(&[0u8; 16]), None);
    }

    #[test]
    fn duplicate_identity_is_refused_and_keeps_the_first_binding() {
        let forest = VolumeForest::new();
        forest.publish(ID_A, &["System"]).expect("publish");
        assert_eq!(
            forest.publish(ID_A, &["Users"]),
            Err(VolumePublishError::AlreadyPublished)
        );
        assert_eq!(
            spelled(forest.resolve(&ID_A)),
            Some(vec!["System".to_string()])
        );
    }

    #[test]
    fn unpublish_withdraws_exactly_the_named_root() {
        let forest = VolumeForest::new();
        forest.publish(ID_A, &["Storage", "usb1"]).expect("publish");
        forest.publish(ID_B, &[]).expect("publish");
        assert_eq!(
            spelled(forest.unpublish(&ID_A)),
            Some(vec!["Storage".to_string(), "usb1".to_string()])
        );
        // The withdrawn identity no longer resolves; the other root is
        // untouched; a repeated detach fails closed.
        assert_eq!(forest.resolve(&ID_A), None);
        assert_eq!(spelled(forest.resolve(&ID_B)), Some(Vec::new()));
        assert_eq!(forest.unpublish(&ID_A), None);
        // The identity can be re-published (the re-insert path).
        forest
            .publish(ID_A, &["Storage", "usb1"])
            .expect("re-publish");
        assert!(forest.resolve(&ID_A).is_some());
    }

    #[test]
    fn unpublish_unknown_identity_fails_closed() {
        let forest = VolumeForest::new();
        assert_eq!(forest.unpublish(&ID_A), None);
        assert_eq!(forest.unpublish(&[0u8; 16]), None);
    }

    #[test]
    fn null_forest_resolves_nothing() {
        assert_eq!(NULL_VOLUME_FOREST.resolve(&ID_A), None);
    }

    #[test]
    fn a_published_root_is_shown_under_its_last_component() {
        let forest = VolumeForest::new();
        forest.publish(ID_A, &["Storage", "usb1"]).expect("publish");
        forest.publish(ID_B, &[]).expect("publish");
        assert_eq!(forest.shown_name(&ID_A).as_deref(), Some("usb1"));
        assert_eq!(
            forest.shown_name(&ID_B),
            None,
            "the view's own root has no name"
        );
        assert_eq!(forest.shown_name(&[7; 16]), None);
    }

    #[test]
    fn publish_error_display_is_non_empty() {
        for e in [
            VolumePublishError::NilIdentity,
            VolumePublishError::AlreadyPublished,
            VolumePublishError::OutOfMemory,
        ] {
            assert!(!e.to_string().is_empty());
        }
    }

    /// A publication the heap cannot hold is refused for want of memory at
    /// each allocation it makes, publishing nothing, until granted them all.
    #[test]
    fn a_publication_the_heap_cannot_hold_publishes_nothing() {
        let forest = VolumeForest::new();
        let refused = Err(VolumePublishError::OutOfMemory);
        let published = crate::test_alloc::past_each_refusal(&refused, || {
            let answer = forest.publish(ID_A, &["Storage", "usb1"]);
            assert_eq!(
                answer.is_ok(),
                forest.publishes(&ID_A),
                "a refusal publishes nothing"
            );
            answer
        });
        assert_eq!(published, Ok(()));
        assert_eq!(
            spelled(forest.resolve(&ID_A)),
            Some(vec!["Storage".to_string(), "usb1".to_string()])
        );
    }

    /// A resolution shares the publication's components, so it asks the heap
    /// for nothing.
    #[test]
    fn a_resolution_asks_the_heap_for_nothing() {
        let forest = VolumeForest::new();
        forest.publish(ID_A, &["Storage", "usb1"]).expect("publish");
        crate::test_alloc::refuse_allocations_on_current_thread(true);
        let parts = forest
            .resolve(&ID_A)
            .map(|prefix| prefix.components().count());
        crate::test_alloc::refuse_allocations_on_current_thread(false);
        assert_eq!(parts, Some(2));
    }

    /// A prefix costs the same few allocations however deep it is, and reads
    /// back each component and its last exactly.
    #[test]
    fn a_prefix_is_one_text_however_many_components_it_has() {
        use crate::test_alloc::{opt_in_current_thread, opt_out_current_thread, LiveBytes};
        static COUNTER: LiveBytes = LiveBytes::new();
        let cost = |parts: &[&str]| {
            opt_in_current_thread(&COUNTER);
            let before = COUNTER.allocations();
            let prefix = ViewPrefix::copied(parts).expect("copied");
            let made = COUNTER.allocations() - before;
            opt_out_current_thread();
            assert_eq!(prefix.components().collect::<Vec<_>>(), parts);
            assert_eq!(prefix.last(), parts.last().copied());
            made
        };
        assert_eq!(
            cost(&["Storage", "usb1"]),
            cost(&["a", "", "bc", "d", "e", "f", "g", "h"])
        );
        assert_eq!(cost(&[]), 1, "an empty prefix holds no text");
    }
}
