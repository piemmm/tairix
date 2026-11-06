use super::*;

#[derive(Clone, Debug, PartialEq)]
struct Big {
    payload: [u8; 1000],
    tag: usize,
    link: u32,
}

impl Big {
    fn tagged(tag: usize) -> Self {
        Self {
            payload: [0; 1000],
            tag,
            link: NIL,
        }
    }
}

impl Linked for Big {
    fn link(&self) -> u32 {
        self.link
    }

    fn set_link(&mut self, link: u32) {
        self.link = link;
    }

    fn write_empty(slot: &mut MaybeUninit<Self>) {
        slot.write(Self::tagged(usize::MAX));
    }
}

/// Make the next node in reserved room, tagged `tag`, answering its index.
fn push(nodes: &mut Nodes<Big>, tag: usize) -> Option<usize> {
    let at = nodes.push_empty()?;
    nodes.get_mut(at)?.tag = tag;
    Some(at)
}

/// Take a node from `arena`, tagged `tag`.
fn take(arena: &mut Arena<Big>, tag: usize) -> u32 {
    let at = arena.take();
    if let Some(node) = arena.nodes.get_mut(at as usize) {
        node.tag = tag;
    }
    at
}

fn filled(count: usize) -> Nodes<Big> {
    let mut nodes = Nodes::new();
    nodes.reserve(count).expect("room");
    for tag in 0..count {
        assert_eq!(push(&mut nodes, tag), Some(tag));
    }
    nodes
}

/// The list of chunks `nodes` holds; none while it holds no core.
fn chunks(nodes: &Nodes<Big>) -> &[Vec<Big>] {
    nodes.core.as_deref().map_or(&[], |core| &core.chunks)
}

/// The room `nodes` holds: its slots, then its list of chunks' length and
/// room.
fn room(nodes: &Nodes<Big>) -> (usize, usize, usize) {
    let list = nodes
        .core
        .as_deref()
        .map_or(0, |core| core.chunks.capacity());
    (nodes.capacity(), chunks(nodes).len(), list)
}

/// An arena costs a structure holding it one word, as `alloc`'s own tree map
/// costs three. Regression: each held its chunk list and counts inline, and a
/// map of two arenas was 136 bytes, so a structure embedding a dozen of them
/// outgrew the kernel stack a mount moved it across.
#[test]
fn an_arena_costs_its_holder_a_word_and_allocates_nothing_while_empty() {
    assert_eq!(
        core::mem::size_of::<Nodes<Big>>(),
        core::mem::size_of::<usize>()
    );
    assert_eq!(
        core::mem::size_of::<Arena<Big>>(),
        core::mem::size_of::<usize>()
    );
    let mut nodes = Nodes::<Big>::new();
    nodes.reserve(0).expect("nothing to reserve");
    assert!(
        nodes.core.is_none(),
        "an empty reservation allocates nothing"
    );
    assert_eq!(nodes.bytes(), 0);
    let mut nodes = filled(3);
    nodes.fit(0).expect("emptied");
    assert!(nodes.core.is_none(), "fitted to nothing, its core goes too");
    let undo = nodes.reserve(5).expect("room");
    nodes.undo(undo);
    assert!(
        nodes.core.is_none(),
        "an undone first reservation leaves no core"
    );
}

/// A node made in reserved room is the type's empty node, and none is made
/// without room.
#[test]
fn a_node_is_made_empty_where_it_lies() {
    let mut nodes = filled(2);
    let fresh = nodes.push_empty();
    assert_eq!(fresh, None, "no room reserved");
    nodes.reserve(1).expect("room");
    let at = nodes.push_empty().expect("room reserved");
    assert_eq!(nodes.get(at), Some(&Big::tagged(usize::MAX)));
}

#[test]
fn every_index_has_one_place_and_chunks_tile_the_indices() {
    let full = Nodes::<Big>::FULL;
    assert_eq!(full, 16, "1000-byte nodes, 16 KiB chunks");
    let mut expected = (0, 0);
    for index in 0..full * 5 {
        let (chunk, at) = Nodes::<Big>::locate(index);
        assert_eq!((chunk, at), expected, "index {index}");
        assert!(at < Nodes::<Big>::chunk_len(chunk));
        assert_eq!(Nodes::<Big>::slots_below(chunk) + at, index);
        expected = if at + 1 == Nodes::<Big>::chunk_len(chunk) {
            (chunk + 1, 0)
        } else {
            (chunk, at + 1)
        };
    }
}

#[test]
fn a_small_arena_allocates_at_most_twice_what_it_holds() {
    for count in 1..=Nodes::<Big>::FULL {
        let nodes = filled(count);
        assert!(
            nodes.capacity() <= 2 * count,
            "{count} nodes, {} slots",
            nodes.capacity()
        );
    }
    let one = filled(1);
    assert_eq!(one.capacity(), 1);
}

#[test]
fn no_chunk_is_larger_than_the_chunk_bound_and_growth_moves_nothing() {
    let mut nodes = filled(1);
    let first = core::ptr::from_ref(nodes.get(0).expect("held"));
    nodes.reserve(500).expect("room");
    for tag in 1..500 {
        assert_eq!(push(&mut nodes, tag), Some(tag));
    }
    assert!(core::ptr::eq(first, nodes.get(0).expect("held")));
    for held in chunks(&nodes) {
        assert!(held.capacity() * core::mem::size_of::<Big>() <= CHUNK_BYTES);
    }
}

#[test]
fn a_push_into_unreserved_room_is_refused_without_allocating() {
    let mut nodes = filled(3);
    let slots = nodes.capacity();
    for tag in 3..slots {
        assert_eq!(push(&mut nodes, tag), Some(tag));
    }
    assert_eq!(push(&mut nodes, slots), None);
    assert_eq!(nodes.len(), slots);
}

#[test]
fn disjoint_borrows_span_chunks_and_refuse_repeats_and_strays() {
    let mut nodes = filled(100);
    let [Some(far), Some(near), Some(middle)] = nodes.disjoint_mut([70, 3, 33]) else {
        panic!("three distinct held nodes");
    };
    assert_eq!((far.tag, near.tag, middle.tag), (70, 3, 33));
    far.tag = 1;
    middle.tag = 2;
    assert_eq!(nodes.get(70).map(|n| n.tag), Some(1));
    assert_eq!(nodes.get(33).map(|n| n.tag), Some(2));
    assert!(nodes.disjoint_mut([5, 5]).iter().all(Option::is_none));
    assert!(nodes.disjoint_mut([5, 100]).iter().all(Option::is_none));
    let [Some(left), Some(right)] = nodes.disjoint_mut([17, 18]) else {
        panic!("neighbours in one chunk");
    };
    assert_eq!((left.tag, right.tag), (17, 18));
}

#[test]
fn swap_exchanges_across_and_within_chunks() {
    let mut nodes = filled(60);
    nodes.swap(0, 59);
    nodes.swap(20, 21);
    assert_eq!(nodes.get(0).map(|n| n.tag), Some(59));
    assert_eq!(nodes.get(59).map(|n| n.tag), Some(0));
    assert_eq!(nodes.get(20).map(|n| n.tag), Some(21));
    nodes.swap(4, 4);
    assert_eq!(nodes.get(4).map(|n| n.tag), Some(4));
}

#[test]
fn fit_gives_back_every_chunk_past_what_it_keeps() {
    let mut nodes = filled(200);
    nodes.reserve(300).expect("room");
    nodes.fit(40).expect("fitted");
    assert_eq!(nodes.len(), 40);
    assert_eq!(
        nodes.capacity(),
        Nodes::<Big>::slots_below(chunks(&nodes).len())
    );
    assert!(nodes.capacity() < 40 + Nodes::<Big>::FULL);
    let (_, held, list) = room(&nodes);
    assert_eq!(list, held);
    assert!((0..40).all(|tag| nodes.get(tag).map(|n| n.tag) == Some(tag)));
    nodes.fit(0).expect("emptied");
    assert_eq!((nodes.len(), room(&nodes)), (0, (0, 0, 0)));
}

#[test]
fn a_block_opens_from_both_ends_across_every_chunk() {
    for count in [1, 2, 3, 17, 40, 65] {
        let mut nodes = filled(count);
        let mut block = nodes.block();
        let mut low = 0;
        let mut high = count;
        let mut from_low = true;
        while block.len() > 0 {
            assert_eq!(block.base(), low);
            assert_eq!(block.first().map(|n| n.tag), Some(low));
            assert_eq!(block.last().map(|n| n.tag), Some(high - 1));
            let taken = if from_low {
                low += 1;
                block.take_first().map(|n| n.tag)
            } else {
                high -= 1;
                block.take_last().map(|n| n.tag)
            };
            assert_eq!(taken, Some(if from_low { low - 1 } else { high }));
            from_low = !from_low;
        }
        assert!(block.take_first().is_none() && block.take_last().is_none());
    }
}

#[test]
fn a_block_reaches_and_swaps_any_unopened_node() {
    let mut nodes = filled(70);
    let mut block = nodes.block();
    let opened = block.take_first().expect("first");
    let last = block.take_last().expect("last");
    assert_eq!((opened.tag, last.tag), (0, 69));
    for local in 0..block.len() {
        assert_eq!(block.get(local).map(|n| n.tag), Some(local + 1));
    }
    assert!(block.get(block.len()).is_none());
    block.swap(0, 67);
    block.swap(10, 40);
    block.swap(66, 2);
    assert_eq!(block.get(0).map(|n| n.tag), Some(68));
    assert_eq!(block.get(67).map(|n| n.tag), Some(1));
    assert_eq!(block.get(10).map(|n| n.tag), Some(41));
    assert_eq!(block.get(40).map(|n| n.tag), Some(11));
    assert_eq!(block.get(66).map(|n| n.tag), Some(3));
    assert_eq!(block.get(2).map(|n| n.tag), Some(67));
    if let Some(node) = block.get_mut(5) {
        node.tag = 999;
    }
    opened.tag = 1000;
    assert_eq!(nodes.get(6).map(|n| n.tag), Some(999));
    assert_eq!(nodes.get(0).map(|n| n.tag), Some(1000));
}

#[test]
fn the_arena_reuses_freed_nodes_before_it_grows() {
    let mut arena: Arena<Big> = Arena::new();
    arena.reserve(4).expect("room");
    assert_eq!(arena.nodes.capacity(), 4, "chunks of one, one and two");
    let nodes: [u32; 4] = core::array::from_fn(|tag| take(&mut arena, tag));
    assert_eq!(nodes, [0, 1, 2, 3]);
    arena.free(nodes[1]);
    assert_eq!(arena.live(), 3);
    arena.reserve(1).expect("a freed node serves");
    let bytes = arena.bytes();
    assert_eq!(take(&mut arena, 9), nodes[1]);
    assert_eq!(arena.bytes(), bytes);
    assert_eq!(take(&mut arena, 9), NIL, "nothing reserved");
}

#[test]
fn a_clone_holds_its_room_whole() {
    let nodes = filled(20);
    let mut copy = nodes.try_clone().expect("cloned");
    assert_eq!(copy.capacity(), nodes.capacity());
    for tag in 20..copy.capacity() {
        assert_eq!(push(&mut copy, tag), Some(tag));
    }
    assert!((0..20).all(|tag| copy.get(tag) == nodes.get(tag)));
}

#[test]
fn an_undone_reservation_holds_exactly_the_room_it_had() {
    // A list grown exactly among the small chunks, and one with room to spare
    // among the full ones.
    let mut roomy = filled(16);
    roomy.reserve(1).expect("room");
    let (_, held, list) = room(&roomy);
    assert!(list > held);
    for mut nodes in [filled(1), filled(3), roomy] {
        let (held, before) = (nodes.len(), room(&nodes));
        for more in [1, 2, 20, 100] {
            let undo = nodes.reserve(more).expect("room");
            assert!(nodes.capacity() >= held + more, "{held} held, {more} more");
            nodes.undo(undo);
            assert_eq!(room(&nodes), before, "{held} held, {more} more");
        }
        assert!((0..held).all(|tag| nodes.get(tag).map(|node| node.tag) == Some(tag)));
    }
}

#[test]
fn a_pair_refused_by_its_second_arena_gives_the_first_its_room_back() {
    let mut first: Arena<Big> = Arena::new();
    let mut second: Arena<Big> = Arena::new();
    first.reserve(3).expect("room");
    let bytes = first.bytes();
    assert_eq!(
        reserve_both(&mut first, &mut second, (40, usize::MAX)),
        Err(TryReserveError::CapacityOverflow)
    );
    assert_eq!((first.bytes(), second.bytes()), (bytes, 0));
    reserve_both(&mut first, &mut second, (40, 2)).expect("room for both");
    assert!(first.nodes.capacity() >= 40 && second.nodes.capacity() >= 2);
}
