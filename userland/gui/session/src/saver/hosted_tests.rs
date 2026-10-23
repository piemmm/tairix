//! Host tests of a hosted saver over pipes held in memory: what the host
//! sends, what a saver's frames lay on the screen, the feeds and errands, and
//! the reasons a saver is ended.

use alloc::boxed::Box;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::driver::display::{
    DamageRect, DisplayFormat, DisplayMode, DisplayPower, RefreshRate,
};
use tairix_abi::font_ipc::FontRequest;
use tairix_abi::saver::SaverFeeds;
use tairix_abi::saver_ipc::{
    HostMessage, NextFrame, SaverDamage, SaverMessage, SaverRegion, SlideOutcome, Start,
    WallReading, SAVER_DAMAGE_RECTS, SAVER_START_DEADLINE_NS,
};
use tairix_abi::DriverError;
use tairix_abi::{APPINFO_FLAG_FEED_KEEP, APPINFO_FLAG_FEED_MACHINE, APPINFO_FLAG_FEED_SLIDES};
use tairix_display::winframe;
use tairix_display::{DrawInPlace, Presenter};
use tairix_geometry::{Rect, Region};
use tairix_raster::{Color, Pixel, Surface};
use tairix_wallpaper::TaskNames;
use tairix_wm::{Compositor, Point, WindowId};

use super::super::fixtures::{Errands, Heap, Pipes};
use super::{Ended, Fault, Hosted};
use crate::tests::compositor;

const SIZE: (u32, u32) = (16, 8);

fn start(feeds: u32) -> Start {
    start_of(feeds, SIZE)
}

fn start_of(feeds: u32, size: (u32, u32)) -> Start {
    let feeds = SaverFeeds::from_bits(feeds).expect("feeds");
    let region = SaverRegion::new(size.0, size.1, feeds).expect("a region");
    let mut start = Start::new(region, 100, RefreshRate::ASSUMED);
    start.feeds = feeds;
    if feeds.slides() {
        start.slides = 3;
    }
    start
}

struct Fixture {
    pipes: Pipes,
    hosted: Hosted<Pipes, tairix_log::DiscardSink>,
    compositor: Compositor,
    wm: WindowId,
    errands: Errands,
}

impl Fixture {
    fn new(feeds: u32) -> Self {
        Self::of(&start(feeds))
    }

    fn of(start: &Start) -> Self {
        let pipes = Pipes::default();
        let hosted = Hosted::begin(
            (
                pipes.clone(),
                Box::new(Heap(alloc::vec![0u8; start.region.len()])),
                tairix_log::DiscardSink,
            ),
            start,
            (TaskNames::Own, "Waves"),
            0,
        )
        .expect("hosted");
        let mut compositor = compositor();
        let size = (start.region.width(), start.region.height());
        let mut black = Surface::new(size.0, size.1).expect("a surface");
        black.fill(Color::rgb(0, 0, 0));
        let wm = compositor.add_window(Point::ORIGIN, black);
        let mut fixture = Self {
            pipes,
            hosted,
            compositor,
            wm,
            errands: Errands::default(),
        };
        fixture.flush();
        fixture
    }

    fn flush(&mut self) {
        self.hosted.on_writable(0).expect("written");
    }

    fn read(&mut self, now: u64) -> Result<(), Ended> {
        let result = self.hosted.on_readable(
            now,
            (&mut self.compositor, self.wm, None),
            &mut self.errands,
        );
        self.flush();
        result
    }

    /// [`read`](Self::read), with frames drawn straight into `display` where
    /// the saver covers it.
    fn read_onto(&mut self, now: u64, display: &mut Scanout) -> Result<(), Ended> {
        let result = self.hosted.on_readable(
            now,
            (&mut self.compositor, self.wm, Some(display)),
            &mut self.errands,
        );
        self.flush();
        result
    }

    fn advance(&mut self, now: u64) -> Result<(), Ended> {
        let result = self.hosted.advance(now, &WallReading::unset(0));
        self.flush();
        result
    }

    /// A frame went to the display.
    fn shown(&mut self) -> bool {
        self.hosted.shown((&mut self.compositor, self.wm))
    }

    /// The saver presents frame `serial`, changed over `QUARTER`.
    fn presents(&mut self, serial: u32, now: u64) {
        self.pipes.say(&SaverMessage::Presented {
            serial,
            damage: SaverDamage::new(&[QUARTER]).ok(),
            next: NextFrame::Soon,
        });
        self.read(now).expect("heard");
    }

    /// Draw `color` over `rect` of the frame area as a saver would.
    fn draw(&mut self, color: Color, rect: DamageRect) {
        let layout = self.hosted.layout;
        let mut picture = Surface::new(layout.width(), layout.height()).expect("a surface");
        picture.fill(color);
        let mode = layout.display_mode();
        winframe::encode(
            &picture,
            self.hosted.region_mut(),
            &mode,
            rect,
            &tairix_parallel::SERIAL,
        )
        .expect("encodes");
    }

    fn window_pixel(&self, x: u32, y: u32) -> Pixel {
        self.compositor
            .window(self.wm)
            .and_then(tairix_wm::Window::content)
            .and_then(|content| content.get(x, y))
            .expect("in bounds")
    }
}

/// A display a saver's frames are drawn straight into: one frame at a padded
/// stride in an output's byte order, which takes a present at once or,
/// flipping, holds it until its latch, and may refuse it.
struct Scanout {
    mode: DisplayMode,
    stride: u32,
    frame: alloc::vec::Vec<u8>,
    flips: bool,
    in_flight: bool,
    refuses: bool,
    drawn: alloc::vec::Vec<alloc::vec::Vec<Rect>>,
}

impl Scanout {
    fn new(size: (u32, u32)) -> Self {
        let stride = size.0 * 4 + 8;
        Self {
            mode: DisplayMode {
                width_px: size.0,
                height_px: size.1,
                stride_bytes: stride,
                format: DisplayFormat::Bgra8888,
            },
            stride,
            frame: alloc::vec![0; usize::try_from(stride * size.1).expect("small")],
            flips: false,
            in_flight: false,
            refuses: false,
            drawn: alloc::vec::Vec::new(),
        }
    }

    /// The bytes the display shows at `(x, y)`, in its own order.
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let at = usize::try_from(y * self.stride + x * 4).expect("small");
        self.frame[at..at + 4].try_into().expect("a pixel")
    }
}

impl Presenter for Scanout {
    fn mode(&self) -> DisplayMode {
        self.mode
    }

    fn ready(&self) -> bool {
        !self.in_flight
    }

    fn present(&mut self, _: &[u8], _: &[DamageRect]) -> Result<(), DriverError> {
        Ok(())
    }

    fn set_power(&mut self, _: DisplayPower) -> Result<(), DriverError> {
        Ok(())
    }

    fn present_drawn(
        &mut self,
        damage: &Region,
        draw: &mut DrawInPlace<'_>,
    ) -> Result<(), DriverError> {
        if self.refuses {
            return Err(DriverError::Unsupported);
        }
        if self.in_flight {
            return Err(DriverError::Busy);
        }
        draw(&mut self.frame, self.stride, damage)?;
        self.drawn.push(damage.rects().to_vec());
        self.in_flight = self.flips;
        Ok(())
    }
}

/// A saver covering the output is drawn straight into the display: its whole
/// frame first, every pixel opaque and in the display's order, then only what
/// changed, and never into the window, which the compositor stops presenting.
#[test]
fn a_saver_covering_the_output_is_drawn_straight_into_the_display() {
    let mut saver = Fixture::new(0);
    let mut display = Scanout::new(SIZE);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.draw(Color::rgb(200, 10, 10), QUARTER);
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Soon,
    });
    saver.read_onto(1, &mut display).expect("heard");
    assert_eq!(
        display.drawn,
        [alloc::vec![Rect::new(0, 0, SIZE.0, SIZE.1)]],
        "whole first"
    );
    assert_eq!(display.at(1, 1), [10, 10, 200, 255]);
    let window = saver.window_pixel(1, 1);
    assert_eq!(
        (window.r, window.g, window.b),
        (0, 0, 0),
        "the window is left alone"
    );
    assert!(saver.hosted.scans_out());
    assert!(saver.hosted.take_reached(), "shown as presented");
    let later = 2 + RefreshRate::ASSUMED.frame_ns();
    saver.advance(later).expect("on time");
    saver.pipes.say(&SaverMessage::Presented {
        serial: 2,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Idle,
    });
    saver.read_onto(later + 1, &mut display).expect("heard");
    assert_eq!(
        display.drawn[1],
        [Rect::new(0, 0, 8, 4)],
        "then only what changed"
    );
}

/// A frame presented while the display flips the last waits in the region,
/// with no frame asked meanwhile, and goes to the display at the latch.
#[test]
fn a_frame_drawn_while_the_display_flips_waits_for_its_latch() {
    let mut saver = Fixture::new(0);
    let mut display = Scanout::new(SIZE);
    display.flips = true;
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Soon,
    });
    saver.read_onto(1, &mut display).expect("heard");
    assert!(!saver.hosted.take_reached(), "not yet latched");
    saver
        .advance(1 + RefreshRate::ASSUMED.frame_ns())
        .expect("on time");
    assert_eq!(
        saver.pipes.heard().last(),
        Some(&OP_FRAME),
        "the next is drawn meanwhile"
    );
    saver.pipes.say(&SaverMessage::Presented {
        serial: 2,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Soon,
    });
    saver
        .read_onto(2 + RefreshRate::ASSUMED.frame_ns(), &mut display)
        .expect("heard");
    assert_eq!(display.drawn.len(), 1, "held while the display flips");
    let asked = saver.pipes.heard().len();
    saver
        .advance(3 * RefreshRate::ASSUMED.frame_ns())
        .expect("on time");
    assert_eq!(
        saver.pipes.heard().len(),
        asked,
        "nothing asked while one is held"
    );
    display.in_flight = false;
    assert!(saver
        .hosted
        .latched((&mut saver.compositor, saver.wm), &mut display));
    assert_eq!(display.drawn.len(), 2, "the held frame went at the latch");
}

/// A display still taking the compositor's last frame when the saver's first
/// arrives holds that frame for its latch, and then draws it whole.
#[test]
fn a_first_frame_held_for_the_latch_is_drawn_whole_at_it() {
    let mut saver = Fixture::new(0);
    let mut display = Scanout::new(SIZE);
    display.in_flight = true;
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Soon,
    });
    saver.read_onto(1, &mut display).expect("heard");
    assert!(display.drawn.is_empty(), "held for the latch");
    assert!(
        saver.hosted.scans_out(),
        "the compositor presents nothing meanwhile"
    );
    display.in_flight = false;
    assert!(saver
        .hosted
        .latched((&mut saver.compositor, saver.wm), &mut display));
    assert_eq!(
        display.drawn,
        [alloc::vec![Rect::new(0, 0, SIZE.0, SIZE.1)]],
        "whole"
    );
}

/// A display that refuses a frame drawn in its memory sends it, and every one
/// after, to the window.
#[test]
fn a_display_that_refuses_sends_every_frame_to_the_window() {
    let mut saver = Fixture::new(0);
    let mut display = Scanout::new(SIZE);
    display.refuses = true;
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.draw(Color::rgb(200, 10, 10), QUARTER);
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Soon,
    });
    saver.read_onto(1, &mut display).expect("heard");
    assert!(!saver.hosted.scans_out());
    let pixel = saver.window_pixel(1, 1);
    assert_eq!((pixel.r, pixel.a), (200, 255), "laid on the window");
    assert!(saver.shown(), "the compositor presented it");
    display.refuses = false;
    let later = 2 + RefreshRate::ASSUMED.frame_ns();
    saver.advance(later).expect("on time");
    saver.pipes.say(&SaverMessage::Presented {
        serial: 2,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Idle,
    });
    saver.read_onto(later + 1, &mut display).expect("heard");
    assert!(display.drawn.is_empty(), "never tried again");
}

/// A frame the display's memory cannot hold is refused before a byte of it is
/// drawn and never shown, and goes to the window instead.
#[test]
fn a_frame_the_display_cannot_hold_is_never_shown_there() {
    let mut saver = Fixture::new(0);
    let mut display = Scanout::new(SIZE);
    let short = display.frame.len() / 2;
    display.frame.truncate(short);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.draw(Color::rgb(200, 10, 10), QUARTER);
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Soon,
    });
    saver.read_onto(1, &mut display).expect("heard");
    assert!(display.drawn.is_empty(), "nothing shown");
    assert!(display.frame.iter().all(|byte| *byte == 0), "nothing drawn");
    assert!(!saver.hosted.scans_out());
    let pixel = saver.window_pixel(1, 1);
    assert_eq!((pixel.r, pixel.a), (200, 255), "laid on the window");
}

/// A display of another size than the saver's region is never drawn into.
#[test]
fn a_display_the_saver_does_not_cover_is_left_to_the_compositor() {
    let mut saver = Fixture::new(0);
    let mut display = Scanout::new((SIZE.0 * 2, SIZE.1));
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Idle,
    });
    saver.read_onto(1, &mut display).expect("heard");
    assert!(display.drawn.is_empty());
    assert!(!saver.hosted.scans_out());
}

const QUARTER: DamageRect = DamageRect {
    x: 0,
    y: 0,
    width_px: 8,
    height_px: 4,
};

const OP_START: u8 = 1;
const OP_FRAME: u8 = 2;
const OP_MACHINE: u8 = 3;
const OP_SLIDE: u8 = 4;
const OP_FONT_REPLY: u8 = 5;
const OP_KEPT: u8 = 6;
const OP_PRESSURE: u8 = 7;

/// A saver is sent its start, asked for a frame only once it is ready, and
/// what it presents is laid on its window where it said.
#[test]
fn a_presented_frame_is_laid_on_the_window_where_it_changed() {
    let mut saver = Fixture::new(0);
    assert_eq!(saver.pipes.heard(), [OP_START]);
    saver.advance(0).expect("on time");
    assert_eq!(
        saver.pipes.heard(),
        [OP_START],
        "nothing before it is ready"
    );
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(1).expect("heard");
    saver.advance(1).expect("on time");
    assert_eq!(saver.pipes.heard(), [OP_START, OP_FRAME]);
    saver.draw(Color::rgb(200, 10, 10), QUARTER);
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Idle,
    });
    saver.read(2).expect("heard");
    let inside = saver.window_pixel(1, 1);
    assert_eq!((inside.r, inside.g, inside.b, inside.a), (200, 10, 10, 255));
    let outside = saver.window_pixel(12, 6);
    assert_eq!(
        (outside.r, outside.g, outside.b),
        (0, 0, 0),
        "left as it was"
    );
}

/// However a saver scatters its damage, the compositor is handed no more
/// rectangles than the protocol names, in band form: a staircase of slivers
/// whose band form would cut the screen into thousands is laid within the
/// count, every stair still laid.
#[test]
fn a_saver_scattering_its_damage_cannot_fragment_the_compositors() {
    let size = (300, 140);
    let mut saver = Fixture::of(&start_of(0, size));
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.draw(
        Color::rgb(200, 10, 10),
        DamageRect::full(&saver.hosted.layout.display_mode()),
    );
    let stairs: alloc::vec::Vec<DamageRect> = (0..u32::try_from(SAVER_DAMAGE_RECTS)
        .expect("small"))
        .map(|step| DamageRect {
            x: 2 * step,
            y: step,
            width_px: 1,
            height_px: size.1 - step,
        })
        .collect();
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: Some(SaverDamage::new(&stairs).expect("disjoint stairs")),
        next: NextFrame::Idle,
    });
    saver.read(1).expect("heard");
    let laid = saver.hosted.laying.rects();
    assert!(
        laid.len() <= SAVER_DAMAGE_RECTS,
        "{} rectangles",
        laid.len()
    );
    let mut canonical = Region::new();
    for rect in laid {
        canonical.add(*rect);
    }
    assert_eq!(canonical.rects(), laid, "in band form");
    for stair in &stairs {
        let pixel = saver.window_pixel(stair.x, stair.y + stair.height_px - 1);
        assert_eq!((pixel.r, pixel.a), (200, 255), "{stair:?}");
    }
}

/// However a saver drew its pixels, what it presents covers the screen: a
/// transparent pixel is laid opaque.
#[test]
fn a_transparent_frame_is_laid_opaque() {
    let mut saver = Fixture::new(0);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.draw(Color::rgba(0, 0, 0, 0), QUARTER);
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Idle,
    });
    saver.read(1).expect("heard");
    assert_eq!(saver.window_pixel(1, 1).a, u8::MAX);
}

/// A window that lost its picture takes the whole frame again, and the pixels
/// the saver left transparent are laid opaque there too.
#[test]
fn a_window_that_lost_its_picture_is_laid_whole_and_opaque() {
    let mut saver = Fixture::new(0);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.compositor.teardown_content();
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: SaverDamage::new(&[QUARTER]).ok(),
        next: NextFrame::Idle,
    });
    saver.read(1).expect("heard");
    assert_eq!(
        saver.window_pixel(12, 6).a,
        u8::MAX,
        "beyond the damage too"
    );
    assert!(saver.shown());
}

/// A frame that changed nothing lays nothing, so no frame of the saver's
/// reaches the screen with the next present.
#[test]
fn a_frame_that_changed_nothing_is_not_a_frame_shown() {
    let mut saver = Fixture::new(0);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: None,
        next: NextFrame::Idle,
    });
    saver.read(1).expect("heard");
    assert!(!saver.shown());
}

const FRAME_NS: u64 = RefreshRate::ASSUMED.frame_ns();

/// The next frame is asked for once the last is laid, not once it is shown:
/// the saver draws it while the host composites the one before.
#[test]
fn the_next_frame_is_asked_for_while_the_last_awaits_the_screen() {
    let mut saver = Fixture::new(0);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.presents(1, 1);
    saver.advance(FRAME_NS).expect("on time");
    assert_eq!(
        saver.pipes.heard(),
        [OP_START, OP_FRAME, OP_FRAME],
        "asked before any present"
    );
}

/// A frame presented before the one laid has reached the screen waits in the
/// region, and no further frame is asked for until the screen takes the first.
#[test]
fn a_frame_presented_ahead_of_the_screen_waits_for_it() {
    let mut saver = Fixture::new(0);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver.advance(0).expect("on time");
    saver.draw(Color::rgb(200, 0, 0), QUARTER);
    saver.presents(1, 1);
    saver.advance(FRAME_NS).expect("on time");
    saver.draw(Color::rgb(0, 200, 0), QUARTER);
    saver.presents(2, FRAME_NS + 1);
    assert_eq!(saver.window_pixel(1, 1).r, 200, "the first still laid");
    saver.advance(4 * FRAME_NS).expect("on time");
    assert_eq!(
        saver.pipes.heard(),
        [OP_START, OP_FRAME, OP_FRAME],
        "nothing asked while one waits"
    );
    assert!(saver.shown(), "the first reached the screen");
    assert_eq!(saver.window_pixel(1, 1).g, 200, "the second laid behind it");
    saver.advance(4 * FRAME_NS).expect("on time");
    assert_eq!(
        saver.pipes.heard(),
        [OP_START, OP_FRAME, OP_FRAME, OP_FRAME]
    );
    assert!(saver.shown(), "the second reached it");
    assert!(!saver.shown(), "and nothing waits");
}

#[test]
fn a_saver_out_of_turn_garbled_or_gone_is_ended() {
    let mut early = Fixture::new(0);
    early.pipes.say(&SaverMessage::Presented {
        serial: 1,
        damage: None,
        next: NextFrame::Idle,
    });
    assert_eq!(early.read(0), Err(Ended::Faulted(Fault::OutOfTurn)));

    let mut garbled = Fixture::new(0);
    garbled.pipes.garble();
    assert_eq!(garbled.read(0), Err(Ended::Garbled));

    let mut gone = Fixture::new(0);
    gone.pipes.0.borrow_mut().closed = true;
    assert_eq!(gone.read(0), Err(Ended::Gone));
}

#[test]
fn a_saver_never_ready_is_ended_at_the_start_deadline() {
    let mut saver = Fixture::new(0);
    saver.advance(SAVER_START_DEADLINE_NS - 1).expect("not yet");
    assert_eq!(
        saver.advance(SAVER_START_DEADLINE_NS),
        Err(Ended::Faulted(Fault::NeverReady))
    );
}

/// A font call is handed to the relay and its reply sent back.
#[test]
fn a_font_call_is_relayed_and_answered() {
    let mut saver = Fixture::new(0);
    let call = FontRequest::Families.to_le_bytes();
    saver.pipes.say(&SaverMessage::FontCall(call));
    saver.read(0).expect("heard");
    assert_eq!(saver.errands.fonts, [call]);
    saver.hosted.font_answered(Ok(&[1, 2, 3]), 1).expect("sent");
    saver.flush();
    assert_eq!(saver.pipes.heard().last(), Some(&OP_FONT_REPLY));
}

/// A picture handed over is read out of the feed area, opaque and titled by
/// the saver's own folder, and the host's answer hands the area back.
#[test]
fn a_picture_handed_over_is_kept_and_answered() {
    let mut saver = Fixture::new(APPINFO_FLAG_FEED_KEEP);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    let layout = start(APPINFO_FLAG_FEED_KEEP).region;
    let mut picture = Surface::new(4, 2).expect("a picture");
    picture.fill(Color::rgba(9, 8, 7, 3));
    {
        let feed = &mut saver.hosted.region_mut()[layout.feed_range().expect("a feed area")];
        let area = DamageRect {
            x: 0,
            y: 0,
            width_px: 4,
            height_px: 2,
        };
        winframe::encode(
            &picture,
            feed,
            &layout.display_mode(),
            area,
            &tairix_parallel::SERIAL,
        )
        .expect("encodes");
    }
    saver.pipes.say(&SaverMessage::Keep {
        title: BoundedText::new("Classic").expect("a title"),
        width: 4,
        height: 2,
    });
    saver.read(1).expect("heard");
    let [kept] = saver.errands.kept.as_slice() else {
        panic!("one picture to keep");
    };
    assert_eq!(kept.folder, "Waves");
    assert_eq!(kept.name, "Classic");
    assert_eq!((kept.surface.width(), kept.surface.height()), (4, 2));
    assert!(
        kept.surface.pixels().iter().all(|pixel| pixel.a == u8::MAX),
        "opaque"
    );
    saver.hosted.kept(true, 2).expect("sent");
    saver.flush();
    assert_eq!(saver.pipes.heard().last(), Some(&OP_KEPT));
}

/// A slide is wanted through the errands, laid in the feed area when it
/// lands, and answered; a slide nobody waits for is left alone.
#[test]
fn a_slide_is_laid_in_the_feed_area_and_answered() {
    let mut saver = Fixture::new(APPINFO_FLAG_FEED_SLIDES);
    saver.pipes.say(&SaverMessage::WantSlide(2));
    saver.read(0).expect("heard");
    assert_eq!(saver.errands.slides, [2]);
    let mut slide = Surface::new(SIZE.0, SIZE.1).expect("a slide");
    slide.fill(Color::rgb(1, 2, 3));
    saver
        .hosted
        .slide_landed(1, Some(&slide), 1, &tairix_parallel::SERIAL)
        .expect("ignored");
    saver.flush();
    assert_ne!(
        saver.pipes.heard().last(),
        Some(&OP_SLIDE),
        "not the one wanted"
    );
    saver
        .hosted
        .slide_landed(2, Some(&slide), 1, &tairix_parallel::SERIAL)
        .expect("sent");
    saver.flush();
    let answer = saver.pipes.last();
    assert_eq!(
        HostMessage::decode(&answer),
        Ok(HostMessage::Slide {
            index: 2,
            outcome: SlideOutcome::Shown
        })
    );
    let layout = start(APPINFO_FLAG_FEED_SLIDES).region;
    let feed = &saver.hosted.region_mut()[layout.feed_range().expect("a feed area")];
    assert_eq!(&feed[..4], &[1, 2, 3, 255]);
}

/// A saver slow to read is never sent a backlog of readings: while anything
/// is queued for it the latest reading and pressure band are held, and only
/// they follow once the queue drains.
#[test]
fn readings_wait_behind_a_queue_and_only_the_latest_is_sent() {
    let mut saver = Fixture::new(APPINFO_FLAG_FEED_MACHINE);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    saver
        .hosted
        .advance(0, &WallReading::unset(0))
        .expect("a frame asked for");
    let report = |period_ms| {
        tairix_abi::switchboard_ipc::MachineReport::unread(
            tairix_abi::switchboard_ipc::ReportPeriod::from_millis(period_ms).expect("a period"),
            tairix_abi::switchboard_ipc::MachineScope::Machine,
        )
    };
    saver.hosted.machine(report(1_000), 1).expect("held");
    saver.hosted.machine(report(2_000), 2).expect("held");
    saver
        .hosted
        .pressure(tairix_abi::sysinfo::MemoryBand::NORMAL, 3)
        .expect("held");
    assert_eq!(saver.pipes.heard(), [OP_START], "nothing past the queue");
    saver.flush();
    saver.flush();
    assert_eq!(
        saver.pipes.heard(),
        [OP_START, OP_FRAME, OP_PRESSURE, OP_MACHINE],
        "one reading, the latest"
    );
    let Ok(HostMessage::Machine(sent)) = HostMessage::decode(&saver.pipes.last()) else {
        panic!("a machine report");
    };
    assert_eq!(sent, report(2_000));
}

/// Whose task names reach a saver is the desktop's to say: under the default,
/// another account's busiest task is sent by its cost alone, so no saver is
/// trusted to hide what it was given.
#[test]
fn another_accounts_task_reaches_a_saver_by_its_cost_alone() {
    use tairix_abi::switchboard_ipc::{
        MachineReport, MachineScope, MachineTask, MachineTasks, Permille, ReportPeriod, TaskOwner,
    };

    let mut saver = Fixture::new(APPINFO_FLAG_FEED_MACHINE);
    saver.pipes.say(&SaverMessage::Ready);
    saver.read(0).expect("heard");
    let task = |name: &str, owner| MachineTask {
        name: Some(BoundedText::new(name).expect("a name")),
        owner,
        cpu: Permille::ZERO,
        memory_bytes: 1 << 20,
    };
    let mut report = MachineReport::unread(
        ReportPeriod::from_millis(2_000).expect("a period"),
        MachineScope::Machine,
    );
    report.tasks = MachineTasks::new(
        Some(2),
        0,
        0,
        &[
            task("editor", TaskOwner::Own),
            task("diary", TaskOwner::Other),
        ],
    )
    .expect("tasks");
    saver.hosted.machine(report, 1).expect("sent");
    saver.flush();
    let Ok(HostMessage::Machine(sent)) = HostMessage::decode(&saver.pipes.last()) else {
        panic!("a machine report");
    };
    let names: alloc::vec::Vec<Option<&str>> = sent
        .tasks
        .busiest()
        .map(|task| task.name.as_ref().map(BoundedText::as_str))
        .collect();
    assert_eq!(
        names,
        [Some("editor"), None],
        "the other account's name withheld"
    );
}
