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
//! the same sandboxed decode. The desktop runs a preparer per CPU, and its
//! renders share a memory budget, so as many run at once as fit it — one alone
//! while memory is short; a client may have no more pending than it has
//! preparers across all its windows, which bounds how much decoding any one
//! application can set going, and the backdrop is always handed out first: the
//! picture the user is actually looking at never waits behind a thumbnail. A request is admitted before its
//! region is mapped, and the preparer draws straight into that region, so a
//! refusal costs no mapping and the serve loop copies no pixels.
//!
//! What a closed window still has waiting is withdrawn with it and the regions
//! it granted are let go. A render a preparer has already taken for it is
//! refused its memory, never queued again, and answers into nothing, freeing
//! its slot, so closing and reopening windows can neither queue decodes ahead
//! of another window's nor pin regions in the desktop: a closed window costs at
//! most the renders already under way.
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

use tairix_abi::window_ipc::{PreviewOutcome, PreviewSubject};
use tairix_abi::{Errno, ProcId};
use tairix_geometry::Rect;
use tairix_raster::Surface;
use tairix_reclaim::PressureBand;
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
    /// The bytes preparing this source holds beside its render: the picture
    /// drawn for the screen, and the surface made from it.
    #[must_use]
    pub fn surface_bytes(&self) -> u64 {
        2 * u64::from(self.width) * u64::from(self.height) * 4
    }

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

    /// The bytes of `target` its picture is drawn into: exactly its pixels,
    /// or `None` where the region holds fewer.
    pub fn canvas<'a>(&self, target: &'a mut dyn PreviewTarget) -> Option<&'a mut [u8]> {
        target.bytes_mut().get_mut(..self.pixel_bytes()?)
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

/// A concluded preview: the request it answers, and how it concluded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PreviewDone {
    /// What was asked for.
    pub request: PreviewRequest,
    /// Whether the picture was drawn, and if not, whether asking again could
    /// help.
    pub outcome: PreviewOutcome,
}

/// Conclude `job` as `outcome`, letting its region go before the conclusion is
/// told, so the client's own mapping is the only one left once it is.
#[must_use]
pub fn land_preview(job: PreviewJob, outcome: PreviewOutcome) -> PreviewDone {
    drop(job.target);
    PreviewDone {
        request: job.request,
        outcome,
    }
}

/// What previews render within: the bytes renders may hold together, what a
/// render holds before its worker has planned it, the preparers there are to
/// run them, and whether one may run whatever it costs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PreviewBudget {
    bytes: u64,
    preparation: u64,
    preparers: usize,
    lone: bool,
}

impl PreviewBudget {
    /// One render at a time and nothing beside it: a desk's budget until it is
    /// told the machine.
    pub const SERIAL: Self = Self {
        bytes: 0,
        preparation: u64::MAX,
        preparers: 1,
        lone: true,
    };

    /// The budget `preparers` threads render within on a machine of `total`
    /// bytes in `band`, a render holding `preparation` until it is planned.
    ///
    /// While memory is plentiful, renders share the machine's speculative
    /// budget; while it is not, one runs with nothing beside it; and while it
    /// is critical none starts, since no speculative work runs then.
    #[must_use]
    pub fn of_machine(total: u64, band: PressureBand, preparers: usize, preparation: u64) -> Self {
        Self {
            bytes: if band == PressureBand::Normal {
                tairix_reclaim::speculative_budget(total)
            } else {
                0
            },
            preparation,
            preparers: preparers.max(1),
            lone: band != PressureBand::Critical,
        }
    }

    /// Whether a render reserving `cost` — needing the machine to itself when
    /// `alone` — may start beside `running` renders reserving `reserved`.
    const fn admits(self, running: usize, reserved: u64, cost: u64, alone: bool) -> bool {
        if running >= self.preparers {
            return false;
        }
        if running == 0 {
            return self.lone || cost <= self.bytes;
        }
        !alone && reserved.saturating_add(cost) <= self.bytes
    }

    /// Whether no unplanned render fits the budget at all, so renders only
    /// ever run one at a time.
    const fn lean(self) -> bool {
        self.bytes < self.preparation
    }
}

/// What the desk decides for a preview its worker has planned.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Acquisition {
    /// It holds what it planned and goes on.
    Granted,
    /// It does not fit beside the renders under way: it is queued again,
    /// first, at that cost ([`WallpaperDesk::requeue_preview`]).
    Wait,
    /// It cannot run now — memory is critical, or the desk is stopping — and
    /// concludes unavailable.
    Unavailable,
}

/// A preview accepted and waiting for a preparer.
struct Queued {
    job: PreviewJob,
    /// Its last render ran out of memory beside others, so it runs next with
    /// the machine to itself.
    alone: bool,
    /// What its worker planned when it last started, which it holds when it
    /// starts again.
    cost: Option<u64>,
}

/// A preview a preparer has taken and not yet answered.
struct Rendering {
    request: PreviewRequest,
    /// The bytes it holds against the budget: its preparation until its
    /// worker planned it, then what that plan said.
    reserved: u64,
    /// Nothing starts beside it: it is retrying with the machine to itself,
    /// or its plan was granted past the budget.
    alone: bool,
    /// Its window has closed: it runs no further than it has, and its answer
    /// is dropped.
    forgotten: bool,
}

/// How a preparer's render of a preview ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PreviewRun {
    /// It concluded.
    Concluded(PreviewOutcome),
    /// Its plan, of the carried bytes, did not fit beside the renders under
    /// way: it waits in the queue for room.
    Deferred(u64),
    /// The desk withheld the memory its plan needs ([`Acquisition::Unavailable`]):
    /// memory is critical, the desk is stopping, or its window has closed.
    Withheld,
}

/// A slideshow picture a preparer has taken and not yet answered.
struct Slide {
    source: WallpaperSource,
    /// The bytes it holds against the budget.
    reserved: u64,
    /// The screensaver went down while it was prepared: its answer is
    /// dropped, but it holds its share until the preparer is done with it.
    abandoned: bool,
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
    /// The bytes the backdrop in preparation holds against the budget.
    preparing_reserved: u64,
    /// Previews accepted and not yet taken by a preparer, oldest first.
    previews: VecDeque<Queued>,
    /// Previews a preparer has taken and not yet answered.
    rendering: Vec<Rendering>,
    /// Concluded previews waiting for the serve loop, oldest first.
    rendered: VecDeque<PreviewDone>,
    /// What renders run within.
    budget: PreviewBudget,
    /// The slideshow picture asked for and not yet taken by a preparer.
    wanted_slide: Option<WallpaperSource>,
    /// The slideshow picture a preparer has taken and not yet answered.
    preparing_slide: Option<Slide>,
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
    /// at a time until told the machine it renders on.
    #[must_use]
    pub fn new() -> Self {
        Self {
            wanted: None,
            preparing: false,
            preparing_reserved: 0,
            done: None,
            previews: VecDeque::new(),
            rendering: Vec::new(),
            rendered: VecDeque::new(),
            budget: PreviewBudget::SERIAL,
            wanted_slide: None,
            preparing_slide: None,
            slide_done: None,
            stopping: false,
        }
    }

    /// Render within `budget` from now on, answering whether parked preparers
    /// must be woken: for a preview the new budget lets start, or to let their
    /// workers go now that renders run one at a time.
    ///
    /// Tightening it recalls nothing: renders already taken finish, and no
    /// more start until what they hold fits the new budget.
    pub fn set_budget(&mut self, budget: PreviewBudget) -> bool {
        let became_lean = budget.lean() && !self.budget.lean();
        self.budget = budget;
        became_lean || self.has_preview_work()
    }

    /// Whether renders run only one at a time, so a preparer with nothing to
    /// do lets its worker go rather than hold a whole process idle.
    #[must_use]
    pub const fn lean(&self) -> bool {
        self.budget.lean()
    }

    /// How many renders of any kind are under way.
    fn running(&self) -> usize {
        self.rendering.len()
            + usize::from(self.preparing)
            + usize::from(self.preparing_slide.is_some())
    }

    /// The bytes every render under way holds against the budget.
    fn reserved(&self) -> u64 {
        let slide = self
            .preparing_slide
            .as_ref()
            .map_or(0, |slide| slide.reserved);
        self.rendering.iter().map(|render| render.reserved).fold(
            self.preparing_reserved.saturating_add(slide),
            u64::saturating_add,
        )
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

    /// Whether the oldest waiting preview may start now. Nothing starts beside
    /// a render retrying with the machine to itself.
    fn has_preview_work(&self) -> bool {
        !self.rendering.iter().any(|render| render.alone)
            && self.previews.front().is_some_and(|queued| {
                self.budget.admits(
                    self.running(),
                    self.reserved(),
                    queued.cost.unwrap_or(self.budget.preparation),
                    queued.alone,
                )
            })
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
        // The desktop's own pictures start whatever the budget says, but hold
        // their share of it, so no preview starts beside them unless it fits.
        if let Some(source) = self.wanted.clone().filter(|_| !self.preparing) {
            self.preparing = true;
            self.preparing_reserved = self
                .budget
                .preparation
                .saturating_add(source.surface_bytes());
            return Some(WallpaperJob::Backdrop(source));
        }
        if self.has_slide_work() {
            let source = self.wanted_slide.take()?;
            self.preparing_slide = Some(Slide {
                source: source.clone(),
                reserved: self
                    .budget
                    .preparation
                    .saturating_add(source.surface_bytes()),
                abandoned: false,
            });
            return Some(WallpaperJob::Slide(source));
        }
        if !self.has_preview_work() {
            return None;
        }
        let Queued { job, alone, cost } = self.previews.pop_front()?;
        self.rendering.push(Rendering {
            request: job.request,
            reserved: cost.unwrap_or(self.budget.preparation),
            alone,
            forgotten: false,
        });
        Some(WallpaperJob::Preview(job))
    }

    /// The worker rendering `request` planned it to hold `planned` bytes.
    ///
    /// It holds that if it fits beside the renders under way, or if nothing
    /// else is under way — then running with nothing beside it, so one render
    /// always makes progress. Otherwise it waits for room, unless memory is
    /// critical. One whose window has closed is refused.
    pub fn acquire_preview(&mut self, request: &PreviewRequest, planned: u64) -> Acquisition {
        let running = self.running();
        let reserved = self.reserved();
        let budget = self.budget;
        let stopping = self.stopping;
        let Some(render) = self
            .rendering
            .iter_mut()
            .find(|render| render.request == *request)
        else {
            return Acquisition::Unavailable;
        };
        let others = reserved.saturating_sub(render.reserved);
        if stopping || render.forgotten {
            return Acquisition::Unavailable;
        }
        if others.saturating_add(planned) <= budget.bytes {
            render.reserved = planned;
            return Acquisition::Granted;
        }
        if !budget.lone {
            return Acquisition::Unavailable;
        }
        if running > 1 {
            return Acquisition::Wait;
        }
        render.reserved = planned;
        render.alone = true;
        Acquisition::Granted
    }

    /// Queue `job` again, first, to start once the `planned` bytes its worker
    /// asked for fit ([`Acquisition::Wait`]). Answers `job` back when it
    /// cannot be queued, to be concluded unavailable.
    pub fn requeue_preview(&mut self, job: PreviewJob, planned: u64) -> Option<PreviewJob> {
        self.requeue_first(job, planned, false)
    }

    /// The backdrop's preparation planned to hold `planned` bytes, its surface
    /// included, which it holds whatever the budget says, answering whether a
    /// waiting preview may now start beside it.
    pub fn plan_backdrop(&mut self, planned: u64) -> bool {
        if self.preparing {
            self.preparing_reserved = planned;
        }
        self.has_preview_work()
    }

    /// The preparation of slide `source` planned to hold `planned` bytes, its
    /// surface included, which it holds whatever the budget says, answering
    /// whether a waiting preview may now start beside it.
    pub fn plan_slide(&mut self, source: &WallpaperSource, planned: u64) -> bool {
        if let Some(slide) = self
            .preparing_slide
            .as_mut()
            .filter(|slide| slide.source == *source)
        {
            slide.reserved = planned;
        }
        self.has_preview_work()
    }

    /// Take `job`'s render off the list under way and queue it first again,
    /// to start holding `cost`, with nothing beside it when `alone`. Answers
    /// `job` back when the desk is not rendering it, its window has closed,
    /// the desk is stopping, or it cannot be queued.
    fn requeue_first(&mut self, job: PreviewJob, cost: u64, alone: bool) -> Option<PreviewJob> {
        let Some(at) = self
            .rendering
            .iter()
            .position(|render| render.request == job.request && !render.forgotten)
        else {
            return Some(job);
        };
        if self.stopping || self.previews.try_reserve(1).is_err() {
            return Some(job);
        }
        self.rendering.swap_remove(at);
        self.previews.push_front(Queued {
            job,
            alone,
            cost: Some(cost),
        });
        None
    }

    /// The render of `job` ran out of memory. Beside other renders, that is
    /// the company it kept rather than the picture, so it is queued first to
    /// run again with the machine to itself, and `None` is answered. Alone,
    /// or alone already, memory is genuinely short: `job` is answered back to
    /// be concluded as unavailable.
    pub fn retry_preview(&mut self, job: PreviewJob) -> Option<PreviewJob> {
        let Some(render) = self
            .rendering
            .iter()
            .find(|render| render.request == job.request)
        else {
            return Some(job);
        };
        if render.alone || self.running() <= 1 {
            return Some(job);
        }
        let cost = render.reserved;
        self.requeue_first(job, cost, true)
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
        let queued = self.previews.iter().map(|queued| &queued.job.request);
        let taken = self.rendering.iter().map(|render| &render.request);
        let answered = self.rendered.iter().map(|done| &done.request);
        let mut pending = 0usize;
        for held in queued.chain(taken).chain(answered) {
            if held.client != request.client {
                continue;
            }
            if held == request {
                return Err(Errno::AlreadyExists);
            }
            pending += 1;
        }
        if pending >= self.budget.preparers {
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
        self.previews.push_back(Queued {
            job,
            alone: false,
            cost: None,
        });
        Ok(())
    }

    /// Record the result of rendering a preview, answering whether the desk
    /// kept it (and so owes the serve loop a wake).
    ///
    /// An answer to a request the desk is not rendering is dropped, so a
    /// preparer answering twice cannot conclude a request nobody made, and so
    /// is one for a window that has closed, its slot freed.
    pub fn deliver_preview(&mut self, done: PreviewDone) -> bool {
        let Some(at) = self
            .rendering
            .iter()
            .position(|render| render.request == done.request)
        else {
            return false;
        };
        if self.rendering.swap_remove(at).forgotten {
            return false;
        }
        self.rendered.push_back(done);
        true
    }

    /// Where `job`'s render, ended as `run`, goes next: the outcome it
    /// concludes with and the job to land it with, or `None` where the desk
    /// queued it to run again.
    ///
    /// A render short of memory beside others runs again alone, one that did
    /// not fit beside them waits for room, and one the desk withheld memory
    /// from concludes unavailable at once.
    pub fn after_preview(
        &mut self,
        job: PreviewJob,
        run: PreviewRun,
    ) -> (PreviewOutcome, Option<PreviewJob>) {
        match run {
            PreviewRun::Concluded(PreviewOutcome::Unavailable) => {
                (PreviewOutcome::Unavailable, self.retry_preview(job))
            }
            PreviewRun::Concluded(outcome) => (outcome, Some(job)),
            PreviewRun::Deferred(planned) => (
                PreviewOutcome::Unavailable,
                self.requeue_preview(job, planned),
            ),
            PreviewRun::Withheld => (PreviewOutcome::Unavailable, Some(job)),
        }
    }

    /// Take the oldest rendered preview waiting to be handed over, if any.
    pub fn take_preview(&mut self) -> Option<PreviewDone> {
        self.rendered.pop_front()
    }

    /// `window_id` has closed: withdraw the previews it has waiting and those
    /// rendered for it but not yet handed over, answering the withdrawn jobs
    /// so the caller lets their regions go outside whatever guards the desk.
    ///
    /// Its renders a preparer has already taken are refused their memory,
    /// never queued again, and finish into nothing, freeing their slots.
    #[must_use]
    pub fn forget_window(&mut self, window_id: u64) -> Vec<PreviewJob> {
        let (withdrawn, kept): (Vec<_>, Vec<_>) = core::mem::take(&mut self.previews)
            .into_iter()
            .partition(|queued| queued.job.request.window_id == window_id);
        self.previews = kept.into();
        self.rendered
            .retain(|done| done.request.window_id != window_id);
        for render in &mut self.rendering {
            render.forgotten |= render.request.window_id == window_id;
        }
        withdrawn.into_iter().map(|queued| queued.job).collect()
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
        let Some(slide) = self
            .preparing_slide
            .take_if(|slide| slide.source == *source)
        else {
            return false;
        };
        if slide.abandoned {
            return false;
        }
        self.slide_done = Some(outcome);
        true
    }

    /// Take the prepared slideshow picture, if one is waiting.
    pub fn take_slide(&mut self) -> Option<Result<Surface, String>> {
        self.slide_done.take()
    }

    /// Forget every slide wanted, in preparation, or prepared: the
    /// screensaver has gone. One in preparation holds its share of the budget
    /// until its preparer is done, and its answer is dropped.
    pub fn forget_slides(&mut self) {
        self.wanted_slide = None;
        if let Some(slide) = self.preparing_slide.as_mut() {
            slide.abandoned = true;
        }
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
        self.preparing_reserved = 0;
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
