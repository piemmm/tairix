//! The host's half of one saver's conversation (`lib/abi` `saver_ipc`), free
//! of I/O: whose turn it is, which frame is outstanding and when the next is
//! owed, who holds the region's feed area, and the deadlines a saver is held
//! to.
//!
//! A saver is untrusted, so every message is checked against the turn it
//! arrives in: one out of turn, one naming a frame or a slide not asked for,
//! or one asking for a feed it was not given is a fault, and a fault ends the
//! saver. Nothing here waits: the embedder sends what this decides and comes
//! back when [`Talk::due_ns`] says.

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::driver::display::DamageList;
use tairix_abi::font_ipc::FontRequest;
use tairix_abi::saver::SaverFeeds;
use tairix_abi::saver_ipc::{
    NextFrame, SaverMessage, Start, SAVER_FONT_CALLS_MAX, SAVER_FRAME_DEADLINE_NS, SAVER_KEEPS_MAX,
    SAVER_KEEP_SPACING_NS, SAVER_KEEP_TITLE_MAX, SAVER_START_DEADLINE_NS,
};
use tairix_abi::Errno;

/// What a saver said that the embedder acts on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Said {
    /// The saver is ready: its first frame may be asked for.
    Ready,
    /// The frame asked for is in the region, changed where `damage` says.
    Presented {
        /// What changed; `None` when nothing did.
        damage: Option<DamageList>,
    },
    /// Relay this request to the font service, and its reply back.
    FontCall([u8; FontRequest::WIRE_LEN]),
    /// The saver asked to keep a picture sooner, or more often, than one run
    /// may: it is answered as not kept.
    KeepRefused,
    /// The feed area holds a finished picture to keep.
    Keep {
        /// What the saver calls it.
        title: BoundedText<1, SAVER_KEEP_TITLE_MAX>,
        /// Its size, within the screen's.
        size: (u32, u32),
    },
    /// Read slide `index` into the feed area.
    WantSlide(u16),
    /// The saver cannot start, for this reason.
    Unstarted(Errno),
}

/// Why a saver's conversation ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// It spoke out of turn, named what it was not asked for, asked for what
    /// it was not given, or asked the font service more often than one start
    /// or one frame may.
    OutOfTurn,
    /// It did not say it was ready within the start deadline.
    NeverReady,
    /// It did not present a frame within the frame deadline.
    Late,
}

/// Where the conversation stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    /// `Start` sent; `Ready` owed by the deadline.
    Starting { deadline_ns: u64 },
    /// No frame outstanding; the next owed at `next_ns`, or only when a feed
    /// brings something.
    Idle { next_ns: Option<u64> },
    /// Frame `serial` outstanding, owed by the deadline.
    Drawing { serial: u32, deadline_ns: u64 },
}

/// Who holds the region's feed area.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FeedHolder {
    /// The saver: no slide or keep is outstanding.
    Saver,
    /// The host, reading slide `index` into it.
    Slide(u16),
    /// The host, keeping the picture in it.
    Keep,
}

/// The host's half of one saver's conversation.
#[derive(Debug)]
pub struct Talk {
    stage: Stage,
    serial: u32,
    feeds: SaverFeeds,
    slides: u16,
    screen: (u32, u32),
    /// The display's frame, the soonest one saver frame follows another.
    frame_ns: u64,
    /// When the last frame was asked for.
    last_frame_ns: Option<u64>,
    feed: FeedHolder,
    /// Since when a font call has been relayed: the saver waits on the host
    /// meanwhile, so no deadline runs against it.
    font: Option<u64>,
    /// The font calls made since the start or the last frame asked for.
    font_calls: u16,
    /// The pictures kept this run, and when the last was asked for.
    keeps: u16,
    last_keep_ns: Option<u64>,
    /// A feed message went to a saver drawing a frame: the frame after is
    /// owed at once.
    fed: bool,
}

impl Talk {
    /// The conversation begun by sending `start` at `now_ns`.
    #[must_use]
    pub fn begin(start: &Start, now_ns: u64) -> Self {
        Self {
            stage: Stage::Starting {
                deadline_ns: now_ns.saturating_add(SAVER_START_DEADLINE_NS),
            },
            serial: 0,
            feeds: start.feeds,
            slides: start.slides,
            screen: (start.region.width(), start.region.height()),
            frame_ns: start.refresh.frame_ns(),
            last_frame_ns: None,
            feed: FeedHolder::Saver,
            font: None,
            font_calls: 0,
            keeps: 0,
            last_keep_ns: None,
            fed: false,
        }
    }

    /// Take in what the saver said at `now_ns`.
    ///
    /// # Errors
    ///
    /// [`Fault::OutOfTurn`] for a message the conversation does not allow
    /// now; the saver is then to be ended.
    pub fn hear(&mut self, message: SaverMessage, now_ns: u64) -> Result<Said, Fault> {
        match message {
            SaverMessage::Ready => {
                let Stage::Starting { .. } = self.stage else {
                    return Err(Fault::OutOfTurn);
                };
                self.stage = Stage::Idle {
                    next_ns: Some(now_ns),
                };
                Ok(Said::Ready)
            }
            SaverMessage::Presented {
                serial,
                damage,
                next,
            } => {
                let Stage::Drawing { serial: asked, .. } = self.stage else {
                    return Err(Fault::OutOfTurn);
                };
                if serial != asked
                    || damage
                        .as_ref()
                        .is_some_and(|damage| !on_screen(damage, self.screen))
                {
                    return Err(Fault::OutOfTurn);
                }
                let next_ns = match next {
                    _ if core::mem::take(&mut self.fed) => Some(now_ns),
                    NextFrame::Soon => Some(now_ns),
                    NextFrame::At(at) => Some(at),
                    NextFrame::Idle => None,
                };
                self.stage = Stage::Idle { next_ns };
                Ok(Said::Presented { damage })
            }
            SaverMessage::FontCall(request) => {
                if self.font.is_some() || self.font_calls >= SAVER_FONT_CALLS_MAX {
                    return Err(Fault::OutOfTurn);
                }
                self.font_calls += 1;
                self.font = Some(now_ns);
                Ok(Said::FontCall(request))
            }
            SaverMessage::Keep {
                title,
                width,
                height,
            } => {
                if !self.feeds.keep()
                    || self.feed != FeedHolder::Saver
                    || width == 0
                    || height == 0
                    || width > self.screen.0
                    || height > self.screen.1
                {
                    return Err(Fault::OutOfTurn);
                }
                self.feed = FeedHolder::Keep;
                let soon = self
                    .last_keep_ns
                    .is_some_and(|last| now_ns < last.saturating_add(SAVER_KEEP_SPACING_NS));
                if soon || self.keeps >= SAVER_KEEPS_MAX {
                    return Ok(Said::KeepRefused);
                }
                self.keeps += 1;
                self.last_keep_ns = Some(now_ns);
                Ok(Said::Keep {
                    title,
                    size: (width, height),
                })
            }
            SaverMessage::WantSlide(index) => {
                if !self.feeds.slides() || self.feed != FeedHolder::Saver || index >= self.slides {
                    return Err(Fault::OutOfTurn);
                }
                self.feed = FeedHolder::Slide(index);
                Ok(Said::WantSlide(index))
            }
            SaverMessage::Unstarted(errno) => {
                let Stage::Starting { .. } = self.stage else {
                    return Err(Fault::OutOfTurn);
                };
                Ok(Said::Unstarted(errno))
            }
        }
    }

    /// The serial of a frame to ask for at `now_ns`, if one is owed: none is
    /// outstanding, the one asked last is a display frame behind, and the
    /// saver wants one by now.
    pub fn frame_due(&mut self, now_ns: u64) -> Option<u32> {
        let Stage::Idle {
            next_ns: Some(next_ns),
        } = self.stage
        else {
            return None;
        };
        if now_ns < self.paced(next_ns) {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.last_frame_ns = Some(now_ns);
        self.font_calls = 0;
        self.stage = Stage::Drawing {
            serial: self.serial,
            deadline_ns: now_ns.saturating_add(SAVER_FRAME_DEADLINE_NS),
        };
        // A call relayed since before this frame holds up only the frame's own
        // time from here on.
        if let Some(since) = self.font.as_mut() {
            *since = now_ns;
        }
        Some(self.serial)
    }

    /// `wanted_ns` held to a display frame after the last frame asked for, so
    /// a saver asking at once is drawn at the display's pace and no faster.
    fn paced(&self, wanted_ns: u64) -> u64 {
        self.last_frame_ns.map_or(wanted_ns, |last| {
            wanted_ns.max(last.saturating_add(self.frame_ns))
        })
    }

    /// When the embedder is next to come back: the next frame owed, or the
    /// deadline the saver is held to; `None` while it waits only on a feed or
    /// on the font call being relayed.
    #[must_use]
    pub fn due_ns(&self) -> Option<u64> {
        match self.stage {
            Stage::Starting { .. } | Stage::Drawing { .. } if self.font.is_some() => None,
            Stage::Starting { deadline_ns } | Stage::Drawing { deadline_ns, .. } => {
                Some(deadline_ns)
            }
            Stage::Idle { next_ns } => next_ns.map(|next| self.paced(next)),
        }
    }

    /// Whether the saver has missed the deadline it is held to at `now_ns`.
    ///
    /// # Errors
    ///
    /// [`Fault::NeverReady`] or [`Fault::Late`] for a deadline passed.
    pub fn check(&self, now_ns: u64) -> Result<(), Fault> {
        if self.font.is_some() {
            return Ok(());
        }
        match self.stage {
            Stage::Starting { deadline_ns } if now_ns >= deadline_ns => Err(Fault::NeverReady),
            Stage::Drawing { deadline_ns, .. } if now_ns >= deadline_ns => Err(Fault::Late),
            _ => Ok(()),
        }
    }

    /// A feed message was sent at `now_ns`: what it brought is owed a frame
    /// at once, or straight after the one being drawn.
    pub fn fed(&mut self, now_ns: u64) {
        match &mut self.stage {
            Stage::Idle { next_ns } => *next_ns = Some(now_ns),
            Stage::Drawing { .. } => self.fed = true,
            Stage::Starting { .. } => {}
        }
    }

    /// The font call being relayed was answered at `now_ns`: the deadline it
    /// held up moves on by the time the saver spent waiting on the host.
    /// Answers whether a call was being relayed, so the answer is the saver's
    /// to be told.
    pub fn font_answered(&mut self, now_ns: u64) -> bool {
        let Some(since) = self.font.take() else {
            return false;
        };
        let waited = now_ns.saturating_sub(since);
        if let Stage::Starting { deadline_ns } | Stage::Drawing { deadline_ns, .. } =
            &mut self.stage
        {
            *deadline_ns = deadline_ns.saturating_add(waited);
        }
        true
    }

    /// The feed area went back to the saver with the host's answer, which is
    /// itself a feed message.
    pub fn feed_answered(&mut self, now_ns: u64) {
        self.feed = FeedHolder::Saver;
        self.fed(now_ns);
    }

    /// Who holds the feed area.
    #[must_use]
    pub const fn feed(&self) -> FeedHolder {
        self.feed
    }

    /// The feeds the saver was given.
    #[must_use]
    pub const fn feeds(&self) -> SaverFeeds {
        self.feeds
    }

    /// Whether the saver has said it is ready.
    #[must_use]
    pub const fn ready(&self) -> bool {
        !matches!(self.stage, Stage::Starting { .. })
    }
}

/// Whether every rectangle of `damage` lies on a `screen`.
fn on_screen(damage: &DamageList, screen: (u32, u32)) -> bool {
    let bounds = damage.bounds();
    bounds
        .x
        .checked_add(bounds.width_px)
        .is_some_and(|right| right <= screen.0)
        && bounds
            .y
            .checked_add(bounds.height_px)
            .is_some_and(|bottom| bottom <= screen.1)
}

#[cfg(test)]
#[path = "talk_tests.rs"]
mod tests;
