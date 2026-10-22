//! The saver's loop: `Start`, `Ready`, then every frame and feed until the
//! host closes the conversation.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageList, DamageRect, MAX_DAMAGE_RECTS};
use tairix_abi::saver_ipc::{
    HostMessage, SaverMessage, SaverRegion, SlideOutcome, Start, SAVER_SEED_LEN,
};
use tairix_abi::sysinfo::MemoryBand;
use tairix_abi::Errno;
use tairix_display::winframe::{damage_in, encode};
use tairix_geometry::{Region, Scale};
use tairix_hash::HashSeed;
use tairix_raster::Surface;
use tairix_reclaim::PressureBand;
use tairix_rt::sync::Mutex;
use tairix_sandbox::proto::Channel;

use crate::link::{FontRelay, Link};
use crate::{areas, FeedArea, Frame, Host, Scene, Setup, GROUND};

const _: () = assert!(HashSeed::LEN + 8 <= SAVER_SEED_LEN);

/// The most rectangles a frame's damage holds before it is its bounding box:
/// enough for the present's few to be fitted well, few enough that the region
/// stays cheap to build however many small changes a scene reports.
pub(crate) const DAMAGE_BUDGET: usize = 8 * MAX_DAMAGE_RECTS;

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
    /// The region's bytes.
    fn bytes_mut(&mut self) -> &mut [u8];
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
    region: R,
    surface: Surface,
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
) -> Result<Running<R, S>, Ended>
where
    C: Channel + Send + 'static,
    R: RegionBytes,
{
    let start = first_start(link).map_err(Ended::Broken)?;
    let scale = Scale::from_percent(start.scale_percent).ok_or(Ended::Broken(Errno::OutOfRange))?;
    let mut region = map(&start.region).map_err(Ended::Unstarted)?;
    if region.bytes_mut().len() < start.region.len() {
        return Err(Ended::Unstarted(Errno::LengthOutOfRange));
    }
    tairix_font::set_font_transport(Box::new(FontRelay {
        link: Arc::clone(link),
    }));
    let mut key = [0u8; HashSeed::LEN];
    key.copy_from_slice(&start.seed[..HashSeed::LEN]);
    tairix_font::install_caches(start.memory, HashSeed::from_bytes(key));
    feel(start.pressure);
    let mut surface = Surface::new(start.region.width(), start.region.height())
        .ok_or(Ended::Unstarted(Errno::OutOfMemory))?;
    surface.fill(GROUND);
    // From here the frame area holds the whole picture, so the host may read
    // any part of it at any time, not only what the last frame changed.
    let mode = start.region.display_mode();
    let (pixels, _) = areas(region.bytes_mut(), &start.region);
    encode(
        &surface,
        pixels,
        &mode,
        DamageRect::full(&mode),
        &tairix_parallel::Serial,
    )
    .map_err(Ended::Unstarted)?;
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
        region,
        surface,
        damage: Region::with_budget(DAMAGE_BUDGET),
        scene,
        feed: FeedArea::Saver,
    })
}

impl<R: RegionBytes, S: Scene> Running<R, S> {
    /// Answer one host `message`; a message out of turn is refused.
    fn answer<C: Channel>(
        &mut self,
        message: HostMessage<'_>,
        link: &Mutex<Link<C>>,
    ) -> Result<(), Errno> {
        let (pixels, fed) = areas(self.region.bytes_mut(), &self.start.region);
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
                    feed_bytes: fed,
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
                let damage = present(&self.surface, pixels, &self.start.region, &self.damage)?;
                link.lock().send(&SaverMessage::Presented {
                    serial,
                    damage,
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

/// Write what `damage` covers of `surface` into the frame area, answering the
/// rectangles written; `None` when nothing inside the screen changed.
fn present(
    surface: &Surface,
    frame: &mut [u8],
    region: &SaverRegion,
    damage: &Region,
) -> Result<Option<DamageList>, Errno> {
    let mode = region.display_mode();
    let Some(list) = DamageList::fitted(
        damage
            .rects()
            .iter()
            .filter_map(|rect| damage_in(&mode, *rect)),
    ) else {
        return Ok(None);
    };
    for rect in list.rects() {
        encode(surface, frame, &mode, *rect, &tairix_parallel::Serial)?;
    }
    Ok(Some(list))
}
