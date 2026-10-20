//! [`AuthoriseSheet`]: the system's question before a program runs with
//! authority the signed-in account may not hold.
//!
//! The sheet asks for exactly what the authority needs and nothing it already
//! knows. When the signed-in account may allow the program, the sheet shows
//! that account and asks only for its password. When it may not, it lists the
//! accounts that may — the authority answered which, so a tile is never one
//! that could not succeed — and asks for the password of the one chosen. When
//! none may, it says so and asks for nothing.
//!
//! It is a control and nothing more: an embedder gives it events and a
//! rectangle, takes back a painted sheet and an [`AuthoriseAction`], and
//! performs the exchange with the authority itself. What makes the question
//! the system's — the screen dimmed behind it, the seat held, a key no
//! application receives — is the embedder's, because none of it can be done
//! from inside a surface.
//!
//! The password is typed into a [`SecretField`], so it is never drawn, never
//! reallocated while filling, and erased on every path out of the sheet.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::account::MAX_PASSWORD_LEN;
use tairix_abi::driver::display::RefreshRate;
use tairix_abi::time::Duration64;
use tairix_controls::{
    blend_area, paint_icon_slot, paint_run, paint_surface_plate, plate_border, ArtworkTone, Button,
    ButtonContent, ChromeLayer, ControlRole, FocusState, Keystroke, ListRow, SecretField,
    SelectionState, TextAction, ValidationState,
};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::{monogram_disc, IconKind, IconPicture};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme, Timeline};

use crate::chooser::AccountTile;
use crate::motion::Shake;
use crate::surface::{cooldown_notice, rejected_ms, CHECKING, HINT, REFUSED, UNREACHABLE};

/// The sheet's width in logical pixels.
pub const AUTHORISE_WIDTH: u32 = 440;

/// What the band along the sheet's top says: whose question this is.
pub const AUTHORISE_TITLE: &str = "System authorisation";

/// What the sheet says while the authority is asked who may allow the
/// request.
const FINDING: &str = "Finding the accounts that may allow this\u{2026}";

/// What the chooser says over the accounts that may.
const CHOOSE: &str = "Your account may not allow this. Choose one that may:";

/// What the sheet says when no account may.
const NOBODY: &str = "No account on this computer may allow this.";

/// The line under [`NOBODY`]: who can change that.
const NOBODY_DETAIL: &str = "Whoever looks after this computer can let an account allow it.";

/// What the sheet says when the secure attention key finds no question
/// waiting.
const IDLE: &str = "Nothing from the system is waiting for a password.";

/// The line under [`IDLE`]: what that means for whatever just asked.
const IDLE_DETAIL: &str = "If something asked you for one just now, it was not the system.";

/// What the sheet says when the authority could not say who may.
const UNAVAILABLE: &str = "The system could not say who may allow this just now.";

/// What the field says when the password was accepted but nothing could be
/// done with it, so nobody is told to check a password that was right.
const NOT_DONE: &str = "The password was accepted, but this could not be done";

/// The reminder that one key always reaches the system, which no
/// application's imitation of this sheet can make true.
pub const TRUSTED_PATH: &str = "Ctrl+Alt+Delete always reaches the system";

/// The link from the signed-in account to the others that may.
const ANOTHER: &str = "Use another account\u{2026}";

/// Side of the requester's picture, in logical pixels.
pub const REQUESTER_SIDE: u32 = 48;

/// Side of the asked-for account's disc, in logical pixels.
const DISC_SIDE: u32 = 40;

/// Height of one account row on the chooser, in logical pixels.
const ROW_HEIGHT: u32 = 40;

/// The most account rows in view at once; the rest are scrolled to.
pub(crate) const ROWS_IN_VIEW: usize = 4;

/// Height of the system band, in logical pixels.
const BAND_HEIGHT: u32 = 40;

/// The sheet's inner margin, in logical pixels.
const PAD: u32 = 20;

/// The gap between the sheet's sections, in logical pixels.
const SECTION: u32 = 16;

/// The gap between a picture and the text beside it, in logical pixels.
const BESIDE: u32 = 12;

/// The most lines the request is set in before it is cut.
const ACTION_LINES: usize = 3;

/// How long the band answers the secure attention key, in milliseconds.
const ATTEND_MS: u16 = 600;

/// How strongly the band brightens while it answers the key.
const ATTEND_GLOW: u8 = 64;

/// What the user concluded on the sheet.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AuthoriseAction {
    /// Nothing is to be allowed. The embedder takes the sheet down; dropping
    /// it erases whatever was typed.
    Cancelled,
    /// A password was offered for [`AuthoriseSheet::account`]. The embedder
    /// reads [`AuthoriseSheet::secret`], asks the authority, and calls
    /// [`AuthoriseSheet::check`].
    Offered,
}

/// What the sheet is asking.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Mode {
    /// Waiting to hear which accounts may allow the request.
    Finding,
    /// The signed-in account may: its password is asked for.
    Own,
    /// It may not: the accounts that may are offered.
    Choosing,
    /// The other account at this index was chosen: its password is asked for.
    Chosen(usize),
    /// No account may.
    Nobody,
    /// The authority could not say.
    Unavailable,
    /// Nothing is being asked: the secure attention key was pressed with no
    /// question waiting.
    Idle,
}

/// Which part of the sheet holds the keyboard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Focus {
    Field,
    Another,
    Rows,
    Back,
    Cancel,
    Allow,
    Close,
}

/// The line under the password field.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Notice {
    Hint,
    Checking,
    Refused,
    Unreachable,
    NotDone,
    Cooling(i64),
}

impl Notice {
    fn text(&self) -> String {
        match self {
            Self::Hint => HINT.to_string(),
            Self::Checking => CHECKING.to_string(),
            Self::Refused => REFUSED.to_string(),
            Self::Unreachable => UNREACHABLE.to_string(),
            Self::NotDone => NOT_DONE.to_string(),
            Self::Cooling(secs) => cooldown_notice(Duration64::from_secs(*secs)),
        }
    }

    const fn validation(&self) -> ValidationState {
        match self {
            Self::Hint | Self::Checking => ValidationState::Valid,
            Self::Refused | Self::Unreachable | Self::NotDone | Self::Cooling(_) => {
                ValidationState::Invalid
            }
        }
    }
}

/// Where each part of the sheet sits within its bounds: the one geometry the
/// paint, the hit test and the damage all read.
struct Layout {
    band: Rect,
    picture: Rect,
    name: Rect,
    action: Rect,
    divider: Rect,
    body: Body,
    hint: Rect,
    buttons: Buttons,
    height: u32,
}

/// Where the buttons the current question offers sit.
struct Buttons {
    back: Option<Rect>,
    cancel: Option<Rect>,
    allow: Option<Rect>,
    close: Option<Rect>,
}

impl Buttons {
    const NONE: Self = Self {
        back: None,
        cancel: None,
        allow: None,
        close: None,
    };
}

/// The left edge and width every part of the sheet is set against, and the
/// faces it is set in.
struct Column {
    left: i32,
    inner: u32,
    scale: Scale,
    heading: BitmapFont,
    body: BitmapFont,
    caption: BitmapFont,
}

impl Column {
    fn len(&self, logical: u32) -> u32 {
        self.scale.scale_length(logical)
    }
}

/// The body's own parts, by what the sheet is asking.
enum Body {
    Message {
        text: Rect,
        detail: Option<Rect>,
    },
    Account {
        disc: Rect,
        name: Rect,
        login: Rect,
        field: Rect,
        notice: Rect,
        another: Option<Rect>,
    },
    Rows {
        heading: Rect,
        rows: Vec<(usize, Rect)>,
    },
}

/// The system's question before a program runs with authority the signed-in
/// account may not hold: whose password allows it, and that password.
pub struct AuthoriseSheet {
    requester: String,
    action: String,
    picture: Option<Surface>,
    own: Option<AccountTile>,
    others: Vec<AccountTile>,
    mode: Mode,
    field: SecretField,
    notice: Notice,
    focus: Focus,
    rows: Vec<ListRow>,
    /// The chooser's focused row, and the first in view.
    row: usize,
    first: usize,
    another: Button,
    back: Button,
    cancel: Button,
    allow: Button,
    close: Button,
    /// The authority is checking what was offered: the sheet takes nothing
    /// until it answers.
    checking: bool,
    shake: Option<Shake>,
    attended: Option<Timeline>,
    pointer: Point,
}

impl AuthoriseSheet {
    /// A sheet for `requester` asking to do `action`, in words, while the
    /// authority is asked who may allow it.
    #[must_use]
    pub fn new(requester: &str, action: &str) -> Self {
        let mut sheet = Self {
            requester: requester.to_string(),
            action: action.to_string(),
            picture: None,
            own: None,
            others: Vec::new(),
            mode: Mode::Finding,
            field: SecretField::new(MAX_PASSWORD_LEN).with_placeholder("Password"),
            notice: Notice::Hint,
            focus: Focus::Cancel,
            rows: Vec::new(),
            row: 0,
            first: 0,
            another: Button::new(
                ButtonContent::Label(ANOTHER.to_string()),
                ControlRole::Navigation,
            ),
            back: Button::labelled("Back"),
            cancel: Button::labelled("Cancel"),
            allow: Button::new(
                ButtonContent::Label("Allow".to_string()),
                ControlRole::Primary,
            ),
            close: Button::labelled("Close"),
            checking: false,
            shake: None,
            attended: None,
            pointer: Point::ORIGIN,
        };
        sheet.set_focus(Focus::Cancel);
        sheet
    }

    /// The sheet the secure attention key brings up when no question is
    /// waiting: the system's own surface, saying that whatever asked for a
    /// password was not the system.
    #[must_use]
    pub fn nothing_waiting() -> Self {
        let mut sheet = Self::new("", "");
        sheet.mode = Mode::Idle;
        sheet.set_focus(Focus::Close);
        sheet
    }

    /// Show `picture` as the requester's, in place of the generic
    /// application glyph. It is drawn at its own size, centred in
    /// [`REQUESTER_SIDE`].
    pub fn set_picture(&mut self, picture: Surface) {
        self.picture = Some(picture);
    }

    /// Adopt the authority's answer: `own` when the signed-in account may
    /// allow the request, and the `others` that may.
    pub fn set_authorisers(&mut self, own: Option<AccountTile>, others: Vec<AccountTile>) {
        self.rows = others
            .iter()
            .map(|account| {
                ListRow::new(account.display_name())
                    .with_icon(IconKind::User)
                    .with_trailing(account.login_name())
            })
            .collect();
        self.others = others;
        self.own = own;
        self.row = 0;
        self.first = 0;
        if self.own.is_some() {
            self.ask(Mode::Own);
        } else if self.others.is_empty() {
            self.mode = Mode::Nobody;
            self.set_focus(Focus::Close);
        } else {
            self.mode = Mode::Choosing;
            self.set_focus(Focus::Rows);
        }
    }

    /// The authority could not say who may allow the request.
    pub fn set_unavailable(&mut self) {
        self.mode = Mode::Unavailable;
        self.checking = false;
        self.set_focus(Focus::Close);
    }

    /// The account whose password is asked for, while one is.
    #[must_use]
    pub fn account(&self) -> Option<&AccountTile> {
        match self.mode {
            Mode::Own => self.own.as_ref(),
            Mode::Chosen(index) => self.others.get(index),
            Mode::Finding | Mode::Choosing | Mode::Nobody | Mode::Unavailable | Mode::Idle => None,
        }
    }

    /// The password as typed, or `None` when more was typed than any
    /// password can be.
    ///
    /// A secret: an embedder reads it to make one request and lets it go.
    #[must_use]
    pub fn secret(&self) -> Option<&str> {
        self.field.secret()
    }

    /// Whether the authority is checking what was offered.
    #[must_use]
    pub const fn is_checking(&self) -> bool {
        self.checking
    }

    /// Say the offered password is being checked, erasing it from the field:
    /// the sheet takes nothing — not even a cancel, which could not recall
    /// what the authority may already be doing — until it is answered.
    pub fn check(&mut self) {
        self.field.clear();
        self.checking = true;
        self.show(Notice::Checking);
    }

    /// The authority refused the password at `now_ns`: say so, shake the
    /// question in `theme`, and ask again.
    pub fn refuse(&mut self, now_ns: u64, theme: &Theme) {
        self.answered(Notice::Refused);
        self.shake = Shake::start(now_ns, rejected_ms(theme));
    }

    /// No answer could be had: say so, and ask again. Nothing was refused, so
    /// nothing shakes.
    pub fn unreachable(&mut self) {
        self.answered(Notice::Unreachable);
    }

    /// The password was accepted but what it allowed could not be done.
    pub fn not_done(&mut self) {
        self.answered(Notice::NotDone);
    }

    /// Present what is left of the authority's lockout on the asked-for
    /// account. While it is non-zero the sheet states it and offers nothing;
    /// zero clears it. Answers whether what the sheet draws changed.
    pub fn set_cooldown(&mut self, remaining: Duration64) -> bool {
        let remaining = remaining.max(Duration64::ZERO);
        let notice = if remaining > Duration64::ZERO {
            Notice::Cooling(remaining.secs_rounded_up())
        } else if matches!(self.notice, Notice::Cooling(_)) {
            Notice::Hint
        } else {
            return false;
        };
        if notice == self.notice {
            return false;
        }
        self.checking = false;
        self.show(notice);
        true
    }

    /// The secure attention key was pressed at `now_ns` while the sheet is
    /// up: the band answers, and the keyboard goes to the password, if one is
    /// asked for.
    pub fn attend(&mut self, now_ns: u64) {
        self.attended = Some(Timeline::start(now_ns, ATTEND_MS));
        if self.account().is_some() && !self.checking {
            self.set_focus(Focus::Field);
        }
    }

    /// The width and height the sheet needs at `scale`, in physical pixels:
    /// as tall as what it says, so nothing is cut.
    #[must_use]
    pub fn client_extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let width = scale.scale_length(AUTHORISE_WIDTH);
        let layout = self.layout(Rect::new(0, 0, width, 0), scale, theme);
        (width, layout.height)
    }

    /// When the sheet next needs a frame on a display refreshing at
    /// `refresh` — a shake, the band's answer, or the password marker — as a
    /// relative wait from `now_ns`; `None` while it is still.
    #[must_use]
    pub fn next_frame_in(&self, now_ns: u64, refresh: RefreshRate) -> Option<u64> {
        let marker = self
            .field
            .deadline_ns()
            .map(|due| due.saturating_sub(now_ns));
        [
            self.shake
                .and_then(|shake| shake.next_frame_in(now_ns, refresh)),
            self.attended
                .and_then(|attended| attended.next_frame_in(now_ns, refresh)),
            marker,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Step the sheet's motion to `now_ns`, adding what it redrew to
    /// `damage`.
    pub fn advance(
        &mut self,
        now_ns: u64,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let layout = self.layout(bounds, scale, theme);
        if let Some(mut shake) = self.shake {
            let moved = shake.advance(now_ns);
            self.shake = (!shake.finished(now_ns)).then_some(shake);
            if moved || self.shake.is_none() {
                if let Body::Account { field, .. } = &layout.body {
                    damage.add(shaken(*field, scale));
                }
            }
        }
        if let Some(attended) = self.attended {
            damage.add(layout.band);
            if attended.finished(now_ns) {
                self.attended = None;
            }
        }
        if self.field.advance(now_ns) {
            if let Body::Account { field, .. } = &layout.body {
                damage.add(*field);
            }
        }
    }

    /// Paint the sheet at `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        let layout = self.layout(bounds, scale, theme);
        let palette = theme.palette();
        let Some((x, y)) = bounds.surface_origin() else {
            return;
        };
        let radius = scale
            .scale_length(theme.metrics().window_corner_radius)
            .min(bounds.width / 2)
            .min(layout.height / 2);
        let border = plate_border(theme, scale);
        let _ = paint_surface_plate(
            surface,
            (x, y, bounds.width, layout.height),
            (radius, border),
            theme,
            (palette.surface_raised, ChromeLayer::Plate),
        );
        self.paint_band(surface, &layout, (radius, border), scale, theme);
        if self.mode != Mode::Idle {
            self.paint_requester(surface, &layout, scale, theme);
            blend_area(surface, layout.divider, palette.border);
        }
        match &layout.body {
            Body::Message { text, detail } => {
                self.paint_message(surface, *text, *detail, scale, theme);
            }
            Body::Account {
                disc,
                name,
                login,
                field,
                notice,
                another,
            } => {
                self.paint_account(surface, (*disc, *name, *login), scale, theme);
                let reach = Shake::reach(scale);
                let at = self
                    .shake
                    .map_or(0, |shake| shake.offset(scale, (reach, reach)));
                let field_at = Rect::new(
                    field.left().saturating_add(at),
                    field.top(),
                    field.width,
                    field.height,
                );
                self.field.render(surface, field_at, scale, theme);
                self.paint_notice(surface, *notice, scale, theme);
                if let Some(another) = another {
                    self.another.render(surface, *another, scale, theme);
                }
            }
            Body::Rows { heading, rows } => {
                let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
                paint_run(
                    surface,
                    font,
                    font.elide_to_width(CHOOSE, heading.width),
                    (heading.left(), heading.top()),
                    Color::from(palette.on_surface),
                    None,
                );
                self.paint_rows(surface, rows, scale, theme);
            }
        }
        let caption = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        paint_run(
            surface,
            caption,
            caption.elide_to_width(TRUSTED_PATH, layout.hint.width),
            (layout.hint.left(), layout.hint.top()),
            Color::from(palette.on_surface_muted),
            None,
        );
        for (button, rect) in [
            (&self.back, layout.buttons.back),
            (&self.cancel, layout.buttons.cancel),
            (&self.allow, layout.buttons.allow),
            (&self.close, layout.buttons.close),
        ] {
            if let Some(rect) = rect {
                button.render(surface, rect, scale, theme);
            }
        }
    }

    /// Apply one key press to a sheet drawn at `bounds`.
    ///
    /// `Escape` steps back from a chosen account to the chooser, and
    /// otherwise cancels. `Tab` and `Shift-Tab` walk the controls; the arrow
    /// keys walk the accounts. `Enter` offers the password from the field,
    /// chooses the focused account, or presses the focused button. A sheet
    /// that is checking takes nothing.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<AuthoriseAction> {
        if self.checking {
            return None;
        }
        let layout = self.layout(bounds, scale, theme);
        match stroke.key {
            Key::Named(NamedKey::Escape) => {
                if let Mode::Chosen(index) = self.mode {
                    self.choose_again(index);
                    damage.add(bounds);
                    return None;
                }
                Some(AuthoriseAction::Cancelled)
            }
            Key::Named(NamedKey::Tab) => {
                self.step_focus(!stroke.modifiers.shift);
                damage.add(bounds);
                None
            }
            Key::Named(NamedKey::Up | NamedKey::Down) if self.focus == Focus::Rows => {
                let down = stroke.key == Key::Named(NamedKey::Down);
                self.move_row(down);
                damage.add(bounds);
                None
            }
            Key::Named(NamedKey::Enter) | Key::Char(' ') if self.focus != Focus::Field => {
                damage.add(bounds);
                self.activate(self.focus)
            }
            _ if self.focus == Focus::Field => {
                let Body::Account { field, .. } = layout.body else {
                    return None;
                };
                match self.field.on_key(stroke, field, theme, damage) {
                    Some(TextAction::Submitted) => {
                        damage.add(bounds);
                        self.offer()
                    }
                    Some(TextAction::Cancelled) => Some(AuthoriseAction::Cancelled),
                    Some(TextAction::Edited) => {
                        if self.notice != Notice::Hint && !matches!(self.notice, Notice::Cooling(_))
                        {
                            self.show(Notice::Hint);
                            if let Body::Account { notice, .. } = layout.body {
                                damage.add(notice);
                            }
                        }
                        None
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Apply one pointer event to a sheet drawn at `bounds`: a press on the
    /// field gives it the keyboard, a click on an account chooses it, and a
    /// completed click on a button presses it. The wheel scrolls the
    /// accounts.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<AuthoriseAction> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if self.checking {
            return None;
        }
        let layout = self.layout(bounds, scale, theme);
        if let InputEvent::PointerScrolled { dy, .. } = event {
            if matches!(layout.body, Body::Rows { .. }) {
                self.scroll_rows(*dy);
                damage.add(bounds);
            }
            return None;
        }
        match &layout.body {
            Body::Account { field, another, .. } => {
                if matches!(event, InputEvent::PointerPressed { .. })
                    && field.contains(self.pointer)
                {
                    self.set_focus(Focus::Field);
                    damage.add(bounds);
                }
                let _ = self.field.on_pointer(event, *field, scale, theme, damage);
                if let Some(another) = another {
                    if self.another.on_pointer(event, *another, damage).is_some() {
                        self.mode = Mode::Choosing;
                        self.set_focus(Focus::Rows);
                        damage.add(bounds);
                        return None;
                    }
                }
            }
            Body::Rows { rows, .. } => {
                if let Some(chosen) = self.on_rows(event, rows, damage) {
                    self.ask(Mode::Chosen(chosen));
                    damage.add(bounds);
                    return None;
                }
            }
            Body::Message { .. } => {}
        }
        let buttons = [
            (Focus::Back, layout.buttons.back),
            (Focus::Cancel, layout.buttons.cancel),
            (Focus::Allow, layout.buttons.allow),
            (Focus::Close, layout.buttons.close),
        ];
        for (focus, rect) in buttons {
            let Some(rect) = rect else {
                continue;
            };
            if self
                .button_mut(focus)
                .on_pointer(event, rect, damage)
                .is_some()
            {
                damage.add(bounds);
                return self.activate(focus);
            }
        }
        None
    }

    /// The physical rectangle of the password field within a sheet drawn at
    /// `bounds`, while one is asked for — public so an observer clicks where
    /// the field is rather than restating the layout.
    #[must_use]
    pub fn field_rect(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
        match self.layout(bounds, scale, theme).body {
            Body::Account { field, .. } => Some(field),
            Body::Message { .. } | Body::Rows { .. } => None,
        }
    }

    /// The physical rectangles of the accounts in view on the chooser, each
    /// with the index of the account it offers.
    #[must_use]
    pub fn row_rects(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Vec<(usize, Rect)> {
        match self.layout(bounds, scale, theme).body {
            Body::Rows { rows, .. } => rows,
            Body::Message { .. } | Body::Account { .. } => Vec::new(),
        }
    }

    /// What the line under the field states, for an observer.
    #[must_use]
    pub fn stated(&self) -> String {
        self.notice.text()
    }

    /// Ask for the password of the account `mode` names, on an empty field.
    fn ask(&mut self, mode: Mode) {
        self.mode = mode;
        self.field.clear();
        self.notice = Notice::Hint;
        self.field
            .set_state(self.field.state().with_validation(ValidationState::Valid));
        self.set_focus(Focus::Field);
    }

    /// Back from the account at `index` to the chooser, focused on it.
    fn choose_again(&mut self, index: usize) {
        self.field.clear();
        self.mode = Mode::Choosing;
        self.row = index;
        self.set_focus(Focus::Rows);
    }

    /// An answer that leaves the question up, stating `notice` over an empty
    /// field holding the keyboard.
    fn answered(&mut self, notice: Notice) {
        self.checking = false;
        self.field.clear();
        self.show(notice);
        self.set_focus(Focus::Field);
    }

    fn show(&mut self, notice: Notice) {
        self.field
            .set_state(self.field.state().with_validation(notice.validation()));
        self.notice = notice;
    }

    /// Offer what was typed, unless there is nothing to offer, it could
    /// never be a password, or the account is cooling down.
    fn offer(&mut self) -> Option<AuthoriseAction> {
        if self.account().is_none() || matches!(self.notice, Notice::Cooling(_)) {
            return None;
        }
        if self.field.is_empty() {
            return None;
        }
        if self.field.secret().is_none() {
            self.answered(Notice::Refused);
            return None;
        }
        Some(AuthoriseAction::Offered)
    }

    /// Press the control `focus` names.
    fn activate(&mut self, focus: Focus) -> Option<AuthoriseAction> {
        match focus {
            Focus::Field | Focus::Allow => {
                self.field.submit();
                self.offer()
            }
            Focus::Another => {
                self.mode = Mode::Choosing;
                self.set_focus(Focus::Rows);
                None
            }
            Focus::Rows => {
                if self.row < self.others.len() {
                    self.ask(Mode::Chosen(self.row));
                }
                None
            }
            Focus::Back => {
                if let Mode::Chosen(index) = self.mode {
                    self.choose_again(index);
                }
                None
            }
            Focus::Cancel | Focus::Close => Some(AuthoriseAction::Cancelled),
        }
    }

    /// The focus stops the current mode offers, in tab order.
    fn stops(&self) -> Vec<Focus> {
        match self.mode {
            Mode::Finding => alloc::vec![Focus::Cancel],
            Mode::Own if self.others.is_empty() => {
                alloc::vec![Focus::Field, Focus::Cancel, Focus::Allow]
            }
            Mode::Own => alloc::vec![Focus::Field, Focus::Another, Focus::Cancel, Focus::Allow],
            Mode::Choosing => alloc::vec![Focus::Rows, Focus::Cancel],
            Mode::Chosen(_) => {
                alloc::vec![Focus::Field, Focus::Back, Focus::Cancel, Focus::Allow]
            }
            Mode::Nobody | Mode::Unavailable | Mode::Idle => alloc::vec![Focus::Close],
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let stops = self.stops();
        let at = stops
            .iter()
            .position(|stop| *stop == self.focus)
            .unwrap_or(0);
        let len = stops.len();
        let next = if forward {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        };
        if let Some(stop) = stops.get(next) {
            self.set_focus(*stop);
        }
    }

    /// Give the keyboard to `focus` and take it from everything else, so one
    /// ring is drawn and `Enter` means one thing.
    fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        self.field.set_focused(focus == Focus::Field);
        for candidate in [
            Focus::Another,
            Focus::Back,
            Focus::Cancel,
            Focus::Allow,
            Focus::Close,
        ] {
            self.button_mut(candidate).set_focused(focus == candidate);
        }
        let row = self.row;
        for (index, item) in self.rows.iter_mut().enumerate() {
            let chosen = index == row;
            let mut state = item.state();
            state.focus = FocusState {
                focused: chosen && focus == Focus::Rows,
                in_focus_field: false,
            };
            state.selection = if chosen {
                SelectionState::Selected
            } else {
                SelectionState::Unselected
            };
            item.set_state(state);
        }
    }

    fn button_mut(&mut self, focus: Focus) -> &mut Button {
        match focus {
            Focus::Another => &mut self.another,
            Focus::Back => &mut self.back,
            Focus::Allow => &mut self.allow,
            Focus::Close => &mut self.close,
            Focus::Cancel | Focus::Field | Focus::Rows => &mut self.cancel,
        }
    }

    /// Move the chooser's focus one account up or down, keeping it in view.
    fn move_row(&mut self, down: bool) {
        let last = self.others.len().saturating_sub(1);
        self.row = if down {
            (self.row + 1).min(last)
        } else {
            self.row.saturating_sub(1)
        };
        if self.row < self.first {
            self.first = self.row;
        } else if self.row >= self.first + ROWS_IN_VIEW {
            self.first = self.row + 1 - ROWS_IN_VIEW;
        }
        self.set_focus(Focus::Rows);
    }

    /// Scroll the accounts by `dy` wheel steps, a positive one towards the
    /// later accounts.
    fn scroll_rows(&mut self, dy: i32) {
        let room = self.others.len().saturating_sub(ROWS_IN_VIEW);
        let step = usize::try_from(dy.unsigned_abs()).unwrap_or(usize::MAX);
        self.first = if dy > 0 {
            self.first.saturating_add(step).min(room)
        } else {
            self.first.saturating_sub(step)
        };
    }

    /// Feed a pointer event to the rows in view, answering the account a
    /// completed click chose.
    fn on_rows(
        &mut self,
        event: &InputEvent,
        rows: &[(usize, Rect)],
        damage: &mut Region,
    ) -> Option<usize> {
        let mut chosen = None;
        for (index, rect) in rows {
            if let Some(item) = self.rows.get_mut(*index) {
                if item.on_pointer(event, *rect, damage).is_some() {
                    chosen = Some(*index);
                }
            }
        }
        if let Some(index) = chosen {
            self.row = index;
        }
        chosen
    }

    fn layout(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Layout {
        let fonts = theme.fonts();
        let column = Column {
            left: bounds
                .left()
                .saturating_add(to_i32(scale.scale_length(PAD))),
            inner: bounds
                .width
                .saturating_sub(scale.scale_length(PAD).saturating_mul(2)),
            scale,
            heading: BitmapFont::for_role(fonts, TextRole::ItemTitle, scale),
            body: BitmapFont::for_role(fonts, TextRole::Body, scale),
            caption: BitmapFont::for_role(fonts, TextRole::Caption, scale),
        };
        let band = Rect::new(
            bounds.left(),
            bounds.top(),
            bounds.width,
            column.len(BAND_HEIGHT),
        );
        let top = band.bottom().saturating_add(to_i32(column.len(PAD)));
        let side = column.len(REQUESTER_SIDE);
        let picture = Rect::new(column.left, top, side, side);
        let text_left = picture.right().saturating_add(to_i32(column.len(BESIDE)));
        let text_width = column
            .inner
            .saturating_sub(side.saturating_add(column.len(BESIDE)));
        let name = Rect::new(text_left, top, text_width, column.heading.line_height());
        let action_lines = column
            .body
            .wrap_to_width(&self.action, text_width, ACTION_LINES)
            .count();
        let action = Rect::new(
            text_left,
            name.bottom().saturating_add(to_i32(column.len(4))),
            text_width,
            u32::try_from(action_lines)
                .unwrap_or(0)
                .saturating_mul(column.body.line_height()),
        );
        let divider_top = picture
            .bottom()
            .max(action.bottom())
            .saturating_add(to_i32(column.len(SECTION)));
        let divider = Rect::new(
            column.left,
            divider_top,
            column.inner,
            plate_border(theme, scale),
        );
        // The secure attention key's own notice names no requester: nothing
        // asked, so its body follows the band directly.
        let body_top = if self.mode == Mode::Idle {
            top
        } else {
            divider.bottom().saturating_add(to_i32(column.len(SECTION)))
        };
        let (body, body_bottom) = self.body_layout(&column, body_top, theme);
        let hint = Rect::new(
            column.left,
            body_bottom.saturating_add(to_i32(column.len(SECTION))),
            column.inner,
            column.caption.line_height(),
        );
        let band_top = hint.bottom().saturating_add(to_i32(column.len(BESIDE)));
        let buttons = self.button_layout(&column, bounds, band_top, theme);
        let bottom = band_top
            .saturating_add(to_i32(Button::height(scale, theme)))
            .saturating_add(to_i32(column.len(PAD)));
        Layout {
            band,
            picture,
            name,
            action,
            divider,
            body,
            hint,
            buttons,
            height: u32::try_from(bottom.saturating_sub(bounds.top())).unwrap_or(0),
        }
    }

    /// Where the body's parts sit, from `top`, and where it ends.
    fn body_layout(&self, column: &Column, top: i32, theme: &Theme) -> (Body, i32) {
        let (left, inner) = (column.left, column.inner);
        match self.mode {
            Mode::Finding | Mode::Nobody | Mode::Unavailable | Mode::Idle => {
                let text = Rect::new(left, top, inner, column.body.line_height());
                let detail = matches!(self.mode, Mode::Nobody | Mode::Idle).then(|| {
                    Rect::new(
                        left,
                        text.bottom().saturating_add(to_i32(column.len(4))),
                        inner,
                        column.caption.line_height(),
                    )
                });
                let bottom = detail.map_or(text.bottom(), |detail| detail.bottom());
                (Body::Message { text, detail }, bottom)
            }
            Mode::Own | Mode::Chosen(_) => {
                let side = column.len(DISC_SIDE);
                let disc = Rect::new(left, top, side, side);
                let beside = disc.right().saturating_add(to_i32(column.len(BESIDE)));
                let width = inner.saturating_sub(side.saturating_add(column.len(BESIDE)));
                let pair = column
                    .heading
                    .line_height()
                    .saturating_add(column.caption.line_height());
                let name_top = top.saturating_add(to_i32(side.saturating_sub(pair) / 2));
                let name = Rect::new(beside, name_top, width, column.heading.line_height());
                let login = Rect::new(beside, name.bottom(), width, column.caption.line_height());
                let field = Rect::new(
                    left,
                    disc.bottom().saturating_add(to_i32(column.len(BESIDE))),
                    inner,
                    column.len(theme.metrics().control_height).max(1),
                );
                let notice = Rect::new(
                    left,
                    field.bottom().saturating_add(to_i32(column.len(6))),
                    inner,
                    column.caption.line_height(),
                );
                let another = (self.mode == Mode::Own && !self.others.is_empty()).then(|| {
                    Rect::new(
                        left,
                        notice.bottom().saturating_add(to_i32(column.len(8))),
                        self.another.measured_width(column.scale, theme).min(inner),
                        Button::height(column.scale, theme),
                    )
                });
                let bottom = another.map_or(notice.bottom(), |another| another.bottom());
                let body = Body::Account {
                    disc,
                    name,
                    login,
                    field,
                    notice,
                    another,
                };
                (body, bottom)
            }
            Mode::Choosing => {
                let heading = Rect::new(left, top, inner, column.body.line_height());
                let mut row_top = heading.bottom().saturating_add(to_i32(column.len(8)));
                let height = column.len(ROW_HEIGHT);
                let rows = (self.first..self.others.len())
                    .take(ROWS_IN_VIEW)
                    .map(|index| {
                        let rect = Rect::new(left, row_top, inner, height);
                        row_top = row_top.saturating_add(to_i32(height));
                        (index, rect)
                    })
                    .collect();
                (Body::Rows { heading, rows }, row_top)
            }
        }
    }

    /// Where the buttons along the foot sit at `top`: the decisions at the
    /// right, leading with the one that goes ahead, and Back at the left.
    fn button_layout(&self, column: &Column, bounds: Rect, top: i32, theme: &Theme) -> Buttons {
        let scale = column.scale;
        let height = Button::height(scale, theme);
        let gap = column.len(theme.metrics().control_gap).max(1);
        let floor = column.len(tairix_controls::ALERT_BUTTON_MIN_WIDTH);
        let width_of = |button: &Button| button.measured_width(scale, theme).max(floor);
        let mut cursor = bounds
            .left()
            .saturating_add(to_i32(bounds.width.saturating_sub(column.len(PAD))));
        let mut place = |button: &Button| {
            let width = width_of(button);
            cursor = cursor.saturating_sub(to_i32(width));
            let rect = Rect::new(cursor, top, width, height);
            cursor = cursor.saturating_sub(to_i32(gap));
            rect
        };
        match self.mode {
            Mode::Own | Mode::Chosen(_) => {
                let allow = Some(place(&self.allow));
                let cancel = Some(place(&self.cancel));
                let back = matches!(self.mode, Mode::Chosen(_))
                    .then(|| Rect::new(column.left, top, width_of(&self.back), height));
                Buttons {
                    back,
                    cancel,
                    allow,
                    close: None,
                }
            }
            Mode::Finding | Mode::Choosing => Buttons {
                cancel: Some(place(&self.cancel)),
                ..Buttons::NONE
            },
            Mode::Nobody | Mode::Unavailable | Mode::Idle => Buttons {
                close: Some(place(&self.close)),
                ..Buttons::NONE
            },
        }
    }

    /// The band: the system's padlock and title on the accent, brightened for
    /// a moment when the secure attention key reached it.
    fn paint_band(
        &self,
        surface: &mut Surface,
        layout: &Layout,
        (radius, border): (u32, u32),
        scale: Scale,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let band = layout.band;
        let Some((x, y)) = band.surface_origin() else {
            return;
        };
        let inner_radius = radius.saturating_sub(border);
        let inset = border;
        let width = band.width.saturating_sub(inset.saturating_mul(2));
        let height = band.height.saturating_sub(inset);
        let fill = Color::from(palette.accent);
        surface.fill_round_rect(x + inset, y + inset, width, height, inner_radius, fill);
        // Square the band's foot, which meets the body rather than a corner.
        let rounded = height.saturating_sub(inner_radius);
        surface.fill_rect(
            x + inset,
            y + inset + rounded,
            width,
            height - rounded,
            fill,
        );
        if self.attended.is_some() {
            let glow = Color::from(palette.on_accent.with_alpha(ATTEND_GLOW));
            surface.fill_round_rect(x + inset, y + inset, width, height, inner_radius, glow);
        }
        let ink = Color::from(palette.on_accent);
        let font = BitmapFont::for_role(theme.fonts(), TextRole::SectionHeader, scale);
        let glyph = font.line_height();
        let gx = band.left().saturating_add(to_i32(scale.scale_length(PAD)));
        let gy = band
            .top()
            .saturating_add(to_i32(band.height.saturating_sub(glyph) / 2));
        if let (Ok(gx), Ok(gy)) = (u32::try_from(gx), u32::try_from(gy)) {
            paint_icon_slot(
                surface,
                (gx, gy, glyph),
                IconKind::LockScreen,
                ink,
                None,
                ArtworkTone::FULL,
            );
        }
        let tx = gx.saturating_add(to_i32(glyph.saturating_add(scale.scale_length(8))));
        let room = u32::try_from(band.right().saturating_sub(tx)).unwrap_or(0);
        paint_run(
            surface,
            font,
            font.elide_to_width(AUTHORISE_TITLE, room),
            (tx, font.centred_top(band.top(), band.height)),
            ink,
            None,
        );
    }

    /// The requester: its picture, its name, and what it asks, in words.
    fn paint_requester(&self, surface: &mut Surface, layout: &Layout, scale: Scale, theme: &Theme) {
        let palette = theme.palette();
        if let Some((px, py)) = layout.picture.surface_origin() {
            let slot = (px, py, layout.picture.width);
            let picture = self.picture.as_ref().map(IconPicture::coloured);
            paint_icon_slot(
                surface,
                slot,
                IconKind::AppBundle,
                Color::from(palette.on_surface_muted),
                picture,
                ArtworkTone::FULL,
            );
        }
        let heading = BitmapFont::for_role(theme.fonts(), TextRole::ItemTitle, scale);
        paint_run(
            surface,
            heading,
            heading.elide_to_width(&self.requester, layout.name.width),
            (layout.name.left(), layout.name.top()),
            Color::from(palette.on_surface),
            None,
        );
        let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let mut top = layout.action.top();
        for line in body.wrap_to_width(&self.action, layout.action.width, ACTION_LINES) {
            paint_run(
                surface,
                body,
                line,
                (layout.action.left(), top),
                Color::from(palette.on_surface_muted),
                None,
            );
            top = top.saturating_add(to_i32(body.line_height()));
        }
    }

    fn paint_message(
        &self,
        surface: &mut Surface,
        text: Rect,
        detail: Option<Rect>,
        scale: Scale,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let (said, detailed) = match self.mode {
            Mode::Finding => (FINDING, ""),
            Mode::Nobody => (NOBODY, NOBODY_DETAIL),
            Mode::Idle => (IDLE, IDLE_DETAIL),
            Mode::Own | Mode::Choosing | Mode::Chosen(_) | Mode::Unavailable => (UNAVAILABLE, ""),
        };
        let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        paint_run(
            surface,
            body,
            body.elide_to_width(said, text.width),
            (text.left(), text.top()),
            Color::from(palette.on_surface),
            None,
        );
        if let Some(detail) = detail {
            let caption = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
            paint_run(
                surface,
                caption,
                caption.elide_to_width(detailed, detail.width),
                (detail.left(), detail.top()),
                Color::from(palette.on_surface_muted),
                None,
            );
        }
    }

    /// The asked-for account: its disc, shown name and login name.
    fn paint_account(
        &self,
        surface: &mut Surface,
        (disc, name, login): (Rect, Rect, Rect),
        scale: Scale,
        theme: &Theme,
    ) {
        let Some(account) = self.account() else {
            return;
        };
        let palette = theme.palette();
        let mark = BitmapFont::for_role(theme.fonts(), TextRole::Heading, scale);
        let colours = (Color::from(palette.accent), Color::from(palette.on_accent));
        if let Some(picture) = monogram_disc(account.monogram(), disc.width, mark, colours) {
            surface.blit(disc.left(), disc.top(), &picture);
        }
        let heading = BitmapFont::for_role(theme.fonts(), TextRole::ItemTitle, scale);
        paint_run(
            surface,
            heading,
            heading.elide_to_width(account.display_name(), name.width),
            (name.left(), name.top()),
            Color::from(palette.on_surface),
            None,
        );
        let caption = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        paint_run(
            surface,
            caption,
            caption.elide_to_width(account.login_name(), login.width),
            (login.left(), login.top()),
            Color::from(palette.on_surface_muted),
            None,
        );
    }

    fn paint_notice(&self, surface: &mut Surface, notice: Rect, scale: Scale, theme: &Theme) {
        let palette = theme.palette();
        let colour = match self.notice.validation() {
            ValidationState::Invalid => palette.danger,
            ValidationState::Valid | ValidationState::Warning | ValidationState::Pending => {
                palette.on_surface_muted
            }
        };
        let caption = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        let text = self.notice.text();
        paint_run(
            surface,
            caption,
            caption.elide_to_width(&text, notice.width),
            (notice.left(), notice.top()),
            Color::from(colour),
            None,
        );
    }

    /// The accounts in view, each a row with its disc beside it.
    fn paint_rows(
        &self,
        surface: &mut Surface,
        rows: &[(usize, Rect)],
        scale: Scale,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let mark = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let colours = (Color::from(palette.accent), Color::from(palette.on_accent));
        for (index, rect) in rows {
            let (Some(item), Some(account)) = (self.rows.get(*index), self.others.get(*index))
            else {
                continue;
            };
            let side = rect.height.saturating_mul(3) / 5;
            let disc = monogram_disc(account.monogram(), side, mark, colours);
            item.render(
                surface,
                *rect,
                scale,
                theme,
                disc.as_ref().map(IconPicture::coloured),
            );
        }
    }
}

/// The rectangle a shaken field can reach: its own, widened by the shake on
/// either side.
fn shaken(field: Rect, scale: Scale) -> Rect {
    let reach = Shake::reach(scale);
    Rect::new(
        field.left().saturating_sub(to_i32(reach)),
        field.top(),
        field.width.saturating_add(reach.saturating_mul(2)),
        field.height,
    )
}

impl core::fmt::Debug for AuthoriseSheet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AuthoriseSheet")
            .field("requester", &self.requester)
            .field("mode", &self.mode)
            .field("checking", &self.checking)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "authorise_tests.rs"]
mod tests;
