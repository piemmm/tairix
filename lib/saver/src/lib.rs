//! The saver side of a TAIRiX screensaver (`plans/SCREENSAVERS.md`).
//!
//! A screensaver is a bundle its desktop spawns into the drawing sandbox: no
//! file, no IPC endpoint, no wall clock, no randomness of its own — two pipes
//! and the one region it presents through, and the monotonic clock to pace its
//! own work by. This crate is everything between that and a [`Scene`]:
//! it answers the host's `Start`, maps the region, draws each frame the host
//! asks for into the scene's own surface, writes what changed into the region
//! and says so, relays the font service so text draws as anywhere else, and
//! hands the scene the feeds it was given.
//!
//! A saver's `Run` is its [`Scene`] and one call to `run`, which builds the
//! scene from the [`Setup`] the host's `Start` makes.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

mod link;
#[cfg(all(freestanding, feature = "program"))]
mod program;
mod serve;
#[cfg(feature = "test-support")]
pub mod testing;

#[cfg(test)]
mod tests;

#[cfg(all(freestanding, feature = "program"))]
pub use program::run;
pub use serve::{serve, Ended, RegionBytes};
pub use tairix_abi::saver::{ChoiceValue, SaverValue};
pub use tairix_abi::saver_ipc::{NextFrame, Start, WallReading};
pub use tairix_abi::switchboard_ipc::MachineReport;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::desktop::{Appearance, Motion};
use tairix_abi::driver::display::{DamageRect, DisplayMode};
use tairix_abi::saver_ipc::{
    SaverMessage, SaverRegion, SAVER_DATE_TEXT_MAX, SAVER_KEEP_TITLE_MAX, SAVER_TIME_TEXT_MAX,
};
use tairix_abi::Errno;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_parallel::Pool;
use tairix_raster::{Color, Surface};
use tairix_theme::{Accessibility, Theme};

/// What a scene is made from: the host's `Start`, and what this crate read
/// out of it.
pub struct Setup<'a> {
    /// Everything the host started the saver with.
    pub start: &'a Start,
    /// The desktop's UI scale.
    pub scale: Scale,
    /// A seed for the scene's own generator, carved from the host's.
    pub seed: u64,
}

impl Setup<'_> {
    /// The desktop's built-in theme for the start's appearance, drawn with the
    /// user's text and on their accessibility axes: the faces, metrics and
    /// timings a scene that draws text or eases shares with the desktop.
    #[must_use]
    pub fn theme(&self) -> Theme {
        let start = self.start;
        let theme = match start.appearance {
            Appearance::Dark => Theme::dark(),
            Appearance::Light => Theme::light(),
        };
        let motion = if start.reduced_motion {
            Motion::Reduced
        } else {
            Motion::Full
        };
        theme.with_text(start.text).with_axes(Accessibility {
            contrast: start.contrast,
            density: start.density,
            motion,
        })
    }

    /// A worker pool for a scene's own work: one participant per thread the
    /// host granted, up to one per online core, the saver's own thread among
    /// them. A thread the kernel refuses is not made, so the pool may hold
    /// fewer.
    #[must_use]
    pub fn pool(&self) -> Pool {
        let granted = self.start.threads.min(u32::from(self.start.cores));
        Pool::for_cpus(usize::try_from(granted).unwrap_or(1))
    }

    /// The word chosen for choice option `key`.
    ///
    /// The host sends every option the manifest declares, so a scene holds no
    /// default of its own.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when the host sent no choice for `key`.
    pub fn choice(&self, key: &str) -> Result<ChoiceValue, Errno> {
        match self.start.option(key) {
            Some(SaverValue::Choice(word)) => Ok(word),
            _ => Err(Errno::NotFound),
        }
    }

    /// The pace choice option `key` names, in percent of normal speed: the
    /// `slow`, `normal` or `fast` every scene with a speed offers.
    ///
    /// # Errors
    ///
    /// As [`Self::choice`], and [`Errno::OutOfRange`] for any other word.
    pub fn pace(&self, key: &str) -> Result<u32, Errno> {
        match self.choice(key)?.as_str() {
            "slow" => Ok(50),
            "normal" => Ok(100),
            "fast" => Ok(150),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Whether switch option `key` is on.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when the host sent no switch for `key`.
    pub fn switch(&self, key: &str) -> Result<bool, Errno> {
        match self.start.option(key) {
            Some(SaverValue::Switch(on)) => Ok(on),
            _ => Err(Errno::NotFound),
        }
    }

    /// The value of range option `key`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when the host sent no range value for `key`.
    pub fn range(&self, key: &str) -> Result<i32, Errno> {
        match self.start.option(key) {
            Some(SaverValue::Range(value)) => Ok(value),
            _ => Err(Errno::NotFound),
        }
    }
}

/// One frame the host asks for.
pub struct Frame<'a> {
    /// Its serial, which the presented answer names.
    pub serial: u32,
    /// The monotonic time it is drawn for.
    pub now_ns: u64,
    /// The wall clock as the desktop's bar reads it.
    pub wall: &'a WallReading,
}

impl Frame<'_> {
    /// When the wall reading next changes, and a scene that tells the time is
    /// next owed a frame: never sooner than `frame_ns` after this one, so a
    /// change already due is drawn again then rather than in a tight loop.
    #[must_use]
    pub fn reading_changes_ns(&self, frame_ns: u64) -> u64 {
        self.wall
            .next_change_ns
            .max(self.now_ns.saturating_add(frame_ns))
    }
}

/// Which date a face shows beside the time.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DateShown {
    /// None.
    Hidden,
    /// The date as the bar spells it.
    Numeric,
    /// The date written out.
    Long,
}

/// What a face shows of the wall reading. Held by value, so a face that asks
/// every frame whether its reading changed allocates nothing to find out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Told {
    time: BoundedText<0, SAVER_TIME_TEXT_MAX>,
    date: BoundedText<0, SAVER_DATE_TEXT_MAX>,
}

impl Told {
    /// What `wall` tells with `date` beside the time.
    #[must_use]
    pub const fn of(wall: &WallReading, date: DateShown) -> Self {
        Self {
            time: wall.time,
            date: match date {
                DateShown::Hidden => BoundedText::EMPTY,
                DateShown::Numeric => wall.numeric_date,
                DateShown::Long => wall.long_date,
            },
        }
    }

    /// The time; empty while the clock is not set.
    #[must_use]
    pub fn time(&self) -> &str {
        self.time.as_str()
    }

    /// The date; empty when the face shows none or the clock is not set.
    #[must_use]
    pub fn date(&self) -> &str {
        self.date.as_str()
    }
}

/// A screensaver's picture.
///
/// Every method runs on the saver's one main thread, and so does all text
/// drawing: the font service is relayed over the host's pipe, which that
/// thread alone reads. A scene may run threads of its own within what
/// [`Start::threads`] grants.
pub trait Scene {
    /// Draw `frame` into `surface`, the screen-sized picture the host shows,
    /// adding what changed to `damage`, which arrives empty; answers when the
    /// next frame is wanted.
    ///
    /// `surface` is the scene's own and keeps every pixel between frames. It
    /// starts opaque black, as the host's screen does.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        host: &mut Host<'_>,
    ) -> NextFrame;

    /// A machine reading arrived, for a scene given the machine feed. The
    /// host asks for a frame next, which draws what it changed.
    fn machine(&mut self, report: &MachineReport) {
        let _ = report;
    }

    /// The machine feed's source stopped, for a scene given the feed: its last
    /// reading is no longer live.
    fn machine_lost(&mut self) {}

    /// The host answered the last [`Host::want_slide`]: slide `index` is in
    /// the feed area when `shown`, for the next frame to read through
    /// [`Host::feed_picture`], and could not be read otherwise. The host asks
    /// for that frame next.
    fn slide(&mut self, index: u16, shown: bool) {
        let _ = (index, shown);
    }

    /// The host answered the last [`Host::keep`].
    fn kept(&mut self, kept: bool) {
        let _ = kept;
    }
}

/// Who holds the region's feed area: the saver, or the host until it answers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FeedArea {
    Saver,
    Slide(u16),
    Keep,
}

/// What a scene may ask of its host while it draws.
pub struct Host<'a> {
    pub(crate) send: &'a mut dyn FnMut(&SaverMessage) -> Result<(), Errno>,
    pub(crate) start: &'a Start,
    pub(crate) feed: &'a mut FeedArea,
    pub(crate) feed_bytes: Option<&'a mut [u8]>,
}

impl Host<'_> {
    /// Whether the feed area is the saver's: no slide or keep is outstanding.
    #[must_use]
    pub fn feed_free(&self) -> bool {
        *self.feed == FeedArea::Saver
    }

    /// The feed area read as one screen-sized picture: the slide the host last
    /// showed there. `None` while the host holds the area, or for a saver
    /// given none.
    #[must_use]
    pub fn feed_picture(&self) -> Option<FeedPicture<'_>> {
        if !self.feed_free() {
            return None;
        }
        Some(FeedPicture {
            bytes: self.feed_bytes.as_deref()?,
            mode: self.start.region.display_mode(),
        })
    }

    /// Ask for slide `index` of the [`Start::slides`] the slides feed offers.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] without the slides feed,
    /// [`Errno::OutOfRange`] past the last slide, [`Errno::Busy`] while the
    /// feed area is the host's, or the channel's own failure.
    pub fn want_slide(&mut self, index: u16) -> Result<(), Errno> {
        if !self.start.feeds.slides() {
            return Err(Errno::PermissionDenied);
        }
        if index >= self.start.slides {
            return Err(Errno::OutOfRange);
        }
        if !self.feed_free() {
            return Err(Errno::Busy);
        }
        (self.send)(&SaverMessage::WantSlide(index))?;
        *self.feed = FeedArea::Slide(index);
        Ok(())
    }

    /// Hand the host `picture`, a finished picture no larger than the screen,
    /// to keep under `title`.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] without the keeping feed, [`Errno::Busy`]
    /// while the feed area is the host's, [`Errno::LengthOutOfRange`] for a
    /// title or picture out of bounds, or the channel's own failure.
    pub fn keep(&mut self, title: &str, picture: &Surface) -> Result<(), Errno> {
        if !self.start.feeds.keep() {
            return Err(Errno::PermissionDenied);
        }
        if !self.feed_free() {
            return Err(Errno::Busy);
        }
        let title: BoundedText<1, SAVER_KEEP_TITLE_MAX> = BoundedText::new(title)?;
        let region = &self.start.region;
        let (width, height) = (picture.width(), picture.height());
        if width == 0 || height == 0 || width > region.width() || height > region.height() {
            return Err(Errno::LengthOutOfRange);
        }
        let feed = self
            .feed_bytes
            .as_deref_mut()
            .ok_or(Errno::PermissionDenied)?;
        tairix_display::winframe::encode(
            picture,
            feed,
            &region.display_mode(),
            DamageRect {
                x: 0,
                y: 0,
                width_px: width,
                height_px: height,
            },
            &tairix_parallel::Serial,
        )?;
        (self.send)(&SaverMessage::Keep {
            title,
            width,
            height,
        })?;
        *self.feed = FeedArea::Keep;
        Ok(())
    }
}

/// The feed area read as a picture: one screen-sized slide.
pub struct FeedPicture<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) mode: DisplayMode,
}

impl FeedPicture<'_> {
    /// Copy the slide into `surface`, which must be the screen's size.
    ///
    /// # Errors
    ///
    /// The frame format's refusal of a surface of another size.
    pub fn read_into(&self, surface: &mut Surface) -> Result<(), Errno> {
        tairix_display::winframe::decode(
            self.bytes,
            surface,
            &self.mode,
            DamageRect::full(&self.mode),
            tairix_display::winframe::Alpha::AsSent,
            &tairix_parallel::Serial,
        )
        .map(|_| ())
    }
}

/// The ground every scene draws on: the black a saver's picture starts as and
/// the host's screen shows until the first frame.
pub const GROUND: Color = Color::rgb(0, 0, 0);

/// Lay the ground over `rect` of `surface`, for a scene to draw on afresh.
pub fn erase(surface: &mut Surface, rect: Rect) {
    if let Some((x, y)) = rect.surface_origin() {
        surface.fill_rect(x, y, rect.width, rect.height, GROUND);
    }
}

/// What `picture` covers set at `at`; nothing when there is no picture.
#[must_use]
pub fn footprint(picture: Option<&Surface>, at: Point) -> Rect {
    picture.map_or(Rect::EMPTY, |picture| {
        Rect::new(at.x, at.y, picture.width(), picture.height())
    })
}

/// The frame area and feed area of `bytes`, laid out as `region` says.
pub(crate) fn areas<'a>(
    bytes: &'a mut [u8],
    region: &SaverRegion,
) -> (&'a mut [u8], Option<&'a mut [u8]>) {
    let (frame, rest) = bytes.split_at_mut(region.frame_len().min(bytes.len()));
    let feed = region
        .feed_range()
        .and_then(|range| rest.get_mut(..range.len()));
    (frame, feed)
}
