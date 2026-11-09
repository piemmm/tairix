//! When the file manager reads the desktop's settings document.

/// The first wait before a refused read is asked again.
pub const FIRST_RETRY_NS: u64 = 1_000_000_000;

/// The longest such wait: each refusal doubles it, up to this, so a document
/// that stays unreadable costs a read every few minutes.
pub const LONGEST_RETRY_NS: u64 = 300_000_000_000;

/// The reads of a published document: one per publication, and after a
/// refusal one more once its wait is over, never while another is under way.
#[derive(Debug)]
pub struct Reread {
    /// The publication last read.
    generation: u32,
    /// The publication a read is under way for.
    asked: Option<u32>,
    /// When a refused read is asked again.
    retry: Option<u64>,
    backoff_ns: u64,
}

impl Reread {
    /// Reads of the document as it is published at `published`, none yet
    /// under way.
    #[must_use]
    pub const fn new(published: u32) -> Self {
        Self {
            generation: published,
            asked: None,
            retry: None,
            backoff_ns: 0,
        }
    }

    /// Whether the publication `published` is to be read at `now_ns`: it was
    /// not the one read, or a refused read's wait is over, and no read of it is
    /// under way. Answering yes notes the read as asked.
    pub fn wants(&mut self, published: u32, now_ns: u64) -> bool {
        let due = self.retry.is_some_and(|at| at <= now_ns);
        let wanted = (published != self.generation || due) && self.asked != Some(published);
        if wanted {
            self.asked = Some(published);
            self.retry = None;
        }
        wanted
    }

    /// When a refused read is next to be asked again: none while a read is
    /// under way, whose landing is a turn of its own, so a time already past
    /// never stands as the loop's next wake.
    #[must_use]
    pub fn retry_at(&self) -> Option<u64> {
        self.retry.filter(|_| self.asked.is_none())
    }

    /// Note that the read of `generation` landed at `now_ns`, `refused` or
    /// not. A refusal waits twice the last wait before it is asked again,
    /// unless a later read is under way already.
    pub fn landed(&mut self, generation: u32, refused: bool, now_ns: u64) {
        self.generation = generation;
        if self.asked == Some(generation) {
            self.asked = None;
        }
        if !refused {
            self.backoff_ns = 0;
            return;
        }
        self.backoff_ns = if self.backoff_ns == 0 {
            FIRST_RETRY_NS
        } else {
            self.backoff_ns.saturating_mul(2).min(LONGEST_RETRY_NS)
        };
        if self.asked.is_none() {
            self.retry = Some(now_ns.saturating_add(self.backoff_ns));
        }
    }
}

#[cfg(test)]
#[path = "reread_tests.rs"]
mod tests;
