//! Draw's windows that hold no document: its settings, and the questions a
//! PDF asks before it opens. `T` is what a PDF's question is asked for.

use tairix_abi::window_ipc::CursorShape;
use tairix_geometry::{Point, Region, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;
use tairix_window::docapp::settings::SettingsRequest;
use tairix_window::docapp::{AppRequest, AppView, Outcome};

use crate::layout::Faces;
use crate::pdfopen::{Answered, OpenPdf};
use crate::preferences::Preferences;
use crate::settings::DrawSettings;

/// One of Draw's windows that hold no document.
pub enum DrawWindow<T> {
    /// Its settings.
    Settings(DrawSettings),
    /// A PDF's question.
    OpenPdf(OpenPdf<T>),
}

/// What such a window asks Draw for.
pub enum DrawWindowAsk<T> {
    /// What the settings window changed.
    Settings(SettingsRequest<Preferences>),
    /// How a PDF's question was answered.
    OpenPdf(Answered<T>),
}

/// `outcome`, its own request carried as `wrap` makes it.
fn wrapped<R, S>(
    outcome: Outcome<AppRequest<R>>,
    wrap: impl FnOnce(R) -> S,
) -> Outcome<AppRequest<S>> {
    Outcome {
        request: outcome.request.map(|request| match request {
            AppRequest::Close => AppRequest::Close,
            AppRequest::Own(own) => AppRequest::Own(wrap(own)),
        }),
        relayout: outcome.relayout,
    }
}

impl<T> AppView for DrawWindow<T> {
    type Faces = Faces;
    type Own = DrawWindowAsk<T>;

    fn title(&self) -> &str {
        match self {
            Self::Settings(window) => window.title(),
            Self::OpenPdf(window) => window.title(),
        }
    }

    fn size(&self) -> (u32, u32) {
        match self {
            Self::Settings(window) => window.size(),
            Self::OpenPdf(window) => window.size(),
        }
    }

    fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
        damage: &mut Region,
    ) {
        match self {
            Self::Settings(window) => window.lay_out(width, height, theme, scale, faces, damage),
            Self::OpenPdf(window) => window.lay_out(width, height, theme, scale, faces, damage),
        }
    }

    fn min_size(&self, theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        match self {
            Self::Settings(window) => window.min_size(theme, scale, faces),
            Self::OpenPdf(window) => window.min_size(theme, scale, faces),
        }
    }

    fn cursor(&self, at: Point) -> CursorShape {
        match self {
            Self::Settings(window) => window.cursor(at),
            Self::OpenPdf(window) => window.cursor(at),
        }
    }

    fn focus_changed(&mut self, focused: bool, damage: &mut Region) {
        match self {
            Self::Settings(window) => window.focus_changed(focused, damage),
            Self::OpenPdf(window) => window.focus_changed(focused, damage),
        }
    }

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome<AppRequest<DrawWindowAsk<T>>> {
        match self {
            Self::Settings(window) => wrapped(
                window.input(input, now_ns, scale, theme, damage),
                DrawWindowAsk::Settings,
            ),
            Self::OpenPdf(window) => wrapped(
                window.input(input, now_ns, scale, theme, damage),
                DrawWindowAsk::OpenPdf,
            ),
        }
    }

    fn deadline_ns(&mut self, now_ns: u64) -> Option<u64> {
        match self {
            Self::Settings(window) => window.deadline_ns(now_ns),
            Self::OpenPdf(window) => window.deadline_ns(now_ns),
        }
    }

    fn advance(&mut self, now_ns: u64, scale: Scale, theme: &Theme, damage: &mut Region) {
        match self {
            Self::Settings(window) => window.advance(now_ns, scale, theme, damage),
            Self::OpenPdf(window) => window.advance(now_ns, scale, theme, damage),
        }
    }
}
