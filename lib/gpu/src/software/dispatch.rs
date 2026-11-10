//! A dispatch on the software device: its memories locked in one order and
//! shared between the cores as atomic views, its workgroups shared across the
//! process's runner a piece at a time, each piece's tiles run in workspaces
//! the device reserved when the pipeline was made.
//!
//! A workgroup of more invocations than a tile holds runs as several tiles,
//! each suspended at a barrier until every one has reached a barrier or
//! ended; small workgroups share a tile. Every piece counts its work and
//! reads the clock at each fixed amount of it, and a shared flag stops every
//! piece once the deadline passes or one faults.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use tairix_inline::ArrayVec;
use tairix_rt::sync::Mutex;

use super::shader::exec::{Env, Machine, Placement, Ran, Resolved, Space};
use super::shader::ir::{owns_tiles, Program, LANES};
use super::{Meter, Outcome, WORK_PER_CHECK};
use crate::binding::{BindSetState, Bound};
use crate::device::Shared;
use crate::memory::{self, Budget, Charge, Held, Memory};
use crate::model::exhausted;
use crate::pipeline::ComputePipelineState;
use crate::Error;

/// What one participant of a pipeline's dispatch works in: words for its
/// tiles' registers and memory and its workgroups' memory, and lane masks for
/// its tiles' blocks.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Need {
    pub(crate) words: usize,
    pub(crate) masks: usize,
}

impl Need {
    pub(crate) const fn max(self, other: Self) -> Self {
        Self {
            words: if self.words > other.words {
                self.words
            } else {
                other.words
            },
            masks: if self.masks > other.masks {
                self.masks
            } else {
                other.masks
            },
        }
    }

    const fn min(self, other: Self) -> Self {
        Self {
            words: if self.words < other.words {
                self.words
            } else {
                other.words
            },
            masks: if self.masks < other.masks {
                self.masks
            } else {
                other.masks
            },
        }
    }

    const fn covers(self, other: Self) -> bool {
        self.words >= other.words && self.masks >= other.masks
    }

    /// The class of each of its parts, or `None` past every class.
    const fn classes(self) -> Option<[usize; 2]> {
        match (class_of(self.words), class_of(self.masks)) {
            (Some(words), Some(masks)) => Some([words, masks]),
            _ => None,
        }
    }

    /// What the parts of `classes` round up to.
    const fn of_classes([words, masks]: [usize; 2]) -> Self {
        Self {
            words: bound_of(words),
            masks: bound_of(masks),
        }
    }

    fn bytes(self) -> u64 {
        (self.words as u64)
            .saturating_mul(4)
            .saturating_add((self.masks as u64).saturating_mul(8))
    }
}

/// The significant bits a size keeps once rounded up to its class: a
/// workspace's part is at most an eighth larger than the need it is fitted
/// to, and the largest of each part alive is found in a fixed number of
/// counts.
const SIGNIFICANT: u32 = 4;

/// Each class of `HALF` to `2 * HALF - 1`, shifted.
const HALF: usize = 1 << (SIGNIFICANT - 1);

/// The classes a part of a need falls in: the values below `HALF` their own,
/// then `HALF` to each doubling up to the widest a size can be.
const CLASSES: usize = (usize::BITS - SIGNIFICANT + 2) as usize * HALF;

/// The class `n` rounds up to, keeping its `SIGNIFICANT` leading bits, or
/// `None` where that passes every size.
const fn class_of(n: usize) -> Option<usize> {
    let shift = (usize::BITS - n.leading_zeros()).saturating_sub(SIGNIFICANT);
    let mut mantissa = n >> shift;
    let mut shift = shift as usize;
    if n & ((1 << shift) - 1) != 0 {
        mantissa += 1;
        if mantissa == 2 * HALF {
            mantissa = HALF;
            shift += 1;
        }
    }
    if mantissa < HALF {
        return Some(mantissa);
    }
    let class = shift * HALF + mantissa;
    if class < CLASSES {
        Some(class)
    } else {
        None
    }
}

/// The size class `class` stands for.
const fn bound_of(class: usize) -> usize {
    if class < HALF {
        class
    } else {
        (class % HALF + HALF) << (class / HALF - 1)
    }
}

/// How a program's workgroups are cut into tiles: the tiles one workgroup
/// takes, or the workgroups one tile holds.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Tiling {
    pub(crate) invocations: usize,
    pub(crate) tiles: usize,
    pub(crate) packed: usize,
    /// The tiles that hold memory of their own at once: every one where they
    /// wait on one another at barriers, and one where each runs to its end in
    /// turn.
    pub(crate) resident: usize,
}

impl Tiling {
    pub(crate) fn of(program: &Program) -> Self {
        let [x, y, z] = program.workgroup_size;
        let invocations =
            usize::try_from(x.saturating_mul(y).saturating_mul(z)).unwrap_or(usize::MAX);
        let (tiles, packed) = if owns_tiles(invocations) {
            (invocations.div_ceil(LANES), 1)
        } else {
            (1, LANES / invocations.max(1))
        };
        Self {
            invocations,
            tiles,
            packed,
            resident: if program.barriers { tiles } else { 1 },
        }
    }
}

/// Where each of a tile's parts lies in its workspace, in words and masks.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Parts {
    planes: usize,
    uniforms: usize,
    private: usize,
    shared: usize,
    masks: usize,
    pending: usize,
}

impl Parts {
    pub(crate) fn of(program: &Program) -> Self {
        let blocks = program.blocks.len();
        Self {
            planes: program.varying_words as usize * LANES,
            uniforms: (program.uniform_words as usize).saturating_sub(program.constants.len()),
            private: program.private_words as usize * LANES,
            shared: program.shared_words as usize,
            masks: blocks,
            pending: blocks.div_ceil(64),
        }
    }

    pub(crate) const fn words(self) -> usize {
        self.planes + self.uniforms + self.private + self.shared
    }

    /// A tile's masks, its pending bits, and its live and killed lanes.
    pub(crate) const fn masks_len(self) -> usize {
        self.masks + self.pending + 2
    }
}

/// What a participant needs to run `program`'s workgroups.
pub(crate) fn need(program: &Program) -> Need {
    let tiling = Tiling::of(program);
    let parts = Parts::of(program);
    let resident = tiling.resident;
    Need {
        words: resident * parts.words() + tiling.packed * program.workgroup_words as usize,
        masks: resident * parts.masks_len(),
    }
}

/// One participant's working memory and the heap's charge for it. Its parts
/// are made outside the device's lock, as zeroing them takes as long as they
/// are large, and shrunk in place under it: what they held is cleared before
/// it is used.
pub(crate) struct Workspace {
    words: Vec<u32>,
    masks: Vec<u64>,
    charge: Charge,
}

impl Workspace {
    /// A zeroed workspace of `size` under `charge`.
    fn made(size: Need, charge: Charge) -> Result<Self, Error> {
        let words = tairix_util::fallible::filled(size.words, 0u32);
        let masks = tairix_util::fallible::filled(size.masks, 0u64);
        let (Some(words), Some(masks)) = (words, masks) else {
            return Err(Error::OutOfMemory);
        };
        Ok(Self {
            words,
            masks,
            charge,
        })
    }

    /// A tile's space for `program`, carved from its start: its registers
    /// and blocks as the last tile it held left them, which ends with no lane
    /// waiting.
    pub(crate) fn tile(&mut self, program: &Program) -> Option<Space<'_>> {
        let parts = Parts::of(program);
        let words = self.words.get_mut(..parts.words())?;
        let masks = self.masks.get_mut(..parts.masks_len())?;
        carve(words, masks, parts, &mut [])
    }

    /// Every word and mask zeroed, as it last served another job, perhaps
    /// another context's: `false` once the job is stopped.
    pub(crate) fn clear(&mut self, meter: &mut Meter<'_>) -> bool {
        clear(&mut self.words, meter) && clear(&mut self.masks, meter)
    }

    /// The need it is fitted to.
    pub(crate) fn size(&self) -> Need {
        Need {
            words: self.words.len(),
            masks: self.masks.len(),
        }
    }

    /// Each part longer than `size`'s shrunk to it in place, its bytes given
    /// back.
    fn shrink(&mut self, size: Need) {
        shrink_part(&mut self.words, size.words, &mut self.charge);
        shrink_part(&mut self.masks, size.masks, &mut self.charge);
    }

    /// The parts `with` holds that are longer than its own swapped in, each
    /// with its charge, `with` left holding what they replaced.
    fn grow(&mut self, with: (Option<&mut Part<u32>>, Option<&mut Part<u64>>)) {
        if let Some(words) = with.0 {
            swap_longer(
                (&mut self.words, &mut self.charge),
                (&mut words.values, &mut words.charge),
            );
        }
        if let Some(masks) = with.1 {
            swap_longer(
                (&mut self.masks, &mut self.charge),
                (&mut masks.values, &mut masks.charge),
            );
        }
    }

    /// `other`'s parts that are longer than its own swapped in, `other` left
    /// holding what they replaced.
    fn grow_from(&mut self, other: &mut Self) {
        swap_longer(
            (&mut self.words, &mut self.charge),
            (&mut other.words, &mut other.charge),
        );
        swap_longer(
            (&mut self.masks, &mut self.charge),
            (&mut other.masks, &mut other.charge),
        );
    }
}

/// A workspace's part made outside the device's lock, and the heap's charge
/// for it.
pub(crate) struct Part<T> {
    values: Vec<T>,
    charge: Charge,
}

impl<T: Copy + Default> Part<T> {
    /// `len` zeroed entries under `charge`.
    fn made(len: usize, charge: Charge) -> Result<Self, Error> {
        let values = tairix_util::fallible::filled(len, T::default()).ok_or(Error::OutOfMemory)?;
        Ok(Self { values, charge })
    }
}

/// The bytes `len` entries of `T` take.
fn part_bytes<T>(len: usize) -> u64 {
    (len as u64).saturating_mul(size_of::<T>() as u64)
}

/// `part` shrunk in place to `len` entries where it is longer, the bytes it
/// gives back released from `charge`: the heap shrinks a block in place.
fn shrink_part<T>(part: &mut Vec<T>, len: usize, charge: &mut Charge) {
    if part.len() > len {
        let freed = part_bytes::<T>(part.len() - len);
        part.truncate(len);
        part.shrink_to(len);
        charge.release(freed);
    }
}

/// `with`'s values swapped into `part` where they are longer, each side's
/// charge following its values.
fn swap_longer<T>(
    (part, charge): (&mut Vec<T>, &mut Charge),
    (with, theirs): (&mut Vec<T>, &mut Charge),
) {
    if with.len() <= part.len() {
        return;
    }
    let (old, new) = (part_bytes::<T>(part.len()), part_bytes::<T>(with.len()));
    core::mem::swap(part, with);
    let into = theirs.split(new);
    let out = charge.split(old);
    // Of the same heap, so neither refuses.
    let _ = charge.absorb(into);
    let _ = theirs.absorb(out);
}

/// How many live pipelines need each class of each part of a need: what the
/// pool's size is read from. A pipeline's hold counts itself in as it is made,
/// under the device's lock, and out as it is dropped, anywhere, under the
/// lease's lock; the counts publish nothing else. The pool reads them under
/// the device's lock, where a count stale for a hold just dropped only keeps
/// the set larger until the next look, and under the lease's too as it lends
/// the set, so the set lent is no larger than what is held needs.
struct Needs {
    words: [AtomicUsize; CLASSES],
    masks: [AtomicUsize; CLASSES],
    lease: Mutex<Lease>,
}

/// The pool's workspaces a job holds: what was lent, which of its parts a
/// claim kept here pays for, and the claims of the last holds of a part's
/// lent class let go while the job ran, kept until the workspaces come back.
#[derive(Default)]
struct Lease {
    lent: Option<Lent>,
    paid: [bool; 2],
    claims: ArrayVec<Charge, 2>,
}

/// The workspaces lent to a job: the size they were fitted to, and how many.
#[derive(Copy, Clone, Debug)]
struct Lent {
    size: Need,
    count: usize,
}

impl Needs {
    fn new() -> Self {
        Self {
            words: core::array::from_fn(|_| AtomicUsize::new(0)),
            masks: core::array::from_fn(|_| AtomicUsize::new(0)),
            lease: Mutex::new(Lease::default()),
        }
    }

    fn counts(&self, part: usize) -> &[AtomicUsize; CLASSES] {
        if part == 0 {
            &self.words
        } else {
            &self.masks
        }
    }

    /// Whether a live pipeline needs `part` in `class` or above.
    fn held_from(&self, part: usize, class: usize) -> bool {
        self.counts(part)
            .iter()
            .skip(class)
            .any(|count| count.load(Ordering::Relaxed) > 0)
    }

    /// The largest class `part` is needed in, looking down from `from`.
    fn largest(&self, part: usize, from: usize) -> Option<usize> {
        let counts = self.counts(part);
        (0..=from.min(CLASSES - 1)).rev().find(|&class| {
            counts
                .get(class)
                .is_some_and(|n| n.load(Ordering::Relaxed) > 0)
        })
    }
}

/// The working memory a device's dispatches run in: a workspace for each
/// participant its runner can run at once, each part of each fitted to the
/// largest of that part any pipeline still alive needs, rounded up to its
/// class. There is one set, free or lent to a job, made whole at once where
/// there is none. What a pipeline's need adds to it is made outside the
/// device's lock, under charges taken first, and installed under it —
/// swapped into the free set, or kept for the lent one until it comes back —
/// and what the set no longer needs shrinks in place. Kept under the
/// device's lock, so its charges are taken as every other is.
pub(crate) struct Workspaces {
    needs: Arc<Needs>,
    /// The largest of each part a counted pipeline needs.
    size: Need,
    free: Vec<Workspace>,
    width: usize,
    /// While a job holds the set, what pipelines made meanwhile added to it.
    stash: Option<Grown>,
}

/// What a pipeline's need adds to the set, to be made outside the device's
/// lock, the heap charged for all of it first.
pub(crate) struct Plan {
    hold: Hold,
    shape: Shape,
    room: Charge,
}

/// What a plan makes: parts for each workspace the set has, or the set whole
/// where it has none.
#[derive(Copy, Clone, Debug)]
enum Shape {
    /// For each of `count` workspaces a part of each kind `lengths` gives a
    /// length, none of a kind it gives none.
    Parts { lengths: Need, count: usize },
    /// `count` workspaces of `size`.
    Whole { size: Need, count: usize },
}

impl Shape {
    fn bytes(self) -> u64 {
        let (each, count) = match self {
            Self::Parts { lengths, count } => (lengths, count),
            Self::Whole { size, count } => (size, count),
        };
        each.bytes().saturating_mul(count as u64)
    }
}

/// What a plan made, with the hold it was made for: installed into the set,
/// it is left holding what the set let go, to be dropped outside the
/// device's lock.
#[derive(Default)]
pub(crate) struct Grown {
    hold: Option<Hold>,
    words: Vec<Part<u32>>,
    masks: Vec<Part<u64>>,
    whole: Vec<Workspace>,
}

impl Plan {
    /// What it plans, made without the device's lock, each part charged
    /// from its room.
    pub(crate) fn make(mut self) -> Result<Grown, Error> {
        let mut grown = Grown::default();
        match self.shape {
            Shape::Parts { lengths, count } => {
                grown.words = parts(lengths.words, count, &mut self.room)?;
                grown.masks = parts(lengths.masks, count, &mut self.room)?;
            }
            Shape::Whole { size, count } => {
                grown.whole.try_reserve_exact(count).map_err(exhausted)?;
                for _ in 0..count {
                    let charge = self.room.split(size.bytes());
                    grown.whole.push(Workspace::made(size, charge)?);
                }
            }
        }
        grown.hold = Some(self.hold);
        Ok(grown)
    }
}

/// `count` zeroed parts of `len` entries, charged from `room`; none where
/// `len` is zero.
fn parts<T: Copy + Default>(
    len: usize,
    count: usize,
    room: &mut Charge,
) -> Result<Vec<Part<T>>, Error> {
    let mut parts = Vec::new();
    if len == 0 {
        return Ok(parts);
    }
    parts.try_reserve_exact(count).map_err(exhausted)?;
    for _ in 0..count {
        let charge = room.split(part_bytes::<T>(len));
        parts.push(Part::made(len, charge)?);
    }
    Ok(parts)
}

/// A pipeline's hold on its device's workspaces: the classes it is counted
/// in, and the charge to its context for a workspace of its need, rounded up
/// to them, for every participant. Let go when dropped, without the device's
/// lock, and fitted to at the next look; the last hold covering a part of the
/// workspaces a job holds leaves its claim with the lease, to pay for them
/// until they come back.
pub(crate) struct Hold {
    classes: [usize; 2],
    needs: Arc<Needs>,
    claim: Option<Charge>,
}

impl core::fmt::Debug for Hold {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Hold")
            .field("classes", &self.classes)
            .finish_non_exhaustive()
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut lease = self.needs.lease.lock();
        for (part, &class) in self.classes.iter().enumerate() {
            if let Some(count) = self.needs.counts(part).get(class) {
                count.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let Some(lent) = lease.lent.and_then(|lent| lent.size.classes()) else {
            return;
        };
        // Whatever the set was sized by, a part of it needs a payer for as
        // long as it is lent, and the last hold covering it is that.
        let last = self
            .classes
            .iter()
            .zip(&lent)
            .zip(&lease.paid)
            .enumerate()
            .any(|(part, ((&mine, &class), &paid))| {
                class > 0 && mine >= class && !paid && !self.needs.held_from(part, class)
            });
        if !last {
            return;
        }
        // The claim pays for every part it covers, not only those it sized.
        for (paid, (&mine, &class)) in lease.paid.iter_mut().zip(self.classes.iter().zip(&lent)) {
            *paid |= mine >= class;
        }
        if let Some(claim) = self.claim.take() {
            // Never full: a claim is kept only for a part none yet pays for.
            let _ = lease.claims.try_push(claim);
        }
    }
}

impl Workspaces {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            needs: crate::shared(Needs::new())?,
            size: Need::default(),
            free: Vec::new(),
            width: 1,
            stash: None,
        })
    }

    /// Size the pool for a runner `width` participants wide.
    pub(crate) fn set_width(&mut self, width: usize) {
        self.width = width.max(1);
    }

    /// How many participants the pool is sized for.
    pub(crate) const fn width(&self) -> usize {
        self.width
    }

    /// The first phase of holding room for a pipeline needing `need` of each
    /// participant: its claim, its need rounded up to its classes for every
    /// participant, charged to its context's `budget`, and counted in; and
    /// what the set lacks of it, charged to `heap`, to be made without the
    /// device's lock and then installed. A look at the set first, whatever
    /// follows. Refused where the context has no room for the claim and the
    /// `besides` bytes the pipeline charges it and the heap beside, then where
    /// the heap has none for what the set lacks and those. Answers too what
    /// the set let go, to be dropped without the lock.
    pub(crate) fn plan(
        &mut self,
        need: Need,
        (budget, besides): (&Arc<Budget>, u64),
        heap: &Arc<Budget>,
    ) -> (Result<Plan, Error>, Grown) {
        let mut shrunk = self.prune();
        let Some(classes) = need.classes() else {
            return (Err(Error::TooLarge), shrunk);
        };
        let rounded = Need::of_classes(classes);
        let bytes = rounded.bytes().saturating_mul(self.width as u64);
        let claim = budget
            .admit(bytes.saturating_add(besides))
            .and_then(|()| Charge::take(&[budget], bytes, Held::Host));
        let claim = match claim {
            Ok(claim) => claim,
            Err(error) => return (Err(error), shrunk),
        };
        let shape = self.shape(rounded);
        let room = || {
            heap.admit(shape.bytes().saturating_add(besides))
                .and_then(|()| Charge::take(&[heap], shape.bytes(), Held::Host))
        };
        let room = match room() {
            Err(Error::HeapFull) => {
                // Let go here, under the lock: the heap was about to refuse.
                drop(core::mem::take(&mut shrunk));
                room()
            }
            taken => taken,
        };
        let room = match room {
            Ok(room) => room,
            Err(error) => return (Err(error), shrunk),
        };
        for (part, &class) in classes.iter().enumerate() {
            if let Some(count) = self.needs.counts(part).get(class) {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.size = self.size.max(rounded);
        let hold = Hold {
            classes,
            needs: Arc::clone(&self.needs),
            claim: Some(claim),
        };
        (Ok(Plan { hold, shape, room }), shrunk)
    }

    /// What the set lacks of `rounded`: for each workspace it has, lent or
    /// free, a part of each kind shorter there, the lent set counted with what
    /// is kept for it; or the set whole where it has none.
    fn shape(&self, rounded: Need) -> Shape {
        let (have, count) = match self.lent() {
            Some(lent) => (
                self.stash
                    .as_ref()
                    .map_or(lent.size, |stash| stash.covers(lent)),
                lent.count,
            ),
            None => match self.free.first() {
                Some(first) => (first.size(), self.free.len()),
                None => {
                    return Shape::Whole {
                        size: rounded,
                        count: self.width,
                    }
                }
            },
        };
        let longer = |rounded: usize, have: usize| if rounded > have { rounded } else { 0 };
        Shape::Parts {
            lengths: Need {
                words: longer(rounded.words, have.words),
                masks: longer(rounded.masks, have.masks),
            },
            count,
        }
    }

    /// The last phase of holding room: what `grown` made swapped into the
    /// free set where it is longer, or taken as the set where there is none;
    /// while a job holds the set, kept until it comes back. The hold is
    /// answered, and `grown` left holding what the set let go.
    pub(crate) fn install(&mut self, mut grown: Grown) -> (Option<Hold>, Grown) {
        let hold = grown.hold.take();
        if self.lent().is_some() {
            let Some(stash) = self.stash.as_mut() else {
                self.stash = Some(grown);
                return (hold, Grown::default());
            };
            stash.merge(&mut grown);
            return (hold, grown);
        }
        grown.apply(&mut self.free);
        (hold, grown)
    }

    /// Fit the set to the largest of each part still held, shrinking it in
    /// place, and answer what it let go, to be dropped without the device's
    /// lock: once nothing is held, every free workspace, or what pipelines
    /// added while a job holds the set.
    pub(crate) fn prune(&mut self) -> Grown {
        self.size = self.largest();
        if self.size == Need::default() {
            // Lent, the stash is all there is; else there is no stash.
            let mut gone = self.stash.take().unwrap_or_default();
            if gone.whole.is_empty() {
                core::mem::swap(&mut gone.whole, &mut self.free);
            }
            return gone;
        }
        for workspace in &mut self.free {
            workspace.shrink(self.size);
        }
        let lent = self.lent();
        let (Some(stash), Some(lent)) = (self.stash.as_mut(), lent) else {
            return Grown::default();
        };
        // What would grow nothing goes first, so it is not shrunk only to go.
        let gone = stash.outgrown(lent.size, self.size);
        stash.shrink(self.size);
        gone
    }

    /// The largest of each part any counted pipeline needs, found from the
    /// pool's size down, as it covers every counted pipeline.
    fn largest(&self) -> Need {
        let from = self.size.classes().unwrap_or([CLASSES - 1; 2]);
        let largest = [0, 1].map(|part| {
            self.needs
                .largest(part, from.get(part).copied().unwrap_or(CLASSES - 1))
        });
        match largest {
            [Some(words), Some(masks)] => Need::of_classes([words, masks]),
            _ => Need::default(),
        }
    }

    /// What a job holds of the workspaces, while it does.
    fn lent(&self) -> Option<Lent> {
        self.needs.lease.lock().lent
    }

    /// The workspaces a job needing `need` runs in, fitted to what is still
    /// held and lent until the job gives them back: none for a job of no
    /// dispatch, none where they do not cover it, and none while another job
    /// holds them, as a pipeline's claim pays for one set.
    pub(crate) fn take(&mut self, need: Need) -> Vec<Workspace> {
        if need == Need::default() {
            return Vec::new();
        }
        // Fitted and lent under one hold of the lease's lock, so a hold let go
        // before is counted out of the set and one let go after finds it lent.
        let needs = Arc::clone(&self.needs);
        let mut lease = needs.lease.lock();
        if lease.lent.is_some() {
            return Vec::new();
        }
        // The job holds its pipelines, so what is held covers it.
        let size = self.largest();
        if !size.covers(need) {
            return Vec::new();
        }
        self.size = size;
        for workspace in &mut self.free {
            workspace.shrink(size);
        }
        if !self.free.iter().all(|w| w.size().covers(need)) {
            return Vec::new();
        }
        let lent = core::mem::take(&mut self.free);
        if let Some(first) = lent.first() {
            lease.lent = Some(Lent {
                size: first.size(),
                count: lent.len(),
            });
        }
        lent
    }

    /// The workspaces a job ran in back in the pool, what pipelines made
    /// meanwhile added to them, and the set fitted to what is still held:
    /// answering what it let go, to be dropped without the device's lock,
    /// once the claims kept while the job ran are given back.
    pub(crate) fn give(&mut self, workspaces: Vec<Workspace>) -> [Grown; 2] {
        if workspaces.is_empty() {
            return [self.prune(), Grown::default()];
        }
        // Nothing is made into the free set while a job holds it.
        self.free = workspaces;
        let claims = {
            let mut lease = self.needs.lease.lock();
            lease.lent = None;
            lease.paid = [false; 2];
            core::mem::take(&mut lease.claims)
        };
        let added = match self.stash.take() {
            Some(mut stash) => {
                stash.apply(&mut self.free);
                stash
            }
            None => Grown::default(),
        };
        let pruned = self.prune();
        drop(claims);
        [added, pruned]
    }
}

impl Grown {
    /// Made into `set`: taken as the set where there is none, else its parts
    /// and its workspaces' parts swapped in where they are longer; left
    /// holding what that let go.
    fn apply(&mut self, set: &mut Vec<Workspace>) {
        if set.is_empty() {
            core::mem::swap(set, &mut self.whole);
            return;
        }
        for (at, workspace) in set.iter_mut().enumerate() {
            workspace.grow((self.words.get_mut(at), self.masks.get_mut(at)));
            if let Some(whole) = self.whole.get_mut(at) {
                workspace.grow_from(whole);
            }
        }
    }

    /// `other`'s parts and workspaces kept where they are longer than its
    /// own, `other` left holding what they replaced.
    fn merge(&mut self, other: &mut Self) {
        swap_longer_parts(&mut self.words, &mut other.words);
        swap_longer_parts(&mut self.masks, &mut other.masks);
        if other.whole.len() > self.whole.len() {
            core::mem::swap(&mut self.whole, &mut other.whole);
        }
        for (kept, offered) in self.whole.iter_mut().zip(&mut other.whole) {
            kept.grow_from(offered);
        }
    }

    /// What each of the `lent` workspaces holds of each kind once it is
    /// applied to them: the least over them.
    fn covers(&self, lent: Lent) -> Need {
        (0..lent.count)
            .map(|at| {
                let parts = Need {
                    words: self.words.get(at).map_or(0, |part| part.values.len()),
                    masks: self.masks.get(at).map_or(0, |part| part.values.len()),
                };
                let whole = self
                    .whole
                    .get(at)
                    .map_or_else(Need::default, Workspace::size);
                lent.size.max(parts).max(whole)
            })
            .reduce(Need::min)
            .unwrap_or(lent.size)
    }

    /// What of it, fitted to `size`, is no longer than `lent`'s and so would
    /// grow nothing, taken out to be let go.
    fn outgrown(&mut self, lent: Need, size: Need) -> Self {
        let mut gone = Self::default();
        if self
            .words
            .iter()
            .all(|part| part.values.len().min(size.words) <= lent.words)
        {
            core::mem::swap(&mut gone.words, &mut self.words);
        }
        if self
            .masks
            .iter()
            .all(|part| part.values.len().min(size.masks) <= lent.masks)
        {
            core::mem::swap(&mut gone.masks, &mut self.masks);
        }
        if self
            .whole
            .iter()
            .all(|whole| lent.covers(whole.size().min(size)))
        {
            core::mem::swap(&mut gone.whole, &mut self.whole);
        }
        gone
    }

    /// Each part longer than `size`'s shrunk in place.
    fn shrink(&mut self, size: Need) {
        for part in &mut self.words {
            shrink_part(&mut part.values, size.words, &mut part.charge);
        }
        for part in &mut self.masks {
            shrink_part(&mut part.values, size.masks, &mut part.charge);
        }
        for workspace in &mut self.whole {
            workspace.shrink(size);
        }
    }
}

/// The longer of each of `into`'s and `from`'s parts kept in `into`, which
/// keeps as many parts as the two hold the most of.
fn swap_longer_parts<T>(into: &mut Vec<Part<T>>, from: &mut Vec<Part<T>>) {
    if from.len() > into.len() {
        core::mem::swap(into, from);
    }
    for (kept, offered) in into.iter_mut().zip(from.iter_mut()) {
        if offered.values.len() > kept.values.len() {
            core::mem::swap(kept, offered);
        }
    }
}

/// A checked dispatch: its pipeline, its bind sets, its push constants as
/// words, and its workgroup counts.
pub(crate) struct Dispatch {
    pub(crate) pipeline: Arc<ComputePipelineState>,
    pub(crate) sets: Vec<Arc<BindSetState>>,
    /// Shared with the command buffer it was recorded in.
    pub(crate) push: Option<Arc<Vec<u32>>>,
    pub(crate) groups: Groups,
}

impl Dispatch {
    /// The host bytes it holds queued, its push constants and their sharing
    /// counted as though no other held them.
    pub(crate) fn held_bytes(&self) -> u64 {
        [
            core::mem::size_of::<Self>() as u64,
            self.push.as_deref().map_or(0, |words| {
                crate::model::shared_bytes::<Vec<u32>>()
                    .saturating_add(tairix_util::fallible::held(words))
            }),
            tairix_util::fallible::held(&self.sets),
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }

    /// The host bytes the placements its tiles take on a device of
    /// `participants`, kept for its job: a job's dispatches share them.
    pub(crate) fn place_bytes(&self, participants: usize) -> u64 {
        let tiles = Tiling::of(&self.pipeline.stages).tiles.max(1) as u64;
        (participants as u64)
            .saturating_mul(tiles)
            .saturating_mul(size_of::<Placement>() as u64)
    }

    /// The host bytes its run makes on a device of `participants`: the
    /// memories it locks and views, the lists it merges them from, where each
    /// set's memories are among them, the descriptors it resolves, and a slot
    /// for each participant.
    pub(crate) fn run_bytes(&self, participants: usize) -> u64 {
        use core::mem::size_of;
        let wide = |n: usize| n as u64;
        let placed = self.sets.iter().fold(0u64, |total, set| {
            total.saturating_add(wide(set.memories.len()))
        });
        let program = &self.pipeline.stages;
        table_bytes([
            (
                placed.saturating_add(1),
                size_of::<&Arc<Memory>>()
                    + size_of::<memory::Words<'_>>()
                    + size_of::<&[AtomicU32]>(),
            ),
            (
                wide(self.sets.len()).saturating_add(1),
                size_of::<&[Arc<Memory>]>(),
            ),
            (placed, size_of::<u32>()),
            (program.descriptors, size_of::<Resolved<'_>>()),
            (wide(program.bindings.len()), size_of::<&[Resolved<'_>]>()),
            (wide(participants), size_of::<Slot<'_>>()),
        ])
    }
}

/// What a run's tables take, holding each count of entries of so many
/// bytes: in 64 bits, saturating, as what a submission names may pass what
/// a 32-bit target's word can count.
fn table_bytes(tables: [(u64, usize); 6]) -> u64 {
    tables.iter().fold(0, |total, &(count, bytes)| {
        total.saturating_add(count.saturating_mul(bytes as u64))
    })
}

/// How many workgroups a dispatch makes: given, or read from a buffer when
/// the dispatch runs.
pub(crate) enum Groups {
    Direct([u32; 3]),
    Indirect { memory: Arc<Memory>, offset: u64 },
}

/// Run a dispatch in `workspaces`, its tiles placed in the job's `places`,
/// its work counted by the job's `meter`.
pub(crate) fn run(
    dispatch: &Dispatch,
    meter: &mut Meter<'_>,
    device: &Shared,
    (workspaces, places): (&mut [Workspace], &mut Vec<Placement>),
) -> Outcome {
    // Every memory the dispatch reaches, once each, merged from its sets'
    // own lists in the order memories are locked in.
    let mut lists: Vec<&[Arc<Memory>]> = Vec::new();
    if lists.try_reserve_exact(dispatch.sets.len() + 1).is_err() {
        return Outcome::OutOfMemory;
    }
    add_sets(&mut lists, &dispatch.sets);
    if let Groups::Indirect { memory, .. } = &dispatch.groups {
        lists.push(core::slice::from_ref(memory));
    }
    let ran = with_locked(&mut lists, meter, |locked, meter| {
        let program = &dispatch.pipeline.stages;
        let mapping = place_sets(&dispatch.sets, locked.memories, meter)?;
        let table = resolve(program, &dispatch.sets, (locked.views, &mapping), meter)?;
        let rows = rows_of(program, &table)?;
        // Counted, not checked: a dispatch of no workgroups has no work left,
        // and every piece checks before its first workgroup.
        meter.charge(rows.len() as u64);
        let limits = &device.info.limits;
        let view = |memory: &Arc<Memory>| locked.view(memory);
        let groups =
            counts(&dispatch.groups, &view, &limits.max_workgroup_count).ok_or(Outcome::Fault)?;
        let total = u64::from(groups[0]) * u64::from(groups[1]) * u64::from(groups[2]);
        if total == 0 {
            return Ok(Outcome::Done);
        }
        let env = Env {
            program,
            bindings: &rows,
            push: dispatch.push.as_deref().map_or(&[], Vec::as_slice),
            srgb: &device.srgb,
            subgroup_size: limits.subgroup_size,
            groups,
        };
        Ok(share(&env, total, meter, device, (workspaces, places)))
    });
    match ran {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

/// Each distinct set of `sets`' memories, in the order memories are locked
/// in, added to `lists`, which has room for them.
fn add_sets<'m>(lists: &mut Vec<&'m [Arc<Memory>]>, sets: &'m [Arc<BindSetState>]) {
    for (at, set) in sets.iter().enumerate() {
        let repeated = sets.iter().take(at).any(|seen| Arc::ptr_eq(seen, set));
        if !repeated {
            lists.push(&set.memories);
        }
    }
}

/// The memories a run locked, in the order it locked them, and the atomic
/// view of each.
pub(crate) struct Locked<'v> {
    pub(crate) memories: &'v [&'v Arc<Memory>],
    pub(crate) views: &'v [&'v [AtomicU32]],
}

impl Locked<'_> {
    /// The view of `memory`, where the run locked it.
    pub(crate) fn view(&self, memory: &Arc<Memory>) -> Option<&[AtomicU32]> {
        let at = self
            .memories
            .binary_search_by_key(&Arc::as_ptr(memory), |held| Arc::as_ptr(held))
            .ok()?;
        self.views.get(at).copied()
    }
}

/// `body` run with the memories `lists` name — each list in address order —
/// merged into one list in that order, each once, locked in that order and
/// viewed as atomic words; merging, locking and viewing are counted as they
/// go, a memory at a time, so a run of millions reads the clock among them.
pub(crate) fn with_locked<R>(
    lists: &mut [&[Arc<Memory>]],
    meter: &mut Meter<'_>,
    body: impl FnOnce(&Locked<'_>, &mut Meter<'_>) -> Result<R, Outcome>,
) -> Result<R, Outcome> {
    let named = lists.iter().map(|list| list.len()).sum::<usize>();
    let mut memories: Vec<&Arc<Memory>> = Vec::new();
    memories
        .try_reserve_exact(named)
        .map_err(|_| Outcome::OutOfMemory)?;
    if !merge(lists, &mut memories, meter) {
        return Err(Outcome::Overran);
    }
    let mut guards: Vec<memory::Words<'_>> = Vec::new();
    guards
        .try_reserve_exact(memories.len())
        .map_err(|_| Outcome::OutOfMemory)?;
    for memory in &memories {
        if !meter.proceed(1) {
            return Err(Outcome::Overran);
        }
        guards.push(memory.lock());
    }
    let mut views: Vec<&[AtomicU32]> = Vec::new();
    views
        .try_reserve_exact(guards.len())
        .map_err(|_| Outcome::OutOfMemory)?;
    for guard in &mut guards {
        if !meter.proceed(1) {
            return Err(Outcome::Overran);
        }
        views.push(AtomicU32::from_mut_slice(guard.as_mut_slice()));
    }
    body(
        &Locked {
            memories: &memories,
            views: &views,
        },
        meter,
    )
}

/// `table`, `program`'s descriptors resolved binding by binding, as one row
/// of them for each binding.
pub(crate) fn rows_of<'t, 'd>(
    program: &Program,
    table: &'t [Resolved<'d>],
) -> Result<Vec<&'t [Resolved<'d>]>, Outcome> {
    let mut rows: Vec<&[Resolved<'_>]> = Vec::new();
    rows.try_reserve_exact(program.bindings.len())
        .map_err(|_| Outcome::OutOfMemory)?;
    let mut rest = table;
    for binding in &program.bindings {
        let (row, after) = rest
            .split_at_checked(binding.count as usize)
            .ok_or(Outcome::Fault)?;
        rows.push(row);
        rest = after;
    }
    Ok(rows)
}

/// The memories `lists` name, each list in address order and naming each
/// once, merged into one list in that order, each memory once, counted on
/// `meter`: `false` where it stopped.
fn merge<'m>(
    lists: &mut [&'m [Arc<Memory>]],
    into: &mut Vec<&'m Arc<Memory>>,
    meter: &mut Meter<'_>,
) -> bool {
    while let Some(next) = lists
        .iter()
        .filter_map(|list| list.first())
        .min_by_key(|memory| Arc::as_ptr(memory))
    {
        if !meter.proceed(lists.len() as u64) {
            return false;
        }
        let address = Arc::as_ptr(next);
        into.push(next);
        for list in lists.iter_mut() {
            if let Some((first, rest)) = list.split_first() {
                if Arc::as_ptr(first) == address {
                    *list = rest;
                }
            }
        }
    }
    true
}

/// The workgroup counts along each axis, read from their buffer where the
/// dispatch is indirect; `None` past the device's `most`.
fn counts<'d>(
    groups: &Groups,
    view: &dyn Fn(&Arc<Memory>) -> Option<&'d [AtomicU32]>,
    most: &[u32; 3],
) -> Option<[u32; 3]> {
    let counts = match groups {
        Groups::Direct(counts) => *counts,
        Groups::Indirect { memory, offset } => {
            let words = view(memory)?;
            let at = |k: u64| {
                usize::try_from(offset / 4 + k)
                    .ok()
                    .and_then(|i| words.get(i))
                    .map_or(0, |w| w.load(Ordering::Relaxed))
            };
            [at(0), at(1), at(2)]
        }
    };
    counts
        .iter()
        .zip(most)
        .all(|(count, most)| count <= most)
        .then_some(counts)
}

/// A piece's own workspace, its tiles' placements, and, where it claimed any
/// workgroups, the work it left uncounted and whether it read the clock.
type Slot<'w> = Mutex<(&'w mut Workspace, &'w mut [Placement], Option<(u64, bool)>)>;

/// The first `len` of a job's `places`, grown to them where it is shorter:
/// each is written before it is read, so what an earlier dispatch left there
/// is no matter.
fn room_for(places: &mut Vec<Placement>, len: usize) -> Option<&mut [Placement]> {
    if let Some(more) = len.checked_sub(places.len()).filter(|&more| more > 0) {
        places.try_reserve_exact(more).ok()?;
        places.resize(len, Placement::EMPTY);
    }
    places.get_mut(..len)
}

/// Run the `total` workgroups across the device's runner: a piece for each
/// participant, in a workspace of its own, claiming batches of workgroups
/// from a shared cursor until none is left, so a costly stretch of them is
/// shared out rather than left to one piece. Every count starts from the
/// job's `meter`, and every piece stops once the deadline passes or one
/// faults.
fn share(
    env: &Env<'_>,
    total: u64,
    meter: &mut Meter<'_>,
    device: &Shared,
    (workspaces, places): (&mut [Workspace], &mut Vec<Placement>),
) -> Outcome {
    let fault = AtomicBool::new(false);
    let (stop, deadline) = (meter.stop, meter.deadline);
    let tiling = Tiling::of(env.program);
    let runner = &*device.runner;
    let grain = tiling.packed as u64;
    let batches = usize::try_from(total.div_ceil(grain)).unwrap_or(usize::MAX);
    let pieces = runner.width().max(1).min(batches).min(workspaces.len());
    // None only past a broken invariant: a job's pipelines keep the set
    // covering it, and the device runs one job at a time.
    if pieces == 0 {
        return Outcome::OutOfMemory;
    }
    let claim = per_claim(total, grain, pieces.saturating_mul(CLAIMS_PER_PIECE));
    let tiles = tiling.tiles.max(1);
    let Some(places) = room_for(places, pieces.saturating_mul(tiles)) else {
        return Outcome::OutOfMemory;
    };
    let mut slots: Vec<Slot<'_>> = Vec::new();
    if slots.try_reserve_exact(pieces).is_err() {
        return Outcome::OutOfMemory;
    }
    for (workspace, places) in workspaces.iter_mut().zip(places.chunks_mut(tiles)) {
        slots.push(Mutex::new((workspace, places, None)));
    }
    let counted = meter.unchecked;
    let cursor = AtomicU64::new(0);
    runner.run(pieces, &|piece| {
        // Its own slot, so never waited on: a participant running pieces in
        // turn finds the cursor spent by its first, whose count is the job's.
        let Some(slot) = slots.get(piece) else {
            fault.store(true, Ordering::Relaxed);
            stop.store(true, Ordering::Relaxed);
            return;
        };
        let mut held = slot.lock();
        let (workspace, places, left) = &mut *held;
        let mut meter = Meter {
            stop,
            clock: &*device.clock,
            deadline,
            unchecked: counted,
            read: false,
        };
        let claims = (&cursor, claim, total);
        let ran = run_piece(env, tiling, claims, (workspace, places), &mut meter);
        *left = ran.map(|_| (meter.unchecked, meter.read));
        if ran == Some(Ran::Fault) {
            fault.store(true, Ordering::Relaxed);
            stop.store(true, Ordering::Relaxed);
        }
    });
    let left = slots.into_iter().filter_map(|slot| slot.into_inner().2);
    let (unchecked, read) = counted_on(counted, left);
    meter.unchecked = unchecked;
    meter.read |= read;
    if fault.load(Ordering::Relaxed) {
        Outcome::Fault
    } else if stop.load(Ordering::Relaxed) {
        Outcome::Overran
    } else {
        Outcome::Done
    }
}

/// The work a job leaves uncounted after a dispatch whose pieces started from
/// `counted`, from what each piece that claimed workgroups `left` and whether
/// it read the clock; and whether any read it. A reading covers all the job
/// had counted, so once one is taken the others count only their own work;
/// a piece that claimed none has nothing to count.
pub(crate) fn counted_on(counted: u64, left: impl Iterator<Item = (u64, bool)>) -> (u64, bool) {
    let (mut claimed, mut read) = (false, false);
    let (mut readers, mut others) = (0, 0);
    for (count, own) in left {
        claimed = true;
        if own {
            read = true;
            readers = readers.max(count);
        } else {
            others = others.max(count);
        }
    }
    let unchecked = match (claimed, read) {
        (false, _) => counted,
        (true, false) => others,
        (true, true) => readers.max(others.saturating_sub(counted)),
    };
    (unchecked, read)
}

/// How many claims of the cursor each piece makes, about: enough that the
/// last of them even out, few enough that claiming costs nothing beside a
/// claim's work.
const CLAIMS_PER_PIECE: usize = 8;

/// Where each set's memories are among the dispatch's `merged` ones, set by
/// set in turn: found by walking both lists in address order, so a set of
/// many memories costs about its own length and a set of few about their
/// searches, each counted on `meter`. A set holding every merged memory — a
/// lone set, unless an indirect dispatch reads its counts from another memory —
/// needs none: its own places are the merged ones.
pub(crate) fn place_sets(
    sets: &[Arc<BindSetState>],
    merged: &[&Arc<Memory>],
    meter: &mut Meter<'_>,
) -> Result<Mapping, Outcome> {
    let mut mapping = Mapping {
        places: Vec::new(),
        starts: tairix_inline::ArrayVec::new(),
    };
    let whole = |set: &BindSetState| set.memories.len() == merged.len();
    let total = sets
        .iter()
        .filter(|set| !whole(set))
        .map(|set| set.memories.len())
        .sum::<usize>();
    mapping
        .places
        .try_reserve_exact(total)
        .map_err(|_| Outcome::OutOfMemory)?;
    for set in sets {
        let start = (!whole(set)).then_some(mapping.places.len());
        mapping.starts.try_push(start).map_err(|_| Outcome::Fault)?;
        if start.is_none() {
            continue;
        }
        let mut from = 0;
        for memory in &set.memories {
            if !meter.proceed(1) {
                return Err(Outcome::Overran);
            }
            let at = place_from(merged, from, memory).ok_or(Outcome::Fault)?;
            mapping
                .places
                .push(u32::try_from(at).map_err(|_| Outcome::Fault)?);
            from = at + 1;
        }
    }
    Ok(mapping)
}

/// Each set's memories by their place among a dispatch's, and where each
/// set's run of them starts: `None` for a set whose own are the dispatch's.
pub(crate) struct Mapping {
    places: Vec<u32>,
    starts: tairix_inline::ArrayVec<Option<usize>, { crate::pipeline::MAX_DISPATCH_SETS }>,
}

impl Mapping {
    /// The place among the dispatch's memories of set `set`'s memory `place`;
    /// none for a descriptor that reaches no memory.
    fn merged(&self, set: usize, place: u32) -> Option<usize> {
        if place == NO_MEMORY {
            return None;
        }
        let place = place as usize;
        match *self.starts.get(set)? {
            None => Some(place),
            Some(start) => self
                .places
                .get(start.checked_add(place)?)
                .map(|&at| at as usize),
        }
    }
}

/// The place of `memory` in `merged` at or past `from`: steps that double,
/// then a search of the last one.
fn place_from(merged: &[&Arc<Memory>], from: usize, memory: &Arc<Memory>) -> Option<usize> {
    let key = Arc::as_ptr(memory);
    let rest = merged.get(from..)?;
    let mut reach = 1;
    while rest.get(reach).is_some_and(|held| Arc::as_ptr(held) < key) {
        reach *= 2;
    }
    let window = rest.get(..(reach + 1).min(rest.len()))?;
    let at = window
        .binary_search_by_key(&key, |held| Arc::as_ptr(held))
        .ok()?;
    Some(from + at)
}

/// Each of the program's bindings, descriptor by descriptor, through the
/// dispatch's bind sets, one row after another, each counted on `meter`:
/// [`Outcome::Fault`] where one cannot be, [`Outcome::OutOfMemory`] where the
/// host refuses the table, and [`Outcome::Overran`] where the deadline passes
/// first.
pub(crate) fn resolve<'d>(
    program: &Program,
    sets: &'d [Arc<BindSetState>],
    (views, mapping): (&[&'d [AtomicU32]], &Mapping),
    meter: &mut Meter<'_>,
) -> Result<Vec<Resolved<'d>>, Outcome> {
    let mut table = Vec::new();
    let descriptors = usize::try_from(program.descriptors).map_err(|_| Outcome::OutOfMemory)?;
    table
        .try_reserve_exact(descriptors)
        .map_err(|_| Outcome::OutOfMemory)?;
    for binding in &program.bindings {
        let at = binding.set as usize;
        let set = sets.get(at).ok_or(Outcome::Fault)?;
        let first = binding.first as usize;
        let count = binding.count as usize;
        let bound = set
            .resources
            .get(first..first + count)
            .ok_or(Outcome::Fault)?;
        let places = set.places.get(first..first + count).ok_or(Outcome::Fault)?;
        for (bound, &place) in bound.iter().zip(places) {
            if !meter.proceed(1) {
                return Err(Outcome::Overran);
            }
            let words = mapping
                .merged(at, place)
                .and_then(|merged| views.get(merged).copied());
            table.push(resolved(bound, words).ok_or(Outcome::Fault)?);
        }
    }
    Ok(table)
}

/// `bound` as a dispatch reads it, its memory's words `words`: none for a
/// sampler, which reaches none.
fn resolved<'d>(bound: &'d Bound, words: Option<&'d [AtomicU32]>) -> Option<Resolved<'d>> {
    let image = |state: &'d crate::resource::ViewState| {
        Some(super::shader::exec::ImageAt {
            words: words?,
            base: state.image.offset,
            layout: &state.image.layout,
            desc: &state.image.desc,
            view: &state.desc,
        })
    };
    Some(match bound {
        Bound::Buffer {
            buffer,
            offset,
            size,
        } => Resolved::Buffer {
            words: words?,
            offset: buffer.offset + offset,
            size: *size,
        },
        Bound::TexelBuffer(texels) => Resolved::Texels {
            words: words?,
            offset: texels.buffer.offset + texels.desc.offset,
            size: texels.desc.size,
            format: texels.desc.format,
        },
        Bound::Image(state) => Resolved::Image(image(state)?),
        Bound::ImageSampler(state, sampler) => Resolved::ImageSampler(image(state)?, &sampler.desc),
        Bound::Sampler(sampler) => Resolved::Sampler(&sampler.desc),
    })
}

/// Run the batches of workgroups claimed from `cursor`, `claim` at a time,
/// until none of the `total` is left, in `workspace`, each tile placed in
/// `places`: cleared the first time it claims one, as it last served another
/// job, perhaps another context's. `None` where it claimed none.
fn run_piece<'d>(
    env: &Env<'d>,
    tiling: Tiling,
    (cursor, claim, total): (&AtomicU64, u64, u64),
    (workspace, places): (&mut Workspace, &mut [Placement]),
    meter: &mut Meter<'d>,
) -> Option<Ran> {
    let program = env.program;
    let parts = Parts::of(program);
    let wg_words = program.workgroup_words as usize;
    let (tiles, resident) = (tiling.tiles, tiling.resident.max(1));
    let tile_words = parts.words();
    let tile_masks = parts.masks_len();
    let split = (resident * tile_words).min(workspace.words.len());
    let (tile_area, workgroup_area) = workspace.words.split_at_mut(split);
    let masks = workspace
        .masks
        .get_mut(..resident * tile_masks)
        .unwrap_or(&mut []);
    let mut cleared = false;
    // The workgroups a batch holds that the tiles' lanes are laid out for.
    let mut laid = 0;
    loop {
        let first = cursor.fetch_add(claim, Ordering::Relaxed);
        if first >= total {
            // Cleared only once it had claimed some.
            return cleared.then_some(Ran::Finished);
        }
        let last = first.saturating_add(claim).min(total);
        if !cleared {
            // Each tile's registers and blocks; the memory each tile start
            // clears itself.
            let registers = parts.planes + parts.uniforms;
            for tile in tile_area.chunks_mut(tile_words.max(1)) {
                if !clear(tile.get_mut(..registers).unwrap_or(&mut []), meter) {
                    return Some(Ran::Stopped);
                }
            }
            if !clear(masks, meter) {
                return Some(Ran::Stopped);
            }
            cleared = true;
        }
        let mut group = first;
        while group < last {
            let count = if tiling.packed > 1 {
                usize::try_from(last - group).map_or(tiling.packed, |left| tiling.packed.min(left))
            } else {
                1
            };
            if !meter.proceed(0) {
                return Some(Ran::Stopped);
            }
            if let Some(memory) = workgroup_area.get_mut(..count * wg_words) {
                memory.fill(0);
            }
            meter.charge((count * wg_words) as u64 + 1);
            for (tile, place) in places.iter_mut().enumerate().take(tiles) {
                if laid != count {
                    *place = lanes_of(env, tiling, count, tile);
                    meter.charge(LANES as u64);
                }
                stamp_ids(place, (group, count), env.groups);
                meter.charge(count as u64);
            }
            laid = count;
            let mut ran = [Ran::Barrier; MAX_TILES];
            let mut started = false;
            loop {
                let mut waiting = false;
                for (tile, place) in places.iter().enumerate().take(tiles) {
                    if started && ran.get(tile) != Some(&Ran::Barrier) {
                        continue;
                    }
                    let region = tile % resident;
                    let words = tile_area.get_mut(region * tile_words..(region + 1) * tile_words);
                    let blocks = masks.get_mut(region * tile_masks..(region + 1) * tile_masks);
                    let (Some(words), Some(blocks)) = (words, blocks) else {
                        return Some(Ran::Fault);
                    };
                    let Some(space) = carve(words, blocks, parts, workgroup_area) else {
                        return Some(Ran::Fault);
                    };
                    let mut machine = Machine::new(env, place, space, meter);
                    if !started && !machine.begin() {
                        return Some(Ran::Stopped);
                    }
                    let outcome = machine.run();
                    match outcome {
                        Ran::Stopped | Ran::Fault => return Some(outcome),
                        Ran::Barrier => waiting = true,
                        Ran::Finished => {}
                    }
                    if let Some(slot) = ran.get_mut(tile) {
                        *slot = outcome;
                    }
                }
                started = true;
                if !waiting {
                    break;
                }
            }
            group += count as u64;
        }
    }
}

/// `part` zeroed a check's work at a time, the clock read before each:
/// `false` once the dispatch is stopped.
fn clear<T: Copy + Default>(part: &mut [T], meter: &mut Meter<'_>) -> bool {
    let per = usize::try_from(WORK_PER_CHECK).unwrap_or(usize::MAX);
    for chunk in part.chunks_mut(per) {
        if !meter.proceed(0) {
            return false;
        }
        chunk.fill(T::default());
        meter.charge(chunk.len() as u64);
    }
    true
}

/// The most tiles one workgroup takes: what a workgroup of the most
/// invocations any device the seam describes states fills.
pub(crate) const MAX_TILES: usize = 1024 / LANES;

/// A tile's parts carved from its words and masks.
pub(crate) fn carve<'w>(
    words: &'w mut [u32],
    masks: &'w mut [u64],
    parts: Parts,
    workgroup: &'w mut [u32],
) -> Option<Space<'w>> {
    let (planes, rest) = words.split_at_mut(parts.planes.min(words.len()));
    let (uniforms, rest) = rest.split_at_mut(parts.uniforms.min(rest.len()));
    let (private, rest) = rest.split_at_mut(parts.private.min(rest.len()));
    let shared = rest.get_mut(..parts.shared)?;
    let (block_masks, rest) = masks.split_at_mut(parts.masks.min(masks.len()));
    let (pending, rest) = rest.split_at_mut(parts.pending.min(rest.len()));
    let [live, killed, ..] = rest else {
        return None;
    };
    Some(Space {
        planes,
        uniforms,
        private,
        shared,
        masks: block_masks,
        pending,
        live,
        killed,
        workgroup,
    })
}

/// Where each lane of tile `tile` stands among the `count` workgroups it
/// holds, or in its share of one: all of its placement but the workgroups'
/// ids, which each batch stamps.
fn lanes_of(env: &Env<'_>, tiling: Tiling, count: usize, tile: usize) -> Placement {
    let n = tiling.invocations;
    let width = env.subgroup_size.max(1) as usize;
    let mut place = Placement::EMPTY;
    for lane in 0..LANES {
        let (slot, local) = if tiling.packed > 1 {
            (lane / n.max(1), lane % n.max(1))
        } else {
            (0, tile * LANES + lane)
        };
        if slot >= count || local >= n {
            continue;
        }
        place.live |= 1 << lane;
        let within = local % width;
        let first = lane - within;
        let len = width.min(n - (local - within));
        if let (Some(l), Some(s), Some(sg)) = (
            place.local.get_mut(lane),
            place.group_slot.get_mut(lane),
            place.subgroup.get_mut(lane),
        ) {
            *l = u32::try_from(local).unwrap_or(0);
            *s = u32::try_from(slot).unwrap_or(0);
            *sg = (
                u8::try_from(first).unwrap_or(0),
                u8::try_from(len.min(LANES - first)).unwrap_or(0),
            );
        }
    }
    place
}

/// The ids of the `count` workgroups from linear workgroup `group` of a
/// dispatch of `groups`, stamped into `place` by their place among its own:
/// each stepped from the one before, so only the first is divided out.
fn stamp_ids(place: &mut Placement, (group, count): (u64, usize), groups: [u32; 3]) {
    let [x, y, _] = groups.map(|axis| axis.max(1));
    let mut id = workgroup_id(group, groups);
    for slot in place.group_id.iter_mut().take(count) {
        *slot = id;
        id[0] = id[0].saturating_add(1);
        if id[0] == x {
            id[0] = 0;
            id[1] = id[1].saturating_add(1);
            if id[1] == y {
                id[1] = 0;
                id[2] = id[2].saturating_add(1);
            }
        }
    }
}

/// How many of `total` workgroups make each of `claims` claims: a whole
/// number of tiles of `grain` workgroups, at least one. Counted in 64 bits,
/// as a dispatch may make more workgroups than a 32-bit target can count.
fn per_claim(total: u64, grain: u64, claims: usize) -> u64 {
    let grain = grain.max(1);
    total.div_ceil(grain).div_ceil(claims.max(1) as u64).max(1) * grain
}

/// The id of the workgroup at `linear` in a dispatch of `groups`: past four
/// billion workgroups the index needs all 64 bits.
fn workgroup_id(linear: u64, groups: [u32; 3]) -> [u32; 3] {
    let [x, y, _] = groups.map(|count| u64::from(count.max(1)));
    let axis = |at: u64| u32::try_from(at).unwrap_or(u32::MAX);
    [
        axis(linear % x),
        axis(linear / x % y),
        axis(linear / (x * y)),
    ]
}

/// The memories `resources` reach, once each, in the order every holder
/// locks memories in: by address; and each resource's memory by its place
/// among them, [`NO_MEMORY`] for a sampler's. Sorted as borrowed, so each
/// memory is counted once more however many resources reach it.
pub(crate) fn memories_of(resources: &[Bound]) -> Result<(Vec<Arc<Memory>>, Vec<u32>), Error> {
    let mut reached: Vec<(&Arc<Memory>, u32)> = Vec::new();
    reached
        .try_reserve_exact(resources.len())
        .map_err(exhausted)?;
    for (index, bound) in resources.iter().enumerate() {
        if let Some(memory) = bound.memory() {
            reached.push((memory, u32::try_from(index).map_err(exhausted)?));
        }
    }
    reached.sort_unstable_by_key(|&(memory, _)| Arc::as_ptr(memory));
    let same = |a: &(&Arc<Memory>, u32), b: &(&Arc<Memory>, u32)| Arc::ptr_eq(a.0, b.0);
    let unique = reached.chunk_by(same).count();
    let mut memories = Vec::new();
    memories.try_reserve_exact(unique).map_err(exhausted)?;
    let mut places =
        tairix_util::fallible::filled(resources.len(), NO_MEMORY).ok_or(Error::OutOfMemory)?;
    for run in reached.chunk_by(same) {
        let Some(&(memory, _)) = run.first() else {
            continue;
        };
        let place = u32::try_from(memories.len()).map_err(exhausted)?;
        memories.push(Arc::clone(memory));
        for &(_, index) in run {
            if let Some(slot) = places.get_mut(index as usize) {
                *slot = place;
            }
        }
    }
    Ok((memories, places))
}

/// The place a resource that reaches no memory has.
pub(crate) const NO_MEMORY: u32 = u32::MAX;

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
