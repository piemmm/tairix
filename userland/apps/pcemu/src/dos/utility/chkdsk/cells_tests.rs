//! The chain logic over a FAT and map held in plain vectors.

use super::*;
use alloc::vec;
use alloc::vec::Vec;

struct Plain {
    fat: Vec<u16>,
    map: Vec<u8>,
}

impl Cells for Plain {
    fn entry(&mut self, cluster: u16) -> u16 {
        self.fat[usize::from(cluster)]
    }

    fn set_entry(&mut self, cluster: u16, value: u16) {
        self.fat[usize::from(cluster)] = value;
    }

    fn mark(&mut self, cluster: u16) -> u8 {
        self.map[usize::from(cluster)]
    }

    fn set_mark(&mut self, cluster: u16, mark: u8) {
        self.map[usize::from(cluster)] = mark;
    }
}

const VOLUME: Volume = Volume {
    kind: Kind::Fat12,
    max: 20,
};
const END: u16 = 0xFFF;

/// A FAT of `links`, each `(cluster, entry)`, the rest free.
fn plain(links: &[(u16, u16)]) -> Plain {
    let mut fat = vec![0u16; usize::from(VOLUME.max) + 1];
    fat[0] = 0xFF8;
    fat[1] = END;
    for &(cluster, entry) in links {
        fat[usize::from(cluster)] = entry;
    }
    Plain {
        fat,
        map: vec![0; usize::from(VOLUME.max) + 1],
    }
}

#[test]
fn a_chain_is_marked_to_its_end_its_head_apart() {
    let mut cells = plain(&[(2, 3), (3, 4), (4, END)]);
    let chain = mark_chain(&mut cells, VOLUME, 2, mark::FILE);
    assert_eq!(
        chain,
        Chain {
            clusters: 3,
            ending: Ending::End
        }
    );
    assert_eq!(
        cells.map[2..5],
        [mark::HEAD | mark::FILE, mark::FILE, mark::FILE]
    );
    assert_eq!(
        mark_chain(&mut cells, VOLUME, 0, mark::FILE).ending,
        Ending::BadFirst
    );
    assert_eq!(
        mark_chain(&mut cells, VOLUME, 21, mark::FILE).ending,
        Ending::BadFirst,
        "past the volume"
    );
}

#[test]
fn a_chain_ends_where_it_crosses_another_or_links_to_nothing() {
    let mut cells = plain(&[
        (2, 3),
        (3, END),
        (5, 3),
        (7, 8),
        (8, 0),
        (9, 0xFF7),
        (10, 30),
    ]);
    mark_chain(&mut cells, VOLUME, 2, mark::FILE);
    let crossed = mark_chain(&mut cells, VOLUME, 5, mark::FILE);
    assert_eq!(
        crossed,
        Chain {
            clusters: 1,
            ending: Ending::Crossed
        }
    );
    assert_eq!(cells.map[3], mark::FILE | mark::CROSSED);
    assert_eq!(first_crossed(&mut cells, VOLUME, 5), Some(3));
    assert_eq!(first_crossed(&mut cells, VOLUME, 2), Some(3));
    assert_eq!(
        mark_chain(&mut cells, VOLUME, 7, mark::FILE).ending,
        Ending::BadLink { last: 8 },
        "a free cluster in a chain"
    );
    assert_eq!(
        mark_chain(&mut cells, VOLUME, 9, mark::FILE).ending,
        Ending::BadLink { last: 9 },
        "a cluster marked bad"
    );
    assert_eq!(
        mark_chain(&mut cells, VOLUME, 10, mark::FILE).ending,
        Ending::BadLink { last: 10 },
        "a cluster past the volume"
    );
}

#[test]
fn a_chain_looping_back_on_itself_ends_where_it_meets_itself() {
    let mut cells = plain(&[(2, 3), (3, 4), (4, 2)]);
    assert_eq!(
        mark_chain(&mut cells, VOLUME, 2, mark::DIRECTORY),
        Chain {
            clusters: 3,
            ending: Ending::Crossed
        }
    );
    assert_eq!(
        chain_length(&mut cells, VOLUME, 2),
        u32::from(VOLUME.max),
        "a loop is counted no further than the volume"
    );
    assert_eq!(
        breaks(&mut cells, VOLUME, 2),
        6,
        "the link back counted each time round, as far as the volume"
    );
}

#[test]
fn lost_and_bad_clusters_are_what_no_chain_reached() {
    let mut cells = plain(&[(2, END), (4, 5), (5, END), (6, 0xFF7), (7, END)]);
    mark_chain(&mut cells, VOLUME, 2, mark::FILE);
    assert_eq!(unreached(&mut cells, VOLUME), (1, 3));
    assert_eq!(
        cells.map[4..8],
        [mark::LOST, mark::LOST, mark::BAD, mark::LOST]
    );
}

#[test]
fn lost_clusters_gather_into_chains_and_a_tail_joins_the_head_it_reaches() {
    // 6 -> 7 -> 4 -> 5 is one chain whose middle is met first, 9 and 11 are
    // chains of their own, 13 links nowhere and 15 back into itself.
    let mut cells = plain(&[
        (4, 5),
        (5, END),
        (6, 7),
        (7, 4),
        (9, 10),
        (10, END),
        (11, END),
        (13, 1),
        (15, 16),
        (16, 15),
    ]);
    unreached(&mut cells, VOLUME);
    assert_eq!(gather(&mut cells, VOLUME, true), (5, true));
    let head = mark::HEAD | mark::LOST | mark::FILE;
    assert_eq!(cells.map[4], mark::LOST | mark::FILE, "no longer a head");
    assert_eq!(
        [
            cells.map[6],
            cells.map[9],
            cells.map[11],
            cells.map[13],
            cells.map[15]
        ],
        [head; 5]
    );
    assert_eq!(cells.fat[13], END, "a link to nothing ends its chain");
    assert_eq!(cells.fat[16], END, "a link back into its own chain");
    let heads: Vec<u16> = core::iter::successors(next_head(&mut cells, VOLUME, 2), |&at| {
        next_head(&mut cells, VOLUME, at + 1)
    })
    .collect();
    assert_eq!(heads, [6, 9, 11, 13, 15]);
    assert_eq!(chain_length(&mut cells, VOLUME, 6), 4);
    free_lost(&mut cells, VOLUME);
    assert!([4, 5, 6, 7, 9, 13, 16].iter().all(|&n| cells.fat[n] == 0));
}

#[test]
fn gathering_sound_chains_changes_nothing() {
    let mut cells = plain(&[(4, 5), (5, END), (7, END)]);
    unreached(&mut cells, VOLUME);
    assert_eq!(gather(&mut cells, VOLUME, true), (2, false));
}

#[test]
fn gathering_without_fixing_leaves_the_fat_as_it_was() {
    let mut cells = plain(&[(4, 1), (6, 6)]);
    unreached(&mut cells, VOLUME);
    let before = cells.fat.clone();
    assert_eq!(
        gather(&mut cells, VOLUME, false),
        (2, false),
        "a cluster linking to itself is a chain of its own"
    );
    assert_eq!(cells.fat, before);
}

#[test]
fn a_files_breaks_are_its_links_to_any_cluster_but_the_next() {
    let mut cells = plain(&[
        (2, 3),
        (3, 4),
        (4, END),
        (5, 9),
        (9, 10),
        (10, 6),
        (6, END),
        (11, 0),
    ]);
    assert_eq!(breaks(&mut cells, VOLUME, 2), 0);
    assert_eq!(breaks(&mut cells, VOLUME, 5), 2);
    assert_eq!(
        breaks(&mut cells, VOLUME, 11),
        1,
        "a link to nothing breaks it"
    );
    assert_eq!(breaks(&mut cells, VOLUME, 0), 0, "an empty file");
}
