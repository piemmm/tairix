//! The FAT `CHKDSK` reads into its memory and the map it keeps beside it, a
//! byte a cluster, and what it works out over the two: a chain followed and
//! marked, the clusters no chain reached, those gathered into chains, and a
//! file's breaks. Every walk is bounded by the volume's clusters, so a FAT
//! looping back on itself ends it.

use crate::dos::program::Program;
use crate::dos::utility::{place, HeldFat};
use tairix_fat::boot::Kind;
use tairix_fat::table::{self, Link};

/// What the map holds of a cluster.
pub mod mark {
    /// The first cluster of a chain.
    pub const HEAD: u8 = 0x80;
    /// A file's cluster, or, among lost ones, one already gathered.
    pub const FILE: u8 = 0x01;
    pub const DIRECTORY: u8 = 0x02;
    /// Marked bad in the FAT.
    pub const BAD: u8 = 0x04;
    /// Neither free nor reached by any chain the tree holds.
    pub const LOST: u8 = 0x08;
    /// Reached by a second chain.
    pub const CROSSED: u8 = 0x10;
}

/// A cluster's FAT entry and its mark, wherever the two are kept.
pub trait Cells {
    fn entry(&mut self, cluster: u16) -> u16;
    fn set_entry(&mut self, cluster: u16, value: u16);
    fn mark(&mut self, cluster: u16) -> u8;
    fn set_mark(&mut self, cluster: u16, mark: u8);
}

/// The volume the cells describe: its FAT's kind and its highest cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Volume {
    pub kind: Kind,
    pub max: u16,
}

impl Volume {
    fn link(self, value: u16) -> Link {
        table::classify(self.kind, u32::from(value), u32::from(self.max))
    }

    /// Whether `cluster` is one of the volume's.
    #[must_use]
    pub fn holds(self, cluster: u16) -> bool {
        (2..=self.max).contains(&cluster)
    }

    /// The entry a chain's last cluster is written with.
    #[must_use]
    pub fn end(self) -> u16 {
        u16::try_from(table::end(self.kind)).unwrap_or(u16::MAX)
    }

    fn bad(self) -> u16 {
        u16::try_from(table::bad(self.kind)).unwrap_or(u16::MAX)
    }

    /// The most links a chain can have.
    fn bound(self) -> u32 {
        u32::from(self.max)
    }
}

/// How following a chain ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// At its last cluster.
    End,
    /// At a cluster another chain had reached.
    Crossed,
    /// Its first cluster is none of the volume's.
    BadFirst,
    /// The link out of `last` names none of the volume's clusters.
    BadLink { last: u16 },
}

/// A chain followed and marked: its clusters, up to where it ended, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chain {
    pub clusters: u32,
    pub ending: Ending,
}

/// Follows the chain from `first`, marking each cluster `kind`, its first
/// as the head; a cluster already marked is crossed, and ends it.
pub fn mark_chain(cells: &mut impl Cells, volume: Volume, first: u16, kind: u8) -> Chain {
    if !volume.holds(first) {
        return Chain {
            clusters: 0,
            ending: Ending::BadFirst,
        };
    }
    let (mut cluster, mut mark, mut clusters) = (first, kind | mark::HEAD, 0);
    loop {
        let held = cells.mark(cluster);
        if held != 0 {
            cells.set_mark(cluster, held | mark::CROSSED);
            return Chain {
                clusters,
                ending: Ending::Crossed,
            };
        }
        cells.set_mark(cluster, mark);
        mark = kind;
        clusters += 1;
        match volume.link(cells.entry(cluster)) {
            Link::End => {
                return Chain {
                    clusters,
                    ending: Ending::End,
                }
            }
            Link::Next(next) => cluster = u16::try_from(next).unwrap_or(0),
            Link::Free | Link::Bad => {
                return Chain {
                    clusters,
                    ending: Ending::BadLink { last: cluster },
                }
            }
        }
    }
}

/// The first cluster on from `first` that a chain has crossed, if the
/// chain reaches one.
pub fn first_crossed(cells: &mut impl Cells, volume: Volume, first: u16) -> Option<u16> {
    let mut cluster = first;
    for _ in 0..volume.bound() {
        if !volume.holds(cluster) {
            return None;
        }
        if cells.mark(cluster) & mark::CROSSED != 0 {
            return Some(cluster);
        }
        match volume.link(cells.entry(cluster)) {
            Link::Next(next) => cluster = u16::try_from(next).unwrap_or(0),
            _ => return None,
        }
    }
    None
}

/// The clusters on from `first` to the chain's end, or to a link that
/// names no cluster.
pub fn chain_length(cells: &mut impl Cells, volume: Volume, first: u16) -> u32 {
    let mut cluster = first;
    for counted in 0..volume.bound() {
        if !volume.holds(cluster) {
            return counted;
        }
        match volume.link(cells.entry(cluster)) {
            Link::Next(next) => cluster = u16::try_from(next).unwrap_or(0),
            _ => return counted + 1,
        }
    }
    volume.bound()
}

/// Marks the clusters no chain reached: bad, or, holding anything else
/// but free, lost. Gives how many of each.
pub fn unreached(cells: &mut impl Cells, volume: Volume) -> (u32, u32) {
    let (mut bad, mut lost) = (0, 0);
    for cluster in 2..=volume.max {
        if cells.mark(cluster) != 0 {
            continue;
        }
        match cells.entry(cluster) {
            0 => {}
            entry if entry == volume.bad() => {
                bad += 1;
                cells.set_mark(cluster, mark::BAD);
            }
            _ => {
                lost += 1;
                cells.set_mark(cluster, mark::LOST);
            }
        }
    }
    (bad, lost)
}

/// Gathers the lost clusters into chains, each head marked, a chain that
/// runs into another's head joined to it; gives how many there are, and
/// whether the FAT was changed. A link out of a chain — to no cluster, to
/// one that is not lost, or back into its own chain or one gathered already
/// — ends the chain there, written into the FAT when `fix`.
pub fn gather(cells: &mut impl Cells, volume: Volume, fix: bool) -> (u32, bool) {
    let (mut chains, mut changed) = (0u32, false);
    for head in 2..=volume.max {
        let held = cells.mark(head);
        if held & mark::LOST == 0 || held & mark::FILE != 0 {
            continue;
        }
        cells.set_mark(head, held | mark::HEAD | mark::FILE);
        chains += 1;
        let mut cluster = head;
        loop {
            let next = match volume.link(cells.entry(cluster)) {
                Link::End => break,
                Link::Next(next) => u16::try_from(next).unwrap_or(0),
                Link::Free | Link::Bad => 0,
            };
            let ahead = if next == 0 || next == head {
                0
            } else {
                cells.mark(next)
            };
            let joins = ahead & mark::LOST != 0 && ahead & mark::HEAD != 0;
            if joins {
                cells.set_mark(next, ahead & !mark::HEAD);
                chains -= 1;
                break;
            }
            if ahead & mark::LOST == 0 || ahead & mark::FILE != 0 {
                if fix {
                    cells.set_entry(cluster, volume.end());
                    changed = true;
                }
                break;
            }
            cells.set_mark(next, ahead | mark::FILE);
            cluster = next;
        }
    }
    (chains, changed)
}

/// The first lost chain's head at or after `from`.
pub fn next_head(cells: &mut impl Cells, volume: Volume, from: u16) -> Option<u16> {
    let head = mark::HEAD | mark::LOST | mark::FILE;
    (from.max(2)..=volume.max).find(|&cluster| cells.mark(cluster) == head)
}

/// Frees every lost cluster in the FAT.
pub fn free_lost(cells: &mut impl Cells, volume: Volume) {
    for cluster in 2..=volume.max {
        if cells.mark(cluster) & mark::LOST != 0 {
            cells.set_entry(cluster, 0);
        }
    }
}

/// The breaks in the chain from `first`: the links that do not name the
/// cluster after their own.
pub fn breaks(cells: &mut impl Cells, volume: Volume, first: u16) -> u32 {
    let (mut cluster, mut breaks) = (first, 0);
    for _ in 0..volume.bound() {
        if !volume.holds(cluster) {
            return breaks;
        }
        match volume.link(cells.entry(cluster)) {
            Link::End => return breaks,
            Link::Next(next) => {
                let next = u16::try_from(next).unwrap_or(0);
                breaks += u32::from(next != cluster.wrapping_add(1));
                cluster = next;
            }
            Link::Free | Link::Bad => return breaks + 1,
        }
    }
    breaks
}

/// The FAT and map in a program's memory: the FAT read whole, the map a
/// byte a cluster from segment `map`.
pub struct Held<'p, 'a, 'b> {
    pub p: &'p mut Program<'a, 'b>,
    pub fat: HeldFat,
    pub map: u16,
}

impl Cells for Held<'_, '_, '_> {
    fn entry(&mut self, cluster: u16) -> u16 {
        self.fat.entry(self.p, cluster)
    }

    fn set_entry(&mut self, cluster: u16, value: u16) {
        self.fat.set_entry(self.p, cluster, value);
    }

    fn mark(&mut self, cluster: u16) -> u8 {
        let (segment, offset) = place(self.map, u32::from(cluster));
        self.p.g.read8(segment, offset)
    }

    fn set_mark(&mut self, cluster: u16, mark: u8) {
        let (segment, offset) = place(self.map, u32::from(cluster));
        self.p.g.write8(segment, offset, mark);
    }
}

#[cfg(test)]
#[path = "cells_tests.rs"]
mod tests;
