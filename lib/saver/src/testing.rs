//! A host for a scene's own tests: what `serve` would hand a scene, with no
//! pipe, region or host process behind it.

use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageRect, RefreshRate};
use tairix_abi::saver::{ChoiceValue, OptionKey, SaverFeeds, SaverValue};
use tairix_abi::saver_ipc::{OptionValue, SaverMessage, SaverRegion, Start, WallReading};
use tairix_display::winframe::{decode, encode, Alpha};
use tairix_geometry::{Region, Scale};
use tairix_raster::Surface;
use tairix_theme::motion::SceneClock;

use crate::serve::DAMAGE_BUDGET;
use crate::{FeedArea, Frame, Host, NextFrame, Scene, Setup, GROUND};

/// The refresh rate a test's display runs at.
pub const TEST_REFRESH: RefreshRate = RefreshRate::ASSUMED;

/// One scene frame on [`TEST_REFRESH`].
pub const FRAME_NS: u64 = SceneClock::frame_on(TEST_REFRESH);

/// A start for a `width` × `height` screen at 100% with no feed, one thread,
/// no option values and 256 MiB.
///
/// # Panics
///
/// For an empty screen: a test asks for a real one.
#[must_use]
pub fn start(width: u32, height: u32) -> Start {
    start_fed(width, height, SaverFeeds::NONE)
}

/// [`start`] for a saver given `feeds`, its region laid out for them.
///
/// # Panics
///
/// For an empty screen: a test asks for a real one.
#[must_use]
pub fn start_fed(width: u32, height: u32, feeds: SaverFeeds) -> Start {
    let region = SaverRegion::new(width, height, feeds).expect("a screen");
    let mut start = Start::new(region, 100, TEST_REFRESH);
    start.feeds = feeds;
    start.memory = 256 << 20;
    start
}

/// Give option `key` the value `value` in `start`, as the host would.
///
/// # Panics
///
/// For a key or value no manifest could declare, or a key given twice.
pub fn give(start: &mut Start, key: &str, value: SaverValue) {
    let key = OptionKey::new(key).expect("a key");
    start.push_option(OptionValue { key, value }).expect("room");
}

/// The choice `word`.
///
/// # Panics
///
/// For a word no choice could carry.
#[must_use]
pub fn choice(word: &str) -> SaverValue {
    SaverValue::Choice(ChoiceValue::new(word).expect("a word"))
}

/// The setup `serve` makes of `start`, with `seed` for the scene's generator.
///
/// # Panics
///
/// For a start whose scale no desktop could have.
#[must_use]
pub fn setup(start: &Start, seed: u64) -> Setup<'_> {
    Setup {
        start,
        scale: Scale::from_percent(start.scale_percent).expect("a scale"),
        seed,
    }
}

/// What `serve` hands a scene, held across frames: its own picture, the
/// damage region a frame reports into, and the feed area with whoever holds
/// it.
pub struct Screen {
    /// The scene's picture, opaque black to begin with.
    pub surface: Surface,
    /// What the last frame reported changed.
    pub damage: Region,
    /// The wall reading frames are drawn with.
    pub wall: WallReading,
    start: Start,
    feed: FeedArea,
    feed_bytes: Vec<u8>,
}

impl Screen {
    /// The screen `start` describes.
    ///
    /// # Panics
    ///
    /// For a screen no surface can be.
    #[must_use]
    pub fn new(start: Start) -> Self {
        let mut surface =
            Surface::new(start.region.width(), start.region.height()).expect("a surface");
        surface.fill(GROUND);
        let feed = start.region.feed_range().map_or(0, |range| range.len());
        Self {
            surface,
            damage: Region::with_budget(DAMAGE_BUDGET),
            wall: WallReading::unset(0),
            start,
            feed: FeedArea::Saver,
            feed_bytes: alloc::vec![0; feed],
        }
    }

    /// The start the screen was made from.
    #[must_use]
    pub const fn start(&self) -> &Start {
        &self.start
    }

    /// Draw `scene`'s frame `serial` at `now_ns`, answering when it wants the
    /// next and what it asked of its host.
    pub fn draw(
        &mut self,
        scene: &mut impl Scene,
        serial: u32,
        now_ns: u64,
    ) -> (NextFrame, Vec<SaverMessage>) {
        self.damage.clear();
        let mut asked = Vec::new();
        let frame = Frame {
            serial,
            now_ns,
            wall: &self.wall,
        };
        let mut send = |message: &SaverMessage| {
            asked.push(*message);
            Ok(())
        };
        let mut host = Host {
            send: &mut send,
            start: &self.start,
            feed: &mut self.feed,
            feed_bytes: (!self.feed_bytes.is_empty()).then_some(self.feed_bytes.as_mut_slice()),
        };
        let next = scene.frame(&mut self.surface, &mut self.damage, &frame, &mut host);
        (next, asked)
    }

    /// Answer the slide `scene` wants with `picture` written into the feed
    /// area, as the host does, or with none where the host could not read it.
    ///
    /// # Panics
    ///
    /// When the scene wants no slide, or `picture` is not the screen's size.
    pub fn slide(&mut self, scene: &mut impl Scene, picture: Option<&Surface>) {
        let FeedArea::Slide(index) = self.feed else {
            panic!("no slide is wanted");
        };
        self.feed = FeedArea::Saver;
        let mode = self.start.region.display_mode();
        if let Some(picture) = picture {
            encode(
                picture,
                &mut self.feed_bytes,
                &mode,
                DamageRect::full(&mode),
                &tairix_parallel::Serial,
            )
            .expect("a screen-sized slide");
        }
        scene.slide(index, picture.is_some());
    }

    /// The picture `scene` handed over to keep, which the host answers with
    /// `kept`.
    ///
    /// # Panics
    ///
    /// When the scene handed over nothing to keep.
    pub fn keep(&mut self, scene: &mut impl Scene, width: u32, height: u32, kept: bool) -> Surface {
        assert_eq!(self.feed, FeedArea::Keep, "nothing to keep");
        self.feed = FeedArea::Saver;
        let mode = self.start.region.display_mode();
        let mut picture = Surface::new(width, height).expect("a picture");
        let area = DamageRect {
            x: 0,
            y: 0,
            width_px: width,
            height_px: height,
        };
        decode(
            &self.feed_bytes,
            &mut picture,
            &mode,
            area,
            Alpha::AsSent,
            &tairix_parallel::Serial,
        )
        .expect("the kept picture");
        scene.kept(kept);
        picture
    }
}
