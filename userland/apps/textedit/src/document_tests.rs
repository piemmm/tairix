//! Unit tests for the document store, against a plain byte vector as the
//! model of what it must hold.

use alloc::vec::Vec;
use core::ops::ControlFlow;

use super::{
    count_newlines, rows_of, Change, Document, LineBounds, ACTIVE_CHUNK, MAX_PIECE, MAX_ROW_BYTES,
};

/// A small deterministic generator for the property tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            usize::try_from(self.next() % n as u64).expect("below a usize")
        }
    }
}

/// The rows each line of `model` takes, a line's CRLF not counted.
fn line_rows(model: &[u8]) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut start = 0;
    for (at, _) in model.iter().enumerate().filter(|(_, &b)| b == b'\n') {
        let end = if at > start && model[at - 1] == b'\r' {
            at - 1
        } else {
            at
        };
        rows.push(rows_of(end - start));
        start = at + 1;
    }
    rows.push(rows_of(model.len() - start));
    rows
}

/// The document's row observations, checked against `model`.
fn assert_rows_match(doc: &Document, model: &[u8]) {
    let rows = line_rows(model);
    let firsts: Vec<usize> = rows
        .iter()
        .scan(0, |at, &n| {
            let first = *at;
            *at += n;
            Some(first)
        })
        .collect();
    let total: usize = rows.iter().sum();
    assert_eq!(doc.row_count(), total);
    let lines = rows.len();
    for line in (0..lines.min(16))
        .chain(lines.saturating_sub(16)..=lines)
        .chain((0..=lines).step_by(lines / 32 + 1))
    {
        let want = firsts.get(line).copied().unwrap_or(total);
        assert_eq!(doc.first_row(line), want, "line {line}");
    }
    for row in (0..total.min(24))
        .chain(total.saturating_sub(24)..total + 2)
        .chain((0..total).step_by(total / 32 + 1))
    {
        let line = firsts.partition_point(|&first| first <= row) - 1;
        assert_eq!(doc.line_of_row(row), (line, firsts[line]), "row {row}");
    }
}

/// Every observation the document offers, checked against `model`.
fn assert_matches(doc: &Document, model: &[u8]) {
    assert_rows_match(doc, model);
    assert_eq!(doc.len(), model.len());
    assert_eq!(doc.to_vec().expect("room"), model);
    let feeds: Vec<usize> = model
        .iter()
        .enumerate()
        .filter(|(_, &b)| b == b'\n')
        .map(|(at, _)| at)
        .collect();
    assert_eq!(doc.line_count(), feeds.len() + 1);
    let last = feeds.len() + 1;
    let sampled = (0..last.min(16))
        .chain(last.saturating_sub(16)..=last)
        .chain((0..=last).step_by(last / 32 + 1));
    for line in sampled {
        let start = if line == 0 {
            0
        } else {
            feeds.get(line - 1).map_or(model.len(), |at| at + 1)
        };
        assert_eq!(doc.line_start(line), start, "line {line}");
    }
    for offset in [
        0,
        model.len() / 3,
        model.len() / 2,
        model.len().saturating_sub(1),
        model.len(),
    ] {
        let line = model[..offset]
            .iter()
            .fold(0, |n, &b| n + usize::from(b == b'\n'));
        assert_eq!(doc.line_of(offset), line, "offset {offset}");
        assert_eq!(doc.byte(offset), model.get(offset).copied());
    }
}

#[test]
fn an_empty_document_has_one_empty_line() {
    let doc = Document::new();
    assert!(doc.is_empty());
    assert_eq!(doc.line_count(), 1);
    assert_eq!(
        doc.line_bounds(0),
        LineBounds {
            start: 0,
            end: 0,
            next: 0
        }
    );
    assert_eq!(doc.byte(0), None);
    assert_matches(&doc, b"");
}

#[test]
fn a_load_keeps_every_byte_and_line_across_chunks() {
    let mut model = Vec::new();
    let mut chunks = Vec::new();
    for size in [10, MAX_PIECE * 3 + 7, 1, 0, MAX_PIECE] {
        let chunk: Vec<u8> = (0..size)
            .map(|at| {
                if at % 37 == 0 {
                    b'\n'
                } else {
                    b'a' + u8::try_from(at % 26).expect("a letter")
                }
            })
            .collect();
        model.extend_from_slice(&chunk);
        chunks.push(chunk);
    }
    let doc = Document::from_chunks(chunks).expect("loads");
    assert_matches(&doc, &model);
}

#[test]
fn line_bounds_hide_a_crlf_but_keep_a_lone_cr() {
    let mut doc = Document::new();
    doc.replace(0..0, b"one\r\ntwo\rthree\nfour\r")
        .expect("room");
    assert_eq!(
        doc.line_bounds(0),
        LineBounds {
            start: 0,
            end: 3,
            next: 5
        }
    );
    assert_eq!(
        doc.line_bounds(1),
        LineBounds {
            start: 5,
            end: 14,
            next: 15
        }
    );
    assert_eq!(
        doc.line_bounds(2),
        LineBounds {
            start: 15,
            end: 20,
            next: 20
        }
    );
}

#[test]
fn edits_hold_against_the_model_through_undo_and_redo() {
    let mut rng = Rng(0x5eed_0000_0000_0001);
    let mut doc = Document::new();
    let mut model: Vec<u8> = Vec::new();
    let mut history: Vec<(Change, Vec<u8>)> = Vec::new();
    for step in 0..600 {
        let start = rng.below(model.len() + 1);
        let end = (start + rng.below(40)).min(model.len());
        let len = rng.below(if step % 50 == 0 { ACTIVE_CHUNK * 2 } else { 12 });
        let text: Vec<u8> = (0..len).map(|_| b"ab\n\r\xc3\x80 "[rng.below(7)]).collect();
        let before = model.clone();
        let change = doc.replace(start..end, &text).expect("room");
        model.splice(start..end, text.iter().copied());
        history.push((change, before));
        assert_matches(&doc, &model);
        if step % 7 == 0 {
            // Undo the last few changes, then redo them, and land where we
            // were.
            let depth = rng.below(history.len().min(5) + 1);
            let now = model.clone();
            for (change, before) in history.iter().rev().take(depth) {
                doc.revert(change);
                assert_matches(&doc, before);
            }
            for (change, _) in history.iter().rev().take(depth).rev() {
                doc.reapply(change);
            }
            assert_matches(&doc, &now);
        }
    }
}

#[test]
fn a_typing_run_grows_one_piece_and_undoes_as_one_change() {
    let mut doc = Document::new();
    let mut run = doc.replace(0..0, b"h").expect("room");
    for (at, byte) in b"ello, world".iter().enumerate() {
        let next = doc.replace(at + 1..at + 1, &[*byte]).expect("room");
        run.absorb(next).expect("it continues the run");
    }
    assert_eq!(doc.to_vec().expect("room"), b"hello, world");
    assert_eq!(
        doc.count_pieces(0..doc.len()),
        1,
        "typing coalesced into one piece"
    );
    assert_eq!(run.inserted.len(), 1);
    doc.revert(&run);
    assert!(doc.is_empty());
    doc.reapply(&run);
    assert_eq!(doc.to_vec().expect("room"), b"hello, world");
}

#[test]
fn a_typing_run_past_the_scan_bound_redoes_in_bounded_pieces() {
    let mut doc = Document::new();
    let mut run = doc.replace(0..0, b"a").expect("room");
    for at in 1..MAX_PIECE * 2 + 3 {
        let next = doc.replace(at..at, b"b").expect("room");
        run.absorb(next).expect("it continues the run");
    }
    assert!(
        run.inserted.iter().all(|piece| piece.len() <= MAX_PIECE),
        "a merged piece past the bound"
    );
    doc.revert(&run);
    doc.reapply(&run);
    let mut pieces = Vec::new();
    doc.nodes.collect(doc.root, &mut pieces);
    assert!(
        pieces.iter().all(|piece| piece.len() <= MAX_PIECE),
        "a redone piece past the bound"
    );
    assert_eq!(doc.len(), MAX_PIECE * 2 + 3);
}

#[test]
fn a_change_elsewhere_does_not_absorb() {
    let mut doc = Document::new();
    let mut first = doc.replace(0..0, b"abc").expect("room");
    let apart = doc.replace(1..1, b"x").expect("room");
    assert!(first.absorb(apart).is_err());
    let deletion = doc.replace(0..1, b"").expect("room");
    assert!(first.absorb(deletion).is_err());
}

#[test]
fn a_paste_larger_than_a_chunk_spills_across_chunks() {
    let mut doc = Document::new();
    let text: Vec<u8> = (0..ACTIVE_CHUNK * 3 + 5)
        .map(|at| u8::try_from(at % 251).expect("a byte"))
        .collect();
    doc.replace(0..0, b"[]").expect("room");
    doc.replace(1..1, &text).expect("room");
    let mut model = b"[]".to_vec();
    model.splice(1..1, text.iter().copied());
    assert_matches(&doc, &model);
}

#[test]
fn a_snapshot_is_the_document_as_it_was_whatever_follows() {
    let mut doc =
        Document::from_chunks(alloc::vec![b"first\n".to_vec(), b"second".to_vec()]).expect("loads");
    let snapshot = doc.snapshot().expect("room");
    doc.replace(0..5, b"changed").expect("room");
    doc.replace(doc.len()..doc.len(), b" and more")
        .expect("room");
    let read = |from| {
        let mut out = Vec::new();
        snapshot.walk(from, |slice| {
            out.extend_from_slice(slice);
            ControlFlow::Continue(())
        });
        out
    };
    assert_eq!(read(0), b"first\nsecond");
    assert_eq!(read(3), b"st\nsecond");
    assert_eq!(read(6), b"second", "a read may start on a piece boundary");
    assert_eq!(read(12), b"");
    assert_eq!(snapshot.len(), 12);
    assert_eq!(doc.to_vec().expect("room"), b"changed\nsecond and more");
}

#[test]
fn a_gather_hands_over_full_runs_then_the_rest_and_stops_at_a_refusal() {
    let mut doc = Document::from_chunks(alloc::vec![b"abcdefg".to_vec()]).expect("loads");
    doc.replace(3..3, b"XY").expect("room");
    let snapshot = doc.snapshot().expect("room");
    let whole = b"abcXYdefg";

    let mut run = Vec::with_capacity(4);
    let mut runs: Vec<Vec<u8>> = Vec::new();
    snapshot
        .gather(&mut run, |bytes| {
            runs.push(bytes.to_vec());
            Ok::<(), ()>(())
        })
        .expect("nothing refused");
    assert_eq!(runs, [b"abcX".to_vec(), b"Ydef".to_vec(), b"g".to_vec()]);
    assert_eq!(run.capacity(), 4, "the run is reused, never grown");

    let mut pieces: Vec<Vec<u8>> = Vec::new();
    snapshot
        .gather(&mut Vec::new(), |bytes| {
            pieces.push(bytes.to_vec());
            Ok::<(), ()>(())
        })
        .expect("nothing refused");
    assert_eq!(pieces.len(), 3, "no capacity hands over each piece");
    assert_eq!(pieces.concat(), whole);

    let mut calls = 0;
    let refused = snapshot.gather(&mut Vec::with_capacity(2), |_| {
        calls += 1;
        if calls == 2 {
            Err("full")
        } else {
            Ok(())
        }
    });
    assert_eq!(refused, Err("full"));
    assert_eq!(calls, 2, "nothing is written after a refusal");
}

#[test]
fn a_walk_stops_when_asked_and_starts_where_asked() {
    let doc = Document::from_chunks(alloc::vec![b"abc".to_vec(), b"defg".to_vec()]).expect("loads");
    let mut seen = Vec::new();
    doc.walk(2, |slice| {
        seen.extend_from_slice(slice);
        if seen.len() >= 3 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    assert_eq!(
        seen, b"cdefg",
        "the walk stops after the slice that satisfied it"
    );
    let mut copied = Vec::new();
    doc.copy_range(1..5, &mut copied).expect("room");
    assert_eq!(copied, b"bcde");
}

#[test]
fn owned_chunks_replace_a_range_and_typing_after_them_lands_right() {
    let mut doc = Document::new();
    doc.replace(0..0, b"head tail").expect("room");
    let change = doc
        .replace_owned(4..5, alloc::vec![b"-one\n".to_vec(), b"two-".to_vec()])
        .expect("room");
    assert_eq!(doc.to_vec().expect("room"), b"head-one\ntwo-tail");
    assert_eq!(doc.line_count(), 2);
    // The active chunk was sealed first, so text typed now is its own.
    let end = doc.len();
    let typed = doc.replace(end..end, b"!").expect("room");
    assert_eq!(doc.to_vec().expect("room"), b"head-one\ntwo-tail!");
    doc.revert(&typed);
    doc.revert(&change);
    assert_eq!(doc.to_vec().expect("room"), b"head tail");
    doc.reapply(&change);
    assert_eq!(doc.to_vec().expect("room"), b"head-one\ntwo-tail");
}

#[test]
fn replacing_past_the_end_clamps_to_it() {
    let mut doc = Document::new();
    doc.replace(0..0, b"abc").expect("room");
    let change = doc.replace(2..99, b"Z").expect("room");
    assert_eq!(doc.to_vec().expect("room"), b"abZ");
    assert_eq!(change.removed_len(), 1);
    assert_eq!(change.inserted_len(), 1);
}

#[test]
fn line_feeds_are_counted_exactly_a_word_at_a_time() {
    let mut rng = Rng(7);
    for len in 0..70 {
        let bytes: Vec<u8> = (0..len)
            .map(|_| [b'\n', 0x0a ^ 0x80, 0x8a, 0, 0xff, b'a'][rng.below(6)])
            .collect();
        let expected = bytes.iter().fold(0, |n, &b| n + usize::from(b == b'\n'));
        assert_eq!(count_newlines(&bytes), expected, "{bytes:?}");
    }
}

#[test]
fn a_large_document_keeps_a_shallow_tree() {
    fn depth(nodes: &super::Nodes, tree: super::Tree) -> usize {
        tree.map_or(0, |id| {
            let node = nodes.node(id);
            1 + depth(nodes, node.left).max(depth(nodes, node.right))
        })
    }
    let chunk = alloc::vec![b'x'; MAX_PIECE * 2000];
    let doc = Document::from_chunks(alloc::vec![chunk]).expect("loads");
    let deepest = depth(&doc.nodes, doc.root);
    assert!(deepest <= 64, "2000 pieces reach depth {deepest}");
}

#[test]
fn no_piece_outgrows_the_scan_bound_however_it_was_made() {
    let mut doc = Document::new();
    let text = alloc::vec![b'y'; MAX_PIECE * 3 + 1];
    doc.replace(0..0, &text).expect("room");
    for _ in 0..MAX_PIECE + 10 {
        let end = doc.len();
        doc.replace(end..end, b"z").expect("room");
    }
    let mut pieces = Vec::new();
    doc.nodes.collect(doc.root, &mut pieces);
    assert!(
        pieces.iter().all(|piece| piece.len() <= MAX_PIECE),
        "a piece past the bound"
    );
}

#[test]
fn a_snapshot_seals_what_was_typed_and_keeps_the_active_room() {
    let mut doc = Document::new();
    for round in 0..200 {
        let end = doc.len();
        doc.replace(end..end, b"typed ").expect("room");
        let snapshot = doc.snapshot().expect("room");
        assert_eq!(snapshot.len(), (round + 1) * 6);
    }
    assert!(
        doc.held() <= ACTIVE_CHUNK + 200 * 6,
        "200 snapshots pin {} bytes",
        doc.held()
    );
    assert_eq!(doc.to_vec().expect("room"), b"typed ".repeat(200));
}

#[test]
fn chunks_nothing_names_are_let_go_and_named_ones_kept() {
    let mut doc = Document::from_chunks(alloc::vec![alloc::vec![b'a'; 4096]]).expect("loads");
    let first = doc
        .replace_owned(0..4096, alloc::vec![alloc::vec![b'b'; 4096]])
        .expect("room");
    let second = doc
        .replace_owned(0..4096, alloc::vec![alloc::vec![b'c'; 4096]])
        .expect("room");
    let held = doc.held();
    doc.release_unnamed([&second]).expect("room");
    assert_eq!(doc.held(), held - 4096, "only the first load is unnamed");
    doc.release_unnamed([]).expect("room");
    assert_eq!(doc.held(), 4096, "the document's own text stays");
    assert_eq!(doc.to_vec().expect("room"), alloc::vec![b'c'; 4096]);
    let _ = first;
}

#[test]
fn a_group_undoes_in_the_room_made_for_it() {
    let mut doc = Document::from_chunks(alloc::vec![b"one two three".to_vec()]).expect("loads");
    let changes = [
        doc.replace(0..3, b"1").expect("room"),
        doc.replace(2..5, b"2").expect("room"),
    ];
    doc.room_to_revert(&changes).expect("room");
    let slots = doc.nodes.slots.capacity();
    for change in changes.iter().rev() {
        doc.revert(change);
    }
    assert_eq!(
        doc.nodes.slots.capacity(),
        slots,
        "the undo made no room of its own"
    );
    assert_eq!(doc.to_vec().expect("room"), b"one two three");
    doc.room_to_reapply(&changes).expect("room");
    for change in &changes {
        doc.reapply(change);
    }
    assert_eq!(doc.to_vec().expect("room"), b"1 2 three");
}

#[test]
fn rows_hold_against_the_model_as_long_lines_grow_split_and_join() {
    let mut rng = Rng(0x5eed_0000_0000_0002);
    let mut doc = Document::new();
    let mut model: Vec<u8> = Vec::new();
    let mut long_lines = 0;
    for step in 0..400 {
        let start = rng.below(model.len() + 1);
        let end = (start + rng.below(if step % 9 == 0 { 20_000 } else { 64 })).min(model.len());
        let len = rng.below(if step % 5 == 0 { MAX_ROW_BYTES * 3 } else { 40 });
        // Feeds are rare, so lines run across rows and pieces; CRs are
        // common enough to land before a feed and at a piece's end.
        let text: Vec<u8> = (0..len)
            .map(|_| match rng.below(3000) {
                0 => b'\n',
                1..=3 => b'\r',
                _ => b'x',
            })
            .collect();
        let change = doc.replace(start..end, &text).expect("room");
        let before = model.clone();
        model.splice(start..end, text.iter().copied());
        assert_rows_match(&doc, &model);
        long_lines += usize::from(doc.row_count() > doc.line_count());
        if step % 11 == 0 {
            doc.revert(&change);
            assert_rows_match(&doc, &before);
            doc.reapply(&change);
            assert_rows_match(&doc, &model);
        }
    }
    assert!(
        long_lines > 100,
        "only {long_lines} steps held a line longer than a row"
    );
}

#[test]
fn a_crlf_split_across_pieces_ends_its_line_where_one_row_does() {
    let mut exact = alloc::vec![b'x'; MAX_ROW_BYTES];
    exact.push(b'\r');
    let doc = Document::from_chunks(alloc::vec![exact.clone(), b"\ny".to_vec()]).expect("loads");
    assert_eq!(
        doc.row_count(),
        2,
        "a row's worth, its CRLF, then one more line"
    );
    assert_eq!(doc.line_of_row(1), (1, 1));

    exact.insert(0, b'x');
    let doc = Document::from_chunks(alloc::vec![exact, b"\ny".to_vec()]).expect("loads");
    assert_eq!(doc.row_count(), 3, "one byte more spills onto a second row");
    assert_eq!(doc.first_row(1), 2);
}

#[test]
fn a_crlf_ends_a_middle_line_whichever_piece_holds_its_cr() {
    for extra in [0, 1] {
        let body = alloc::vec![b'x'; MAX_ROW_BYTES + extra];
        let rows = 3 + extra;
        let mut together = b"a\n".to_vec();
        together.extend_from_slice(&body);
        together.extend_from_slice(b"\r\nb");
        let doc = Document::from_chunks(alloc::vec![together]).expect("loads");
        assert_eq!(
            doc.row_count(),
            rows,
            "CR and LF in one piece, {extra} over"
        );
        assert_eq!(doc.first_row(2), rows - 1);

        let mut apart = b"a\n".to_vec();
        apart.extend_from_slice(&body);
        apart.push(b'\r');
        let doc = Document::from_chunks(alloc::vec![apart, b"\nb".to_vec()]).expect("loads");
        assert_eq!(doc.row_count(), rows, "the CR ends a piece, {extra} over");
        assert_eq!(doc.line_of_row(rows - 1), (2, rows - 1));
    }
}

#[test]
fn a_long_line_s_rows_all_belong_to_it() {
    let mut text = b"short\n".to_vec();
    text.extend(core::iter::repeat_n(b'x', 3 * MAX_ROW_BYTES));
    text.extend_from_slice(b"\nend");
    let doc = Document::from_chunks(alloc::vec![text]).expect("loads");
    assert_eq!(doc.row_count(), 5);
    let lines: Vec<(usize, usize)> = (0..6).map(|row| doc.line_of_row(row)).collect();
    assert_eq!(lines, [(0, 0), (1, 1), (1, 1), (1, 1), (2, 4), (2, 4)]);
    assert_eq!(
        [
            doc.first_row(0),
            doc.first_row(1),
            doc.first_row(2),
            doc.first_row(3)
        ],
        [0, 1, 4, 5]
    );
}

/// `model` with each of `matches`, ascending and apart, replaced by `text`.
fn replaced(model: &[u8], matches: &[core::ops::Range<usize>], text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    for range in matches {
        out.extend_from_slice(&model[at..range.start]);
        out.extend_from_slice(text);
        at = range.end;
    }
    out.extend_from_slice(&model[at..]);
    out
}

#[test]
fn replacing_each_match_holds_against_the_model_through_undo_and_redo() {
    let mut rng = Rng(0x5eed_0000_0000_0003);
    for round in 0..60 {
        let len = rng.below(3 * MAX_PIECE) + 1;
        let model: Vec<u8> = (0..len).map(|_| b"ab\n\r"[rng.below(4)]).collect();
        // Load in chunks of every size, so matches meet piece ends anywhere.
        let mut chunks = Vec::new();
        let mut at = 0;
        while at < len {
            let take = (rng.below(MAX_PIECE + 700) + 1).min(len - at);
            chunks.push(model[at..at + take].to_vec());
            at += take;
        }
        let mut doc = Document::from_chunks(chunks).expect("loads");
        let mut matches = Vec::new();
        let mut at = rng.below(8);
        while at < len {
            let end = (at + rng.below(if round % 3 == 0 { 9000 } else { 6 })).min(len);
            matches.push(at..end);
            at = end + rng.below(if round % 2 == 0 { 3 } else { 400 });
        }
        let text: Vec<u8> = (0..rng.below(if round % 5 == 0 { 2 * MAX_PIECE } else { 5 }))
            .map(|_| b"xy\n"[rng.below(3)])
            .collect();
        let change = doc.replace_each(&matches, &text).expect("room");
        let want = replaced(&model, &matches, &text);
        assert_matches(&doc, &want);
        doc.revert(&change);
        assert_matches(&doc, &model);
        doc.reapply(&change);
        assert_matches(&doc, &want);
    }
}

#[test]
fn replacing_each_match_stores_the_replacement_once() {
    let text = b"one two ".repeat(1000);
    let mut doc = Document::from_chunks(alloc::vec![text.clone()]).expect("loads");
    let matches: Vec<_> = (0..1000).map(|at| at * 8..at * 8 + 3).collect();
    let held = doc.held();
    doc.replace_each(&matches, b"uno").expect("room");
    assert_eq!(doc.to_vec().expect("room"), b"uno two ".repeat(1000));
    assert_eq!(doc.active.len(), 3, "one copy for every match");
    assert!(doc.held() <= held + ACTIVE_CHUNK);
}

#[test]
fn replacing_each_match_skips_one_that_overlaps_the_last() {
    let mut doc = Document::from_chunks(alloc::vec![b"abcdef".to_vec()]).expect("loads");
    let change = doc.replace_each(&[0..2, 1..3, 4..5], b"-").expect("room");
    assert_eq!(doc.to_vec().expect("room"), b"-cd-f");
    doc.revert(&change);
    assert_eq!(doc.to_vec().expect("room"), b"abcdef");
    let nothing = doc.replace_each(&[], b"-").expect("room");
    assert_eq!((nothing.removed_len(), nothing.inserted_len()), (0, 0));
}
