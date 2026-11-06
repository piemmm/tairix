//! Bringing a window a document window owns into line with the one its view
//! wants: re-mapped where its size moved, its range restated where that
//! moved, and drawn again whole where it shows another part of the drawing.

use tairix_abi::window_ipc::WindowSizing;
use tairix_geometry::Rect;

/// What bringing an owned window into line with the one its view wants asks
/// of the desktop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Refit {
    /// The client size to re-map it at, where its size moved.
    pub(super) resize: Option<(u32, u32)>,
    /// The range to hold it to, where that moved: asked once it is re-mapped,
    /// so it already lies inside.
    pub(super) restate: Option<WindowSizing>,
    /// Whether it is drawn again whole: re-mapped onto a fresh surface, or
    /// showing another part of the drawing.
    pub(super) repaint: bool,
}

impl Refit {
    /// What a window showing the rectangle `shown` of the drawing, held to
    /// `told`, owes to show `wanted` held to `sizing` instead; `None` where
    /// `sizing` is of another kind than `told`, which the desktop never
    /// changes on an open window, so it is opened afresh.
    pub(super) fn of(
        (shown, told): (Rect, WindowSizing),
        (wanted, sizing): (Rect, WindowSizing),
    ) -> Option<Self> {
        if told.resizable() != sizing.resizable() {
            return None;
        }
        let resize = ((shown.width, shown.height) != (wanted.width, wanted.height))
            .then_some((wanted.width, wanted.height));
        Some(Self {
            resize,
            restate: (told != sizing).then_some(sizing),
            repaint: resize.is_some() || shown.origin != wanted.origin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A palette's range.
    const RANGE: WindowSizing = WindowSizing::Resizable {
        min_width_px: 40,
        min_height_px: 60,
        max_width_px: 400,
        max_height_px: 600,
    };

    /// The same range, wider.
    const WIDER: WindowSizing = WindowSizing::Resizable {
        min_width_px: 40,
        min_height_px: 60,
        max_width_px: 800,
        max_height_px: 600,
    };

    /// The part of the drawing a tool window shows.
    const SHOWN: Rect = Rect::new(900, 0, 120, 200);

    #[test]
    fn a_window_that_stays_as_it_is_owes_nothing() {
        assert_eq!(
            Refit::of((SHOWN, RANGE), (SHOWN, RANGE)),
            Some(Refit {
                resize: None,
                restate: None,
                repaint: false,
            })
        );
    }

    #[test]
    fn a_new_size_is_re_mapped_and_drawn_whole() {
        let wanted = Rect::new(900, 0, 160, 240);
        assert_eq!(
            Refit::of((SHOWN, RANGE), (wanted, RANGE)),
            Some(Refit {
                resize: Some((160, 240)),
                restate: None,
                repaint: true,
            })
        );
    }

    #[test]
    fn a_moved_range_is_restated_and_a_moved_rectangle_drawn_whole() {
        assert_eq!(
            Refit::of((SHOWN, RANGE), (SHOWN, WIDER)),
            Some(Refit {
                resize: None,
                restate: Some(WIDER),
                repaint: false,
            }),
            "a range alone draws nothing"
        );
        let below = Rect::new(900, 200, 120, 200);
        assert_eq!(
            Refit::of((SHOWN, WindowSizing::Fixed), (below, WindowSizing::Fixed)),
            Some(Refit {
                resize: None,
                restate: None,
                repaint: true,
            }),
            "another part of the drawing"
        );
    }

    #[test]
    fn no_window_is_made_resizable_or_fixed_in_place() {
        assert_eq!(
            Refit::of((SHOWN, WindowSizing::Fixed), (SHOWN, RANGE)),
            None
        );
        assert_eq!(
            Refit::of((SHOWN, RANGE), (SHOWN, WindowSizing::Fixed)),
            None
        );
    }
}
