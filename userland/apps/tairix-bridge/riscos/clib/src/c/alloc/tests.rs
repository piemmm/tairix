use alloc::vec::Vec;

use super::*;
use crate::c::testing::{Client, HEAP, HEAP_ARENA_WORDS as ARENA_WORDS, STATICS};
use crate::kernel::statics::at as kernel_at;

/// A free block's size word.
const fn freed(size: u32) -> u32 {
    size | FREE
}

fn client() -> Client {
    Client::with_heap()
}

fn header(client: &mut Client, data: u32) -> [u32; 2] {
    [client.word(data - 8), client.word(data - 4)]
}

#[test]
fn sizes_are_words_four_bytes_at_least_and_blocks_follow_their_headers() {
    let mut client = client();
    let mut expected = HEAP + 8;
    for (n, size) in [
        (0, 4),
        (1, 4),
        (4, 4),
        (5, 8),
        (7, 8),
        (9, 12),
        (13, 16),
        (17, 20),
        (100, 100),
    ] {
        let data = client.answer(malloc, &[n]);
        assert_eq!(data, expected, "malloc({n})");
        assert_eq!(header(&mut client, data), [GUARD, size], "malloc({n})");
        expected = data + size + 8;
    }
}

#[test]
fn a_freed_size_is_reused_last_in_first_out_its_first_word_the_link() {
    let mut client = client();
    let blocks: Vec<u32> = (0..5).map(|_| client.answer(malloc, &[16])).collect();
    let (b, d) = (blocks[1], blocks[3]);
    client.call(free, &[b]);
    client.call(free, &[d]);
    assert_eq!(header(&mut client, d), [GUARD, freed(16)]);
    assert_eq!(
        client.word(d),
        b - 8,
        "the link to the next free block's header"
    );
    assert_eq!(client.answer(malloc, &[16]), d);
    assert_eq!(client.answer(malloc, &[16]), b);
    assert_eq!(
        client.answer(malloc, &[16]),
        blocks[4] + 24,
        "nothing free: the top"
    );
    client.call(free, &[blocks[2]]);
    let small = client.answer(malloc, &[8]);
    assert_eq!(small, blocks[2], "a larger free block, too small to split");
    assert_eq!(header(&mut client, small), [GUARD, 16]);
}

#[test]
fn realloc_keeps_a_shrinking_block_in_place_a_word_over_and_splits_what_it_can() {
    let mut client = client();
    for (n, size) in [
        (50, 56),
        (100, 104),
        (196, 200),
        (190, 200),
        (20, 24),
        (8, 12),
        (1, 8),
    ] {
        let data = client.answer(malloc, &[200]);
        client.answer(malloc, &[4]);
        assert_eq!(client.answer(realloc, &[data, n]), data, "200 to {n}");
        assert_eq!(header(&mut client, data), [GUARD, size], "200 to {n}");
        if size < 200 {
            assert_eq!(
                header(&mut client, data + size + 8),
                [GUARD, freed(200 - size - 8)]
            );
        }
    }
}

#[test]
fn realloc_grows_in_place_at_the_top_and_moves_otherwise_keeping_the_data() {
    let mut client = client();
    let last = client.answer(malloc, &[16]);
    client.put(last, b"0123456789ABCDEF");
    assert_eq!(client.answer(realloc, &[last, 40]), last, "at the top");
    assert_eq!(header(&mut client, last), [GUARD, 44]);
    client.answer(malloc, &[4]);
    let moved = client.answer(realloc, &[last, 100]);
    assert_ne!(moved, last);
    assert_eq!(
        header(&mut client, moved),
        [GUARD, 100],
        "sized as malloc sizes"
    );
    assert_eq!(client.bytes(moved, 16), b"0123456789ABCDEF");
    assert_eq!(
        header(&mut client, last)[1],
        freed(44),
        "the old block freed"
    );
    assert_eq!(
        client.answer(realloc, &[0, 10]),
        last,
        "realloc(NULL) is malloc, into the freed 44"
    );
    assert_eq!(client.answer(realloc, &[moved, 0]), 0);
    assert_eq!(header(&mut client, moved)[1], freed(100));
}

#[test]
fn calloc_zeroes_and_impossible_sizes_get_nothing() {
    let mut client = client();
    let data = client.answer(malloc, &[16]);
    client.put(data, &[0xAA; 16]);
    client.call(free, &[data]);
    assert_eq!(client.answer(calloc, &[5, 3]), data);
    assert_eq!(client.bytes(data, 15), [0; 15]);
    assert_eq!(
        client.answer(calloc, &[0x1_0000, 0x1_0000]),
        0,
        "the product overflows"
    );
    assert_eq!(client.answer(malloc, &[0x7FFF_FFF0]), 0);
    assert_eq!(client.answer(malloc, &[u32::MAX]), 0);
    client.call(free, &[0]);
}

#[test]
fn freed_neighbours_merge_before_the_heap_grows() {
    let mut client = client();
    let fill = (4 * ARENA_WORDS - ARENA_END_BLOCK) / 24;
    let blocks: Vec<u32> = (0..fill).map(|_| client.answer(malloc, &[16])).collect();
    for &block in &blocks[..20] {
        client.call(free, &[block]);
    }
    let heap = client.word(STATICS + kernel_at::HEAP);
    let big = client.answer(malloc, &[200]);
    assert_eq!(big, blocks[0], "the first twenty merged");
    assert_eq!(client.word(STATICS + kernel_at::HEAP), heap, "not grown");
}

#[test]
fn the_heap_grows_by_kernel_alloc_continuing_its_arena_or_opening_another() {
    let mut client = client();
    let heap = client.word(STATICS + kernel_at::HEAP);
    let big = client.answer(malloc, &[12_000]);
    assert!(big > HEAP && big < heap + 0x4000, "continued: {big:#x}");
    assert!(client.word(STATICS + kernel_at::HEAP) > heap, "grown");
    let statics = Statics::of_base(STATICS);
    let (taken, _) = heap::take(&mut client.session.kernel.os(), statics, 2048).expect("taken");
    let other = client.answer(malloc, &[20_000]);
    assert!(
        other > taken + 8192,
        "a new arena past the client's own claim"
    );
    client.call(free, &[big]);
    client.call(free, &[other]);
    let reused = client.answer(malloc, &[30_000]);
    assert_eq!(reused, other, "the walk crossed into the second arena");
}

/// The return code a misuse left, the program ended.
fn ended(client: &mut Client, entry: tairix_riscos_kernel::module::Code, arguments: &[u32]) -> u32 {
    let _ = client.outcome(entry, arguments);
    client.word(STATICS + kernel_at::RETURN_CODE)
}

#[test]
fn freeing_twice_or_a_foreign_block_ends_the_program() {
    let mut client = client();
    let data = client.answer(malloc, &[24]);
    client.call(free, &[data]);
    assert_eq!(ended(&mut client, free, &[data]), 1, "twice");
    let mut client = self::client();
    assert_eq!(
        ended(&mut client, free, &[HEAP - 0x100]),
        1,
        "outside the heap"
    );
    let mut client = self::client();
    let data = client.answer(malloc, &[24]);
    client.put(data - 8, &0x1234_5678_u32.to_le_bytes());
    assert_eq!(ended(&mut client, free, &[data]), 1, "a header overwritten");
}

#[test]
fn a_poisoned_free_list_ends_the_program_rather_than_handing_out_its_link() {
    let mut client = client();
    let a = client.answer(malloc, &[16]);
    client.answer(malloc, &[16]);
    client.call(free, &[a]);
    client.put(a, &(STATICS + 0x10).to_le_bytes());
    let b = client.answer(malloc, &[16]);
    assert_eq!(b, a, "the head itself is sound");
    client.call(free, &[b]);
    client.put(b - 4, &0x0BAD_0000_u32.to_le_bytes());
    assert_eq!(
        ended(&mut client, malloc, &[16]),
        1,
        "a listed block no longer free"
    );
}

#[test]
fn a_size_word_no_block_could_have_ends_the_program_freed_or_resized() {
    for size in [0u32, 0x7FFF_FFF0, 0xFFFF_FFF8] {
        let mut client = client();
        let data = client.answer(malloc, &[16]);
        client.put(data - 4, &size.to_le_bytes());
        assert_eq!(ended(&mut client, free, &[data]), 1, "freed: {size:#x}");
        let mut client = self::client();
        let data = client.answer(malloc, &[16]);
        client.put(data - 4, &size.to_le_bytes());
        assert_eq!(
            ended(&mut client, realloc, &[data, 32]),
            1,
            "resized: {size:#x}"
        );
    }
}

#[test]
fn a_merge_meeting_a_free_block_of_no_size_ends_the_program() {
    let mut client = client();
    let fill = (4 * ARENA_WORDS - ARENA_END_BLOCK) / 24;
    let blocks: Vec<u32> = (0..fill).map(|_| client.answer(malloc, &[16])).collect();
    for &block in &blocks[..20] {
        client.call(free, &[block]);
    }
    // The first freed block made of no size, a sound block in use where
    // its data was.
    client.put(blocks[0] - 4, &freed(0).to_le_bytes());
    client.put(
        blocks[0],
        &[GUARD.to_le_bytes(), 8u32.to_le_bytes()].concat(),
    );
    assert_eq!(ended(&mut client, malloc, &[200]), 1);
}

#[test]
fn an_aligned_block_of_no_size_is_one_free_takes_back() {
    let mut client = client();
    let aligned = client.answer(aligned_alloc, &[16, 0]);
    assert_eq!(
        aligned,
        HEAP + 32,
        "a free block's room past the first data"
    );
    assert_eq!(header(&mut client, aligned), [GUARD, LEAST]);
    client.call(free, &[aligned]);
    assert_eq!(header(&mut client, aligned)[1], freed(LEAST));
}

#[test]
fn a_heap_never_begun_answers_null() {
    let mut client = Client::new();
    assert_eq!(client.answer(malloc, &[16]), 0);
    assert_eq!(client.answer(calloc, &[4, 4]), 0);
}

#[test]
fn a_first_arena_too_small_for_its_end_block_leaves_the_heap_unbegun() {
    let mut client = Client::new();
    let statics = Statics::of_base(STATICS);
    {
        let mut os = client.session.kernel.os();
        statics.write(&mut os, kernel_at::HEAP, HEAP).expect("heap");
        statics
            .write(&mut os, kernel_at::HEAP_LIMIT, HEAP + 12)
            .expect("limit");
        assert!(!begin(&mut os, statics).expect("answered"));
    }
    assert_eq!(client.answer(malloc, &[4]), 0);
    assert_eq!(client.answer(malloc, &[4]), 0, "never past the limit");
}

#[test]
fn a_module_heap_whose_arenas_go_round_is_kept_and_left_at_once() {
    let mut client = Client::new();
    let statics = Statics::of_base(STATICS);
    let arena = {
        let mut os = client.session.kernel.os();
        let arena = os.rma_claim(64).expect("an arena");
        statics
            .write(&mut os, kernel_at::MODULE, 1)
            .expect("a module's");
        start(&mut os, statics, arena, 16).expect("laid");
        let end = Block(arena);
        end.mark(&mut os, (ARENA_END_BLOCK - HEADER) | ARENA_END)
            .expect("marked");
        end.set_link(&mut os, arena).expect("back to itself");
        statics
            .write(&mut os, at::HEAP_TOP, arena + 32)
            .expect("a top the walk never meets");
        statics
            .write(&mut os, at::HEAP_BYTES, u32::MAX)
            .expect("steps for a whole word");
        arena
    };
    // Stopped at its first slice of work: the walk must see it go round
    // before that.
    client.session.allow_words(Some(0));
    let mut os = client.session.kernel.os();
    release(&mut os, statics).expect("left at once");
    assert_eq!(statics.read(&mut os, at::HEAP_FIRST).expect("read"), arena);
    os.rma_free(arena).expect("still claimed");
}
