//! Preparing the desktop's wallpaper off the session's event loop
//! (`plans/FIX-DESKTOP.md` DESK-4).
//!
//! Painting the backdrop means reading a user-chosen image file — up to
//! `tairix_wallpaper::MAX_WALLPAPER_BYTES` of untrusted bytes — and decoding and
//! fitting it in a capability-empty sandbox worker. Run on the session's own
//! task that is a visible stall at login and again on every settings change: a
//! file read from a slow disk, then a round trip through another process.
//!
//! [`WallpaperDesk`] is the arrangement's whole policy, and it holds no lock, no
//! thread, and no syscall: what the desktop wants painted, what has come back,
//! and the staleness rule that discards a picture prepared for a screen or a
//! choice the desktop has since moved on from. The `Run` binary wraps it in the
//! runtime's futex mutex and parks a worker on a condition variable over it.
//!
//! # Their own sandbox workers, deliberately
//!
//! The icon rasteriser's sandbox worker stays where it is, driven from the
//! session's own task through the handle it has always used. Each wallpaper
//! preparer thread owns another, created inside the thread, so no sandbox
//! handle ever has to cross a thread boundary and the icon path is not changed
//! by any of this. A sandbox is spawned only when its preparer first has work,
//! so an idle preparer costs a parked thread, not a process.
//!
//! # The choosers' previews share the same preparers
//!
//! The Settings application browses the shipped pictures through the desktop
//! rather than reading them itself — the wallpapers, and each screensaver's
//! preview — so a chooser's picture is prepared here too: the same read and
//! the same sandboxed decode. The desktop runs a preparer per CPU and renders
//! as many previews at once as it has preparers, fewer while memory is short;
//! a client may have no more than that pending across all its windows, which
//! bounds how much decoding any one application can set going, and the
//! backdrop is always handed out first: the picture the user is actually
//! looking at never waits behind a thumbnail. A request is admitted before its
//! region is mapped, and the preparer draws straight into that region, so a
//! refusal costs no mapping and the serve loop copies no pixels.
//!
//! A render a preparer has taken is never recalled: it finishes, answers
//! exactly once, and frees its slot. What a closed window still has waiting is
//! withdrawn with it and the regions it granted are let go, so closing and
//! reopening windows can neither queue decodes ahead of another window's nor
//! pin regions in the desktop: a closed window costs at most the renders
//! already under way.
//!
//! # A wallpaper is never load-bearing
//!
//! Every refusal — an unreadable file, one larger than any wallpaper, a
//! malformed image, a crashed worker, a reply that does not fill the screen —
//! answers "no surface", and the desktop paints its backdrop colour. The reason
//! travels *with* the answer rather than being written where it was noticed:
//! `stderr` is one descriptor and a formatted line reaches it in several writes,
//! so two threads stating something at once would interleave into an unreadable
//! diagnosis. The session states it, once, on its own thread. The desktop never
//! fails over a picture.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::PreviewSubject;
use tairix_abi::{Errno, ProcId};
use tairix_geometry::Rect;
use tairix_raster::Surface;
use tairix_wallpaper::{DesktopSettings, WallpaperChoice, WallpaperFit};
use tairix_window::PreviewSize;

/// Everything a prepared wallpaper depends on: the chosen file, how it is
/// placed, and the screen it was placed on.
///
/// Preparing one reads a file and runs a sandboxed decode, so it happens only
/// when one of these really changed — never on a frame path. Comparing the whole
/// value is what makes that decision exact, and what lets a picture prepared for
/// a screen size the session has since left be discarded rather than stretched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperSource {
    /// The user's choice: a colour-only backdrop, or an image file.
    pub choice: WallpaperChoice,
    /// How the image is placed on the screen.
    pub fit: WallpaperFit,
    /// Screen width in physical pixels.
    pub width: u32,
    /// Screen height in physical pixels.
    pub height: u32,
}

impl WallpaperSource {
    /// What `settings` ask for on a `screen`-sized display.
    ///
    /// The one place the desktop's settings and its output become a wallpaper
    /// request, so the comparison that decides whether to prepare and the
    /// request that is prepared cannot disagree.
    #[must_use]
    pub fn wanted(settings: &DesktopSettings, screen: Rect) -> Self {
        Self {
            choice: settings.wallpaper.clone(),
            fit: settings.fit,
            width: screen.width,
            height: screen.height,
        }
    }

    /// The image file this source names, or `None` for a colour-only backdrop
    /// that needs no preparation at all.
    #[must_use]
    pub fn image_path(&self) -> Option<&str> {
        match &self.choice {
            WallpaperChoice::None => None,
            WallpaperChoice::Image(path) => Some(path.as_str()),
        }
    }
}

/// One chooser preview the desktop has been asked to render: which picture,
/// at what size, for which window of which client.
///
/// The picture is named by a closed [`PreviewSubject`] rather than by a path,
/// because the asking application named it that way: it browses a store it
/// cannot read, so it can only point at what the desktop offers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PreviewRequest {
    /// The asking window, which the conclusion is delivered to.
    pub window_id: u64,
    /// The client the kernel attests owns the window: what a render's share
    /// of the preparers is counted against, however many windows it opens.
    pub client: ProcId,
    /// The picture, and the size to render it at.
    pub size: PreviewSize,
}

impl PreviewRequest {
    /// How many bytes the rendered straight-alpha RGBA8 picture is, or `None`
    /// when that does not fit this target's address width.
    #[must_use]
    pub fn pixel_bytes(&self) -> Option<usize> {
        usize::from(self.size.width)
            .checked_mul(usize::from(self.size.height))?
            .checked_mul(4)
    }
}

/// Where a rendered preview's pixels go: the asking client's own region,
/// mapped for the one render and let go with it.
pub trait PreviewTarget: Send {
    /// The region's bytes.
    fn bytes_mut(&mut self) -> &mut [u8];
}

/// A preview the worker has taken: the request, the file to read for it —
/// resolved against what the desktop holds before it left the serve loop —
/// with the most bytes that kind of picture may be, and where it is drawn.
pub struct PreviewJob {
    /// What was asked for.
    pub request: PreviewRequest,
    /// The absolute path of the shipped picture to read.
    pub path: String,
    /// The largest file this picture's kind may be.
    pub bound: usize,
    /// The client's region the preparer draws the picture into, so no copy
    /// of it is made on the serve loop.
    pub target: Box<dyn PreviewTarget>,
}

impl core::fmt::Debug for PreviewJob {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PreviewJob")
            .field("request", &self.request)
            .field("path", &self.path)
            .field("bound", &self.bound)
            .finish_non_exhaustive()
    }
}

/// A rendered preview: the request it answers, and whether its picture is in
/// the client's region — `false` for a refusal the asking window is told
/// about rather than left waiting on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PreviewDone {
    /// What was asked for.
    pub request: PreviewRequest,
    /// Whether the picture was drawn.
    pub rendered: bool,
}

/// Draw `pixels` — the rendered picture, or `None` for a refusal — into
/// `target` when they are exactly the picture `request` asked for and fit
/// the region, answering whether they were drawn.
fn draw_preview(
    target: &mut dyn PreviewTarget,
    request: &PreviewRequest,
    pixels: Option<&[u8]>,
) -> bool {
    let Some(pixels) = pixels.filter(|pixels| Some(pixels.len()) == request.pixel_bytes()) else {
        return false;
    };
    target
        .bytes_mut()
        .get_mut(..pixels.len())
        .is_some_and(|slot| {
            slot.copy_from_slice(pixels);
            true
        })
}

/// Finish `job` with `pixels`: drawn into its client's region, the region let
/// go before the conclusion is told, so the client's own mapping is the only
/// one left once it is.
#[must_use]
pub fn land_preview(mut job: PreviewJob, pixels: Option<&[u8]>) -> PreviewDone {
    let rendered = draw_preview(&mut *job.target, &job.request, pixels);
    drop(job.target);
    PreviewDone {
        request: job.request,
        rendered,
    }
}

/// One unit of work a wallpaper preparer takes.
pub enum WallpaperJob {
    /// The desktop's own backdrop, which is always taken first.
    Backdrop(WallpaperSource),
    /// One picture of the screensaver's slideshow.
    Slide(WallpaperSource),
    /// One gallery tile for a browsing application.
    Preview(PreviewJob),
}

/// What the desk has for a wallpaper request right now.
pub enum Prepared {
    /// The preparation finished.
    Ready {
        /// The surface to paint the desktop layer over, or `None` for "paint the
        /// backdrop colour" — the answer both for a colour-only choice and for
        /// every refusal, since a wallpaper is never load-bearing.
        surface: Option<Surface>,
        /// Why there is no surface, for the session to state once on its own
        /// thread. `None` when nothing went wrong.
        refusal: Option<String>,
    },
    /// The preparation is under way somewhere else. The desktop keeps whatever
    /// it is painting until the answer arrives.
    Pending,
}

/// The wallpaper arrangement's policy: what is wanted, what has been prepared,
/// and whether a preparer is already working on it.
///
/// Deliberately free of locks, threads, and syscalls, so every rule below is a
/// host test rather than an argument.
pub struct WallpaperDesk {
    /// What the desktop wants painted, cleared when its answer is stored.
    wanted: Option<WallpaperSource>,
    /// Whether a preparer has taken [`WallpaperDesk::wanted`] and not yet
    /// answered it, so the same picture is never prepared twice at once.
    preparing: bool,
    /// The prepared surface (or the reason there is none), kept until the
    /// desktop asks for that same source.
    done: Option<(WallpaperSource, Result<Surface, String>)>,
    /// Previews accepted and not yet taken by a preparer, oldest first.
    previews: VecDeque<PreviewJob>,
    /// Previews a preparer has taken and not yet answered.
    rendering: Vec<PreviewRequest>,
    /// Rendered previews waiting for the serve loop, oldest first.
    rendered: VecDeque<PreviewDone>,
    /// How many previews render at once, which is also how many one client
    /// may have pending. Never zero.
    preview_slots: usize,
    /// Memory is short, so a preparer with nothing to do lets its sandbox
    /// worker go rather than hold a whole process idle.
    lean: bool,
    /// The slideshow picture asked for and not yet taken by a preparer.
    wanted_slide: Option<WallpaperSource>,
    /// The slideshow picture a preparer has taken and not yet answered.
    preparing_slide: Option<WallpaperSource>,
    /// The prepared slideshow picture waiting for the serve loop.
    slide_done: Option<Result<Surface, String>>,
    /// Set once the embedder is tearing down, so a parked preparer leaves.
    stopping: bool,
}

impl Default for WallpaperDesk {
    fn default() -> Self {
        Self::new()
    }
}

impl WallpaperDesk {
    /// A desk with nothing wanted and nothing prepared, rendering one preview
    /// at a time until told how many preparers it has.
    #[must_use]
    pub fn new() -> Self {
        Self {
            wanted: None,
            preparing: false,
            done: None,
            previews: VecDeque::new(),
            rendering: Vec::new(),
            rendered: VecDeque::new(),
            preview_slots: 1,
            lean: false,
            wanted_slide: None,
            preparing_slide: None,
            slide_done: None,
            stopping: false,
        }
    }

    /// Set how many previews may render at once, and so how many one client
    /// may have pending: the preparers the embedder runs, or fewer while
    /// memory is short. At least one.
    ///
    /// Lowering it recalls nothing: renders already taken finish, and no more
    /// are handed out until they drop below the new bound. Answers whether
    /// raising it made a waiting preview takeable, so the embedder wakes the
    /// preparers the new slots are for rather than leaving them parked.
    pub fn set_preview_slots(&mut self, slots: usize) -> bool {
        let raised = slots.max(1) > self.preview_slots;
        self.preview_slots = slots.max(1);
        raised && self.has_preview_work()
    }

    /// Say whether memory is short, answering whether it just became so:
    /// parked preparers must then be woken to let their workers go.
    pub fn set_lean(&mut self, lean: bool) -> bool {
        let became = lean && !self.lean;
        self.lean = lean;
        became
    }

    /// Whether a preparer with nothing to do should let its worker go.
    #[must_use]
    pub const fn lean(&self) -> bool {
        self.lean
    }

    /// Answer the desktop's request for `source`, recording it if this desk does
    /// not already hold the answer.
    ///
    /// [`Prepared::Ready`] once — the desktop installs the surface, so a later
    /// ask for the same source means it genuinely wants it prepared again. A
    /// surface prepared for a *different* source is dropped as stale rather than
    /// stretched onto the wrong screen.
    ///
    /// A colour-only choice needs nothing prepared and is answered at once, so
    /// the common case never reaches a worker at all.
    pub fn take(&mut self, source: &WallpaperSource) -> Prepared {
        if source.image_path().is_none() {
            self.wanted = None;
            self.done = None;
            return Prepared::Ready {
                surface: None,
                refusal: None,
            };
        }
        match self.done.take() {
            Some((prepared, outcome)) if prepared == *source => {
                return match outcome {
                    Ok(surface) => Prepared::Ready {
                        surface: Some(surface),
                        refusal: None,
                    },
                    Err(refusal) => Prepared::Ready {
                        surface: None,
                        refusal: Some(refusal),
                    },
                };
            }
            Some(_) | None => {}
        }
        if self.wanted.as_ref() != Some(source) {
            self.wanted = Some(source.clone());
        }
        Prepared::Pending
    }

    /// Whether a wallpaper is wanted that no preparer has taken.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.stopping
            && ((self.wanted.is_some() && !self.preparing)
                || self.has_slide_work()
                || self.has_preview_work())
    }

    /// Whether a slideshow picture is wanted that no preparer has taken.
    const fn has_slide_work(&self) -> bool {
        self.wanted_slide.is_some() && self.preparing_slide.is_none()
    }

    /// Whether a preview is waiting and a render slot is free for it.
    fn has_preview_work(&self) -> bool {
        !self.previews.is_empty() && self.rendering.len() < self.preview_slots
    }

    /// Take the next thing to prepare, or `None` when there is nothing to
    /// do.
    ///
    /// The desktop's own backdrop is always taken first: it is the picture
    /// the user is looking at, and a gallery of thumbnails must never make
    /// it wait.
    pub fn next_job(&mut self) -> Option<WallpaperJob> {
        if self.stopping {
            return None;
        }
        if self.wanted.is_some() && !self.preparing {
            self.preparing = true;
            return self.wanted.clone().map(WallpaperJob::Backdrop);
        }
        if self.has_slide_work() {
            let source = self.wanted_slide.take()?;
            self.preparing_slide = Some(source.clone());
            return Some(WallpaperJob::Slide(source));
        }
        if !self.has_preview_work() {
            return None;
        }
        let job = self.previews.pop_front()?;
        self.rendering.push(job.request);
        Some(WallpaperJob::Preview(job))
    }

    /// Whether a preview of `request` would be accepted now: asked before its
    /// region is mapped, so a refusal costs the serve loop no mapping.
    ///
    /// A client may have no more pending — accepted and not yet handed over,
    /// across all its windows — than render at once, so no client can hold
    /// another's picture back by more than those: the bound that also caps
    /// how much sandboxed decoding one application can set going.
    ///
    /// # Errors
    ///
    /// * [`Errno::AlreadyExists`] — the window already has this picture
    ///   pending at this size.
    /// * [`Errno::LimitExceeded`] — the client already has as many previews
    ///   pending as render at once; it asks again once one is answered.
    /// * [`Errno::Busy`] — the desk is stopping.
    pub fn admits(&self, request: &PreviewRequest) -> Result<(), Errno> {
        if self.stopping {
            return Err(Errno::Busy);
        }
        let queued = self.previews.iter().map(|queued| &queued.request);
        let answered = self.rendered.iter().map(|done| &done.request);
        let mut pending = 0usize;
        for held in queued.chain(self.rendering.iter()).chain(answered) {
            if held.client != request.client {
                continue;
            }
            if held == request {
                return Err(Errno::AlreadyExists);
            }
            pending += 1;
        }
        if pending >= self.preview_slots {
            return Err(Errno::LimitExceeded);
        }
        Ok(())
    }

    /// Record a wanted preview behind those already waiting; they are
    /// rendered in the order they were accepted.
    ///
    /// # Errors
    ///
    /// [`admits`](Self::admits)'s refusals, and [`Errno::OutOfMemory`] when
    /// the queue cannot grow.
    pub fn want_preview(&mut self, job: PreviewJob) -> Result<(), Errno> {
        self.admits(&job.request)?;
        self.previews
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        self.previews.push_back(job);
        Ok(())
    }

    /// Record the result of rendering a preview, answering whether the desk
    /// kept it (and so owes the serve loop a wake).
    ///
    /// An answer to a request the desk is not rendering is dropped, so a
    /// preparer answering twice cannot conclude a request nobody made.
    pub fn deliver_preview(&mut self, done: PreviewDone) -> bool {
        let Some(at) = self
            .rendering
            .iter()
            .position(|request| *request == done.request)
        else {
            return false;
        };
        self.rendering.swap_remove(at);
        self.rendered.push_back(done);
        true
    }

    /// Take the oldest rendered preview waiting to be handed over, if any.
    pub fn take_preview(&mut self) -> Option<PreviewDone> {
        self.rendered.pop_front()
    }

    /// `window_id` has closed: withdraw the previews it has waiting and those
    /// rendered for it but not yet handed over, answering the withdrawn jobs
    /// so the caller lets their regions go outside whatever guards the desk.
    ///
    /// Its renders a preparer has already taken finish into nothing and free
    /// their slots.
    #[must_use]
    pub fn forget_window(&mut self, window_id: u64) -> Vec<PreviewJob> {
        let (withdrawn, kept): (Vec<_>, Vec<_>) = core::mem::take(&mut self.previews)
            .into_iter()
            .partition(|job| job.request.window_id == window_id);
        self.previews = kept.into();
        self.rendered
            .retain(|done| done.request.window_id != window_id);
        withdrawn
    }

    /// Record a wanted slideshow picture, replacing one not yet taken:
    /// only the newest slide is worth showing.
    pub fn want_slide(&mut self, source: WallpaperSource) {
        if !self.stopping {
            self.wanted_slide = Some(source);
        }
    }

    /// Record the result of preparing slide `source`, answering whether the
    /// desk kept it (and so owes the serve loop a wake).
    ///
    /// An answer for a slide no longer being prepared is dropped: the
    /// screensaver went down, or moved on to a newer picture.
    pub fn deliver_slide(
        &mut self,
        source: &WallpaperSource,
        outcome: Result<Surface, String>,
    ) -> bool {
        if self.preparing_slide.as_ref() != Some(source) {
            return false;
        }
        self.preparing_slide = None;
        self.slide_done = Some(outcome);
        true
    }

    /// Take the prepared slideshow picture, if one is waiting.
    pub fn take_slide(&mut self) -> Option<Result<Surface, String>> {
        self.slide_done.take()
    }

    /// Forget every slide wanted, in preparation, or prepared: the
    /// screensaver has gone.
    pub fn forget_slides(&mut self) {
        self.wanted_slide = None;
        self.preparing_slide = None;
        self.slide_done = None;
    }

    /// Record the result of preparing `source`.
    ///
    /// Answers `false` — and keeps nothing — when the desktop has since asked
    /// for something else, so an abandoned preparation owes the session no wake
    /// and its pixels are dropped rather than painted.
    ///
    /// An accepted answer **clears the request it answers**. Leaving it standing
    /// made the desk workable again the instant it was answered, so a preparer
    /// handed itself the same picture forever — a decode loop, and this one
    /// reads and rasterises a whole screen's worth each time round.
    pub fn deliver(&mut self, source: WallpaperSource, outcome: Result<Surface, String>) -> bool {
        self.preparing = false;
        if self.wanted.as_ref() != Some(&source) {
            return false;
        }
        self.wanted = None;
        self.done = Some((source, outcome));
        true
    }

    /// Stop handing out work, so a parked preparer leaves its loop.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// Whether the embedder has asked preparers to leave.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }
}

/// The desktop's shipped-picture service as the window channel reaches it:
/// the wallpaper catalog a browsing application may list, and the previews
/// it may ask for.
///
/// A seam because the two answers need the session's own filesystem reach,
/// its parser sandbox, and its shared-memory mapping — none of which the
/// host-testable bridge has — while the rules around them (who owns the
/// catalog, how many render at once, a closed window owes nothing) are policy
/// worth testing without any of it.
pub trait WallpaperService {
    /// The flat catalog of shipped wallpapers this desktop offers, in
    /// catalog order. Empty for a desktop whose store could not be listed.
    fn catalog(&self) -> &[tairix_window::WallpaperName];

    /// Render `request` into `region`, concluding to `window_id`.
    ///
    /// Accepting is all this does: the read and the decode happen off the
    /// compositing loop, and the conclusion is delivered later.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such catalog entry, or the region is not one
    ///   its client granted.
    /// * [`Errno::LengthOutOfRange`] — the region is too small for the size
    ///   asked for.
    /// * [`Errno::AlreadyExists`] — the window already has this picture
    ///   pending at this size.
    /// * [`Errno::LimitExceeded`] — the window already has as many renders
    ///   pending as the desktop runs at once; the caller asks again once one
    ///   is answered.
    fn render(
        &mut self,
        window_id: u64,
        region: tairix_window::ClientRegion,
        request: PreviewSize,
    ) -> Result<(), Errno>;
}

/// The file a preview of `subject` is read from and the most bytes it may be,
/// resolved against `catalog`: `None` for a catalog position the desktop does
/// not hold.
///
/// The one place a subject becomes a path, so the window channel can only
/// ever make the session read a picture it ships itself.
#[must_use]
pub fn preview_source(
    subject: PreviewSubject,
    catalog: &[tairix_window::WallpaperName],
) -> Option<(String, usize)> {
    match subject {
        PreviewSubject::Wallpaper(index) => catalog.get(usize::from(index)).map(|name| {
            (
                tairix_wallpaper::wallpaper_path(&name.category, &name.file),
                tairix_wallpaper::MAX_WALLPAPER_BYTES,
            )
        }),
        PreviewSubject::Screensaver(kind) => Some((
            tairix_wallpaper::preview_path(kind),
            tairix_wallpaper::MAX_SCREENSAVER_PREVIEW_BYTES,
        )),
    }
}

#[cfg(test)]
#[path = "wallpaper_tests.rs"]
mod tests;
