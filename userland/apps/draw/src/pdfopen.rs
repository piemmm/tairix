//! The window a PDF is opened through when it asks something first: which of
//! its pages, or its password. Each is a form in a dialog window of its own,
//! answered or turned down; what it is for travels through it untouched.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::window_ipc::CursorShape;
use tairix_controls::{
    Button, ButtonContent, ControlRole, Dialog, DialogAction, FieldAction, FieldControl,
    FieldGroup, FieldGroupAction, FieldLayout, FieldRow, Keystroke, NumberField, SecretField,
    TextAction, ValidationState,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_sandbox::drawedit::MAX_PASSWORD_BYTES;
use tairix_theme::Theme;
use tairix_util::secret::WipedBuf;
use tairix_window::docapp::{AppRequest, AppView, Outcome};

use crate::layout::Faces;

/// The window's width, in logical pixels.
const WIDTH: u32 = 440;

/// The height it opens at, in logical pixels; it is held to what its form
/// needs when that is more.
const HEIGHT: u32 = 210;

/// What the window asks.
#[derive(Clone, Debug, PartialEq)]
pub enum Asked {
    /// Which page: the document's count, and the sizes of as many as were
    /// listed, in points.
    Page {
        /// How many pages it has.
        count: u32,
        /// Their widths and heights, first first.
        sizes: Vec<(f64, f64)>,
    },
    /// Its password, `wrong` where one given did not open it.
    Password {
        /// The password last given was wrong.
        wrong: bool,
    },
}

/// How the window was answered, and what it was asked for.
#[derive(Debug)]
pub enum Answered<T> {
    /// Open the document: page `page` where a page was asked, with
    /// `password` where one was.
    Open {
        /// What the window was asked for.
        ticket: T,
        /// The page chosen, counting from 0.
        page: Option<u32>,
        /// The password given, erased when dropped.
        password: Option<WipedBuf>,
    },
    /// Turned down: the document is not opened.
    Cancelled(T),
}

/// The window, asking for `T`.
pub struct OpenPdf<T> {
    title: String,
    asked: Asked,
    dialog: Dialog,
    group: FieldGroup,
    ticket: Option<T>,
}

impl<T> OpenPdf<T> {
    /// The window asking `asked` of the document named `name`, for `ticket`.
    #[must_use]
    pub fn new(name: &str, asked: Asked, ticket: T) -> Self {
        let (message, row) = match &asked {
            Asked::Page { count, sizes } => {
                let most = i32::try_from(*count).unwrap_or(i32::MAX);
                let mut row =
                    FieldRow::new("Page", FieldControl::Number(NumberField::new(1, 1, most)));
                row.set_description(page_size(sizes, 1));
                (
                    format!("\u{201c}{name}\u{201d} has {count} pages. Which one opens?"),
                    row,
                )
            }
            Asked::Password { wrong } => (
                if *wrong {
                    format!("That password does not open \u{201c}{name}\u{201d}. Try another.")
                } else {
                    format!("\u{201c}{name}\u{201d} is protected by a password.")
                },
                FieldRow::new(
                    "Password",
                    FieldControl::Secret(SecretField::new(MAX_PASSWORD_BYTES)),
                ),
            ),
        };
        let title = format!("Open \u{201c}{name}\u{201d}");
        let dialog = Dialog::new(title.as_str())
            .in_window()
            .with_message(message)
            .with_actions(vec![
                Button::labelled("Cancel"),
                Button::new(
                    ButtonContent::Label(String::from("Open")),
                    ControlRole::Recommended,
                ),
            ]);
        let mut group = FieldGroup::new("", vec![row]);
        group.adopt_focus(Some(0));
        Self {
            title,
            asked,
            dialog,
            group,
            ticket: Some(ticket),
        }
    }

    /// What it was asked for, taken: a window closed unanswered hands it
    /// back once.
    pub fn take_ticket(&mut self) -> Option<T> {
        self.ticket.take()
    }

    /// Its form's rows laid out in a window `bounds`.
    fn rows_layout(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<FieldLayout> {
        let content = self.dialog.content_rect(bounds, scale, theme)?;
        Some(self.group.layout(content, bounds, scale, theme))
    }

    /// The window's content drawn into `surface`, laid out over `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        self.dialog.render(surface, bounds, scale, theme);
        if let Some(layout) = self.rows_layout(bounds, scale, theme) {
            self.group.render(surface, layout, scale, theme);
        }
    }

    /// The answer the window gives now, `confirmed` or turned down: the
    /// ticket handed back, and the page or password read from the form. A
    /// confirmation the form holds nothing to open with answers nothing, and
    /// the form or the dialog says why.
    fn answer(
        &mut self,
        confirmed: bool,
        bounds: Rect,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Answered<T>>> {
        if self.ticket.is_none() {
            return Outcome::none();
        }
        if !confirmed {
            return self.answered(Answered::Cancelled);
        }
        let control = self.group.rows_mut().first_mut().map(FieldRow::control_mut);
        match control {
            Some(FieldControl::Number(field)) => {
                // An invalid entry blocks the action: the page typed out of
                // range is held to the range and shown, not opened unseen.
                if field.state().validation == ValidationState::Invalid {
                    let _ = field.commit(bounds, damage);
                    let page = field.value();
                    return self.describe(page, bounds, damage);
                }
                let page = u32::try_from(field.value() - 1).ok();
                self.answered(|ticket| Answered::Open {
                    ticket,
                    page,
                    password: None,
                })
            }
            Some(FieldControl::Secret(field)) => match typed_password(field) {
                Ok(password) => self.answered(|ticket| Answered::Open {
                    ticket,
                    page: None,
                    password: Some(password),
                }),
                Err(unoffered) => {
                    // Nothing typed is ever shown, so too much is typed again
                    // whole rather than trimmed unseen.
                    if unoffered == Unoffered::TooLong {
                        field.clear();
                        damage.add(bounds);
                    }
                    self.refuse(Some(unoffered.why()))
                }
            },
            _ => Outcome::none(),
        }
    }

    /// The ticket handed back in the answer `answer` makes of it.
    fn answered(
        &mut self,
        answer: impl FnOnce(T) -> Answered<T>,
    ) -> Outcome<AppRequest<Answered<T>>> {
        self.ticket.take().map_or_else(Outcome::none, |ticket| {
            Outcome::asking(AppRequest::Own(answer(ticket)))
        })
    }

    /// The dialog's reason for doing nothing set to `why`, the window laid
    /// out again where that moved its bands.
    fn refuse(&mut self, why: Option<&str>) -> Outcome<AppRequest<Answered<T>>> {
        if self.dialog.set_reason(why.map(String::from)) {
            Outcome::relaid()
        } else {
            Outcome::none()
        }
    }

    /// Restate page `page`'s size beside its number, the window laid out
    /// again where that changed the row.
    fn describe(
        &mut self,
        page: i32,
        bounds: Rect,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Answered<T>>> {
        let (Asked::Page { sizes, .. }, Some(row)) =
            (&self.asked, self.group.rows_mut().first_mut())
        else {
            return Outcome::none();
        };
        if !row.set_description(page_size(sizes, page)) {
            return Outcome::none();
        }
        damage.add(bounds);
        Outcome::reshaped()
    }

    /// Follow what the form's row reported: a page's size restated as its
    /// number moves, and the reason for refusing a password taken down once
    /// another is typed.
    fn follow(
        &mut self,
        acted: &FieldGroupAction,
        bounds: Rect,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Answered<T>>> {
        match acted.action {
            FieldAction::Number(action) => self.describe(action.value(), bounds, damage),
            FieldAction::Text(TextAction::Edited) => self.refuse(None),
            _ => Outcome::none(),
        }
    }
}

/// Why a password typed cannot be offered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Unoffered {
    /// None was typed, and the empty password was tried before asking.
    Empty,
    /// More was typed than any password can be.
    TooLong,
    /// There was no memory to hold it.
    NoMemory,
}

impl Unoffered {
    /// The reason the dialog gives.
    const fn why(self) -> &'static str {
        match self {
            Self::Empty => "Type the document\u{2019}s password.",
            Self::TooLong => "That is longer than any password can be. Type it again.",
            Self::NoMemory => "There is not enough memory to hold the password.",
        }
    }
}

/// The password typed in `field`, held where it is erased when dropped.
fn typed_password(field: &SecretField) -> Result<WipedBuf, Unoffered> {
    if field.is_empty() {
        return Err(Unoffered::Empty);
    }
    let typed = field.secret().ok_or(Unoffered::TooLong)?;
    let mut held = WipedBuf::with_capacity(typed.len()).map_err(|_| Unoffered::NoMemory)?;
    held.extend_zeroed(typed.len())
        .ok_or(Unoffered::NoMemory)?
        .copy_from_slice(typed.as_bytes());
    Ok(held)
}

/// Page `page`'s size as its row describes it, where it was listed.
fn page_size(sizes: &[(f64, f64)], page: i32) -> Option<String> {
    let listed = usize::try_from(page - 1)
        .ok()
        .and_then(|index| sizes.get(index));
    listed.map(|&(width, height)| format!("{} \u{d7} {} points", whole(width), whole(height)))
}

/// A page side, to the nearest point.
fn whole(side: f64) -> i32 {
    tairix_util::mathf::round_i32(side)
}

impl<T> AppView for OpenPdf<T> {
    type Layout = Rect;
    type Faces = Faces;
    type Own = Answered<T>;

    fn title(&self) -> &str {
        &self.title
    }

    fn size(&self) -> (u32, u32) {
        (WIDTH, HEIGHT)
    }

    fn layout(&self, width: u32, height: u32, _: &Theme, _: Scale, _: Faces) -> Rect {
        Rect::new(0, 0, width, height)
    }

    fn min_size(&self, theme: &Theme, scale: Scale, _: Faces) -> (u32, u32) {
        let width = scale.scale_length(WIDTH);
        let band = self.dialog.content_width(width, scale, theme);
        let column = self.group.slot_column(band, scale, theme);
        let content = self.group.measured_height(band, column, scale, theme);
        (
            width,
            self.dialog.height_for_content(content, width, scale, theme),
        )
    }

    fn settle(&mut self, layout: &Rect, damage: &mut Region) {
        damage.add(*layout);
    }

    fn cursor(&self, _: &Rect, _: Point) -> CursorShape {
        CursorShape::Arrow
    }

    fn focus_changed(&mut self, _: bool, layout: &Rect, damage: &mut Region) {
        damage.add(*layout);
    }

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        layout: &Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Answered<T>>> {
        let bounds = *layout;
        let Some(rows) = self.rows_layout(bounds, scale, theme) else {
            return Outcome::none();
        };
        match *input {
            InputEvent::KeyPressed { key, modifiers } => match key {
                Key::Named(NamedKey::Escape) => self.answer(false, bounds, damage),
                Key::Named(NamedKey::Enter) => self.answer(true, bounds, damage),
                _ => {
                    let stroke = Keystroke {
                        key,
                        modifiers,
                        at_ns: now_ns,
                    };
                    match self.group.on_key(stroke, rows, scale, theme, damage) {
                        Some(acted) => self.follow(&acted, bounds, damage),
                        None => Outcome::none(),
                    }
                }
            },
            InputEvent::KeyReleased { .. } => Outcome::none(),
            _ => match self.group.on_pointer(input, rows, scale, theme, damage) {
                Some(acted) => self.follow(&acted, bounds, damage),
                None => match self.dialog.on_pointer(input, bounds, scale, theme, damage) {
                    Some(DialogAction::ActionActivated { index }) => {
                        self.answer(index == 1, bounds, damage)
                    }
                    None => Outcome::none(),
                },
            },
        }
    }

    fn deadline_ns(&mut self, _: u64) -> Option<u64> {
        self.group.deadline_ns()
    }

    fn advance(
        &mut self,
        now_ns: u64,
        layout: &Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        // Laid out nowhere, the rows' clocks still move: a deadline left
        // passed would wake the window without end.
        let rows = self
            .rows_layout(*layout, scale, theme)
            .unwrap_or_else(|| self.group.layout(Rect::default(), *layout, scale, theme));
        self.group.advance(now_ns, rows, (scale, theme), damage);
    }
}

#[cfg(test)]
#[path = "pdfopen_tests.rs"]
mod tests;
