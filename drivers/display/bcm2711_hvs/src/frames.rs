//! The frames the engine scans out of, and what each lacks of the latest
//! one presented.
//!
//! A present copies into a frame no channel composites or is about to, so the
//! HVS never reads a frame being written and no present waits. Each frame
//! keeps the damage presented since it was last written, so bringing it up to
//! date copies only that.

use tairix_abi::driver::display::{DamageRect, DisplayMode, MAX_DAMAGE_RECTS};
use tairix_abi::DriverError;
use tairix_inline::ArrayVec;

use crate::hvs::FRAMES_MAX;

/// Memory a frame lives in, which the HVS reads by its bus address.
pub trait FrameMemory {
    /// The address the HVS reads the frame's first byte at.
    fn bus(&self) -> u32;

    /// The frame's bytes, for the CPU to write.
    fn bytes_mut(&mut self) -> &mut [u8];

    /// Make the CPU's writes to `len` bytes at `offset` visible to the HVS.
    fn flush(&self, offset: usize, len: usize);
}

/// Where frames come from.
pub trait FrameAllocator {
    /// The memory a frame lives in.
    type Memory: FrameMemory;

    /// A zeroed frame of `len` bytes the HVS can read.
    ///
    /// # Errors
    ///
    /// The allocator's refusal: no memory, or none the HVS reaches.
    fn alloc(&self, len: usize) -> Result<Self::Memory, DriverError>;
}

/// The damage presented since a frame was last written, bounded: past the
/// bound the whole frame is out of date.
const STALE_MAX: usize = 2 * MAX_DAMAGE_RECTS;

/// What a frame lacks of the latest frame presented: the rectangles named,
/// or, past the bound, all of it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Stale {
    rects: ArrayVec<DamageRect, STALE_MAX>,
    whole: bool,
}

impl Stale {
    const WHOLE: Self = Self {
        rects: ArrayVec::new(),
        whole: true,
    };

    /// `damage` added: `whole` when it covers the frame.
    fn note(&mut self, damage: &[DamageRect], whole: bool) {
        if self.whole {
            return;
        }
        if whole {
            *self = Self::WHOLE;
            return;
        }
        for &rect in damage {
            if self.rects.try_push(rect).is_err() {
                *self = Self::WHOLE;
                return;
            }
        }
    }
}

struct Frame<M> {
    memory: M,
    stale: Stale,
}

/// The frames of one surface mode.
pub(crate) struct Frames<M> {
    mode: DisplayMode,
    held: ArrayVec<Frame<M>, FRAMES_MAX>,
    latest: Option<usize>,
}

impl<M: FrameMemory> Frames<M> {
    /// No frames yet for `mode`.
    pub fn new(mode: DisplayMode) -> Self {
        Self {
            mode,
            held: ArrayVec::new(),
            latest: None,
        }
    }

    pub const fn mode(&self) -> &DisplayMode {
        &self.mode
    }

    /// Bytes one frame holds.
    pub fn frame_len(&self) -> Result<usize, DriverError> {
        let len = u64::from(self.mode.stride_bytes) * u64::from(self.mode.height_px);
        usize::try_from(len).map_err(|_| DriverError::LengthOutOfRange)
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// The bus address of frame `index`.
    pub fn bus(&self, index: usize) -> Option<u32> {
        self.held.get(index).map(|frame| frame.memory.bus())
    }

    /// The frame last presented.
    pub const fn latest(&self) -> Option<usize> {
        self.latest
    }

    /// Hold at least `count` frames, the new ones wholly out of date,
    /// answering how many there were before.
    pub fn grow<A: FrameAllocator<Memory = M>>(
        &mut self,
        alloc: &A,
        count: usize,
    ) -> Result<usize, DriverError> {
        let before = self.held.len();
        let len = self.frame_len()?;
        while self.held.len() < count.min(FRAMES_MAX) {
            let memory = alloc.alloc(len)?;
            self.held
                .try_push(Frame {
                    memory,
                    stale: Stale::WHOLE,
                })
                .map_err(|_| DriverError::OutOfRange)?;
        }
        Ok(before)
    }

    /// Every frame out of date and none presented: what a departed
    /// presenter left must never be shown to the next.
    pub fn forget(&mut self) {
        for frame in &mut self.held {
            frame.stale = Stale::WHOLE;
        }
        self.latest = None;
    }

    /// Copy what frame `index` lacks of `source` — its own stale damage and
    /// `damage` — making it the latest, and note `damage` against the rest.
    ///
    /// `source` is a whole frame of the mode, and every rectangle lies
    /// inside the mode: the caller has validated both.
    pub fn present(
        &mut self,
        index: usize,
        source: &[u8],
        damage: &[DamageRect],
    ) -> Result<(), DriverError> {
        let len = self.frame_len()?;
        let source = source.get(..len).ok_or(DriverError::BufferTooSmall)?;
        let mode = self.mode;
        let whole = damage.iter().any(|rect| rect.covers(&mode));
        let frame = self.held.get_mut(index).ok_or(DriverError::OutOfRange)?;
        if frame.stale.whole || whole {
            frame
                .memory
                .bytes_mut()
                .get_mut(..len)
                .ok_or(DriverError::LengthOutOfRange)?
                .copy_from_slice(source);
            frame.memory.flush(0, len);
        } else {
            for rect in frame.stale.rects.iter().chain(damage) {
                copy_rect(&mut frame.memory, source, &mode, rect)?;
            }
        }
        frame.stale = Stale::default();
        for (at, other) in self.held.iter_mut().enumerate() {
            if at != index {
                other.stale.note(damage, whole);
            }
        }
        self.latest = Some(index);
        Ok(())
    }
}

/// Copy `rect` of `source` into `memory`, both laid out as `mode`.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a rectangle past either buffer, which the
/// caller's validation leaves none of.
fn copy_rect<M: FrameMemory>(
    memory: &mut M,
    source: &[u8],
    mode: &DisplayMode,
    rect: &DamageRect,
) -> Result<(), DriverError> {
    let stride = mode.stride_bytes as usize;
    let start = rect.x as usize * 4;
    let span = rect.width_px as usize * 4;
    let first = rect.y as usize * stride;
    let rows = rect.height_px as usize;
    let bytes = memory.bytes_mut();
    for row in 0..rows {
        let at = first + row * stride + start;
        let (Some(to), Some(from)) = (bytes.get_mut(at..at + span), source.get(at..at + span))
        else {
            return Err(DriverError::OutOfRange);
        };
        to.copy_from_slice(from);
    }
    if span == stride {
        memory.flush(first, rows * stride);
    } else {
        for row in 0..rows {
            memory.flush(first + row * stride + start, span);
        }
    }
    Ok(())
}
