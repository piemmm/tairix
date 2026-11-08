//! A column of form groups: the one routing that carries the keyboard and the
//! pointer between a settings surface's plates.
//!
//! The groups stack down the one plate column ([`stack`]), every control in
//! them on one slot column, and an expanded choice list is placed by the
//! drop-down rule inside the client the owner names. The column is laid out
//! whole from its top: an owner whose column outgrows its room shows it
//! through a scroll view, states both rectangles in the column's own
//! unscrolled layout, and keeps the cursor in view with
//! [`FieldColumn::cursor_reveal`].
//!
//! - **An open choice list is modal across the column**: only its group sees
//!   the pointer and every key until it resolves.
//! - **The cursor crosses plates.** A group clamps at its own ends; Up or Down
//!   past one carries the cursor into its neighbour, which is what makes every
//!   row below the first plate reachable from the keyboard.
//! - **A primary press gives the cursor** to the row it lands on, off
//!   whichever group held it.
//! - **A number or colour row the cursor leaves settles**, wherever the cursor
//!   goes: to another row, another plate, or out of the column.

use alloc::vec::Vec;

use tairix_collections::SmallVec;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::form::{FieldControl, FieldGroup, FieldGroupAction, FieldLayout, FieldRow};
use crate::stack;
use crate::text::Keystroke;

/// The groups a column seats without asking the heap for room.
const INLINE_GROUPS: usize = 8;

/// Where each group of a column is laid out, by its index.
pub type ColumnLayouts = SmallVec<(usize, FieldLayout), INLINE_GROUPS>;

/// Where a column is drawn, and what it is drawn with.
///
/// The four facts every entry point needs together: the rectangle the groups
/// stack down, the client an expanded choice list has to fit inside, and the
/// density and theme every length and colour is resolved through. A scrolled
/// owner states both rectangles in the column's own unscrolled layout, so the
/// column never learns it is scrolled.
#[derive(Copy, Clone, Debug)]
pub struct ColumnPlace<'a> {
    /// The rectangle the groups stack down.
    pub bounds: Rect,
    /// The whole client, which an expanded choice list must fit inside.
    pub viewport: Rect,
    /// The desktop density every logical length is resolved through.
    pub scale: Scale,
    /// The theme every colour and metric comes from.
    pub theme: &'a Theme,
}

/// What a column's row asked for: the group it is in, and the row's own
/// request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnAction {
    /// The zero-based index of the group the row is in.
    pub group: usize,
    /// The row, and what its slot requested.
    pub action: FieldGroupAction,
}

/// Which way the cursor steps out of a group that has nothing further.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Landing {
    /// Stepping downward: the first row.
    First,
    /// Stepping upward: the last.
    Last,
}

/// A column of [`FieldGroup`]s and the keyboard cursor across them.
#[derive(Clone, Debug, Default)]
pub struct FieldColumn {
    groups: Vec<FieldGroup>,
    /// Which group holds the keyboard cursor.
    focus: usize,
    /// Where the pointer last moved to in the column's layout, which is where
    /// a press lands; `None` until it has moved.
    pointer: Option<Point>,
}

impl FieldColumn {
    /// A column of `groups`, the cursor on none of them.
    #[must_use]
    pub fn new(groups: Vec<FieldGroup>) -> Self {
        Self {
            groups,
            focus: 0,
            pointer: None,
        }
    }

    /// The groups, in stacking order.
    #[must_use]
    pub fn groups(&self) -> &[FieldGroup] {
        &self.groups
    }

    /// The groups, for an owner restating a row in place: a row holding a
    /// caret or a selection survives that, as it would not a rebuild.
    pub fn groups_mut(&mut self) -> &mut Vec<FieldGroup> {
        &mut self.groups
    }

    /// How many groups the column stacks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Whether the column stacks no group.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Stack `groups` in place of the column's, the cursor kept on the same
    /// group and row where the new ones still have them and on the nearest
    /// where they do not.
    pub fn replace(&mut self, groups: Vec<FieldGroup>) {
        let held = self.groups.get(self.focus).and_then(FieldGroup::focus);
        self.groups = groups;
        self.focus = self.focus.min(self.groups.len().saturating_sub(1));
        if let (Some(item), Some(group)) = (held, self.groups.get_mut(self.focus)) {
            group.adopt_focus(Some(item.min(group.len().saturating_sub(1))));
        }
    }

    /// The height the column needs stacked in a rectangle `width` pixels wide.
    #[must_use]
    pub fn measured_height(&self, width: u32, scale: Scale, theme: &Theme) -> u32 {
        let plate = stack::plate_width(width, scale, theme);
        let column = FieldGroup::shared_column(&self.groups, plate, scale, theme);
        stack::height(
            self.groups
                .iter()
                .map(|group| group.measured_height(plate, column, scale, theme)),
            scale,
            theme,
        )
    }

    /// The narrowest column that seats every group's controls whole.
    #[must_use]
    pub fn natural_width(&self, scale: Scale, theme: &Theme) -> u32 {
        let plate = self
            .groups
            .iter()
            .map(|group| group.natural_width(scale, theme))
            .max()
            .unwrap_or(0);
        stack::column_width(plate, scale, theme)
    }

    /// Where each group is laid out in `place`: stacked down its bounds on one
    /// slot column, the list a group has open placed inside its viewport. A
    /// column the heap cannot lay out has nothing drawn, so nothing to press.
    #[must_use]
    pub fn layouts(&self, place: ColumnPlace<'_>) -> ColumnLayouts {
        let ColumnPlace {
            bounds,
            viewport,
            scale,
            theme,
        } = place;
        let across = stack::plate_width(bounds.width, scale, theme);
        let column = FieldGroup::shared_column(&self.groups, across, scale, theme);
        let mut layouts = ColumnLayouts::new();
        let plates = stack::placements(bounds, self.groups.len(), scale, theme, |index| {
            self.groups.get(index).map_or(0, |group| {
                group.measured_height(across, column, scale, theme)
            })
        });
        for (index, rect) in plates {
            let Some(group) = self.groups.get(index) else {
                continue;
            };
            let layout = FieldLayout::new(rect, column).within(viewport);
            if layouts
                .try_push((index, group.place_popup(layout, scale, theme)))
                .is_err()
            {
                return ColumnLayouts::new();
            }
        }
        layouts
    }

    /// Draw the plates stacked down `place`.
    ///
    /// An open choice list is not among them: it hangs over whatever the
    /// column shares the window with, so the owner paints it with
    /// [`render_popup`](Self::render_popup) once everything else is drawn.
    pub fn render(&self, surface: &mut Surface, place: ColumnPlace<'_>) {
        for (index, layout) in self.layouts(place) {
            if let Some(group) = self.groups.get(index) {
                group.render(surface, layout, place.scale, place.theme);
            }
        }
    }

    /// Draw the choice list a row has open, if one has, where the layout
    /// placed it.
    pub fn render_popup(&self, surface: &mut Surface, place: ColumnPlace<'_>) {
        let Some(open) = self.listing() else {
            return;
        };
        let layout = self
            .layouts(place)
            .into_iter()
            .find_map(|(index, layout)| (index == open).then_some(layout));
        if let (Some(group), Some(layout)) = (self.groups.get(open), layout) {
            group.render_popup(surface, layout.popup, place.scale, place.theme);
        }
    }

    /// Route one pointer event, reporting into `damage` what it redrew.
    ///
    /// The keyboard cursor follows a primary press, never a hover, onto the
    /// row or picture choice pressed — off whichever group held it, which
    /// settles a number or colour row it leaves — so the keys typed next reach
    /// the entry pressed. That settle is answered ahead of what the press
    /// itself did.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        place: ColumnPlace<'_>,
        damage: &mut Region,
    ) -> Option<ColumnAction> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = Some(*to);
        }
        let layouts = self.layouts(place);
        // An open list is modal across the whole column, so a press on it can
        // never also reach a row of another plate drawn beneath it.
        let listing = self.listing();
        let primary = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        let mut settled = None;
        if let Some(at) = self.pointer.filter(|_| primary && listing.is_none()) {
            settled = self.take_cursor_at(at, &layouts, place, damage);
        }
        let mut acted = None;
        for (index, layout) in layouts {
            if listing.is_some_and(|open| open != index) {
                continue;
            }
            let Some(group) = self.groups.get_mut(index) else {
                continue;
            };
            if let Some(action) = group.on_pointer(event, layout, place.scale, place.theme, damage)
            {
                acted = Some(ColumnAction {
                    group: index,
                    action,
                });
            }
        }
        settled.or(acted)
    }

    /// Route one key press, reporting into `damage` what it redrew.
    ///
    /// The group holding the cursor answers it first; Up or Down it has
    /// nowhere further to take carries the cursor to the neighbouring group.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        place: ColumnPlace<'_>,
        damage: &mut Region,
    ) -> Option<ColumnAction> {
        let layouts = self.layouts(place);
        let seated = layouts
            .iter()
            .find_map(|&(index, layout)| (index == self.focus).then_some(layout));
        let mut acted = None;
        let mut kept = false;
        if let (Some(layout), Some(group)) = (seated, self.groups.get_mut(self.focus)) {
            let was = group.focus();
            acted = group
                .on_key(stroke, layout, place.scale, place.theme, damage)
                .map(|action| ColumnAction {
                    group: self.focus,
                    action,
                });
            // An open choice list is modal: every key is the list's until it
            // resolves, so the cursor must not step out from under it.
            let listing = group.rows().iter().any(FieldRow::popup_open);
            kept = acted.is_some() || listing || group.focus() != was;
        }
        if kept {
            return acted;
        }
        self.step_group(stroke.key, &layouts, place, damage)
    }

    /// Move the cursor to the neighbouring group when the focused one has
    /// nothing further in the direction `key` asks for, answering the settle
    /// of a row it leaves.
    fn step_group(
        &mut self,
        key: Key,
        layouts: &[(usize, FieldLayout)],
        place: ColumnPlace<'_>,
        damage: &mut Region,
    ) -> Option<ColumnAction> {
        let (next, landing) = match key {
            Key::Named(NamedKey::Down) if self.focus + 1 < self.groups.len() => {
                (self.focus + 1, Landing::First)
            }
            Key::Named(NamedKey::Up) if self.focus > 0 => (self.focus - 1, Landing::Last),
            _ => return None,
        };
        let settled = self.leave_group(self.focus, layouts, place, damage);
        self.focus = next;
        let group = self.groups.get_mut(next)?;
        let row = match landing {
            Landing::First => 0,
            Landing::Last => group.len().saturating_sub(1),
        };
        group.adopt_focus(Some(row));
        damage.add(place.bounds);
        settled
    }

    /// Take the cursor off group `index`, settling the row it leaves.
    fn leave_group(
        &mut self,
        index: usize,
        layouts: &[(usize, FieldLayout)],
        place: ColumnPlace<'_>,
        damage: &mut Region,
    ) -> Option<ColumnAction> {
        let layout = layouts
            .iter()
            .find_map(|&(at, layout)| (at == index).then_some(layout));
        let group = self.groups.get_mut(index)?;
        let Some(layout) = layout else {
            group.adopt_focus(None);
            return None;
        };
        group
            .set_focus(None, layout, place.scale, place.theme, damage)
            .map(|action| ColumnAction {
                group: index,
                action,
            })
    }

    /// Carry the keyboard cursor to the group holding an item under the
    /// layout `point`, the groups laid out as `layouts`, clearing every other
    /// group's ring and answering the settle of a row that leaves; the group
    /// the press reaches gives it to the item itself. A point over no item
    /// leaves it where it is.
    fn take_cursor_at(
        &mut self,
        point: Point,
        layouts: &[(usize, FieldLayout)],
        place: ColumnPlace<'_>,
        damage: &mut Region,
    ) -> Option<ColumnAction> {
        let taken = layouts.iter().find_map(|&(index, layout)| {
            let group = self.groups.get(index)?;
            group
                .row_at(layout, place.scale, place.theme, point)
                .map(|_| index)
        })?;
        let mut settled = None;
        for &(index, layout) in layouts {
            if index == taken {
                continue;
            }
            if let Some(group) = self.groups.get_mut(index) {
                if let Some(action) =
                    group.set_focus(None, layout, place.scale, place.theme, damage)
                {
                    settled = Some(ColumnAction {
                        group: index,
                        action,
                    });
                }
            }
        }
        self.focus = taken;
        settled
    }

    /// Take the keyboard cursor out of the column — to another part of the
    /// window — settling the row it rests on and clearing its ring.
    pub fn leave(&mut self, place: ColumnPlace<'_>, damage: &mut Region) -> Option<ColumnAction> {
        let layouts = self.layouts(place);
        self.leave_group(self.focus, &layouts, place, damage)
    }

    /// Which group and row the keyboard cursor is on.
    #[must_use]
    pub fn cursor(&self) -> Option<(usize, usize)> {
        let row = self.groups.get(self.focus)?.focus()?;
        Some((self.focus, row))
    }

    /// Put the keyboard cursor on the column — where a press already placed
    /// it, else on the first row — or take its ring off without reporting,
    /// for an owner that presents the column whole.
    pub fn set_focused(&mut self, focused: bool) {
        if focused && self.cursor().is_some() {
            return;
        }
        if focused {
            self.focus = 0;
        }
        for (index, group) in self.groups.iter_mut().enumerate() {
            group.adopt_focus((focused && index == self.focus).then_some(0));
        }
    }

    /// The part of `place` to scroll into view for the keyboard cursor, in a
    /// column that shows `seen` pixels of it: the cursor's whole group with
    /// the gap that frames it where that fits, else its row together with the
    /// caption above a first row or the footnote below a last one, else the
    /// row alone.
    ///
    /// A row the cursor reached but the column does not show is a control
    /// the reader cannot use, which is a correctness property rather than a
    /// convenience; the rest is what keeps the row's group readable around
    /// it.
    #[must_use]
    pub fn cursor_reveal(&self, place: ColumnPlace<'_>, seen: u32) -> Option<Rect> {
        let (index, layout) = self
            .layouts(place)
            .into_iter()
            .find(|(index, _)| *index == self.focus)?;
        let group = self.groups.get(index)?;
        let bounds = layout.bounds;
        let gap = to_i32(stack::gap(place.scale, place.theme));
        let top = bounds.top().saturating_sub(gap).max(place.bounds.top());
        let bottom = bounds
            .bottom()
            .saturating_add(gap)
            .min(place.bounds.bottom());
        // A chooser's cursor is one picture of it, which is what has to show.
        let row = group
            .focus()
            .and_then(|row| Some((row, group.focus_rect(layout, place.scale, place.theme)?)));
        let fits = |from: i32, to: i32| to.saturating_sub(from) <= to_i32(seen);
        let (from, to) = match row {
            _ if fits(top, bottom) => (top, bottom),
            None => (top, bottom),
            Some((0, rect)) if fits(top, rect.bottom()) => (top, rect.bottom()),
            Some((at, rect)) if at + 1 == group.len() && fits(rect.top(), bottom) => {
                (rect.top(), bottom)
            }
            Some((_, rect)) => (rect.top(), rect.bottom()),
        };
        let height = u32::try_from(to.saturating_sub(from)).unwrap_or(0);
        Some(Rect::new(bounds.left(), from, bounds.width, height))
    }

    /// The group holding an open choice list, if one is open.
    #[must_use]
    pub fn listing(&self) -> Option<usize> {
        self.groups
            .iter()
            .position(|group| group.rows().iter().any(FieldRow::popup_open))
    }

    /// Whether a choice list is open, which is modal: the list keeps the
    /// pointer even where it hangs outside the column.
    #[must_use]
    pub fn is_listing(&self) -> bool {
        self.listing().is_some()
    }

    /// Whether a row is in the middle of a gesture — a choice list open, or a
    /// slider being dragged — which rebuilding the rows would drop.
    #[must_use]
    pub fn mid_gesture(&self) -> bool {
        self.groups.iter().flat_map(FieldGroup::rows).any(|row| {
            row.popup_open()
                || matches!(row.control(), FieldControl::Slider(slider) if slider.is_dragging())
        })
    }

    /// When a row's control next changes on its own, the soonest across the
    /// column.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        self.groups.iter().filter_map(FieldGroup::deadline_ns).min()
    }

    /// Bring every row's control up to `now_ns`, reporting what they redrew.
    pub fn advance(&mut self, now_ns: u64, place: ColumnPlace<'_>, damage: &mut Region) {
        if self.deadline_ns().is_none_or(|due| due > now_ns) {
            return;
        }
        for (index, layout) in self.layouts(place) {
            if let Some(group) = self.groups.get_mut(index) {
                group.advance(now_ns, layout, (place.scale, place.theme), damage);
            }
        }
    }
}

#[cfg(test)]
#[path = "column_tests.rs"]
mod tests;
