//! Whether a machine window asks the desktop to enlarge its picture as a
//! view, which a refusal at a window size withdraws at that size.

/// The window size at which the desktop refused the window's view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewRefusal {
    at: Option<(u32, u32)>,
}

impl ViewRefusal {
    /// The desktop refused a view for the window at `size`.
    pub const fn refused(&mut self, size: (u32, u32)) {
        self.at = Some(size);
    }

    /// The window is now `size`: a refusal at any other is forgotten, so a
    /// view is asked for once more at each size the window is given.
    pub fn resized(&mut self, size: (u32, u32)) {
        if self.at != Some(size) {
            self.at = None;
        }
    }

    /// Whether the window may ask for a view at `size`.
    #[must_use]
    pub fn allows(&self, size: (u32, u32)) -> bool {
        self.at != Some(size)
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
