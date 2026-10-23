//! Address-space tags: how a port marks each TLB entry with the address space
//! it belongs to (an aarch64 or riscv64 ASID, an x86_64 PCID), so a CPU
//! switching between spaces keeps the entries of the one it leaves instead of
//! discarding them.
//!
//! The kernel decides which tag a space is loaded under on each CPU, and when
//! a tag's entries are stale; the port says how many tags it has, how far its
//! invalidations reach, and does the loading and the discarding. Tag `0` is
//! the kernel-only park root's, on every port.

use core::sync::atomic::{AtomicU64, Ordering};

/// The most tags beside `0` the kernel gives user spaces on one CPU: enough
/// for the spaces it switches among in a burst, beyond which the oldest
/// entries have left the TLB anyway and every switch would scan a longer
/// table.
pub const MAX_USER_TAGS: usize = 8;

/// The root a CPU parks on, under tag `0`, whenever it must leave a user root:
/// the boot space's, whose tables live for the image's lifetime, published
/// once by the first switch to it.
#[derive(Debug)]
#[repr(transparent)]
pub struct ParkRoot(AtomicU64);

impl ParkRoot {
    /// Nothing published.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Publish `root` unless one already is: a later root is not the boot
    /// space's.
    pub fn publish(&self, root: u64) {
        let _ = self
            .0
            .compare_exchange(0, root, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// The published root, if one is: no root table is ever at physical `0`.
    #[must_use]
    pub fn published(&self) -> Option<u64> {
        match self.0.load(Ordering::Acquire) {
            0 => None,
            root => Some(root),
        }
    }

    /// The word the root is published in, for a port that must push it past
    /// the caches a CPU not yet running with them reads it around.
    #[must_use]
    pub const fn word(&self) -> &AtomicU64 {
        &self.0
    }

    /// Load the published root under tag `0` through `activate`, discarding
    /// the tag's entries when `flush`, answering whether one was published:
    /// with none the CPU's translation is left as it was (fail closed).
    ///
    /// # Safety
    ///
    /// `activate` is the port's root load, and the published root satisfies
    /// its contract on the calling CPU: kernel mode, the root mapping the
    /// running kernel context as the regime in force does.
    pub unsafe fn park(&self, flush: bool, activate: unsafe fn(u64, AddressTag)) -> bool {
        let Some(root) = self.published() else {
            return false;
        };
        // SAFETY: the caller's contract.
        unsafe { activate(root, AddressTag { tag: 0, flush }) };
        true
    }
}

impl Default for ParkRoot {
    fn default() -> Self {
        Self::new()
    }
}

/// The tag a root is loaded under on one CPU, and whether the entries that tag
/// holds must be discarded first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AddressTag {
    /// `0` is the park root's; every other names one user space on the CPU.
    pub tag: u16,
    /// The tag holds entries of another space, or stale ones of this space.
    pub flush: bool,
}

impl AddressTag {
    /// Tag `0`, discarded first: how a root is loaded on a CPU that tags
    /// nothing, exactly as a port without tags always loads one.
    pub const UNTAGGED: Self = Self {
        tag: 0,
        flush: true,
    };
}

/// A port's address-space tags.
///
/// # Safety of the switches
///
/// [`park`](Self::park) and [`flush_current_tag`](Self::flush_current_tag)
/// rewrite the calling CPU's translation state, so they run in kernel mode on
/// the dispatcher's context, exactly where a port's root switch runs.
pub trait AddressTags: Sync {
    /// The tags beside `0` the calling CPU can give user spaces: `0` when it
    /// tags nothing, so every switch discards what the outgoing space left.
    fn user_tags(&self) -> u16;

    /// The value the port's root register takes for `root_phys` under `tag`:
    /// the one composition every switch of the port loads, so a host test
    /// holds its layout.
    fn root_register(&self, root_phys: u64, tag: u16) -> u64;

    /// Whether the port's invalidation of a user space's translations reaches
    /// every CPU under every tag, as a broadcast does, so a CPU that left the
    /// space keeps none of the stale ones.
    fn invalidation_reaches_every_tag(&self) -> bool;

    /// A count the port advances before every invalidation of the kernel's
    /// shared mappings that reaches only the tag in use on each CPU, so a CPU
    /// discards its other tags' entries before it next uses them. Constant on
    /// a port whose kernel invalidations reach every tag.
    fn kernel_epoch(&self) -> u64 {
        0
    }

    /// Discard every entry of the tag the calling CPU's root is loaded under.
    ///
    /// # Safety
    ///
    /// Kernel mode on the dispatcher's context, as the root switch.
    unsafe fn flush_current_tag(&self);

    /// Load the calling CPU's park root under tag `0`, discarding that tag's
    /// entries first when `flush`, and answer whether the CPU left the user
    /// root it was on.
    ///
    /// # Safety
    ///
    /// Kernel mode on the dispatcher's context, as the root switch.
    unsafe fn park(&self, flush: bool) -> bool;
}

/// The address-tag conformance vertical: what every port's tags must hold
/// for the kernel's choice of tag to mean what it says.
pub mod conformance {
    use super::{AddressTags, MAX_USER_TAGS};

    /// The most tags the walk probes: those the kernel gives out, one per bit
    /// of the widest range, and that range's last.
    const PROBES: usize = MAX_USER_TAGS + 1 + u16::BITS as usize + 1;

    /// The port counts no more user tags than `widest`, the most its root
    /// register ever carries; every tag the kernel gives out loads `root` as a
    /// distinct register value, as does one tag per bit of `widest`'s range,
    /// so the composition holds whatever count the CPUs settle on; two roots
    /// under one tag stay apart, and tag `0` composes as a tag like any other.
    ///
    /// # Panics
    ///
    /// When the port's composition merges two tags or two roots: a test
    /// harness's report, never a production path.
    pub fn run_all(tags: &dyn AddressTags, widest: u16, root: u64, other_root: u64) {
        assert!(
            tags.user_tags() <= widest,
            "the port counts more tags than its register carries"
        );
        let given = (0..=widest).take(MAX_USER_TAGS + 1);
        let bits = (0..u16::BITS)
            .map(|bit| 1 << bit)
            .take_while(|tag| *tag <= widest);
        let mut probed = [0; PROBES];
        let mut count = 0;
        for tag in given.chain(bits).chain([widest]) {
            if !probed[..count].contains(&tag) {
                probed[count] = tag;
                count += 1;
            }
        }
        let probed = &probed[..count];
        for (at, &tag) in probed.iter().enumerate() {
            let value = tags.root_register(root, tag);
            assert_ne!(
                value,
                tags.root_register(other_root, tag),
                "two roots under tag {tag} load as one register value"
            );
            for &earlier in &probed[..at] {
                assert_ne!(
                    value,
                    tags.root_register(root, earlier),
                    "tags {earlier} and {tag} load one register value"
                );
            }
        }
        assert_eq!(
            tags.kernel_epoch(),
            tags.kernel_epoch(),
            "the kernel epoch moved with no invalidation"
        );
    }
}
