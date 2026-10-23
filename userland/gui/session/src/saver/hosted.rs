//! One saver bundle the session hosts: its sandboxed session, the region it
//! presents through, and the conversation ([`Talk`]) that decides every turn.
//!
//! Nothing here blocks. The session's loop hands each readiness of the saver's
//! pipes to [`Hosted::on_readable`] and [`Hosted::on_writable`], asks
//! [`Hosted::advance`] for what is owed when [`Hosted::due_ns`] comes, and
//! answers what the saver asked of its workers as each lands. A saver that
//! breaks the conversation, misses a deadline, or ends its pipe is ended with
//! the reason, for the embedder to say and to blank the screen.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::driver::display::DamageRect;
use tairix_abi::font_ipc::FontRequest;
use tairix_abi::saver_ipc::{
    HostMessage, SaverDamage, SaverMessage, SaverRegion, SlideOutcome, Start, WallReading,
    SAVER_DAMAGE_RECTS, SAVER_HOST_MESSAGE_MAX, SAVER_MESSAGE_MAX,
};
use tairix_abi::switchboard_ipc::MachineReport;
use tairix_abi::sysinfo::MemoryBand;
use tairix_abi::{DriverError, Errno};
use tairix_display::{driver_error_from_errno, winframe, ChannelOrder, Presenter};
use tairix_geometry::{Rect, Region};
use tairix_log::Sink;
use tairix_parallel::JobRunner;
use tairix_raster::Surface;
use tairix_sandbox::proto::FRAME_HEADER_LEN;
use tairix_sandbox::{SandboxSession, SessionBounds, SessionDescriptors, SessionTransport};
use tairix_wallpaper::TaskNames;
use tairix_wm::{Compositor, WindowId};

use super::keep::Picture;
use super::talk::{Fault, FeedHolder, Said, Shown, Talk};

/// The memory a saver's region is held in.
pub trait SaverMemory {
    /// The region's bytes.
    fn bytes(&self) -> &[u8];

    /// The region's bytes, to write a feed into.
    fn bytes_mut(&mut self) -> &mut [u8];
}

/// What a hosted saver asked of the session's workers, which answer through
/// [`Hosted`]'s answering methods as each lands.
pub trait SaverErrands {
    /// Relay `request` to the font service.
    fn relay_font(&mut self, request: [u8; FontRequest::WIRE_LEN]);

    /// Keep `picture`, which the saver handed over.
    fn keep(&mut self, picture: Picture);

    /// Read slide `index` of those the saver was offered, fitted to the
    /// screen.
    fn want_slide(&mut self, index: u16);
}

/// Why a hosted saver was ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Ended {
    /// It could not be started.
    Refused(Errno),
    /// It said it cannot start, and why.
    Declined(Errno),
    /// It broke the conversation, missed a deadline, or asked for what it
    /// was not given.
    Faulted(Fault),
    /// It said what does not decode.
    Garbled,
    /// It closed its pipe, crashed, or its transport failed.
    Gone,
    /// The session could not watch its pipes, so could not hear it.
    Unwatched(Errno),
}

/// The queue bound each way. The conversation keeps at most one of each
/// message the host owes outstanding, and the readings it pushes are held
/// back while anything is queued, so two of the longest message hold every
/// message the host can have queued at once.
const OUTBOUND_BYTES: usize = 2 * (SAVER_HOST_MESSAGE_MAX + FRAME_HEADER_LEN);
const INBOUND_BYTES: usize = 8 * (SAVER_MESSAGE_MAX + FRAME_HEADER_LEN);

/// What the host holds back while what is queued for the saver drains: only
/// the latest of each is worth sending.
#[derive(Debug, Default)]
struct Held {
    /// The machine's latest reading.
    report: Option<MachineReport>,
    /// The readings stopped since any reading held.
    lost: bool,
    /// The memory pressure band.
    band: Option<MemoryBand>,
}

/// One saver bundle the session hosts.
pub struct Hosted<T: SessionTransport, S: Sink> {
    session: SandboxSession<T, S>,
    region: Box<dyn SaverMemory>,
    layout: SaverRegion,
    talk: Talk,
    /// Which tasks' names the machine feed may carry.
    names: TaskNames,
    /// The folder under the user's pictures what it keeps goes in.
    folder: alloc::string::String,
    /// One message's encoding, kept for every send.
    out: Vec<u8>,
    /// What to send once nothing is queued: a saver slow to read is told the
    /// latest, never a backlog.
    held: Held,
    /// What the frame waiting in the region for the screen changed.
    waiting: Option<SaverDamage>,
    /// What a frame laid changed, held to the band form within the protocol's
    /// rectangle count, so no saver's geometry makes the compositor's own
    /// unbounded damage fragment.
    laying: Region,
    /// How the saver's frames reach the screen.
    path: Path,
    /// The display refused a frame drawn in its memory: every frame from then
    /// on is laid on the window.
    refused: bool,
    /// A frame drawn in the display's memory reached the screen as it was
    /// presented, which a display that latches nothing says no other way.
    reached: bool,
    /// The display holds none of the saver's picture yet, so the next frame
    /// drawn into it is drawn whole.
    whole_owed: bool,
}

/// How a hosted saver's frames reach the screen.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Path {
    /// Laid on the saver window, which the compositor presents.
    Window,
    /// Drawn straight into the display's own frames, the compositor
    /// presenting nothing meanwhile: the window's picture is stale, and is
    /// owed the whole frame should it show one again.
    Display,
}

impl<T: SessionTransport, S: Sink> Hosted<T, S> {
    /// Host the saver at the far end of `transport`, which presents through
    /// `region`, starting it with `start` at `now_ns`.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a region smaller than `start`'s
    /// layout, [`Errno::OutOfMemory`] when the queues or the message buffer
    /// cannot be held, or the session's refusal of the start.
    pub fn begin(
        (transport, region, sink): (T, Box<dyn SaverMemory>, S),
        start: &Start,
        (names, folder): (TaskNames, &str),
        now_ns: u64,
    ) -> Result<Self, Errno> {
        if region.bytes().len() < start.region.len() {
            return Err(Errno::LengthOutOfRange);
        }
        let bounds =
            SessionBounds::new(OUTBOUND_BYTES, INBOUND_BYTES).map_err(|_| Errno::OutOfRange)?;
        let session =
            SandboxSession::new(transport, bounds, sink).map_err(|_| Errno::OutOfMemory)?;
        let mut out = Vec::new();
        out.try_reserve_exact(SAVER_HOST_MESSAGE_MAX)
            .map_err(|_| Errno::OutOfMemory)?;
        out.resize(SAVER_HOST_MESSAGE_MAX, 0);
        let mut hosted = Self {
            session,
            region,
            layout: start.region,
            talk: Talk::begin(start, now_ns),
            names,
            folder: alloc::string::String::from(folder),
            out,
            held: Held::default(),
            waiting: None,
            laying: Region::with_budget(SAVER_DAMAGE_RECTS),
            path: Path::Window,
            refused: false,
            reached: false,
            whole_owed: false,
        };
        hosted.send(&HostMessage::Start(start.clone()))?;
        Ok(hosted)
    }

    /// The saver's two descriptors, for the embedder's wait-set.
    #[must_use]
    pub fn descriptors(&self) -> Option<SessionDescriptors> {
        self.session.descriptors()
    }

    /// Whether the saver's pipe is to be read.
    #[must_use]
    pub fn wants_read(&self) -> bool {
        self.session.wants_read()
    }

    /// Whether what is queued for the saver is waiting for room.
    #[must_use]
    pub fn wants_write(&self) -> bool {
        self.session.wants_write()
    }

    /// When the embedder is next owed a call to [`advance`](Self::advance).
    #[must_use]
    pub fn due_ns(&self) -> Option<u64> {
        self.talk.due_ns()
    }

    /// The saver's pipe has room at `now_ns`: send what is queued, and what
    /// was held back once it has gone.
    ///
    /// # Errors
    ///
    /// [`Ended::Gone`] when the transport failed.
    pub fn on_writable(&mut self, now_ns: u64) -> Result<(), Ended> {
        self.session.on_writable().map_err(|_| Ended::Gone)?;
        self.release_held(now_ns)
    }

    /// The saver's pipe is readable at `now_ns`: take in everything it said,
    /// presenting its frames into the saver window `wm` and handing what it
    /// asked of the workers to `errands`.
    ///
    /// # Errors
    ///
    /// Why the saver is to be ended.
    pub fn on_readable(
        &mut self,
        now_ns: u64,
        (compositor, wm, mut display): (&mut Compositor, WindowId, Option<&mut dyn Presenter>),
        errands: &mut dyn SaverErrands,
    ) -> Result<(), Ended> {
        self.session.on_readable().map_err(|_| Ended::Gone)?;
        loop {
            let heard = self
                .session
                .recv(SaverMessage::decode)
                .map_err(|_| Ended::Gone)?;
            let Some(message) = heard else {
                break;
            };
            let message = message.map_err(|_| Ended::Garbled)?;
            match self.talk.hear(&message, now_ns).map_err(Ended::Faulted)? {
                Said::Ready => {}
                Said::Presented { damage } => {
                    self.show(damage, (compositor, wm), display.as_deref_mut());
                }
                Said::Held(damage) => self.waiting = Some(*damage),
                Said::FontCall(request) => errands.relay_font(request),
                Said::KeepRefused => self.kept(false, now_ns)?,
                Said::Keep { title, size } => {
                    match self.handed_over(title.as_str(), size, compositor.job_runner()) {
                        Some(picture) => errands.keep(picture),
                        None => self.kept(false, now_ns)?,
                    }
                }
                Said::WantSlide(index) => errands.want_slide(index),
                Said::Unstarted(errno) => return Err(Ended::Declined(errno)),
            }
        }
        if self.session.peer_finished() {
            return Err(Ended::Gone);
        }
        Ok(())
    }

    /// Carry the conversation on to `now_ns`: end a saver past its deadline,
    /// and ask for the frame owed, drawn for `wall`.
    ///
    /// # Errors
    ///
    /// Why the saver is to be ended.
    pub fn advance(&mut self, now_ns: u64, wall: &WallReading) -> Result<(), Ended> {
        self.talk.check(now_ns).map_err(Ended::Faulted)?;
        if let Some(serial) = self.talk.frame_due(now_ns) {
            self.send(&HostMessage::Frame {
                serial,
                now_ns,
                wall: *wall,
            })
            .map_err(|_| Ended::Gone)?;
        }
        Ok(())
    }

    /// The font service answered, at `now_ns`, the call relayed for the
    /// saver, if it still waits for one.
    ///
    /// # Errors
    ///
    /// [`Ended::Gone`] when the answer cannot be sent.
    pub fn font_answered(&mut self, reply: Result<&[u8], Errno>, now_ns: u64) -> Result<(), Ended> {
        if !self.talk.font_answered(now_ns) {
            return Ok(());
        }
        self.send(&HostMessage::FontReply(reply))
            .map_err(|_| Ended::Gone)
    }

    /// Slide `index` landed, fitted to the screen, or could not be read: lay
    /// it in the feed area and say so, if the saver still waits for it.
    ///
    /// # Errors
    ///
    /// [`Ended::Gone`] when the answer cannot be sent.
    pub fn slide_landed(
        &mut self,
        index: u16,
        slide: Option<&Surface>,
        now_ns: u64,
        runner: &dyn JobRunner,
    ) -> Result<(), Ended> {
        if self.talk.feed() != FeedHolder::Slide(index) {
            return Ok(());
        }
        let shown = slide.is_some_and(|slide| self.lay_slide(slide, runner));
        let outcome = if shown {
            SlideOutcome::Shown
        } else {
            SlideOutcome::Unavailable
        };
        self.talk.feed_answered(now_ns);
        self.send(&HostMessage::Slide { index, outcome })
            .map_err(|_| Ended::Gone)
    }

    /// The picture the saver handed over was kept, or was not.
    ///
    /// # Errors
    ///
    /// [`Ended::Gone`] when the answer cannot be sent.
    pub fn kept(&mut self, kept: bool, now_ns: u64) -> Result<(), Ended> {
        if self.talk.feed() != FeedHolder::Keep {
            return Ok(());
        }
        self.talk.feed_answered(now_ns);
        self.send(&HostMessage::Kept { kept })
            .map_err(|_| Ended::Gone)
    }

    /// A frame was presented to the display, carrying whatever was laid into
    /// the saver window `wm`: the frame held behind it is laid now. Answers
    /// whether a frame of the saver's reached the screen.
    pub fn shown(&mut self, (compositor, wm): (&mut Compositor, WindowId)) -> bool {
        match self.talk.shown() {
            Shown::Nothing => false,
            Shown::Laid => true,
            Shown::Next => {
                let waiting = self.waiting.take();
                self.lay(waiting.as_ref(), compositor, wm);
                true
            }
        }
    }

    /// The display latched the frame drawn in its memory: the frame held
    /// behind it, if one is, goes to the display now. Answers whether a frame
    /// of the saver's reached the screen.
    pub fn latched(
        &mut self,
        (compositor, wm): (&mut Compositor, WindowId),
        display: &mut dyn Presenter,
    ) -> bool {
        if self.path != Path::Display {
            return false;
        }
        match self.talk.shown() {
            Shown::Nothing => false,
            Shown::Laid => true,
            Shown::Next => {
                let waiting = self.waiting.take();
                self.show(waiting.as_ref(), (compositor, wm), Some(display));
                true
            }
        }
    }

    /// Whether the saver's frames go straight to the display, the compositor
    /// to present nothing meanwhile.
    #[must_use]
    pub fn scans_out(&self) -> bool {
        self.path == Path::Display
    }

    /// Show what `damage` names of the frame area: drawn straight into
    /// `display`'s own frame where the saver covers it and the display draws
    /// in place, else laid on the saver window.
    fn show(
        &mut self,
        damage: Option<&SaverDamage>,
        (compositor, wm): (&mut Compositor, WindowId),
        display: Option<&mut (dyn Presenter + '_)>,
    ) {
        if let Some(display) = display.filter(|_| !self.refused) {
            if self.draw_on(display, damage, compositor.job_runner()) {
                return;
            }
        }
        self.lay(damage, compositor, wm);
    }

    /// Draw what `damage` names straight into `display`'s frame, the whole
    /// frame where the display holds none of the saver's yet, answering
    /// whether the display took it — or will once it has latched the last.
    /// A display that refuses sends this and every later frame to the window.
    fn draw_on(
        &mut self,
        display: &mut dyn Presenter,
        damage: Option<&SaverDamage>,
        runner: &dyn JobRunner,
    ) -> bool {
        let output = display.mode();
        let size = (self.layout.width(), self.layout.height());
        let Some(order) = ChannelOrder::for_format(output.format) else {
            return false;
        };
        if (output.width_px, output.height_px) != size {
            return false;
        }
        if self.path == Path::Window {
            self.path = Path::Display;
            self.whole_owed = true;
        }
        if !display.ready() {
            self.waiting = damage.copied();
            self.talk.hold();
            return true;
        }
        self.laying.clear();
        if self.whole_owed {
            self.laying.add(Rect::new(0, 0, size.0, size.1));
        } else {
            for rect in damage.map_or(&[][..], SaverDamage::rects) {
                if let (Ok(x), Ok(y)) = (i32::try_from(rect.x), i32::try_from(rect.y)) {
                    self.laying
                        .add(Rect::new(x, y, rect.width_px, rect.height_px));
                }
            }
        }
        if self.laying.is_empty() {
            return true;
        }
        let mode = self.layout.display_mode();
        let frame = self
            .region
            .bytes()
            .get(..self.layout.frame_len())
            .unwrap_or(&[]);
        let drawn = display.present_drawn(&self.laying, &mut |memory, stride, refresh| {
            let stride = usize::try_from(stride).map_err(|_| DriverError::LengthOutOfRange)?;
            winframe::lay_opaque_into(frame, &mode, (memory, stride, order), refresh, runner)
                .map_err(driver_error_from_errno)
        });
        if drawn.is_err() {
            self.refused = true;
            return false;
        }
        self.whole_owed = false;
        self.talk.laid();
        if display.ready() {
            // A display that shows a frame as it is presented posts no latch.
            self.reached = !matches!(self.talk.shown(), Shown::Nothing);
        }
        true
    }

    /// Whether a frame drawn in the display's memory reached the screen as it
    /// was presented, since this was last asked.
    pub fn take_reached(&mut self) -> bool {
        core::mem::take(&mut self.reached)
    }

    /// Whether the saver was given the machine feed.
    #[must_use]
    pub fn wants_machine_reports(&self) -> bool {
        self.talk.feeds().machine() && self.talk.ready()
    }

    /// Hand the saver `report`, its task names withheld as the settings say.
    ///
    /// # Errors
    ///
    /// [`Ended::Gone`] when it cannot be sent.
    pub fn machine(&mut self, mut report: MachineReport, now_ns: u64) -> Result<(), Ended> {
        if !self.wants_machine_reports() {
            return Ok(());
        }
        let names = self.names;
        report.tasks.withhold_names(|owner| names.names(owner));
        self.held.report = Some(report);
        self.held.lost = false;
        self.release_held(now_ns)
    }

    /// Tell the saver the machine feed's source stopped.
    ///
    /// # Errors
    ///
    /// [`Ended::Gone`] when it cannot be sent.
    pub fn machine_lost(&mut self, now_ns: u64) -> Result<(), Ended> {
        if !self.wants_machine_reports() {
            return Ok(());
        }
        self.held.report = None;
        self.held.lost = true;
        self.release_held(now_ns)
    }

    /// Tell the saver the machine's memory pressure moved to `band`, at
    /// `now_ns`.
    ///
    /// # Errors
    ///
    /// [`Ended::Gone`] when it cannot be sent.
    pub fn pressure(&mut self, band: MemoryBand, now_ns: u64) -> Result<(), Ended> {
        self.held.band = Some(band);
        self.release_held(now_ns)
    }

    /// Send the held machine message and pressure band, once nothing is
    /// queued ahead of them.
    fn release_held(&mut self, now_ns: u64) -> Result<(), Ended> {
        if self.session.wants_write() {
            return Ok(());
        }
        if let Some(band) = self.held.band.take() {
            self.send(&HostMessage::Pressure(band))
                .map_err(|_| Ended::Gone)?;
        }
        let message = if core::mem::take(&mut self.held.lost) {
            HostMessage::MachineLost
        } else if let Some(report) = self.held.report.take() {
            HostMessage::Machine(report)
        } else {
            return Ok(());
        };
        self.talk.fed(now_ns);
        self.send(&message).map_err(|_| Ended::Gone)
    }

    /// End the saver: close its pipes and reap it.
    #[must_use]
    pub fn end(self) -> Option<i32> {
        self.session.end()
    }

    /// The region, as a saver draws in it.
    #[cfg(test)]
    pub(super) fn region_mut(&mut self) -> &mut [u8] {
        self.region.bytes_mut()
    }

    /// Queue `message` for the saver.
    fn send(&mut self, message: &HostMessage<'_>) -> Result<(), Errno> {
        let len = message.encode(&mut self.out)?;
        self.session
            .send(&self.out[..len])
            .map_err(|_| Errno::BrokenPipe)
    }

    /// Lay what `damage` names of the frame area onto the saver window `wm`,
    /// the whole frame where the window holds no picture of its own any more,
    /// and tell the conversation it waits for the screen. Every pixel laid is
    /// opaque, so no saver can show what it covers.
    fn lay(&mut self, damage: Option<&SaverDamage>, compositor: &mut Compositor, wm: WindowId) {
        let mode = self.layout.display_mode();
        let size = (self.layout.width(), self.layout.height());
        self.laying.clear();
        if compositor.keeps_content(wm, size) && self.path == Path::Window {
            for rect in damage.map_or(&[][..], SaverDamage::rects) {
                if let (Ok(x), Ok(y)) = (i32::try_from(rect.x), i32::try_from(rect.y)) {
                    self.laying
                        .add(Rect::new(x, y, rect.width_px, rect.height_px));
                }
            }
        } else {
            self.laying.add(Rect::new(0, 0, size.0, size.1));
        }
        if self.laying.is_empty() {
            return;
        }
        let frame = self
            .region
            .bytes()
            .get(..self.layout.frame_len())
            .unwrap_or(&[]);
        let (laying, runner) = (&self.laying, compositor.job_runner());
        let laid = compositor.present_window_content(wm, size.0, size.1, |content, changed| {
            winframe::lay_opaque_region(frame, content, &mode, laying, runner)?;
            changed.add_region(laying);
            Ok::<(), Errno>(())
        });
        if matches!(laid, Some(Ok(()))) {
            self.talk.laid();
            self.path = Path::Window;
        }
    }

    /// The picture of `size` the feed area holds, called `name`, decoded
    /// across `runner`; `None` when it cannot be read or held.
    fn handed_over(
        &self,
        name: &str,
        (width, height): (u32, u32),
        runner: &dyn JobRunner,
    ) -> Option<Picture> {
        let mode = self.layout.display_mode();
        let feed = self.region.bytes().get(self.layout.feed_range()?)?;
        let mut surface = Surface::new(width, height)?;
        let area = DamageRect {
            x: 0,
            y: 0,
            width_px: width,
            height_px: height,
        };
        winframe::lay_opaque(feed, &mut surface, &mode, area, runner).ok()?;
        Some(Picture {
            folder: self.folder.clone(),
            name: alloc::string::String::from(name),
            surface,
        })
    }

    /// Lay `slide`, the screen's size, in the feed area across `runner`;
    /// whether it fitted.
    fn lay_slide(&mut self, slide: &Surface, runner: &dyn JobRunner) -> bool {
        let mode = self.layout.display_mode();
        let Some(range) = self.layout.feed_range() else {
            return false;
        };
        let Some(feed) = self.region.bytes_mut().get_mut(range) else {
            return false;
        };
        winframe::encode(slide, feed, &mode, DamageRect::full(&mode), runner).is_ok()
    }
}

#[cfg(test)]
#[path = "hosted_tests.rs"]
mod tests;
