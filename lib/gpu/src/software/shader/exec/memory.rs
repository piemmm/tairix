//! Memory as a tile reaches it: pointers walked through access chains, loads
//! and stores checked scalar by scalar against what their object holds, and
//! atomics. An access past its object reads zero and writes nothing, and a
//! pointer once past stays past.

use core::sync::atomic::{AtomicU32, Ordering};

use super::{each_lane, Env, Lanes, Machine};
use crate::format::Format;
use crate::image::{ImageDesc, ImageViewDesc};
use crate::sampler::SamplerDesc;
use crate::software::layout::ImageLayout;
use crate::software::shader::ir::{Atomic, Order, Plan, Reg, Root, Step, LANES};
use crate::software::shader::ops;

/// A pointer's offset once it is past its object.
pub(crate) const INVALID: u32 = u32::MAX;

/// One descriptor of a dispatch's bindings, with the memory it reaches
/// shared between the cores the dispatch runs on.
#[derive(Copy, Clone)]
pub(crate) enum Resolved<'d> {
    /// `size` bytes from byte `offset` of `words`.
    Buffer {
        words: &'d [AtomicU32],
        offset: u64,
        size: u64,
    },
    /// A texel buffer: `size` bytes of `format` texels from byte `offset`.
    Texels {
        words: &'d [AtomicU32],
        offset: u64,
        size: u64,
        format: Format,
    },
    Image(ImageAt<'d>),
    Sampler(&'d SamplerDesc),
    ImageSampler(ImageAt<'d>, &'d SamplerDesc),
}

/// An image as a view sees it: its memory from byte `base`, its layout, and
/// the view.
#[derive(Copy, Clone)]
pub(crate) struct ImageAt<'d> {
    pub(crate) words: &'d [AtomicU32],
    pub(crate) base: u64,
    pub(crate) layout: &'d ImageLayout,
    pub(crate) desc: &'d ImageDesc,
    pub(crate) view: &'d ImageViewDesc,
}

impl<'d> Resolved<'d> {
    /// The buffer it is, if it is one.
    fn buffer(&self) -> Option<Buffer<'d>> {
        match *self {
            Self::Buffer {
                words,
                offset,
                size,
            } => Some(Buffer {
                words,
                offset,
                size,
            }),
            _ => None,
        }
    }
}

/// A buffer's memory: `size` bytes from byte `offset` of `words`.
#[derive(Copy, Clone)]
struct Buffer<'d> {
    words: &'d [AtomicU32],
    offset: u64,
    size: u64,
}

/// What a load or store of a buffer reaches, found once for all its
/// scalars: the push constants, one buffer for the descriptor every lane
/// names, or each lane's own.
#[expect(
    clippy::large_enum_variant,
    reason = "one instruction's stack temporary: boxing each lane's buffer would allocate on every load and store"
)]
enum Reach<'d> {
    Push(&'d [u32]),
    /// The buffer descriptor `element` names, the one any lane reaches.
    One {
        buffer: Option<Buffer<'d>>,
        element: u64,
    },
    Each([Option<Buffer<'d>>; LANES]),
}

impl Reach<'_> {
    /// The buffer `lane` reaches through descriptor `element`.
    fn buffer(&self, (lane, element): (usize, u64)) -> Option<Buffer<'_>> {
        match self {
            Self::Push(_) => None,
            Self::One {
                buffer,
                element: named,
            } => buffer.filter(|_| element == *named),
            Self::Each(each) => each.get(lane).copied().flatten(),
        }
    }

    /// The scalar of `bits` at byte `byte` past `at`: zero past its object.
    fn read(&self, lane: (usize, u64), at: u64, (byte, bits): (u32, u8)) -> u64 {
        let Some(byte) = past(at, byte) else {
            return 0;
        };
        if let Self::Push(push) = self {
            return if fits(byte, bits, push.len() as u64 * 4) {
                push_read(push, byte, bits)
            } else {
                0
            };
        }
        match self.buffer(lane) {
            Some(buffer) if fits(byte, bits, buffer.size) => {
                read(buffer.words, buffer.offset + byte, bits)
            }
            _ => 0,
        }
    }

    /// `value` written as the scalar of `bits` at byte `byte` past `at`:
    /// nothing past its object, and nothing to the push constants.
    fn write(&self, lane: (usize, u64), at: u64, (byte, bits): (u32, u8), value: u64) {
        let Some(byte) = past(at, byte) else {
            return;
        };
        if let Some(buffer) = self.buffer(lane) {
            if fits(byte, bits, buffer.size) {
                write(buffer.words, buffer.offset + byte, bits, value);
            }
        }
    }
}

/// Byte `byte` past offset `at`, or `None` for a pointer past its object.
fn past(at: u64, byte: u32) -> Option<u64> {
    at.checked_add(u64::from(byte))
        .filter(|_| at != u64::from(INVALID))
}

/// Where each lane's value starts in a root held as registers hold values:
/// one place for every lane, or each lane its own; `None` past the root.
#[expect(
    clippy::large_enum_variant,
    reason = "one instruction's stack temporary: boxing each lane's place would allocate on every load and store"
)]
enum Starts {
    All(Option<usize>),
    Each([Option<usize>; LANES]),
}

impl Starts {
    fn of(&self, lane: usize) -> Option<usize> {
        match self {
            Self::All(first) => *first,
            Self::Each(each) => each.get(lane).copied().flatten(),
        }
    }
}

/// The ordering a load takes for `order`.
const fn load_order(order: Order) -> Ordering {
    match order {
        Order::Relaxed | Order::Release => Ordering::Relaxed,
        Order::Acquire | Order::AcquireRelease => Ordering::Acquire,
        Order::SeqCst => Ordering::SeqCst,
    }
}

/// The ordering a store takes for `order`.
const fn store_order(order: Order) -> Ordering {
    match order {
        Order::Relaxed | Order::Acquire => Ordering::Relaxed,
        Order::Release | Order::AcquireRelease => Ordering::Release,
        Order::SeqCst => Ordering::SeqCst,
    }
}

/// The ordering a read-modify-write takes for `order`.
const fn rmw_order(order: Order) -> Ordering {
    match order {
        Order::Relaxed => Ordering::Relaxed,
        Order::Acquire => Ordering::Acquire,
        Order::Release => Ordering::Release,
        Order::AcquireRelease => Ordering::AcqRel,
        Order::SeqCst => Ordering::SeqCst,
    }
}

/// A memory barrier of `order`: none for a relaxed one.
pub(crate) fn fence(order: Order) {
    if order != Order::Relaxed {
        core::sync::atomic::fence(rmw_order(order));
    }
}

/// The 32-bit word at byte `byte` of `words`.
fn word_at(words: &[AtomicU32], byte: u64) -> Option<&AtomicU32> {
    words.get(usize::try_from(byte / 4).ok()?)
}

/// `bits` bits at byte `byte` of `words`, which their width aligns.
fn read(words: &[AtomicU32], byte: u64, bits: u8) -> u64 {
    match bits {
        16 => word_at(words, byte).map_or(0, |word| {
            let value = word.load(Ordering::Relaxed);
            u64::from(value >> ((byte % 4) * 8) & 0xffff)
        }),
        64 => {
            let low = word_at(words, byte).map_or(0, |w| w.load(Ordering::Relaxed));
            let high = word_at(words, byte + 4).map_or(0, |w| w.load(Ordering::Relaxed));
            u64::from(low) | u64::from(high) << 32
        }
        _ => word_at(words, byte).map_or(0, |word| u64::from(word.load(Ordering::Relaxed))),
    }
}

/// Write `bits` bits of `value` at byte `byte` of `words`. A 16-bit write
/// clears its half and sets it, so a write of the other half racing it is
/// never lost.
fn write(words: &[AtomicU32], byte: u64, bits: u8, value: u64) {
    match bits {
        16 => {
            if let Some(word) = word_at(words, byte) {
                let shift = (byte % 4) * 8;
                let half = super::low32(value & 0xffff) << shift;
                word.fetch_and(!(0xffff << shift), Ordering::Relaxed);
                word.fetch_or(half, Ordering::Relaxed);
            }
        }
        64 => {
            if let Some(word) = word_at(words, byte) {
                word.store(super::low32(value), Ordering::Relaxed);
            }
            if let Some(word) = word_at(words, byte + 4) {
                word.store(super::high32(value), Ordering::Relaxed);
            }
        }
        _ => {
            if let Some(word) = word_at(words, byte) {
                word.store(super::low32(value), Ordering::Relaxed);
            }
        }
    }
}

/// Whether a scalar of `bits` bits at byte `byte` lies within `size` bytes,
/// aligned as its width needs.
fn fits(byte: u64, bits: u8, size: u64) -> bool {
    let bytes = u64::from(bits / 8).max(1);
    let align = bytes.min(4);
    byte.is_multiple_of(align) && byte.checked_add(bytes).is_some_and(|end| end <= size)
}

impl<'d> Machine<'_, 'd, '_> {
    /// The pointer `base` walked through `steps`.
    pub(super) fn chain(&mut self, dst: Reg, base: Reg, steps: (u32, u32)) -> Option<()> {
        let program = self.env.program;
        let steps = program.steps.get(steps.0 as usize..steps.1 as usize)?;
        let (mut element, mut offset) = ([0; LANES], [0; LANES]);
        self.fetch(base, 0, false, &mut element);
        self.fetch(base, 1, false, &mut offset);
        let mut index = [0; LANES];
        for step in steps {
            match *step {
                Step::Bytes(bytes) => {
                    for lane in self.span.clone() {
                        if let Some(at) = offset.get_mut(lane) {
                            *at = advance(*at, i128::from(bytes));
                        }
                    }
                }
                Step::Index {
                    index: by,
                    num,
                    stride,
                } => {
                    self.fetch(by, 0, num.words() == 2, &mut index);
                    for lane in self.span.clone() {
                        let value = ops::signed(index.get(lane).copied().unwrap_or(0), num.bits());
                        if let Some(at) = offset.get_mut(lane) {
                            *at = advance(*at, i128::from(value) * i128::from(stride));
                        }
                    }
                }
                Step::Element { index: by, num } => {
                    self.fetch(by, 0, num.words() == 2, &mut index);
                    for lane in self.span.clone() {
                        let value = index.get(lane).copied().unwrap_or(0) & ops::mask(num.bits());
                        if let Some(at) = element.get_mut(lane) {
                            *at = value.min(u64::from(u32::MAX));
                        }
                    }
                }
            }
        }
        self.store(dst, 0, false, &element);
        self.store(dst, 1, false, &offset);
        Some(())
    }

    pub(super) fn load(&mut self, dst: Reg, root: Root, ptr: Reg, plan: u32) -> Option<()> {
        let program = self.env.program;
        let mut offset = [0; LANES];
        self.fetch(ptr, 1, false, &mut offset);
        let mut values = [0; LANES];
        match program.plans.get(plan as usize)? {
            Plan::Words(words) => {
                let starts = self.held_starts(root, ptr, &offset, *words);
                for word in 0..*words {
                    self.held_words(root, &starts, word, &mut values);
                    self.store(dst, word, false, &values);
                }
            }
            Plan::Scalars(first, last) => {
                let scalars = program.scalars.get(*first as usize..*last as usize)?;
                let mut element = [0; LANES];
                self.fetch(ptr, 0, false, &mut element);
                let reach = self.reach(root, ptr, &element);
                for scalar in scalars {
                    for lane in self.span.clone() {
                        let e = element.get(lane).copied().unwrap_or(0);
                        let at = offset.get(lane).copied().unwrap_or(u64::from(INVALID));
                        if let Some(v) = values.get_mut(lane) {
                            *v = reach.read((lane, e), at, (scalar.byte, scalar.bits));
                        }
                    }
                    self.store(dst, scalar.word, scalar.bits == 64, &values);
                }
            }
        }
        Some(())
    }

    pub(super) fn store_memory(&mut self, src: Reg, root: Root, ptr: Reg, plan: u32) -> Option<()> {
        let program = self.env.program;
        let mut offset = [0; LANES];
        self.fetch(ptr, 1, false, &mut offset);
        let lanes = self.mask;
        let mut values = [0; LANES];
        match program.plans.get(plan as usize)? {
            Plan::Words(words) => {
                let starts = self.held_starts(root, ptr, &offset, *words);
                for word in 0..*words {
                    self.fetch(src, word, false, &mut values);
                    self.put_words(root, &starts, word, &values);
                }
            }
            Plan::Scalars(first, last) => {
                let scalars = program.scalars.get(*first as usize..*last as usize)?;
                let mut element = [0; LANES];
                self.fetch(ptr, 0, false, &mut element);
                let reach = self.reach(root, ptr, &element);
                for scalar in scalars {
                    self.fetch(src, scalar.word, scalar.bits == 64, &mut values);
                    for lane in each_lane(lanes) {
                        let e = element.get(lane).copied().unwrap_or(0);
                        let at = offset.get(lane).copied().unwrap_or(u64::from(INVALID));
                        let value = values.get(lane).copied().unwrap_or(0);
                        reach.write((lane, e), at, (scalar.byte, scalar.bits), value);
                    }
                }
            }
        }
        Some(())
    }

    /// Where a `words`-word value at each lane's `offset` starts in a root
    /// held as registers hold values, found once for every word of it, as
    /// the bound holds for all its words alike.
    fn held_starts(&self, root: Root, ptr: Reg, offset: &Lanes, words: u32) -> Starts {
        let start = |lane: usize| {
            let at = offset.get(lane).copied().unwrap_or(u64::from(INVALID));
            held_index(root, at, 0, words)
        };
        if ptr.is_uniform() {
            return Starts::All(start(self.span.start));
        }
        let mut each = [None; LANES];
        for lane in self.span.clone() {
            if let Some(slot) = each.get_mut(lane) {
                *slot = start(lane);
            }
        }
        Starts::Each(each)
    }

    /// Word `word` of the value `starts` places for each lane in held
    /// `root`: zero past its end.
    fn held_words(&self, root: Root, starts: &Starts, word: u32, values: &mut Lanes) {
        let span = self.span.clone();
        let word = word as usize;
        let space = &self.space;
        let Some(out) = values.get_mut(span.clone()) else {
            return;
        };
        let lanes = out.iter_mut().zip(span.clone());
        match (root, starts) {
            (Root::Private { .. }, Starts::All(Some(first))) => {
                let base = (first + word) * LANES;
                match space.private.get(base + span.start..base + span.end) {
                    Some(held) => {
                        for (value, &held) in out.iter_mut().zip(held) {
                            *value = u64::from(held);
                        }
                    }
                    None => out.fill(0),
                }
            }
            (Root::Private { .. }, _) => {
                for (value, lane) in lanes {
                    let held = starts
                        .of(lane)
                        .and_then(|first| space.private.get((first + word) * LANES + lane));
                    *value = held.map_or(0, |&held| u64::from(held));
                }
            }
            (Root::Shared { .. }, _) => {
                for (value, lane) in lanes {
                    let held = starts
                        .of(lane)
                        .and_then(|first| space.shared.get(first + word));
                    *value = held.map_or(0, |&held| u64::from(held));
                }
            }
            (Root::Workgroup { .. }, _) => {
                let per = self.env.program.workgroup_words as usize;
                for (value, lane) in lanes {
                    let group = self.place.group_slot.get(lane).copied().unwrap_or(0) as usize;
                    let held = starts
                        .of(lane)
                        .and_then(|first| space.workgroup.get(group * per + first + word));
                    *value = held.map_or(0, |&held| u64::from(held));
                }
            }
            (Root::Binding(_) | Root::PushConstants, _) => out.fill(0),
        }
    }

    /// Word `word` of each lane's value of the running lanes written where
    /// `starts` places it in held `root`: nothing past its end.
    fn put_words(&mut self, root: Root, starts: &Starts, word: u32, values: &Lanes) {
        let span = self.span.clone();
        let word = word as usize;
        let lanes = self.mask;
        let full = lanes == super::span_mask(&span);
        match (root, starts) {
            (Root::Private { .. }, Starts::All(Some(first))) if full => {
                let base = (first + word) * LANES;
                let held = self
                    .space
                    .private
                    .get_mut(base + span.start..base + span.end);
                if let (Some(held), Some(values)) = (held, values.get(span)) {
                    for (held, &value) in held.iter_mut().zip(values) {
                        *held = super::low32(value);
                    }
                }
            }
            (Root::Private { .. }, _) => {
                for lane in each_lane(lanes) {
                    let slot = starts.of(lane).and_then(|first| {
                        self.space.private.get_mut((first + word) * LANES + lane)
                    });
                    if let (Some(slot), Some(&value)) = (slot, values.get(lane)) {
                        *slot = super::low32(value);
                    }
                }
            }
            (Root::Workgroup { .. }, _) => {
                let per = self.env.program.workgroup_words as usize;
                for lane in each_lane(lanes) {
                    let group = self.place.group_slot.get(lane).copied().unwrap_or(0) as usize;
                    let slot = starts
                        .of(lane)
                        .and_then(|first| self.space.workgroup.get_mut(group * per + first + word));
                    if let (Some(slot), Some(&value)) = (slot, values.get(lane)) {
                        *slot = super::low32(value);
                    }
                }
            }
            (Root::Shared { .. } | Root::Binding(_) | Root::PushConstants, _) => {}
        }
    }

    /// What a load or store of buffer `root` reaches, found once for all its
    /// scalars: from `element`, each lane's descriptor.
    fn reach(&self, root: Root, ptr: Reg, element: &Lanes) -> Reach<'d> {
        let env: &Env<'d> = self.env;
        let row = match root {
            Root::PushConstants => return Reach::Push(env.push),
            Root::Binding(binding) => env.bindings.get(binding as usize).copied().unwrap_or(&[]),
            Root::Private { .. } | Root::Shared { .. } | Root::Workgroup { .. } => &[],
        };
        let at = |e: u64| {
            usize::try_from(e)
                .ok()
                .and_then(|e| row.get(e))
                .and_then(Resolved::buffer)
        };
        // Every lane names the same descriptor, or none but the first.
        if ptr.is_uniform() || row.len() <= 1 {
            let named = if ptr.is_uniform() {
                element.get(self.span.start).copied().unwrap_or(0)
            } else {
                0
            };
            return Reach::One {
                buffer: at(named),
                element: named,
            };
        }
        let mut each = [None; LANES];
        for lane in self.span.clone() {
            if let Some(slot) = each.get_mut(lane) {
                *slot = at(element.get(lane).copied().unwrap_or(0));
            }
        }
        Reach::Each(each)
    }

    /// The buffer a root's descriptor `element` reaches.
    fn buffer(&self, root: Root, element: u64) -> Option<Buffer<'d>> {
        let env: &Env<'d> = self.env;
        let Root::Binding(binding) = root else {
            return None;
        };
        env.bindings
            .get(binding as usize)?
            .get(usize::try_from(element).ok()?)
            .and_then(Resolved::buffer)
    }

    pub(super) fn array_length(
        &mut self,
        dst: Reg,
        root: Root,
        ptr: Reg,
        (offset, stride): (u32, u32),
    ) {
        let (mut element, mut at) = ([0; LANES], [0; LANES]);
        self.fetch(ptr, 0, false, &mut element);
        self.fetch(ptr, 1, false, &mut at);
        let mut out = [0; LANES];
        for lane in self.span.clone() {
            let e = element.get(lane).copied().unwrap_or(0);
            let start = at.get(lane).copied().unwrap_or(u64::from(INVALID));
            let size = self.buffer(root, e).map_or(0, |buffer| buffer.size);
            let length = if start == u64::from(INVALID) || stride == 0 {
                0
            } else {
                size.saturating_sub(start + u64::from(offset)) / u64::from(stride)
            };
            if let Some(o) = out.get_mut(lane) {
                *o = length.min(u64::from(u32::MAX));
            }
        }
        self.store(dst, 0, false, &out);
    }

    /// An atomic on the word a pointer names, lane by lane in order, so the
    /// results within a tile follow its lanes.
    pub(super) fn atomic(
        &mut self,
        op: Atomic,
        order: Order,
        root: Root,
        ptr: Reg,
        dst: Reg,
        (a, b): (Reg, Reg),
    ) {
        let mut out = [0; LANES];
        for lane in each_lane(self.mask) {
            let (element, offset) = (self.word(ptr, 0, lane), self.word(ptr, 1, lane));
            let operands = (
                super::low32(self.word(a, 0, lane)),
                super::low32(self.word(b, 0, lane)),
            );
            let old = match root {
                Root::Workgroup { .. } => self.workgroup_atomic(op, root, lane, offset, operands),
                _ => self
                    .atomic_word(root, element, offset)
                    .map_or(0, |word| device_atomic(word, op, order, operands)),
            };
            if let Some(o) = out.get_mut(lane) {
                *o = u64::from(old);
            }
        }
        if op != Atomic::Store {
            self.store(dst, 0, false, &out);
        }
    }

    /// The word of device memory an atomic's pointer names: a buffer's, or
    /// an image's texel a texel pointer named.
    fn atomic_word(&self, root: Root, element: u64, offset: u64) -> Option<&AtomicU32> {
        if offset == u64::from(INVALID) {
            return None;
        }
        if let Some(buffer) = self.buffer(root, element) {
            return fits(offset, 32, buffer.size)
                .then(|| word_at(buffer.words, buffer.offset + offset))
                .flatten();
        }
        let Root::Binding(binding) = root else {
            return None;
        };
        let resolved = self
            .env
            .bindings
            .get(binding as usize)?
            .get(usize::try_from(element).ok()?)?;
        match resolved {
            Resolved::Image(image) => fits(offset, 32, image.layout.size)
                .then(|| word_at(image.words, image.base + offset))
                .flatten(),
            Resolved::Texels {
                words,
                offset: start,
                size,
                ..
            } => fits(offset, 32, *size)
                .then(|| word_at(words, start + offset))
                .flatten(),
            _ => None,
        }
    }

    fn workgroup_atomic(
        &mut self,
        op: Atomic,
        root: Root,
        lane: usize,
        offset: u64,
        (value, comparator): (u32, u32),
    ) -> u32 {
        let Some(index) = held_index(root, offset, 0, 1) else {
            return 0;
        };
        let slot = self.place.group_slot.get(lane).copied().unwrap_or(0) as usize;
        let per = self.env.program.workgroup_words as usize;
        let Some(word) = self.space.workgroup.get_mut(slot * per + index) else {
            return 0;
        };
        let old = *word;
        if let Some(new) = combine(op, old, value, comparator) {
            *word = new;
        }
        old
    }
}

/// The word a held root's value word lies at, or `None` past the root.
fn held_index(root: Root, at: u64, word: u32, words: u32) -> Option<usize> {
    let (first, size) = match root {
        Root::Private { first, words }
        | Root::Shared { first, words }
        | Root::Workgroup { first, words } => (first, words),
        Root::Binding(_) | Root::PushConstants => return None,
    };
    if at == u64::from(INVALID) || !at.is_multiple_of(4) {
        return None;
    }
    let start = at / 4;
    let end = start.checked_add(u64::from(words))?;
    if end > u64::from(size) {
        return None;
    }
    usize::try_from(u64::from(first) + start + u64::from(word)).ok()
}

/// `offset` moved by `by` bytes, past its object for good once negative or
/// past what a pointer holds.
fn advance(offset: u64, by: i128) -> u64 {
    if offset == u64::from(INVALID) {
        return offset;
    }
    let moved = i128::from(offset) + by;
    if moved < 0 || moved >= i128::from(INVALID) {
        u64::from(INVALID)
    } else {
        u64::try_from(moved).unwrap_or(u64::from(INVALID))
    }
}

fn push_read(words: &[u32], byte: u64, bits: u8) -> u64 {
    let word = |at: u64| {
        usize::try_from(at / 4)
            .ok()
            .and_then(|i| words.get(i))
            .copied()
            .unwrap_or(0)
    };
    match bits {
        16 => u64::from(word(byte) >> ((byte % 4) * 8) & 0xffff),
        64 => u64::from(word(byte)) | u64::from(word(byte + 4)) << 32,
        _ => u64::from(word(byte)),
    }
}

/// What `op` leaves in a word holding `old`, or `None` for a load.
fn combine(op: Atomic, old: u32, value: u32, comparator: u32) -> Option<u32> {
    Some(match op {
        Atomic::Load => return None,
        Atomic::Store | Atomic::Exchange => value,
        Atomic::CompareExchange => {
            if old == comparator {
                value
            } else {
                old
            }
        }
        Atomic::Increment => old.wrapping_add(1),
        Atomic::Decrement => old.wrapping_sub(1),
        Atomic::Add => old.wrapping_add(value),
        Atomic::Sub => old.wrapping_sub(value),
        Atomic::SMin => old.cast_signed().min(value.cast_signed()).cast_unsigned(),
        Atomic::UMin => old.min(value),
        Atomic::SMax => old.cast_signed().max(value.cast_signed()).cast_unsigned(),
        Atomic::UMax => old.max(value),
        Atomic::And => old & value,
        Atomic::Or => old | value,
        Atomic::Xor => old ^ value,
    })
}

/// `op` on a word of device memory, answering what it held.
fn device_atomic(
    word: &AtomicU32,
    op: Atomic,
    order: Order,
    (value, comparator): (u32, u32),
) -> u32 {
    let rmw = rmw_order(order);
    match op {
        Atomic::Load => word.load(load_order(order)),
        Atomic::Store => {
            word.store(value, store_order(order));
            0
        }
        Atomic::Exchange => word.swap(value, rmw),
        Atomic::CompareExchange => {
            let failure = load_order(order);
            word.compare_exchange(comparator, value, rmw, failure)
                .unwrap_or_else(|held| held)
        }
        Atomic::Increment => word.fetch_add(1, rmw),
        Atomic::Decrement => word.fetch_sub(1, rmw),
        Atomic::Add => word.fetch_add(value, rmw),
        Atomic::Sub => word.fetch_sub(value, rmw),
        Atomic::UMin => word.fetch_min(value, rmw),
        Atomic::UMax => word.fetch_max(value, rmw),
        Atomic::And => word.fetch_and(value, rmw),
        Atomic::Or => word.fetch_or(value, rmw),
        Atomic::Xor => word.fetch_xor(value, rmw),
        // A signed minimum or maximum has no instruction of its own: the word
        // is compared and swapped until no other writer came between.
        Atomic::SMin | Atomic::SMax => word
            .fetch_update(rmw, load_order(order), |old| {
                combine(op, old, value, comparator)
            })
            .unwrap_or_else(|held| held),
    }
}

/// Write word `word` of a built-in into `root` for every lane of `lanes`:
/// a lane's own word, or the one word a shared root holds for all.
pub(super) fn give(machine: &mut Machine<'_, '_, '_>, root: Root, word: usize, lanes: &Lanes) {
    let live = machine.place.live;
    match root {
        Root::Private { first, .. } => {
            let index = first as usize + word;
            for lane in each_lane(live) {
                if let (Some(slot), Some(&value)) = (
                    machine.space.private.get_mut(index * LANES + lane),
                    lanes.get(lane),
                ) {
                    *slot = super::low32(value);
                }
            }
        }
        Root::Shared { first, .. } => {
            let lane = live.trailing_zeros() as usize;
            if let (Some(slot), Some(&value)) = (
                machine.space.shared.get_mut(first as usize + word),
                lanes.get(lane),
            ) {
                *slot = super::low32(value);
            }
        }
        Root::Workgroup { .. } | Root::Binding(_) | Root::PushConstants => {}
    }
}
