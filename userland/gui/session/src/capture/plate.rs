//! The capture's two plates: the countdown before a capture is taken and the
//! floating thumbnail after it. Neither takes the pointer, and both are made
//! from what the session already holds.

use alloc::format;

use tairix_controls::damage;
use tairix_controls::Tooltip;
use tairix_geometry::fit_within;
use tairix_wm::{Compositor, Corners, Point, PointerCatch, Rect, Region, Surface, WindowId};

use crate::resolution::{next_step, seconds_left};
use crate::shell::DesktopShell;
use crate::switchuser::park_within;

/// How long the floating thumbnail stays, in nanoseconds.
pub(super) const THUMBNAIL_NS: u64 = 5_000_000_000;

/// The floating thumbnail's longer side, in logical pixels.
const THUMBNAIL_SIDE: u32 = 200;

/// The clearance the plates and the bar keep from the work area's bottom
/// edge, in logical pixels.
pub(super) const PLATE_MARGIN: u32 = 16;

/// The seconds before a capture is taken, counted down on a plate.
pub(super) struct Countdown {
    due_ns: u64,
    recording: bool,
    /// The seconds the plate shows.
    shown: u64,
    plate: Option<WindowId>,
}

impl Countdown {
    /// Count down to `due_ns` from `now_ns`. A plate the heap refuses leaves
    /// the countdown running unseen, since the deadline is what counts.
    pub(super) fn start(
        due_ns: u64,
        recording: bool,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> Self {
        let mut countdown = Self {
            due_ns,
            recording,
            shown: 0,
            plate: None,
        };
        countdown.tick(now_ns, shell, compositor);
        countdown
    }

    /// Whether the countdown has run out by `now_ns`.
    pub(super) const fn due(&self, now_ns: u64) -> bool {
        now_ns >= self.due_ns
    }

    /// `park_ns`, shortened to the moment the plate next drops a second.
    pub(super) fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        let next = next_step(self.due_ns, now_ns).saturating_sub(now_ns);
        park_within(park_ns, Some(next))
    }

    /// Show the seconds left at `now_ns`, painting only when the number moved;
    /// a plate the heap would not repaint keeps what it showed until a later
    /// tick paints it.
    pub(super) fn tick(&mut self, now_ns: u64, shell: &DesktopShell, compositor: &mut Compositor) {
        let left = seconds_left(self.due_ns, now_ns);
        if left == self.shown {
            return;
        }
        let what = if self.recording {
            "Recording"
        } else {
            "Screenshot"
        };
        let tip = Tooltip::new(format!("{what} in {left}"));
        let (plate, painted) = place_plate(self.plate, &tip, shell, compositor);
        self.plate = plate;
        if painted {
            self.shown = left;
        }
    }

    /// Take the plate down.
    pub(super) fn close(self, compositor: &mut Compositor) {
        if let Some(plate) = self.plate {
            compositor.remove(plate);
        }
    }

    /// The plate's window, while it shows.
    pub(super) const fn window(&self) -> Option<WindowId> {
        self.plate
    }
}

/// The picture just taken, shown small at the bottom right of the work area
/// for a few seconds.
pub(super) struct Thumbnail {
    window: WindowId,
    until_ns: u64,
}

impl Thumbnail {
    /// Show `picture` until a few seconds after `now_ns`; `None` where the heap
    /// refuses its pixels.
    pub(super) fn show(
        picture: &Surface,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Self> {
        let scale = compositor.scale();
        let (width, height) = fit_within(
            picture.width(),
            picture.height(),
            scale.scale_length(THUMBNAIL_SIDE),
        )?;
        let pixels = crate::apps::thumbnail(picture, width, height)?;
        let area = shell.work_area(compositor);
        let margin = at(scale.scale_length(PLATE_MARGIN));
        let origin = Point::new(
            area.right()
                .saturating_sub(at(width))
                .saturating_sub(margin),
            area.bottom()
                .saturating_sub(at(height))
                .saturating_sub(margin),
        );
        let window = compositor.add_window(origin, pixels);
        let radius = scale.scale_length(
            shell
                .session()
                .floating_theme()
                .metrics()
                .popup_corner_radius,
        );
        float(window, radius, compositor);
        Some(Self {
            window,
            until_ns: now_ns.saturating_add(THUMBNAIL_NS),
        })
    }

    /// Whether it has been shown long enough by `now_ns`.
    pub(super) const fn expired(&self, now_ns: u64) -> bool {
        now_ns >= self.until_ns
    }

    /// `park_ns`, shortened to the moment it goes.
    pub(super) fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        park_within(park_ns, Some(self.until_ns.saturating_sub(now_ns)))
    }

    /// Take it down.
    pub(super) fn close(self, compositor: &mut Compositor) {
        compositor.remove(self.window);
    }

    /// Its window.
    pub(super) const fn window(&self) -> WindowId {
        self.window
    }
}

/// The top-left a plate of `size` takes centred at the bottom of `area`, the
/// margin above its edge: where the countdown and the bar sit.
pub(super) fn bottom_centre(area: Rect, size: (u32, u32), margin: u32) -> Point {
    Point::new(
        area.left()
            .saturating_add(at(area.width.saturating_sub(size.0) / 2)),
        area.bottom()
            .saturating_sub(at(size.1))
            .saturating_sub(at(margin)),
    )
}

/// Mark `window` as floating chrome: untouched by the pointer, rounded by
/// `radius`, shadowed, and above everything.
pub(super) fn float(window: WindowId, radius: u32, compositor: &mut Compositor) {
    compositor.set_pointer_catch(window, PointerCatch::None);
    compositor.set_corners(window, Corners::painted(radius));
    compositor.set_casts_shadow(window, true);
    compositor.raise(window);
}

/// A length as a screen coordinate, saturating.
pub(super) fn at(length: u32) -> i32 {
    i32::try_from(length).unwrap_or(i32::MAX)
}

/// `tip` on its plate at the bottom centre of the work area, repainting the
/// plate `shown` in place where it still stands: the plate, and whether it
/// shows `tip`. A plate the heap would not repaint stays as it was.
fn place_plate(
    shown: Option<WindowId>,
    tip: &Tooltip,
    shell: &DesktopShell,
    compositor: &mut Compositor,
) -> (Option<WindowId>, bool) {
    let scale = compositor.scale();
    let theme = shell.session().floating_theme();
    let size = tip.preferred_size(scale, theme);
    let whole = Rect::new(0, 0, size.0, size.1);
    let origin = bottom_centre(
        shell.work_area(compositor),
        size,
        scale.scale_length(PLATE_MARGIN),
    );
    let live = shown.filter(|plate| compositor.window(*plate).is_some());
    let plate = if let Some(plate) = live {
        let painted =
            compositor.repaint_window(plate, size, &Region::from(whole), |surface, rects| {
                damage::paint_parts(surface, rects, |surface| {
                    tip.render(surface, whole, scale, theme);
                });
            });
        if !painted {
            return (Some(plate), false);
        }
        compositor.move_window(plate, origin);
        plate
    } else {
        let Some(mut pixels) = Surface::new(size.0, size.1) else {
            return (None, false);
        };
        tip.render(&mut pixels, whole, scale, theme);
        compositor.add_window(origin, pixels)
    };
    float(
        plate,
        scale.scale_length(theme.metrics().popup_corner_radius),
        compositor,
    );
    (Some(plate), true)
}
