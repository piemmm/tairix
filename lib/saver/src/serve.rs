//! The saver's loop: `Start`, `Ready`, then every frame and feed until the
//! host closes the conversation.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::saver_ipc::{
    HostMessage, SaverDamage, SaverMessage, SaverRegion, SlideOutcome, Start, SAVER_DAMAGE_RECTS,
    SAVER_SEED_LEN,
};
use tairix_abi::sysinfo::MemoryBand;
use tairix_abi::Errno;
use tairix_display::winframe::damage_in;
use tairix_geometry::{Region, Scale};
use tairix_hash::HashSeed;
use tairix_raster::{PixelMemory, Surface};
use tairix_reclaim::PressureBand;
use tairix_rt::sync::Mutex;
use tairix_sandbox::proto::Channel;

use crate::link::{FontRelay, Link};
use crate::{feed_of, FeedArea, Frame, Host, Scene, Setup, GROUND};

const _: () = assert!(HashSeed::LEN + 8 <= SAVER_SEED_LEN);

/// The most of what a saver may map each of its two font caches takes, as a
/// divisor: they are sized from the machine's memory as every desktop
/// program's are, and a machine with memory to spare must not size them past
/// the saver's ceiling.
const FONT_CACHE_SHARE: u64 = 16;

/// How a saver's run ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Ended {
    /// The host closed the conversation: the saver was dismissed.
    Dismissed,
    /// The host spoke out of turn, sent what does not decode, or the channel
    /// failed.
    Broken(Errno),
    /// The saver could not begin: its region, its surface or its scene.
    Unstarted(Errno),
}

/// The memory a saver's region is mapped into.
pub trait RegionBytes {
    /// What the region holds after its frame area: the feed area, for a saver
    /// given one.
    type Rest: AsMut<[u8]>;

    /// The frame area, the region's first `frame_len` bytes, as memory the
    /// scene's surface draws straight into, and the bytes after it.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a region shorter than its frame area.
    fn into_parts(self, frame_len: usize) -> Result<(Box<dyn PixelMemory>, Self::Rest), Errno>;
}

/// Run a saver over `channel` until its host closes it: map the region the
/// host's `Start` describes with `map`, make the scene with `make`, and draw
/// every frame the host asks for. A scene `make` refuses ends the saver
/// unstarted with its reason.
///
/// Installs the process's font transport and caches, so a scene's text is
/// relayed through the host and kept within the saver's own memory.
pub fn serve<C, R, S>(
    channel: C,
    map: impl FnOnce(&SaverRegion) -> Result<R, Errno>,
    make: impl FnOnce(&Setup<'_>) -> Result<S, Errno>,
) -> Ended
where
    C: Channel + Send + 'static,
    R: RegionBytes,
    S: Scene,
{
    let link = Arc::new(Mutex::new(Link::new(channel)));
    let mut saver = match begin(&link, map, make) {
        Ok(saver) => saver,
        Err(Ended::Unstarted(errno)) => {
            // Said so the host records why; a pipe that is gone says enough.
            let _ = link.lock().send(&SaverMessage::Unstarted(errno));
            return Ended::Unstarted(errno);
        }
        Err(ended) => return ended,
    };
    let mut bytes = Vec::new();
    loop {
        let next = link.lock().next_into(&mut bytes);
        match next {
            Ok(true) => {}
            Ok(false) => return Ended::Dismissed,
            Err(errno) => return Ended::Broken(errno),
        }
        let answered = HostMessage::decode(&bytes).and_then(|message| saver.answer(message, &link));
        if let Err(errno) = answered {
            return Ended::Broken(errno);
        }
    }
}

/// A saver under way.
struct Running<R, S> {
    start: Start,
    /// The scene's picture, drawn straight into the region's frame area.
    surface: Surface,
    /// The region after its frame area.
    rest: R,
    damage: Region,
    scene: S,
    feed: FeedArea,
}

/// Read the host's `Start`, map the region, install the font relay and make
/// the scene, then say `Ready`.
fn begin<C, R, S>(
    link: &Arc<Mutex<Link<C>>>,
    map: impl FnOnce(&SaverRegion) -> Result<R, Errno>,
    make: impl FnOnce(&Setup<'_>) -> Result<S, Errno>,
) -> Result<Running<R::Rest, S>, Ended>
where
    C: Channel + Send + 'static,
    R: RegionBytes,
{
    let start = first_start(link).map_err(Ended::Broken)?;
    let scale = Scale::from_percent(start.scale_percent).ok_or(Ended::Broken(Errno::OutOfRange))?;
    let layout = start.region;
    let (frame, mut rest) = map(&layout)
        .and_then(|region| region.into_parts(layout.frame_len()))
        .map_err(Ended::Unstarted)?;
    if layout
        .feed_range()
        .is_some_and(|range| rest.as_mut().len() < range.len())
    {
        return Err(Ended::Unstarted(Errno::LengthOutOfRange));
    }
    tairix_font::set_font_transport(Box::new(FontRelay {
        link: Arc::clone(link),
    }));
    let mut key = [0u8; HashSeed::LEN];
    key.copy_from_slice(&start.seed[..HashSeed::LEN]);
    tairix_font::install_caches(
        start.installed_memory,
        start.memory / FONT_CACHE_SHARE,
        HashSeed::from_bytes(key),
    );
    feel(start.pressure);
    // The frame area is the picture from here on; the host reads it only from
    // a `Presented` to the `Frame` after it.
    let mut surface = Surface::lent(layout.width(), layout.height(), frame)
        .ok_or(Ended::Unstarted(Errno::LengthOutOfRange))?;
    surface.fill(GROUND);
    let mut seed = [0u8; 8];
    seed.copy_from_slice(&start.seed[HashSeed::LEN..HashSeed::LEN + 8]);
    let setup = Setup {
        start: &start,
        scale,
        seed: u64::from_le_bytes(seed),
    };
    let scene = make(&setup).map_err(Ended::Unstarted)?;
    link.lock()
        .send(&SaverMessage::Ready)
        .map_err(Ended::Broken)?;
    Ok(Running {
        start,
        surface,
        rest,
        damage: Region::with_budget(SAVER_DAMAGE_RECTS),
        scene,
        feed: FeedArea::Saver,
    })
}

impl<R: AsMut<[u8]>, S: Scene> Running<R, S> {
    /// Answer one host `message`; a message out of turn is refused.
    fn answer<C: Channel>(
        &mut self,
        message: HostMessage<'_>,
        link: &Mutex<Link<C>>,
    ) -> Result<(), Errno> {
        match message {
            HostMessage::Frame {
                serial,
                now_ns,
                wall,
            } => {
                let mut send = |message: &SaverMessage| link.lock().send(message);
                let mut host = Host {
                    send: &mut send,
                    start: &self.start,
                    feed: &mut self.feed,
                    feed_bytes: feed_of(self.rest.as_mut(), &self.start.region),
                };
                let frame = Frame {
                    serial,
                    now_ns,
                    wall: &wall,
                };
                self.damage.clear();
                let next = self
                    .scene
                    .frame(&mut self.surface, &mut self.damage, &frame, &mut host);
                link.lock().send(&SaverMessage::Presented {
                    serial,
                    damage: named(&self.start.region, &self.damage),
                    next,
                })
            }
            HostMessage::Machine(report) if self.start.feeds.machine() => {
                self.scene.machine(&report);
                Ok(())
            }
            HostMessage::MachineLost if self.start.feeds.machine() => {
                self.scene.machine_lost();
                Ok(())
            }
            HostMessage::Slide { index, outcome } if self.feed == FeedArea::Slide(index) => {
                self.feed = FeedArea::Saver;
                self.scene.slide(index, outcome == SlideOutcome::Shown);
                Ok(())
            }
            HostMessage::Kept { kept } if self.feed == FeedArea::Keep => {
                self.feed = FeedArea::Saver;
                self.scene.kept(kept);
                Ok(())
            }
            HostMessage::Pressure(band) => {
                feel(band);
                Ok(())
            }
            _ => Err(Errno::BadMagic),
        }
    }
}

/// The host's first message, which must be its `Start`.
fn first_start<C: Channel>(link: &Mutex<Link<C>>) -> Result<Start, Errno> {
    let mut bytes = Vec::new();
    if !link.lock().next_into(&mut bytes)? {
        return Err(Errno::BrokenPipe);
    }
    match HostMessage::decode(&bytes)? {
        HostMessage::Start(start) => Ok(start),
        _ => Err(Errno::BadMagic),
    }
}

/// Publish `band` to the process's gauge, which its caches and heap read.
fn feel(band: MemoryBand) {
    relay_pressure(band, tairix_rt::pressure::report, || {
        tairix_font::trim_glyph_cache();
    });
}

/// Hand a known `band` to `report`, and on a change `trim` the font caches,
/// which would otherwise hold their glyphs until their next lookup.
pub(crate) fn relay_pressure(
    band: MemoryBand,
    report: impl FnOnce(PressureBand) -> bool,
    trim: impl FnOnce(),
) {
    if let Some(band) = PressureBand::from_known_depth(band.depth()) {
        if report(band) {
            trim();
        }
    }
}

/// What `damage` names inside the screen `region` lays out, as a `Presented`
/// carries it; `None` when nothing inside the screen changed.
fn named(region: &SaverRegion, damage: &Region) -> Option<SaverDamage> {
    let mode = region.display_mode();
    SaverDamage::fitted(
        damage
            .rects()
            .iter()
            .filter_map(|rect| damage_in(&mode, *rect)),
    )
}
