//! The window's share of the preview worker: the ticket every job it asks
//! carries, the one job it has out, which of what it wants is asked next, and
//! where each answer lands.

use tairix_geometry::{Region, Scale};
use tairix_reclaim::PressureBand;

use super::layers::Looked;
use super::operations::Basis;
use super::{Outcome, Own, Request, View};
use crate::op::Halted;
use crate::preview::{PreviewAnswer, Previewed, Ticket};

/// What a job asked of the preview worker works out, and what its answer is
/// landed against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Asking {
    /// The open operation's overlay's tiles.
    Tiles,
    /// The histogram of the layer as it stood at the basis.
    Histogram(Basis),
    /// The composite beneath the layer whose opacity is changing, laid from
    /// the entry, layer and generation named.
    Under((usize, usize, u64)),
    /// The Layers pane's thumbnails, asked of the state looked at.
    Thumbnails(Looked),
}

/// The one job the window has out, by its count, and what it works out.
#[derive(Debug)]
struct Asked {
    job: u64,
    asking: Asking,
}

/// What holds asking back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Hold {
    /// The preview worker had no room for a job: the next input asks again.
    Refused,
    /// A job ran short of memory, or could not be worked out, at this
    /// generation of the document and count of the ticket: asked again once
    /// either moves, the memory pressure falls or the window is laid out
    /// anew, and never for an input that moves none of them.
    Short { generation: u64, ticket: u64 },
}

/// The window's share of the preview worker.
#[derive(Debug)]
pub(crate) struct Previews {
    /// The count every job the window asks carries: bumped, it abandons them.
    ticket: Ticket,
    /// The newest job asked and not yet answered: the only one whose answer
    /// is taken.
    asked: Option<Asked>,
    /// What holds asking back, if anything does.
    held_back: Option<Hold>,
    /// The memory-pressure band last told, so a fall is seen.
    band: PressureBand,
}

impl Default for Previews {
    fn default() -> Self {
        Self {
            ticket: Ticket::new(),
            asked: None,
            held_back: None,
            band: PressureBand::Normal,
        }
    }
}

impl Drop for Previews {
    /// A window gone abandons what it asked: nothing is left to show it.
    fn drop(&mut self) {
        self.ticket.bump();
    }
}

impl View {
    /// What the window wants of the preview worker now, as a job for it: the
    /// open operation's overlay's tiles, those in view first, then a histogram
    /// a pane reads, then the composite beneath a layer whose opacity is
    /// changing, then the Layers pane's thumbnails. One is out at a time, and
    /// none is asked while a refusal holds asking back.
    pub fn wanted_preview(&mut self, scale: Scale) -> Option<Request> {
        if self.previews.asked.is_some() || self.holding_back() {
            return None;
        }
        let (asking, work) = self
            .wanted_tiles()
            .or_else(|| self.wanted_histogram())
            .or_else(|| self.wanted_under())
            .or_else(|| self.wanted_thumbnails(scale))?;
        let job = self.next_job;
        self.next_job += 1;
        self.previews.asked = Some(Asked { job, asking });
        Some(Request::Own(Own::Preview {
            job,
            stamp: self.previews.ticket.stamp(),
            work,
        }))
    }

    /// The preview worker answered: only the newest job asked, under the
    /// ticket it was asked under, is landed — where what it worked out still
    /// applies — and the next is asked once it has, one given up to the
    /// queue's work asked again.
    pub fn previewed(&mut self, answer: PreviewAnswer, damage: &mut Region) {
        let PreviewAnswer {
            job,
            ticket,
            previewed,
            ..
        } = answer;
        let Some(asked) = self.previews.asked.take_if(|asked| asked.job == job) else {
            return;
        };
        if !self.previews.ticket.holds(ticket) {
            return;
        }
        match (asked.asking, previewed) {
            (Asking::Tiles, Previewed::Tiles(Ok(tiles))) => self.land_tiles(tiles, damage),
            (Asking::Histogram(basis), Previewed::Histogram(Ok(histogram))) => {
                self.land_histogram(histogram, basis, damage);
            }
            (Asking::Under(basis), Previewed::Under(Ok(canvas))) => self.land_under(basis, canvas),
            (
                Asking::Thumbnails(looked),
                Previewed::LayerThumbnails {
                    side,
                    drawn: Ok(drawn),
                },
            ) => self.land_thumbnails(looked, side, drawn, damage),
            (
                Asking::Thumbnails(looked),
                Previewed::LayerThumbnails {
                    drawn: Err(Halted::OutOfMemory | Halted::NeedsColour),
                    ..
                },
            ) => self.thumbnails_refused(looked),
            (
                _,
                Previewed::Tiles(Err(halted))
                | Previewed::Histogram(Err(halted))
                | Previewed::Under(Err(halted)),
            ) => self.preview_halted(halted, ticket, damage),
            _ => {}
        }
    }

    /// A preview job asked under the ticket's count `ticket` stopped short of
    /// its answer for want of what it needed: said, and nothing asked again
    /// until the picture or what is asked moves, the memory pressure falls or
    /// the window is laid out anew.
    fn preview_halted(&mut self, halted: Halted, ticket: u64, damage: &mut Region) {
        if halted == Halted::Abandoned {
            return;
        }
        self.previews.held_back = Some(Hold::Short {
            generation: self.document.generation(),
            ticket,
        });
        let said = alloc::format!("The preview could not be worked out: {halted}");
        self.state(&said, damage);
    }

    /// The preview worker had no room for job `job`: it is asked again by the
    /// next input, never at once.
    pub fn preview_refused(&mut self, job: u64) {
        let Some(asked) = self.previews.asked.take_if(|asked| asked.job == job) else {
            return;
        };
        if let Asking::Thumbnails(looked) = asked.asking {
            self.thumbnails_refused(looked);
        }
        self.previews.held_back = Some(Hold::Refused);
    }

    /// Abandon every preview job the window has asked: whatever lands of them
    /// is dropped, and what is still wanted is asked afresh.
    pub(super) fn disown_previews(&mut self) {
        self.previews.ticket.bump();
        self.previews.asked = None;
    }

    /// Whether a hold keeps asking back, one for want of memory let go once
    /// the picture or what is asked has moved since.
    fn holding_back(&mut self) -> bool {
        let moved = matches!(
            self.previews.held_back,
            Some(Hold::Short { generation, ticket })
                if generation != self.document.generation()
                    || !self.previews.ticket.holds(ticket)
        );
        if moved {
            self.previews.held_back = None;
        }
        self.previews.held_back.is_some()
    }

    /// The memory-pressure band moved to `band`: a fall lets a hold for want
    /// of memory go.
    pub(super) fn previews_pressured(&mut self, band: PressureBand) {
        let fell = band < self.previews.band;
        self.previews.band = band;
        if fell {
            self.lift_short_hold();
        }
    }

    /// Let a hold for want of memory go: what is asked may fit now.
    pub(super) fn lift_short_hold(&mut self) {
        if matches!(self.previews.held_back, Some(Hold::Short { .. })) {
            self.previews.held_back = None;
        }
    }

    /// After an input or an answer: the window's preview jobs abandoned where
    /// the picture moved from `before`, a hold for want of room on the
    /// worker's queue lifted, and what the picture shows of the open operation
    /// brought up to date; `outcome` passes through.
    pub(super) fn follow_picture(
        &mut self,
        before: Basis,
        outcome: Outcome,
        damage: &mut Region,
    ) -> Outcome {
        if self.previews.held_back == Some(Hold::Refused) {
            self.previews.held_back = None;
        }
        if self.basis() != before {
            self.disown_previews();
        }
        self.refresh_previews(damage);
        outcome
    }
}
