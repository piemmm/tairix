//! The matcher. A component is matched by tracking the set of candidate
//! positions (`0..=n`, as bit sets) each node can reach from the positions
//! the previous one reached; the candidate matches when position `n` is
//! reachable at the end. Nothing backtracks.
//!
//! A group's reach from each start position is memoised, as is what its
//! alternatives reach in one match ("core"), so a group under a repetition
//! or a negation is evaluated once per start however often it is reached.
//! A repetition's rows are built from the candidate's end downwards: a match
//! only moves forward, so a row is the union of rows already known. Leaves of
//! an extended pattern cache the positions they accept as masks, making each
//! step a word-wise shift.
//!
//! The working memory grows fallibly: a heap that refuses it ends the match
//! with [`OutOfMemory`], never the process.

use alloc::vec::Vec;

use tairix_util::fallible::{grow_to, push};

use crate::compile::{fold, GroupOp, Node};
use crate::{Component, Options, OutOfMemory, Scratch, MAX_EXTENDED_LEN};

const BITS: usize = u64::BITS as usize;

/// The memo tables a group's sets are kept in.
const TABLES: usize = 3;

pub(crate) fn matches(
    component: &Component,
    name: &str,
    scratch: &mut Scratch,
) -> Result<bool, OutOfMemory> {
    let n = name.chars().count();
    if component.groups > 0 && n > MAX_EXTENDED_LEN {
        return Ok(false);
    }
    let Scratch {
        raw,
        folded,
        pool,
        masks,
        masked,
        rows,
        slots,
        star_floor,
    } = scratch;
    raw.clear();
    raw.try_reserve(n).map_err(|_| OutOfMemory)?;
    raw.extend(name.chars());
    let case_insensitive = component.options.contains(Options::CASE_INSENSITIVE);
    folded.clear();
    if case_insensitive {
        folded.try_reserve(n).map_err(|_| OutOfMemory)?;
        folded.extend(raw.iter().copied().map(fold));
    }
    let raw = raw.as_slice();
    let text = if case_insensitive {
        folded.as_slice()
    } else {
        raw
    };
    let width = n / BITS + 1;
    let extended = component.groups > 0;
    masked.clear();
    masks.clear();
    rows.clear();
    slots.clear();
    star_floor.clear();
    if extended {
        grown(masked, component.leaves, false)?;
        grown(masks, component.leaves * width, 0)?;
        grown(slots, TABLES * component.groups * (n + 1), 0)?;
        grown(star_floor, component.groups, n + 1)?;
    }
    let mut engine = Engine {
        raw,
        text,
        n,
        width,
        dot0: component.options.contains(Options::PERIOD) && raw.first() == Some(&'.'),
        cached_leaves: extended,
        pool,
        masks,
        masked,
        rows,
        slots,
        star_floor,
        groups: component.groups,
    };
    let mut start = engine.take()?;
    insert(&mut start, 0);
    let end = engine.sequence(&component.nodes, &start)?;
    let hit = contains(&end, n);
    engine.give(start);
    engine.give(end);
    Ok(hit)
}

/// Lengthen `scratch` to `count` copies of `value`, or refuse as the heap does.
fn grown<T: Clone>(scratch: &mut Vec<T>, count: usize, value: T) -> Result<(), OutOfMemory> {
    if grow_to(scratch, count, value) {
        Ok(())
    } else {
        Err(OutOfMemory)
    }
}

/// Which memo table a group's set belongs to.
#[derive(Clone, Copy)]
enum Table {
    /// What one match of the alternatives reaches.
    Core = 0,
    /// What any number of matches reach (`*(…)`, and `+(…)` after its first).
    Star = 1,
    /// What the whole group reaches.
    Row = 2,
}

struct Engine<'s> {
    raw: &'s [char],
    text: &'s [char],
    n: usize,
    width: usize,
    /// The candidate begins with a `.` the period rule protects.
    dot0: bool,
    cached_leaves: bool,
    pool: &'s mut Vec<Vec<u64>>,
    masks: &'s mut Vec<u64>,
    masked: &'s mut Vec<bool>,
    rows: &'s mut Vec<u64>,
    /// Per table, group and start: the memoised set's index in `rows`, plus
    /// one; zero when not yet computed.
    slots: &'s mut Vec<usize>,
    /// Per group, the lowest start whose `Star` row is known: every start
    /// from it to `n` is.
    star_floor: &'s mut Vec<usize>,
    groups: usize,
}

impl Engine<'_> {
    fn take(&mut self) -> Result<Vec<u64>, OutOfMemory> {
        let mut set = self.pool.pop().unwrap_or_default();
        set.clear();
        grown(&mut set, self.width, 0)?;
        Ok(set)
    }

    /// Return `set` to the pool, or free it when the pool cannot grow.
    fn give(&mut self, set: Vec<u64>) {
        let _ = push(self.pool, set);
    }

    fn copy_of(&mut self, from: &[u64]) -> Result<Vec<u64>, OutOfMemory> {
        let mut set = self.take()?;
        set.copy_from_slice(from);
        Ok(set)
    }

    fn sequence(&mut self, nodes: &[Node], start: &[u64]) -> Result<Vec<u64>, OutOfMemory> {
        let mut current = self.copy_of(start)?;
        for node in nodes {
            if is_empty(&current) {
                break;
            }
            let next = self.step(node, &current)?;
            let spent = core::mem::replace(&mut current, next);
            self.give(spent);
        }
        Ok(current)
    }

    fn step(&mut self, node: &Node, from: &[u64]) -> Result<Vec<u64>, OutOfMemory> {
        match node {
            Node::Literal { leaf, .. } | Node::AnyOne { leaf } | Node::Bracket { leaf, .. } => {
                self.leaf_step(node, *leaf, from)
            }
            Node::AnySequence => self.any_sequence(from),
            Node::Group { op, alts, id } => {
                let mut out = self.take()?;
                for start in Ones::new(from) {
                    self.group_into(*op, alts, *id, start, &mut out)?;
                }
                Ok(out)
            }
        }
    }

    /// Whether the leaf `node` consumes the character at `at`.
    fn accepts(&self, node: &Node, at: usize) -> bool {
        if at >= self.n {
            return false;
        }
        let guarded = at == 0 && self.dot0;
        match node {
            Node::Literal { ch, .. } => self.text[at] == *ch,
            Node::AnyOne { .. } => !guarded,
            Node::Bracket { set, .. } => !guarded && set.contains(self.raw[at], self.text[at]),
            Node::AnySequence | Node::Group { .. } => false,
        }
    }

    fn leaf_step(
        &mut self,
        node: &Node,
        leaf: usize,
        from: &[u64],
    ) -> Result<Vec<u64>, OutOfMemory> {
        let mut out = self.take()?;
        if !self.cached_leaves {
            for at in Ones::new(from) {
                if self.accepts(node, at) {
                    insert(&mut out, at + 1);
                }
            }
            return Ok(out);
        }
        let base = leaf * self.width;
        if !self.masked[leaf] {
            for at in 0..self.n {
                if self.accepts(node, at) {
                    insert(&mut self.masks[base..base + self.width], at);
                }
            }
            self.masked[leaf] = true;
        }
        // Each accepted position advances by one: a word-wise shift carrying
        // the top bit into the next word. No bit reaches past `n`, as no
        // leaf accepts position `n`.
        let mask = &self.masks[base..base + self.width];
        let mut carry = 0;
        for ((slot, &reach), &accept) in out.iter_mut().zip(from).zip(mask) {
            let moved = reach & accept;
            *slot = (moved << 1) | carry;
            carry = moved >> (BITS - 1);
        }
        Ok(out)
    }

    /// `*` extends every reached position to every later one — except that
    /// it consumes nothing at a guarded leading `.`.
    fn any_sequence(&mut self, from: &[u64]) -> Result<Vec<u64>, OutOfMemory> {
        let mut out = self.copy_of(from)?;
        if let Some(start) = first_at_or_after(from, usize::from(self.dot0)) {
            fill(&mut out, start, self.n);
        }
        Ok(out)
    }

    fn slot(&self, table: Table, id: usize, start: usize) -> usize {
        ((table as usize) * self.groups + id) * (self.n + 1) + start
    }

    fn known(&self, table: Table, id: usize, start: usize) -> Option<&[u64]> {
        let index = self.slots[self.slot(table, id, start)].checked_sub(1)?;
        let begin = index * self.width;
        Some(&self.rows[begin..begin + self.width])
    }

    /// Union the memoised set into `into`, reporting whether there was one.
    fn add_known(&self, table: Table, id: usize, start: usize, into: &mut [u64]) -> bool {
        self.known(table, id, start)
            .map(|set| or_into(into, set))
            .is_some()
    }

    fn remember(
        &mut self,
        table: Table,
        id: usize,
        start: usize,
        set: &[u64],
    ) -> Result<(), OutOfMemory> {
        let begin = self.rows.len();
        grown(self.rows, begin + self.width, 0)?;
        self.rows[begin..].copy_from_slice(set);
        let slot = self.slot(table, id, start);
        self.slots[slot] = begin / self.width + 1;
        Ok(())
    }

    /// Union what the group reaches from `start` into `into`.
    fn group_into(
        &mut self,
        op: GroupOp,
        alts: &[Vec<Node>],
        id: usize,
        start: usize,
        into: &mut [u64],
    ) -> Result<(), OutOfMemory> {
        if self.add_known(Table::Row, id, start, into) {
            return Ok(());
        }
        let reached = match op {
            GroupOp::ExactlyOne => self.core(alts, id, start)?,
            GroupOp::ZeroOrOne => {
                let mut reached = self.core(alts, id, start)?;
                insert(&mut reached, start);
                reached
            }
            GroupOp::ZeroOrMore => {
                self.ensure_star(alts, id, start)?;
                let mut reached = self.take()?;
                self.add_known(Table::Star, id, start, &mut reached);
                reached
            }
            GroupOp::OneOrMore => {
                // One match, then any number more from wherever it ended.
                let once = self.core(alts, id, start)?;
                let mut reached = self.take()?;
                if let Some(first) = first_at_or_after(&once, start) {
                    self.ensure_star(alts, id, first)?;
                }
                for mid in Ones::new(&once) {
                    self.add_known(Table::Star, id, mid, &mut reached);
                }
                self.give(once);
                reached
            }
            GroupOp::Not => {
                let once = self.core(alts, id, start)?;
                let mut reached = self.take()?;
                // A guarded leading `.` is consumed by nothing but a literal,
                // so a negation starting on it matches nothing at all.
                if !(start == 0 && self.dot0) {
                    fill(&mut reached, start, self.n);
                    and_not(&mut reached, &once);
                }
                self.give(once);
                reached
            }
        };
        or_into(into, &reached);
        self.remember(Table::Row, id, start, &reached)?;
        self.give(reached);
        Ok(())
    }

    /// What one match of any alternative reaches from `start`.
    fn core(
        &mut self,
        alts: &[Vec<Node>],
        id: usize,
        start: usize,
    ) -> Result<Vec<u64>, OutOfMemory> {
        let mut reached = self.take()?;
        if self.add_known(Table::Core, id, start, &mut reached) {
            return Ok(reached);
        }
        let mut seed = self.take()?;
        insert(&mut seed, start);
        for alt in alts {
            let end = self.sequence(alt, &seed)?;
            or_into(&mut reached, &end);
            self.give(end);
        }
        self.give(seed);
        self.remember(Table::Core, id, start, &reached)?;
        Ok(reached)
    }

    /// Make the `Star` row of every start from `start` to `n` known, building
    /// the missing ones from the highest down: a start's row is itself plus
    /// the rows of every later position one match reaches, all built already.
    fn ensure_star(
        &mut self,
        alts: &[Vec<Node>],
        id: usize,
        start: usize,
    ) -> Result<(), OutOfMemory> {
        let floor = self.star_floor[id];
        for at in (start..floor).rev() {
            let once = self.core(alts, id, at)?;
            let mut reached = self.take()?;
            insert(&mut reached, at);
            for mid in Ones::new(&once).filter(|&mid| mid != at) {
                self.add_known(Table::Star, id, mid, &mut reached);
            }
            self.give(once);
            self.remember(Table::Star, id, at, &reached)?;
            self.give(reached);
        }
        self.star_floor[id] = floor.min(start);
        Ok(())
    }
}

fn insert(set: &mut [u64], at: usize) {
    set[at / BITS] |= 1 << (at % BITS);
}

fn contains(set: &[u64], at: usize) -> bool {
    (set[at / BITS] >> (at % BITS)) & 1 == 1
}

fn is_empty(set: &[u64]) -> bool {
    set.iter().all(|&word| word == 0)
}

fn or_into(into: &mut [u64], from: &[u64]) {
    for (into, from) in into.iter_mut().zip(from) {
        *into |= from;
    }
}

fn and_not(into: &mut [u64], remove: &[u64]) {
    for (into, remove) in into.iter_mut().zip(remove) {
        *into &= !remove;
    }
}

/// Set every position in `from..=to`.
fn fill(set: &mut [u64], from: usize, to: usize) {
    let (first, last) = (from / BITS, to / BITS);
    for (at, word) in set.iter_mut().enumerate().take(last + 1).skip(first) {
        let low = if at == first { from % BITS } else { 0 };
        let high = if at == last { to % BITS } else { BITS - 1 };
        // Bits `low..=high` of one word; `high - low` is at most 63.
        let span = u64::MAX >> (BITS - 1 - (high - low));
        *word |= span << low;
    }
}

/// The first set position at or after `from`.
fn first_at_or_after(set: &[u64], from: usize) -> Option<usize> {
    let mut word = from / BITS;
    let mut bits = *set.get(word)? & (u64::MAX << (from % BITS));
    loop {
        if bits != 0 {
            return Some(word * BITS + bits.trailing_zeros() as usize);
        }
        word += 1;
        bits = *set.get(word)?;
    }
}

/// The set positions of a bit set, ascending.
struct Ones<'a> {
    words: &'a [u64],
    index: usize,
    bits: u64,
}

impl<'a> Ones<'a> {
    fn new(words: &'a [u64]) -> Self {
        Self {
            words,
            index: 0,
            bits: words.first().copied().unwrap_or(0),
        }
    }
}

impl Iterator for Ones<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while self.bits == 0 {
            self.index += 1;
            self.bits = *self.words.get(self.index)?;
        }
        let bit = self.bits.trailing_zeros() as usize;
        self.bits &= self.bits - 1;
        Some(self.index * BITS + bit)
    }
}
