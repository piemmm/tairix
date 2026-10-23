//! A host for a scene's own tests: what `serve` would hand a scene, with no
//! pipe, region or host process behind it; and the scripted host and heap
//! region `serve`'s own tests run it over.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageRect, RefreshRate};
use tairix_abi::saver::{ChoiceValue, OptionKey, SaverFeeds, SaverValue};
use tairix_abi::saver_ipc::{
    HostMessage, OptionValue, SaverMessage, SaverRegion, Start, WallReading, SAVER_DAMAGE_RECTS,
    SAVER_HOST_MESSAGE_MAX,
};
use tairix_abi::Errno;
use tairix_display::winframe::{decode, encode};
use tairix_geometry::{Region, Scale};
use tairix_raster::{PixelMemory, Surface};
use tairix_rt::sync::Mutex;
use tairix_sandbox::proto::Channel;
use tairix_theme::motion::SceneClock;

use crate::{FeedArea, Frame, Host, NextFrame, RegionBytes, Scene, Setup, GROUND};

/// A host recorded in advance: what it says, then the end of its pipe; and
/// everything the saver says back.
pub struct Scripted {
    input: VecDeque<u8>,
    said: Arc<Mutex<Vec<u8>>>,
}

impl Channel for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let n = buf.len().min(self.input.len());
        for (slot, byte) in buf.iter_mut().zip(self.input.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.said.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
}

/// A host that says `messages` and closes its pipe, and where what the saver
/// says back is kept.
///
/// # Panics
///
/// For a message that does not encode.
#[must_use]
pub fn script(messages: &[HostMessage<'_>]) -> (Scripted, Arc<Mutex<Vec<u8>>>) {
    let mut input = VecDeque::new();
    let mut out = alloc::vec![0u8; SAVER_HOST_MESSAGE_MAX];
    for message in messages {
        let len = message.encode(&mut out).expect("encodes");
        input.extend(u32::try_from(len).expect("fits").to_le_bytes());
        input.extend(&out[..len]);
    }
    let said = Arc::new(Mutex::new(Vec::new()));
    (
        Scripted {
            input,
            said: Arc::clone(&said),
        },
        said,
    )
}

/// The messages `said`, as [`script`] kept them.
///
/// # Panics
///
/// For bytes that are not whole saver messages.
#[must_use]
pub fn replies(said: &[u8]) -> Vec<SaverMessage> {
    let mut at = 0;
    let mut out = Vec::new();
    while at + 4 <= said.len() {
        let len = u32::from_le_bytes(said[at..at + 4].try_into().expect("four")) as usize;
        out.push(SaverMessage::decode(&said[at + 4..at + 4 + len]).expect("a message"));
        at += 4 + len;
    }
    out
}

/// A region held in the test's own memory, its frame area handed to `out`, if
/// any, once the saver lets it go.
pub struct HeapRegion {
    bytes: Vec<u8>,
    out: Option<Arc<Mutex<Vec<u8>>>>,
}

impl HeapRegion {
    /// A region of `bytes`.
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self { bytes, out: None }
    }

    /// A region of `bytes` whose frame area is handed to `out` at the end.
    #[must_use]
    pub const fn recorded(bytes: Vec<u8>, out: Arc<Mutex<Vec<u8>>>) -> Self {
        Self {
            bytes,
            out: Some(out),
        }
    }
}

/// A [`HeapRegion`]'s frame area.
struct HeapFrame {
    bytes: Vec<u8>,
    out: Option<Arc<Mutex<Vec<u8>>>>,
}

impl PixelMemory for HeapFrame {
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl Drop for HeapFrame {
    fn drop(&mut self) {
        if let Some(out) = &self.out {
            *out.lock() = core::mem::take(&mut self.bytes);
        }
    }
}

impl RegionBytes for HeapRegion {
    type Rest = Vec<u8>;

    fn into_parts(mut self, frame_len: usize) -> Result<(Box<dyn PixelMemory>, Vec<u8>), Errno> {
        if self.bytes.len() < frame_len {
            return Err(Errno::LengthOutOfRange);
        }
        let rest = self.bytes.split_off(frame_len);
        let frame = HeapFrame {
            bytes: core::mem::take(&mut self.bytes),
            out: self.out.take(),
        };
        Ok((Box::new(frame), rest))
    }
}

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
            damage: Region::with_budget(SAVER_DAMAGE_RECTS),
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
            &tairix_parallel::Serial,
        )
        .expect("the kept picture");
        scene.kept(kept);
        picture
    }
}
