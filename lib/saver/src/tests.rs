extern crate std;

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::saver::{ChoiceValue, OptionKey, SaverFeeds, SaverValue};
use tairix_abi::saver_ipc::{
    HostMessage, OptionValue, SaverMessage, SaverRegion, SlideOutcome, Start, WallReading,
};
use tairix_abi::{Errno, APPINFO_FLAG_FEED_KEEP, APPINFO_FLAG_FEED_SLIDES};
use tairix_geometry::{Rect, Region};
use tairix_raster::{Color, Pixel, Surface};
use tairix_rt::sync::Mutex;

use crate::link::Link;
use crate::testing::{replies, script, HeapRegion};
use crate::{serve, Ended, Frame, Host, NextFrame, Scene, Setup};

fn start(width: u32, height: u32, feeds: u32) -> Start {
    let feeds = SaverFeeds::from_bits(feeds).expect("feeds");
    let region = SaverRegion::new(width, height, feeds).expect("a region");
    let mut start = Start::new(region, 100, RefreshRate::from_mhz(60_000).expect("a rate"));
    start.feeds = feeds;
    start.memory = 64 << 20;
    if feeds.slides() {
        start.slides = 2;
    }
    start
}

/// The setup `serve` makes of `given` at 100%.
fn setup(given: &Start) -> Setup<'_> {
    Setup {
        start: given,
        scale: tairix_geometry::Scale::ONE,
        seed: 0,
    }
}

fn frame(serial: u32) -> HostMessage<'static> {
    HostMessage::Frame {
        serial,
        now_ns: u64::from(serial) * 1_000,
        wall: WallReading::unset(0),
    }
}

const RED: Color = Color::rgba(255, 0, 0, 255);

/// Fills a 2 × 2 square at the origin on its first frame and asks again at 5.
#[derive(Default)]
struct Square {
    frames: u32,
    slides: Vec<(u16, bool)>,
    /// The first pixel of each slide a frame read from the feed area.
    read: Vec<Pixel>,
    kept: Vec<bool>,
    want: Option<u16>,
    keep: bool,
    refused: Vec<Errno>,
    lost: bool,
}

impl Scene for Square {
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        _frame: &Frame<'_>,
        host: &mut Host<'_>,
    ) -> NextFrame {
        self.frames += 1;
        surface.fill_rect(0, 0, 2, 2, RED);
        if self.slides.last().is_some_and(|(_, shown)| *shown) {
            if let Some(picture) = host.feed_picture() {
                let mut slide = Surface::new(4, 4).expect("a surface");
                picture.read_into(&mut slide).expect("read");
                self.read.push(slide.pixels()[0]);
            }
        }
        if let Some(index) = self.want.take() {
            host.want_slide(index).expect("wanted");
            if let Err(errno) = host.want_slide(index) {
                self.refused.push(errno);
            }
        }
        if core::mem::take(&mut self.keep) {
            let picture = Surface::new(2, 1).expect("a picture");
            host.keep("A picture", &picture).expect("kept");
        }
        damage.add(Rect::new(0, 0, 2, 2));
        NextFrame::At(5)
    }

    fn slide(&mut self, index: u16, shown: bool) {
        self.slides.push((index, shown));
    }

    fn kept(&mut self, kept: bool) {
        self.kept.push(kept);
    }

    fn machine_lost(&mut self) {
        self.lost = true;
    }
}

#[test]
fn a_frame_is_drawn_written_into_the_region_and_presented() {
    let (channel, said) = script(&[HostMessage::Start(start(4, 4, 0)), frame(1)]);
    let mut region = None;
    let mut drawn = 0;
    let ended = serve(
        channel,
        |layout| {
            region = Some(layout.len());
            Ok(HeapRegion::new(vec![0; layout.len()]))
        },
        |_| {
            drawn += 1;
            Ok(Square::default())
        },
    );
    assert_eq!(ended, Ended::Dismissed);
    assert_eq!(region, Some(4 * 4 * 4));
    assert_eq!(drawn, 1);
    let said = replies(&said.lock());
    assert_eq!(said[0], SaverMessage::Ready);
    let SaverMessage::Presented {
        serial,
        damage,
        next,
    } = said[1]
    else {
        panic!("not presented: {:?}", said[1]);
    };
    assert_eq!((serial, next), (1, NextFrame::At(5)));
    let rects = damage.expect("damage");
    assert_eq!(rects.rects().len(), 1);
    assert_eq!(rects.rects()[0].width_px, 2);
}

/// The scene draws straight into the frame area, which holds the whole
/// opaque-black picture from the start; a frame changes only what it draws.
#[test]
fn the_region_holds_the_whole_picture_from_the_start() {
    let (channel, _) = script(&[HostMessage::Start(start(4, 4, 0)), frame(1)]);
    let out = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&out);
    let ended = serve(
        channel,
        |layout| Ok(HeapRegion::recorded(vec![0x55; layout.len()], recorded)),
        |_| Ok(Square::default()),
    );
    assert_eq!(ended, Ended::Dismissed);
    let bytes = out.lock();
    for (index, pixel) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let red = index % 4 < 2 && index / 4 < 2;
        let want = if red {
            [255, 0, 0, 255]
        } else {
            [0, 0, 0, 255]
        };
        assert_eq!(*pixel, want, "pixel {index}");
    }
}

/// A scene that cannot be built ends the saver unstarted with the scene's
/// reason, and the host is told it.
#[test]
fn a_scene_refused_by_its_maker_starts_nothing() {
    let (channel, said) = script(&[HostMessage::Start(start(4, 4, 0)), frame(1)]);
    let ended = serve(
        channel,
        |layout| Ok(HeapRegion::new(vec![0; layout.len()])),
        |setup: &Setup<'_>| setup.switch("warp").map(|_| Square::default()),
    );
    assert_eq!(ended, Ended::Unstarted(Errno::NotFound));
    assert_eq!(
        replies(&said.lock()),
        [SaverMessage::Unstarted(Errno::NotFound)],
        "the host is told why"
    );
}

/// Each option reads only as the kind the host sent it as.
#[test]
fn an_option_reads_as_its_own_kind_alone() {
    let mut given = start(4, 4, 0);
    for (key, value) in [
        (
            "stars",
            SaverValue::Choice(ChoiceValue::new("dense").expect("a word")),
        ),
        ("warp", SaverValue::Switch(true)),
        ("pause", SaverValue::Range(-3)),
    ] {
        let key = OptionKey::new(key).expect("a key");
        given.push_option(OptionValue { key, value }).expect("room");
    }
    let setup = setup(&given);
    assert_eq!(
        setup.choice("stars").map(|word| word.as_str() == "dense"),
        Ok(true)
    );
    assert_eq!(setup.switch("warp"), Ok(true));
    assert_eq!(setup.range("pause"), Ok(-3));
    assert_eq!(setup.switch("stars"), Err(Errno::NotFound));
    assert_eq!(setup.range("warp"), Err(Errno::NotFound));
    assert_eq!(setup.choice("pause").map(|_| ()), Err(Errno::NotFound));
    assert_eq!(setup.switch("absent"), Err(Errno::NotFound));
}

/// A scene that tells the time is next owed a frame when the reading changes,
/// and a change the host already counts as due a frame later, never at once.
#[test]
fn a_reading_change_already_due_waits_a_frame() {
    let wall = WallReading::unset(50);
    let due = Frame {
        serial: 0,
        now_ns: 40,
        wall: &wall,
    };
    assert_eq!(due.reading_changes_ns(5), 50);
    let late = Frame {
        serial: 0,
        now_ns: 60,
        wall: &wall,
    };
    assert_eq!(late.reading_changes_ns(5), 65);
}

/// A scene's pool holds no more participants than the host granted threads,
/// nor than the machine has cores.
#[test]
fn a_pool_is_held_to_the_granted_threads() {
    let mut granted = start(4, 4, 0);
    granted.cores = 8;
    granted.threads = 3;
    // The host refuses a thread, so only the bound the pool asked for shows.
    assert!(setup(&granted).pool().worker_count() <= 2);
    granted.threads = 1;
    assert_eq!(setup(&granted).pool().worker_count(), 0);
}

#[test]
fn the_first_message_must_be_a_start() {
    let (channel, _) = script(&[frame(1)]);
    let ended = serve(
        channel,
        |layout| Ok(HeapRegion::new(vec![0; layout.len()])),
        |_| Ok(Square::default()),
    );
    assert_eq!(ended, Ended::Broken(Errno::BadMagic));
}

#[test]
fn a_message_out_of_turn_ends_the_saver() {
    for out_of_turn in [
        HostMessage::Slide {
            index: 0,
            outcome: SlideOutcome::Shown,
        },
        HostMessage::Kept { kept: true },
        HostMessage::Start(start(4, 4, 0)),
        HostMessage::FontReply(Ok(&[])),
        HostMessage::MachineLost,
    ] {
        let (channel, _) = script(&[HostMessage::Start(start(4, 4, 0)), out_of_turn]);
        let ended = serve(
            channel,
            |layout| Ok(HeapRegion::new(vec![0; layout.len()])),
            |_| Ok(Square::default()),
        );
        assert_eq!(ended, Ended::Broken(Errno::BadMagic));
    }
}

/// A saver given the machine feed hears when its source stops.
#[test]
fn a_lost_machine_feed_reaches_the_scene() {
    let (channel, _) = script(&[
        HostMessage::Start(start(4, 4, tairix_abi::APPINFO_FLAG_FEED_MACHINE)),
        HostMessage::MachineLost,
    ]);
    let scene = Arc::new(Mutex::new(None));
    let out = Arc::clone(&scene);
    let ended = serve(
        channel,
        |layout| Ok(HeapRegion::new(vec![0; layout.len()])),
        |_| {
            Ok(Watched {
                inner: Square::default(),
                out,
            })
        },
    );
    assert_eq!(ended, Ended::Dismissed);
    assert!(scene.lock().as_ref().is_some_and(|square| square.lost));
}

#[test]
fn a_region_smaller_than_its_layout_or_an_unusable_scale_starts_nothing() {
    let (channel, said) = script(&[HostMessage::Start(start(4, 4, 0))]);
    let ended = serve(
        channel,
        |_| Ok(HeapRegion::new(vec![0; 8])),
        |_| Ok(Square::default()),
    );
    assert_eq!(ended, Ended::Unstarted(Errno::LengthOutOfRange));
    assert_eq!(
        replies(&said.lock()),
        [SaverMessage::Unstarted(Errno::LengthOutOfRange)]
    );
    let mut scaled = start(4, 4, 0);
    scaled.scale_percent = 9_000;
    let (channel, _) = script(&[HostMessage::Start(scaled)]);
    let ended = serve(
        channel,
        |layout| Ok(HeapRegion::new(vec![0; layout.len()])),
        |_: &Setup<'_>| Ok(Square::default()),
    );
    assert_eq!(ended, Ended::Broken(Errno::OutOfRange));
}

#[test]
fn a_slide_is_wanted_once_and_read_from_the_feed_area() {
    let (channel, said) = script(&[
        HostMessage::Start(start(4, 4, APPINFO_FLAG_FEED_SLIDES)),
        frame(1),
        HostMessage::Slide {
            index: 1,
            outcome: SlideOutcome::Shown,
        },
        frame(2),
    ]);
    let scene = Arc::new(Mutex::new(None));
    let kept = Arc::clone(&scene);
    let ended = serve(
        channel,
        |layout| {
            let mut bytes = vec![0; layout.len()];
            let feed = layout.feed_range().expect("a feed area");
            bytes[feed.start..feed.start + 4].copy_from_slice(&[9, 8, 7, 255]);
            Ok(HeapRegion::new(bytes))
        },
        |_| {
            Ok(Watched {
                inner: Square {
                    want: Some(1),
                    ..Square::default()
                },
                out: kept,
            })
        },
    );
    assert_eq!(ended, Ended::Dismissed);
    let said = replies(&said.lock());
    assert!(said.contains(&SaverMessage::WantSlide(1)));
    let seen = scene.lock().take().expect("watched");
    assert_eq!(
        seen.refused,
        [Errno::Busy],
        "one slide is outstanding at once"
    );
    assert_eq!(seen.slides, [(1, true)]);
    assert_eq!(
        seen.read,
        [Pixel {
            r: 9,
            g: 8,
            b: 7,
            a: 255
        }],
        "the next frame reads it"
    );
}

#[test]
fn a_kept_picture_is_written_into_the_feed_area_and_answered() {
    let (channel, said) = script(&[
        HostMessage::Start(start(4, 4, APPINFO_FLAG_FEED_KEEP)),
        frame(1),
        HostMessage::Kept { kept: true },
    ]);
    let scene = Arc::new(Mutex::new(None));
    let kept = Arc::clone(&scene);
    let ended = serve(
        channel,
        |layout| Ok(HeapRegion::new(vec![0xAA; layout.len()])),
        |_| {
            Ok(Watched {
                inner: Square {
                    keep: true,
                    ..Square::default()
                },
                out: kept,
            })
        },
    );
    assert_eq!(ended, Ended::Dismissed);
    let said = replies(&said.lock());
    assert!(said.iter().any(|message| matches!(
        message,
        SaverMessage::Keep {
            width: 2,
            height: 1,
            ..
        }
    )));
    assert_eq!(scene.lock().take().expect("watched").kept, [true]);
}

#[test]
fn a_feed_the_saver_was_not_given_is_refused() {
    let mut sent = Vec::new();
    {
        let mut relay = |message: &SaverMessage| {
            sent.push(*message);
            Ok(())
        };
        let plain = start(4, 4, 0);
        let mut feed = crate::FeedArea::Saver;
        let mut host = Host {
            send: &mut relay,
            start: &plain,
            feed: &mut feed,
            feed_bytes: None,
        };
        assert_eq!(host.want_slide(0), Err(Errno::PermissionDenied));
        assert!(host.feed_picture().is_none(), "no feed area to read");
        let picture = Surface::new(1, 1).expect("a picture");
        assert_eq!(host.keep("A", &picture), Err(Errno::PermissionDenied));
        let given = start(4, 4, APPINFO_FLAG_FEED_SLIDES | APPINFO_FLAG_FEED_KEEP);
        let mut feed = crate::FeedArea::Saver;
        let mut bytes = vec![0u8; 64];
        let mut host = Host {
            send: &mut relay,
            start: &given,
            feed: &mut feed,
            feed_bytes: Some(&mut bytes),
        };
        assert_eq!(host.want_slide(2), Err(Errno::OutOfRange));
        let wide = Surface::new(5, 1).expect("a picture");
        assert_eq!(host.keep("A", &wide), Err(Errno::LengthOutOfRange));
        assert_eq!(host.keep("", &picture), Err(Errno::LengthOutOfRange));
    }
    assert!(sent.is_empty());
}

/// A changed band reaches the gauge and empties the font caches at once, not
/// at their next lookup; an unchanged one trims nothing.
#[test]
fn a_changed_pressure_band_trims_the_font_caches() {
    use core::cell::Cell;
    use tairix_abi::sysinfo::MemoryBand;
    let (reported, trims) = (Cell::new(None), Cell::new(0));
    let severe = MemoryBand::new(3).expect("a band");
    crate::serve::relay_pressure(
        severe,
        |band| {
            reported.set(Some(band.depth()));
            true
        },
        || trims.set(trims.get() + 1),
    );
    assert_eq!(reported.get(), Some(3));
    assert_eq!(trims.get(), 1);
    crate::serve::relay_pressure(severe, |_| false, || trims.set(trims.get() + 1));
    assert_eq!(trims.get(), 1, "the band held, so nothing is trimmed");
}

#[test]
fn a_font_call_holds_what_arrives_before_its_reply() {
    let report = HostMessage::Pressure(tairix_abi::sysinfo::MemoryBand::NORMAL);
    let (channel, said) = script(&[report.clone(), HostMessage::FontReply(Ok(&[1, 2, 3]))]);
    let mut link = Link::new(channel);
    let request = tairix_abi::font_ipc::FontRequest::Families.to_le_bytes();
    let mut reply = [0u8; 8];
    let relay = crate::link::FontRelay {
        link: Arc::new(Mutex::new(link)),
    };
    let mut relay = relay;
    let got = tairix_font::FontTransport::call(&mut relay, &request, &mut reply);
    assert_eq!(got, Ok(3));
    assert_eq!(&reply[..3], &[1, 2, 3]);
    assert!(matches!(
        replies(&said.lock())[..],
        [SaverMessage::FontCall(_)]
    ));
    link = Arc::into_inner(relay.link)
        .expect("one holder")
        .into_inner();
    let mut held = Vec::new();
    assert_eq!(link.next_into(&mut held), Ok(true), "held");
    assert_eq!(HostMessage::decode(&held), Ok(report));
    assert_eq!(link.next_into(&mut held), Ok(false));
}

#[test]
fn a_font_call_cut_short_breaks_the_conversation() {
    let (channel, _) = script(&[]);
    let relay = crate::link::FontRelay {
        link: Arc::new(Mutex::new(Link::new(channel))),
    };
    let mut relay = relay;
    let request = tairix_abi::font_ipc::FontRequest::Families.to_le_bytes();
    assert_eq!(
        tairix_font::FontTransport::call(&mut relay, &request, &mut [0u8; 8]),
        Err(Errno::BrokenPipe)
    );
    assert_eq!(
        relay.link.lock().next_into(&mut Vec::new()),
        Err(Errno::BrokenPipe)
    );
}

/// A scene whose final state the test reads back once the loop ends.
struct Watched {
    inner: Square,
    out: Arc<Mutex<Option<Square>>>,
}

impl Scene for Watched {
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        host: &mut Host<'_>,
    ) -> NextFrame {
        self.inner.frame(surface, damage, frame, host)
    }

    fn slide(&mut self, index: u16, shown: bool) {
        self.inner.slide(index, shown);
    }

    fn kept(&mut self, kept: bool) {
        self.inner.kept(kept);
    }

    fn machine_lost(&mut self) {
        self.inner.machine_lost();
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        *self.out.lock() = Some(core::mem::take(&mut self.inner));
    }
}
