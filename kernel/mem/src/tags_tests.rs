//! Host tests of one CPU's tags: a space keeps its tag while nothing changes,
//! a new one takes the least recently loaded, an invalidation or a kernel
//! epoch makes a tag stale where the port's reach leaves one behind unless the
//! CPU made the invalidation itself with the space's root loaded, and a CPU
//! that tags nothing flushes every switch.

extern crate std;

use std::vec::Vec;

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

use tairix_arch_api::{AddressTag, AddressTags, MAX_USER_TAGS};

use super::TagSlots;
use crate::retire::TlbContext;

/// A port's tags, as far as the table reads them.
struct Port {
    tags: AtomicU16,
    reaches: AtomicBool,
    epoch: AtomicU64,
}

impl Port {
    fn new(tags: u16, reaches: bool) -> Self {
        Self {
            tags: AtomicU16::new(tags),
            reaches: AtomicBool::new(reaches),
            epoch: AtomicU64::new(0),
        }
    }

    fn move_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

impl AddressTags for Port {
    fn user_tags(&self) -> u16 {
        self.tags.load(Ordering::SeqCst)
    }

    fn root_register(&self, root_phys: u64, tag: u16) -> u64 {
        root_phys | u64::from(tag)
    }

    fn invalidation_reaches_every_tag(&self) -> bool {
        self.reaches.load(Ordering::SeqCst)
    }

    fn kernel_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    unsafe fn flush_current_tag(&self) {}

    unsafe fn park(&self, _flush: bool) -> bool {
        true
    }
}

fn space() -> TlbContext {
    TlbContext::new(4).expect("a context allocates")
}

/// An unmap of `space` as the reach records it: the generation advanced.
fn invalidate(space: &TlbContext) {
    space.advance();
}

const fn tagged(tag: u16, flush: bool) -> AddressTag {
    AddressTag { tag, flush }
}

/// A switch into `space` as the dispatcher makes one: the tag chosen, the
/// root loaded under it, and the recheck after the load.
fn run(slots: &mut TagSlots, space: &TlbContext, port: &Port) -> AddressTag {
    let tag = slots.tag(space, port);
    assert!(!slots.recheck(tag, space, port), "nothing raced the switch");
    tag
}

#[test]
fn a_space_keeps_its_tag_and_its_entries_while_nothing_changes() {
    let port = Port::new(4095, false);
    let mut slots = TagSlots::new();
    let (a, b) = (space(), space());
    assert_eq!(
        slots.tag(&a, &port),
        tagged(1, true),
        "a fresh tag is flushed"
    );
    assert_eq!(slots.tag(&b, &port), tagged(2, true));
    for _ in 0..3 {
        assert_eq!(slots.tag(&a, &port), tagged(1, false));
        assert_eq!(slots.tag(&b, &port), tagged(2, false));
    }
    assert!(slots.park(&port), "what boot left under tag 0 goes once");
    assert!(
        !slots.park(&port),
        "the park root's entries never went stale"
    );
}

/// Past the table's last tag the one loaded longest ago goes to the
/// newcomer, discarded, and its old space takes another when it returns.
#[test]
fn a_new_space_takes_the_tag_loaded_longest_ago() {
    let port = Port::new(3, true);
    let mut slots = TagSlots::new();
    let spaces: Vec<TlbContext> = (0..4).map(|_| space()).collect();
    for (index, space) in spaces.iter().take(3).enumerate() {
        let tag = u16::try_from(index + 1).expect("small");
        assert_eq!(slots.tag(space, &port), tagged(tag, true));
    }
    assert_eq!(slots.tag(&spaces[0], &port), tagged(1, false));
    assert_eq!(
        slots.tag(&spaces[3], &port),
        tagged(2, true),
        "the second space's tag was loaded longest ago"
    );
    assert_eq!(slots.tag(&spaces[1], &port), tagged(3, true));
    assert_eq!(slots.tag(&spaces[0], &port), tagged(1, false));
}

/// A port with more tags than the table gives out only the table's, and a
/// port with fewer only its own.
#[test]
fn the_table_gives_out_no_more_tags_than_it_or_the_port_holds() {
    for (port_tags, given) in [(4095u16, MAX_USER_TAGS), (5, 5), (1, 1)] {
        let port = Port::new(port_tags, true);
        let mut slots = TagSlots::new();
        let spaces: Vec<TlbContext> = (0..MAX_USER_TAGS * 2).map(|_| space()).collect();
        let most = spaces
            .iter()
            .map(|space| slots.tag(space, &port).tag)
            .max()
            .expect("tags were given");
        assert_eq!(usize::from(most), given, "a port of {port_tags} tags");
    }
}

/// On a port whose invalidations reach only the CPUs running a space, one
/// that left catches up on its return; on one whose reach every tag, it has
/// nothing to catch up on.
#[test]
fn an_invalidation_stales_a_tag_only_where_the_reach_leaves_one_behind() {
    for (reaches, flushed) in [(false, true), (true, false)] {
        let port = Port::new(8, reaches);
        let mut slots = TagSlots::new();
        let (a, b) = (space(), space());
        slots.tag(&a, &port);
        slots.tag(&b, &port);
        invalidate(&a);
        assert_eq!(slots.tag(&a, &port), tagged(1, flushed), "reach {reaches}");
        assert_eq!(slots.tag(&a, &port), tagged(1, false), "caught up once");
        assert_eq!(
            slots.tag(&b, &port),
            tagged(2, false),
            "another space is untouched"
        );
    }
}

/// A CPU that invalidated the space it runs discarded the entries under the
/// space's own tag, so the tag stays current however many it made.
#[test]
fn a_cpu_that_invalidated_the_space_it_runs_keeps_its_tag() {
    let port = Port::new(8, false);
    let mut slots = TagSlots::new();
    let a = space();
    run(&mut slots, &a, &port);
    for _ in 0..3 {
        let from = a.advance();
        slots.credit(&a, from, &port);
    }
    assert_eq!(slots.tag(&a, &port), tagged(1, false));
}

/// Before the recheck that follows the root's load the CPU runs under the tag
/// it left, and after a park under the park root's, so a discard it makes then
/// never reaches the space's own tag.
#[test]
fn a_credit_off_the_loaded_root_changes_nothing() {
    let port = Port::new(8, false);
    let mut slots = TagSlots::new();
    let a = space();
    let tag = slots.tag(&a, &port);
    let from = a.advance();
    slots.credit(&a, from, &port);
    assert!(slots.recheck(tag, &a, &port), "made before the load");
    slots.park(&port);
    let from = a.advance();
    slots.credit(&a, from, &port);
    assert_eq!(slots.tag(&a, &port), tagged(1, true), "made after the park");
}

/// A credit covers only the invalidation after the last one the tag is
/// current with, and only the space it names.
#[test]
fn a_credit_never_covers_an_invalidation_the_cpu_missed() {
    let port = Port::new(8, false);
    let mut slots = TagSlots::new();
    let (a, b) = (space(), space());
    slots.tag(&a, &port);
    slots.tag(&b, &port);
    run(&mut slots, &a, &port);
    invalidate(&a);
    let from = a.advance();
    slots.credit(&a, from, &port);
    assert_eq!(
        run(&mut slots, &a, &port),
        tagged(1, true),
        "one came first"
    );
    invalidate(&b);
    let from = a.advance();
    slots.credit(&a, from, &port);
    assert_eq!(slots.tag(&a, &port), tagged(1, false));
    assert_eq!(
        slots.tag(&b, &port),
        tagged(2, true),
        "another space's credit"
    );
}

/// Where invalidations reach every tag nothing is ever stale, and a credit
/// leaves it so.
#[test]
fn a_credit_where_invalidations_reach_every_tag_changes_nothing() {
    let port = Port::new(8, true);
    let mut slots = TagSlots::new();
    let a = space();
    run(&mut slots, &a, &port);
    let from = a.advance();
    slots.credit(&a, from, &port);
    assert_eq!(slots.tag(&a, &port), tagged(1, false));
}

/// A kernel invalidation that reached only the tag in use leaves every other
/// tag, the park root's among them, to be discarded once before its next use.
#[test]
fn a_moved_kernel_epoch_stales_every_tag_once() {
    let port = Port::new(8, false);
    let mut slots = TagSlots::new();
    let (a, b) = (space(), space());
    slots.tag(&a, &port);
    slots.tag(&b, &port);
    slots.park(&port);
    assert!(!slots.park(&port));
    port.move_epoch();
    assert_eq!(slots.tag(&a, &port), tagged(1, true));
    assert_eq!(slots.tag(&b, &port), tagged(2, true));
    assert!(slots.park(&port));
    assert_eq!(slots.tag(&a, &port), tagged(1, false));
    assert!(!slots.park(&port));
}

/// An invalidation that lands between choosing a tag and loading the root
/// may have reached this CPU only under the tag it left: the recheck after
/// the load catches it, and finds nothing when nothing raced.
#[test]
fn a_recheck_catches_an_invalidation_that_raced_the_switch() {
    let port = Port::new(8, false);
    let mut slots = TagSlots::new();
    let a = space();
    let tag = slots.tag(&a, &port);
    assert!(!slots.recheck(tag, &a, &port), "nothing raced");
    let tag = slots.tag(&a, &port);
    invalidate(&a);
    assert!(slots.recheck(tag, &a, &port));
    assert!(!slots.recheck(tag, &a, &port), "caught up");
    assert_eq!(slots.tag(&a, &port), tagged(1, false));
    port.move_epoch();
    assert!(
        slots.recheck(tag, &a, &port),
        "a kernel invalidation raced too"
    );
    assert!(slots.park(&port), "the park's tag went stale with it");
    assert!(!slots.recheck_park(&port), "nothing raced the park");
    port.move_epoch();
    assert!(
        slots.recheck_park(&port),
        "a kernel invalidation raced the park"
    );
}

/// A recheck of a tag that holds another space, or of tag `0`, finds nothing
/// to do.
#[test]
fn a_recheck_of_a_tag_the_space_does_not_hold_changes_nothing() {
    let port = Port::new(8, false);
    let mut slots = TagSlots::new();
    let (a, b) = (space(), space());
    let tag = slots.tag(&a, &port);
    invalidate(&b);
    assert!(!slots.recheck(tag, &b, &port));
    assert!(!slots.recheck(AddressTag::UNTAGGED, &a, &port));
}

/// A CPU that tags nothing loads every root under tag `0`, discarded, and
/// parks discarding it again: exactly an untagged port's switches.
#[test]
fn a_cpu_that_tags_nothing_flushes_every_switch() {
    let port = Port::new(0, false);
    let mut slots = TagSlots::new();
    let a = space();
    for _ in 0..3 {
        assert_eq!(slots.tag(&a, &port), AddressTag::UNTAGGED);
        assert!(!slots.recheck(AddressTag::UNTAGGED, &a, &port));
        assert!(slots.park(&port));
        assert!(!slots.recheck_park(&port));
    }
}

/// A root with no standing with the TLBs on a CPU that tags is loaded under
/// tag `0`, and the park after it discards that tag, once.
#[test]
fn a_root_with_no_context_dirties_the_park_tag() {
    let port = Port::new(8, true);
    let mut slots = TagSlots::new();
    slots.park(&port);
    assert!(!slots.park(&port));
    assert_eq!(slots.untagged(), AddressTag::UNTAGGED);
    assert!(slots.park(&port));
    assert!(!slots.park(&port));
}

/// A park the port refused discarded nothing, so the next one still owes the
/// discard; one that owed none owes none after.
#[test]
fn a_refused_park_still_owes_its_discard() {
    let port = Port::new(8, true);
    let mut slots = TagSlots::new();
    let owed = slots.park(&port);
    assert!(owed);
    slots.park_refused(owed);
    assert!(slots.park(&port), "still owed");
    let owed = slots.park(&port);
    slots.park_refused(owed);
    assert!(!slots.park(&port), "nothing was owed");
}
