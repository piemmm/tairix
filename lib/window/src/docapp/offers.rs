//! Files another application's drag offers over a document application's
//! windows: the one media type read per drag that their answers wait on, and
//! which offer to answer once it lands, so the loop never waits on the read.

use alloc::string::String;

use tairix_abi::Errno;
use tairix_geometry::Point;

use crate::client::OfferedMedia;

/// A file a drag holds over a document window, at a window-local point.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileOffer {
    pub(crate) window_id: u64,
    pub(crate) drag: u32,
    pub(crate) serial: u32,
    pub(crate) at: Point,
}

/// What an offer, or a read landing, calls for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum OfferStep {
    Rest,
    /// Read the media type of the file the offer holds, under its number.
    Ask(FileOffer),
    /// Answer the offer, its drag's media type read.
    Answer(FileOffer),
}

/// The offers made over the application's windows and the read they wait on.
pub(crate) struct FileOffers {
    /// Whether the application's view places any file: one that places none
    /// reads nothing and answers nothing, which places nothing.
    places: bool,
    /// The media type the last drag read holds, for every offer it makes over
    /// any of the windows.
    read: Option<(u32, Result<String, Errno>)>,
    /// The newest offer whose drag's media type is not yet read.
    waiting: Option<FileOffer>,
    /// The offer the read in flight was asked under.
    asked: Option<FileOffer>,
}

impl FileOffers {
    /// The offers of an application whose view `places` a file or never does.
    pub(crate) const fn new(places: bool) -> Self {
        Self {
            places,
            read: None,
            waiting: None,
            asked: None,
        }
    }

    /// What `offer` calls for.
    pub(crate) fn offered(&mut self, offer: FileOffer) -> OfferStep {
        if !self.places {
            return OfferStep::Rest;
        }
        if self
            .read
            .as_ref()
            .is_some_and(|(drag, _)| *drag == offer.drag)
        {
            self.waiting = None;
            return OfferStep::Answer(offer);
        }
        self.waiting = Some(offer);
        self.ask(offer)
    }

    /// The ask of `offer` never reached the session.
    pub(crate) fn unasked(&mut self, offer: FileOffer) {
        if self.asked == Some(offer) {
            self.asked = None;
        }
    }

    /// What `read` landing calls for, and why the read failed when it did.
    pub(crate) fn landed(&mut self, read: OfferedMedia) -> (OfferStep, Option<Errno>) {
        let Some(asked) = self
            .asked
            .filter(|asked| (asked.window_id, asked.serial) == (read.window_id, read.serial))
        else {
            return (OfferStep::Rest, None);
        };
        self.asked = None;
        // An offer that ended before it was read says nothing of the file; one
        // made since, somewhere else, is read under its own number.
        if read.media == Err(Errno::NotFound) {
            let again = self.waiting.filter(|offer| offer.serial != asked.serial);
            if again.is_none() {
                self.waiting = None;
            }
            return (again.map_or(OfferStep::Rest, |offer| self.ask(offer)), None);
        }
        let failed = read.media.as_ref().err().copied();
        self.read = Some((asked.drag, read.media));
        let step = match self.waiting {
            Some(offer) if offer.drag == asked.drag => {
                self.waiting = None;
                OfferStep::Answer(offer)
            }
            Some(offer) => self.ask(offer),
            None => OfferStep::Rest,
        };
        (step, failed)
    }

    /// The offer over window `window_id` ended, so nothing waits on its
    /// answer.
    pub(crate) fn ended(&mut self, window_id: u64) {
        if self
            .waiting
            .is_some_and(|offer| offer.window_id == window_id)
        {
            self.waiting = None;
        }
    }

    /// What window `window_id` closing calls for: the client delivers no read
    /// asked over it, so an offer elsewhere that waited on one is read itself.
    pub(crate) fn closed(&mut self, window_id: u64) -> OfferStep {
        self.ended(window_id);
        if self.asked.is_none_or(|asked| asked.window_id != window_id) {
            return OfferStep::Rest;
        }
        self.asked = None;
        self.waiting
            .map_or(OfferStep::Rest, |offer| self.ask(offer))
    }

    /// The media type of the file drag `drag` holds, once read.
    pub(crate) fn media(&self, drag: u32) -> Option<&str> {
        match &self.read {
            Some((read, Ok(media))) if *read == drag => Some(media),
            _ => None,
        }
    }

    /// Read under `offer`, unless its drag is already being read.
    fn ask(&mut self, offer: FileOffer) -> OfferStep {
        if self.asked.is_some_and(|asked| asked.drag == offer.drag) {
            return OfferStep::Rest;
        }
        self.asked = Some(offer);
        OfferStep::Ask(offer)
    }
}

#[cfg(test)]
#[path = "offers_tests.rs"]
mod tests;
