//! The window a PDF is opened through when it asks something first: which of
//! its pages, or its password. Each is a form in a dialog window of its own,
//! answered or turned down; what it is for travels through it untouched. A
//! page's size is shown beside its number once it is known, each asked for as
//! the number comes to it.

use alloc::format;
use alloc::string::String;
use alloc::vec;

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
use tairix_util::utf8::shown;
use tairix_window::docapp::{AppRequest, AppView, Outcome, Relayout};

use crate::layout::Faces;

/// The window's width, in logical pixels.
const WIDTH: u32 = 440;

/// The height it opens at, in logical pixels; it is held to what its form
/// needs when that is more.
const HEIGHT: u32 = 210;

/// What the window asks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Asked {
    /// Which page: the document's count, and its first page's width and
    /// height, in points, where it could be had.
    Page {
        /// How many pages it has.
        count: u32,
        /// The first page's width and height.
        first: Option<(f64, f64)>,
    },
    /// Its password, `wrong` where one given did not open it.
    Password {
        /// The password last given was wrong.
        wrong: bool,
    },
}

/// What the window was answered with.
#[derive(Debug)]
pub enum Given {
    /// The page chosen, counting from 0.
    Page(u32),
    /// The password typed, erased when dropped.
    Password(WipedBuf),
}

/// How the window was answered, and what it was asked for.
#[derive(Debug)]
pub enum Answered<T> {
    /// Open the document with what was given.
    Open {
        /// What the window was asked for.
        ticket: T,
        /// The page or the password.
        given: Given,
    },
    /// The size of a page, counting from 0, is wanted beside its number; the
    /// window stays.
    Size(u32),
    /// Turned down: the document is not opened.
    Cancelled(T),
}

/// The page sizes a page question learns as its number moves.
struct Sizes {
    /// The page whose size is known, counting from 0, and that size.
    known: Option<(u32, (f64, f64))>,
    /// The page whose size is being fetched.
    asking: Option<u32>,
    /// The page whose size was last refused, not asked for again until
    /// another is chosen.
    refused: Option<u32>,
}

/// The window, asking for `T`.
pub struct OpenPdf<T> {
    title: String,
    /// The whole window, where its dialog is laid out.
    bounds: Rect,
    /// Its form's rows as last laid out.
    rows: Option<FieldLayout>,
    /// A page question's sizes; a password question has none.
    sizes: Option<Sizes>,
    dialog: Dialog,
    group: FieldGroup,
    ticket: Option<T>,
}

impl<T> OpenPdf<T> {
    /// The window asking `asked` of the document named `name`, for `ticket`.
    #[must_use]
    pub fn new(name: &str, asked: Asked, ticket: T) -> Self {
        let name = shown(name);
        let (message, row, sizes) = match asked {
            Asked::Page { count, first } => {
                let most = i32::try_from(count).unwrap_or(i32::MAX);
                let mut row =
                    FieldRow::new("Page", FieldControl::Number(NumberField::new(1, 1, most)));
                row.set_description(first.map(page_size));
                let sizes = Sizes {
                    known: first.map(|size| (0, size)),
                    asking: None,
                    refused: first.is_none().then_some(0),
                };
                (
                    format!("\u{201c}{name}\u{201d} has {count} pages. Which one opens?"),
                    row,
                    Some(sizes),
                )
            }
            Asked::Password { wrong } => (
                if wrong {
                    format!("That password does not open \u{201c}{name}\u{201d}. Try another.")
                } else {
                    format!("\u{201c}{name}\u{201d} is protected by a password.")
                },
                FieldRow::new(
                    "Password",
                    FieldControl::Secret(SecretField::new(MAX_PASSWORD_BYTES)),
                ),
                None,
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
            bounds: Rect::default(),
            rows: None,
            sizes,
            dialog,
            group,
            ticket: Some(ticket),
        }
    }

    /// What it was asked for, while it is unanswered.
    pub const fn ticket(&self) -> Option<&T> {
        self.ticket.as_ref()
    }

    /// What it was asked for, taken: a window closed unanswered hands it
    /// back once.
    pub fn take_ticket(&mut self) -> Option<T> {
        self.ticket.take()
    }

    /// The window's content drawn into `surface`.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        self.dialog.render(surface, self.bounds, scale, theme);
        if let Some(layout) = self.rows {
            self.group.render(surface, layout, scale, theme);
        }
    }

    /// Page `page`'s size, counting from 0, has come back as `size`, or could
    /// not be had: shown where that page is still the one chosen, and the
    /// chosen one's asked for where it is not.
    pub fn sized(
        &mut self,
        page: u32,
        size: Option<(f64, f64)>,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Answered<T>>> {
        let Some(sizes) = &mut self.sizes else {
            return Outcome::none();
        };
        if sizes.asking != Some(page) {
            return Outcome::none();
        }
        sizes.asking = None;
        match size {
            Some(size) => sizes.known = Some((page, size)),
            None => sizes.refused = Some(page),
        }
        match self.chosen() {
            Some(chosen) => self.describe(chosen, damage),
            None => Outcome::none(),
        }
    }

    /// The page number the form holds, counting from 1.
    fn chosen(&self) -> Option<i32> {
        match self.group.rows().first().map(FieldRow::control) {
            Some(FieldControl::Number(field)) => Some(field.value()),
            _ => None,
        }
    }

    /// The answer the window gives now, `confirmed` or turned down: the
    /// ticket handed back, and the page or password read from the form. A
    /// confirmation the form holds nothing to open with answers nothing, and
    /// the form or the dialog says why.
    fn answer(&mut self, confirmed: bool, damage: &mut Region) -> Outcome<AppRequest<Answered<T>>> {
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
                    let _ = field.commit(self.bounds, damage);
                    let page = field.value();
                    return self.describe(page, damage);
                }
                let Some(page) = page_index(field.value()) else {
                    return Outcome::none();
                };
                self.answered(|ticket| Answered::Open {
                    ticket,
                    given: Given::Page(page),
                })
            }
            Some(FieldControl::Secret(field)) => match typed_password(field) {
                Ok(password) => self.answered(|ticket| Answered::Open {
                    ticket,
                    given: Given::Password(password),
                }),
                Err(unoffered) => {
                    // Nothing typed is ever shown, so too much is typed again
                    // whole rather than trimmed unseen.
                    if unoffered == Unoffered::TooLong {
                        field.clear();
                        damage.add(self.bounds);
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

    /// Restate page `page`'s size beside its number, counting from 1, the
    /// window laid out again where that changed the row: the size where it is
    /// known, and otherwise asked for, one page at a time.
    fn describe(&mut self, page: i32, damage: &mut Region) -> Outcome<AppRequest<Answered<T>>> {
        let (Some(sizes), Some(row)) = (&mut self.sizes, self.group.rows_mut().first_mut()) else {
            return Outcome::none();
        };
        let Some(index) = page_index(page) else {
            return Outcome::none();
        };
        let shown = sizes
            .known
            .filter(|&(known, _)| known == index)
            .map(|(_, size)| page_size(size));
        let ask = shown.is_none() && sizes.asking.is_none() && sizes.refused != Some(index);
        if ask {
            sizes.asking = Some(index);
        }
        let mut outcome = Outcome {
            request: ask.then(|| AppRequest::Own(Answered::Size(index))),
            relayout: Relayout::None,
        };
        if row.set_description(shown) {
            damage.add(self.bounds);
            outcome.relayout = Relayout::Reported;
        }
        outcome
    }

    /// Follow what the form's row reported: a page's size restated as its
    /// number moves, and the reason for refusing a password taken down once
    /// another is typed.
    fn follow(
        &mut self,
        acted: &FieldGroupAction,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Answered<T>>> {
        match acted.action {
            FieldAction::Number(action) => self.describe(action.value(), damage),
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

/// The page a page number names, counting from 0.
fn page_index(number: i32) -> Option<u32> {
    u32::try_from(number.checked_sub(1)?).ok()
}

/// A page's width and height as its row describes them.
fn page_size((width, height): (f64, f64)) -> String {
    format!("{} \u{d7} {} points", whole(width), whole(height))
}

/// A page side, to the nearest point.
fn whole(side: f64) -> i32 {
    tairix_util::mathf::round_i32(side)
}

impl<T> AppView for OpenPdf<T> {
    type Faces = Faces;
    type Own = Answered<T>;

    fn title(&self) -> &str {
        &self.title
    }

    fn size(&self) -> (u32, u32) {
        (WIDTH, HEIGHT)
    }

    fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        _: Faces,
        damage: &mut Region,
    ) {
        self.bounds = Rect::new(0, 0, width, height);
        self.rows = self
            .dialog
            .content_rect(self.bounds, scale, theme)
            .map(|content| self.group.layout(content, self.bounds, scale, theme));
        damage.add(self.bounds);
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

    fn cursor(&self, _: Point) -> CursorShape {
        CursorShape::Arrow
    }

    fn focus_changed(&mut self, _: bool, damage: &mut Region) {
        damage.add(self.bounds);
    }

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome<AppRequest<Answered<T>>> {
        let Some(rows) = self.rows else {
            return Outcome::none();
        };
        match *input {
            InputEvent::KeyPressed { key, modifiers } => match key {
                Key::Named(NamedKey::Escape) => self.answer(false, damage),
                Key::Named(NamedKey::Enter) => self.answer(true, damage),
                _ => {
                    let stroke = Keystroke {
                        key,
                        modifiers,
                        at_ns: now_ns,
                    };
                    match self.group.on_key(stroke, rows, scale, theme, damage) {
                        Some(acted) => self.follow(&acted, damage),
                        None => Outcome::none(),
                    }
                }
            },
            InputEvent::KeyReleased { .. } => Outcome::none(),
            _ => match self.group.on_pointer(input, rows, scale, theme, damage) {
                Some(acted) => self.follow(&acted, damage),
                None => match self
                    .dialog
                    .on_pointer(input, self.bounds, scale, theme, damage)
                {
                    Some(DialogAction::ActionActivated { index }) => {
                        self.answer(index == 1, damage)
                    }
                    None => Outcome::none(),
                },
            },
        }
    }

    fn deadline_ns(&mut self, _: u64) -> Option<u64> {
        self.group.deadline_ns()
    }

    fn advance(&mut self, now_ns: u64, scale: Scale, theme: &Theme, damage: &mut Region) {
        // Laid out nowhere, the rows' clocks still move: a deadline left
        // passed would wake the window without end.
        let rows = self.rows.unwrap_or_else(|| {
            self.group
                .layout(Rect::default(), self.bounds, scale, theme)
        });
        self.group.advance(now_ns, rows, (scale, theme), damage);
    }
}

#[cfg(test)]
#[path = "pdfopen_tests.rs"]
mod tests;
