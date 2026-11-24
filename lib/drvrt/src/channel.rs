//! A device channel's endpoint held by its driver, and let go with it.

/// A device channel bound, taken down when dropped: a driver that cannot
/// serve it after all, or stops serving it, leaves nothing bound for a
/// caller to wait on, whether or not the driver lives on.
#[must_use = "the channel is taken down when its guard is dropped"]
#[derive(Debug)]
pub struct BoundChannel(pub(crate) u64);

impl BoundChannel {
    /// The channel's endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> u64 {
        self.0
    }
}

impl Drop for BoundChannel {
    fn drop(&mut self) {
        // Refused only for an endpoint already gone, which leaves nothing to
        // take down.
        let _ = tairix_rt::call_destroy(self.0);
    }
}
