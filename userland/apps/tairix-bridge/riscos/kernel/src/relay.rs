//! Relays (`plans/RISCOS-EMULATOR.md` §15.1): requests carried to the
//! session's other side — the files HostFS serves, the font service's glyphs
//! — and the answers a SWI performed again is given.
//!
//! A relay to the files is answered at once, or left pending while the
//! program's own SWI that made it waits: the context is parked, and the SWI
//! is performed again once the reply comes. Each relay its earlier
//! performance made is answered the same again, so it sees what that one saw
//! and comes to the same requests. Only a request that changes nothing the
//! other side holds may be left pending, and only while the SWI has made no
//! request that does, so a SWI performed again never repeats a change. Any
//! other relay waits in place for its reply, as one inside a nested call
//! does.

use alloc::vec::Vec;

use crate::os::{Fail, Os, Wait};

/// The side a relay goes to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Side {
    /// The side holding the session's files.
    Files,
    /// The font service.
    Fonts,
}

/// What the host made of a relay.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Relayed {
    /// Its reply is appended.
    Answered,
    /// It is on its way: the reply comes with [`Os::relay_answered`]. Only a
    /// relay the host was told may park is left so.
    Pending,
    /// The session has no such side, or the relay failed.
    Failed,
}

/// The most bytes of requests and answers a SWI's performances keep: past
/// it, its relays wait in place, so a SWI that parks again and again holds
/// no more of the host's memory than this.
const ANSWERS_MOST: usize = 1 << 20;

/// What a relay came to.
#[derive(Clone, Copy)]
enum Came<'r> {
    Answered(&'r [u8]),
    Failed,
}

/// Why an answer kept could not be given again.
enum Replay {
    /// The relay failed when it was made.
    Failed,
    /// There was no room for the answer.
    NoRoom,
}

/// A request and its answer.
struct Answer {
    side: Side,
    request: Vec<u8>,
    /// None for a relay that failed.
    reply: Option<Vec<u8>>,
    /// Given to the performance under way.
    given: bool,
}

/// The current context's relays for the outermost SWI being performed: the
/// answers it has had, the request it waits on, and whether it may be
/// performed again.
pub(crate) struct Relays {
    answers: Vec<Answer>,
    pending: Option<(Side, Vec<u8>)>,
    /// It made a request that changes the other side, or had an answer that
    /// could not be kept.
    unrepeatable: bool,
    kept: usize,
}

impl Relays {
    pub(crate) const fn new() -> Self {
        Self {
            answers: Vec::new(),
            pending: None,
            unrepeatable: false,
            kept: 0,
        }
    }

    /// Whether the SWI may not be performed again.
    pub(crate) const fn unrepeatable(&self) -> bool {
        self.unrepeatable
    }

    /// Everything let go: a new SWI begins, or the one under way ended.
    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    /// The SWI performed again: each answer to be given again.
    pub(crate) fn again(&mut self) {
        for answer in &mut self.answers {
            answer.given = false;
        }
    }

    /// The answer the performance under way has not yet been given to
    /// `request`, taken.
    fn take(&mut self, side: Side, request: &[u8]) -> Option<Came<'_>> {
        let answer = self
            .answers
            .iter_mut()
            .find(|answer| !answer.given && answer.side == side && answer.request == request)?;
        answer.given = true;
        Some(answer.reply.as_deref().map_or(Came::Failed, Came::Answered))
    }

    /// Whether `bytes` more fit beside what is kept.
    const fn fits(&self, bytes: usize) -> bool {
        self.kept + bytes <= ANSWERS_MOST
    }

    /// `request`'s `reply`, none for a relay that failed, kept for the SWI's
    /// next performance, `given` to this one already; where it cannot be,
    /// the SWI may not be performed again.
    fn keep(&mut self, side: Side, request: Vec<u8>, reply: Option<&[u8]>, given: bool) {
        let length = reply.map_or(0, <[u8]>::len);
        let bytes = request.len() + length;
        if !self.fits(bytes) || self.answers.try_reserve(1).is_err() {
            self.unrepeatable = true;
            return;
        }
        let reply = match reply {
            None => None,
            Some(reply) => {
                let Some(owned) = copied(reply) else {
                    self.unrepeatable = true;
                    return;
                };
                Some(owned)
            }
        };
        self.kept += bytes;
        self.answers.push(Answer {
            side,
            request,
            reply,
            given,
        });
    }
}

/// `bytes` copied, none where there is no room.
fn copied(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut owned = Vec::new();
    owned.try_reserve_exact(bytes.len()).ok()?;
    owned.extend_from_slice(bytes);
    Some(owned)
}

impl Os<'_> {
    /// `request` carried to the side holding the session's files, its reply
    /// appended to `reply`: whether it was answered. A request that
    /// `changes` what that side holds waits in place for its reply, and so
    /// does every relay the SWI makes after it.
    ///
    /// # Errors
    ///
    /// The unwind that parks the context until the reply comes: the SWI is
    /// performed again then, this relay answered from what came.
    pub fn relay(
        &mut self,
        request: &[u8],
        reply: &mut Vec<u8>,
        changes: bool,
    ) -> Result<bool, Fail> {
        let side = Side::Files;
        let replayable = !changes && self.resumable();
        if replayable {
            let replayed = match self.world.relays.take(side, request) {
                Some(Came::Answered(answer)) => Some(
                    reply
                        .try_reserve(answer.len())
                        .map(|()| reply.extend_from_slice(answer))
                        .map_err(|_| Replay::NoRoom),
                ),
                Some(Came::Failed) => Some(Err(Replay::Failed)),
                None => None,
            };
            match replayed {
                Some(Ok(())) => return Ok(true),
                Some(Err(Replay::Failed)) => return Ok(false),
                Some(Err(Replay::NoRoom)) => {
                    // This performance sees it fail, so a later one must not
                    // see it answered.
                    self.world.relays.unrepeatable = true;
                    return Ok(false);
                }
                None => {}
            }
        }
        let copy = replayable
            .then(|| copied(request))
            .flatten()
            .filter(|copy| self.world.relays.fits(copy.len()));
        let start = reply.len();
        match self.world.host.relay(side, request, reply, copy.is_some()) {
            Relayed::Answered => {
                match copy {
                    Some(copy) => self
                        .world
                        .relays
                        .keep(side, copy, Some(&reply[start..]), true),
                    None if changes || replayable => self.world.relays.unrepeatable = true,
                    // A read the SWI cannot be performed again past anyway.
                    None => {}
                }
                Ok(true)
            }
            Relayed::Pending => {
                let Some(copy) = copy else {
                    return Ok(false);
                };
                self.world.relays.pending = Some((side, copy));
                if let Some(parked) = self.suspended(Wait::Relay, None) {
                    return Err(parked);
                }
                self.world.relays.pending = None;
                Ok(false)
            }
            Relayed::Failed => Ok(false),
        }
    }

    /// `request` carried to the font service, its reply appended to `reply`:
    /// whether it was answered. It waits in place, as the Font Manager asks
    /// for glyphs part way through a paint, which cannot be painted again.
    pub fn font_relay(&mut self, request: &[u8], reply: &mut Vec<u8>) -> bool {
        self.world.host.relay(Side::Fonts, request, reply, false) == Relayed::Answered
    }

    /// `reply` to the relay the current context waits on, kept for the SWI's
    /// performance again: false where it waits on none.
    pub fn relay_answered(&mut self, reply: &[u8]) -> bool {
        self.settle_pending(Some(reply))
    }

    /// The relay the current context waits on failed: the SWI performed
    /// again finds it failed, as a relay that fails at once is. False where
    /// it waits on none.
    pub fn relay_failed(&mut self) -> bool {
        self.settle_pending(None)
    }

    fn settle_pending(&mut self, reply: Option<&[u8]>) -> bool {
        let Some((side, request)) = self.world.relays.pending.take() else {
            return false;
        };
        self.world.relays.keep(side, request, reply, false);
        true
    }
}

#[cfg(test)]
mod tests;
