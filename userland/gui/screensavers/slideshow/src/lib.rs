//! Slideshow, a TAIRiX screensaver: the shipped pictures, one after another.
//!
//! The pictures are the host's. It offers the slides feed's pictures — the
//! catalogue narrowed as the desktop's settings say — reads each one the
//! saver asks for in its own sandbox, fits it to the screen and lays it in the
//! feed area. The saver keeps only the running order and the pace: in order,
//! or shuffled, every picture shown once before any repeats and never the same
//! one twice running from one pass into the next.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_geometry::{Rect, Region};
use tairix_raster::Surface;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_saver::{Frame, Host, NextFrame, Scene, Setup};

/// The slideshow screensaver.
pub struct Slideshow {
    /// The slides shown, in this pass's order.
    order: Vec<u16>,
    /// Where in `order` the next slide is.
    next: usize,
    shuffled: bool,
    rng: NonCryptoRng,
    interval_ns: u64,
    /// When the next slide is due.
    due_ns: u64,
    /// The slide asked for and not yet answered.
    wanted: Option<u16>,
    /// Whether the host showed a slide in the feed area the screen does not
    /// show yet.
    arrived: bool,
    screen: Rect,
}

impl Slideshow {
    /// The slideshow `setup` describes, its first slide due at once.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] or [`Errno::OutOfRange`] for an option the host
    /// did not send as the manifest declares it, [`Errno::OutOfMemory`] when
    /// the heap will not hold the running order.
    pub fn new(setup: &Setup<'_>) -> Result<Self, Errno> {
        let interval_s: u64 = match setup.choice("interval")?.as_str() {
            "s5" => 5,
            "s10" => 10,
            "s15" => 15,
            "s30" => 30,
            "m1" => 60,
            "m2" => 120,
            "m5" => 300,
            "m10" => 600,
            "m15" => 900,
            "m30" => 1_800,
            "h1" => 3_600,
            _ => return Err(Errno::OutOfRange),
        };
        let shuffled = match setup.choice("order")?.as_str() {
            "sequential" => false,
            "shuffled" => true,
            _ => return Err(Errno::OutOfRange),
        };
        let start = setup.start;
        let mut order = Vec::new();
        order
            .try_reserve_exact(usize::from(start.slides))
            .map_err(|_| Errno::OutOfMemory)?;
        order.extend(0..start.slides);
        let mut show = Self {
            order,
            next: 0,
            shuffled,
            rng: NonCryptoRng::seed_from_u64(setup.seed),
            interval_ns: interval_s * 1_000_000_000,
            due_ns: start.now_ns,
            wanted: None,
            arrived: false,
            screen: Rect::new(0, 0, start.region.width(), start.region.height()),
        };
        if show.shuffled {
            show.shuffle(None);
        }
        Ok(show)
    }

    /// The slide to show next, the one after it falling due an interval from
    /// `now_ns`; `None` with no slide to show.
    fn take_due(&mut self, now_ns: u64) -> Option<u16> {
        let shown = *self.order.get(self.next)?;
        self.next += 1;
        if self.next == self.order.len() {
            self.next = 0;
            if self.shuffled {
                self.shuffle(Some(shown));
            }
        }
        self.due_ns = now_ns.saturating_add(self.interval_ns);
        Some(shown)
    }

    /// Put the pass in a fresh random order that does not open on `after`, the
    /// slide the last pass ended on.
    fn shuffle(&mut self, after: Option<u16>) {
        let len = self.order.len();
        for at in (1..len).rev() {
            let with = self.below(at + 1);
            self.order.swap(at, with);
        }
        if len > 1 && after.is_some() && self.order.first().copied() == after {
            let with = 1 + self.below(len - 1);
            self.order.swap(0, with);
        }
    }

    /// A uniform draw in `0..bound`, which is not zero.
    fn below(&mut self, bound: usize) -> usize {
        let drawn = self
            .rng
            .next_below(u64::try_from(bound).unwrap_or(u64::MAX));
        usize::try_from(drawn).unwrap_or(0)
    }
}

impl Scene for Slideshow {
    /// Show a slide the host has laid in the feed area, and ask for the next
    /// once it falls due.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        host: &mut Host<'_>,
    ) -> NextFrame {
        if core::mem::take(&mut self.arrived) {
            let read = host
                .feed_picture()
                .is_some_and(|picture| picture.read_into(surface).is_ok());
            if read {
                damage.add(self.screen);
            }
        }
        if self.wanted.is_none() && frame.now_ns >= self.due_ns {
            if let Some(index) = self.take_due(frame.now_ns) {
                self.wanted = host.want_slide(index).ok().map(|()| index);
            }
        }
        if self.wanted.is_some() || self.order.is_empty() {
            NextFrame::Idle
        } else {
            NextFrame::At(self.due_ns)
        }
    }

    fn slide(&mut self, index: u16, shown: bool) {
        if self.wanted == Some(index) {
            self.wanted = None;
            self.arrived = shown;
        }
    }
}

#[cfg(test)]
mod tests;
