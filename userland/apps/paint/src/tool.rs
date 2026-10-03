//! The tools, what each is set to, and the strip that chooses them.

use alloc::string::String;

use tairix_controls::{
    ComboBox, ControlRole, ControlState, FieldControl, FieldGroup, FieldRow, IconButton, Slider,
    Toggle, Toolbar,
};
use tairix_icon::IconKind;

/// Something the pointer does to the picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tool {
    /// Mark out a rectangle, and move what it holds.
    Select,
    /// Set single pixels, exactly.
    Pencil,
    /// Paint with a round brush.
    Brush,
    /// Scatter paint.
    Spray,
    /// Rub paint out.
    Eraser,
    /// Fill the area joined to a pixel.
    Fill,
    /// Take a pixel's colour.
    Eyedropper,
    /// Draw a straight line.
    Line,
    /// Draw a rectangle.
    Rectangle,
    /// Draw an ellipse.
    Ellipse,
}

impl Tool {
    /// Every tool, in the order the strip shows them.
    pub const ALL: [Self; 10] = [
        Self::Select,
        Self::Pencil,
        Self::Brush,
        Self::Spray,
        Self::Eraser,
        Self::Fill,
        Self::Eyedropper,
        Self::Line,
        Self::Rectangle,
        Self::Ellipse,
    ];

    /// The glyph the strip draws.
    #[must_use]
    pub const fn icon(self) -> IconKind {
        match self {
            Self::Select => IconKind::ToolSelect,
            Self::Pencil => IconKind::ToolPencil,
            Self::Brush => IconKind::ToolBrush,
            Self::Spray => IconKind::ToolSpray,
            Self::Eraser => IconKind::ToolEraser,
            Self::Fill => IconKind::ToolFill,
            Self::Eyedropper => IconKind::ToolEyedropper,
            Self::Line => IconKind::ToolLine,
            Self::Rectangle => IconKind::ToolRectangle,
            Self::Ellipse => IconKind::ToolEllipse,
        }
    }

    /// What the tool is called, with the key that chooses it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "Select (S)",
            Self::Pencil => "Pencil (P)",
            Self::Brush => "Brush (B)",
            Self::Spray => "Spray (A)",
            Self::Eraser => "Eraser (E)",
            Self::Fill => "Fill (F)",
            Self::Eyedropper => "Eyedropper (I)",
            Self::Line => "Line (L)",
            Self::Rectangle => "Rectangle (R)",
            Self::Ellipse => "Ellipse (O)",
        }
    }

    /// The name alone, as a panel is captioned.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Pencil => "Pencil",
            Self::Brush => "Brush",
            Self::Spray => "Spray",
            Self::Eraser => "Eraser",
            Self::Fill => "Fill",
            Self::Eyedropper => "Eyedropper",
            Self::Line => "Line",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
        }
    }

    /// The tool the letter key `ch` chooses.
    #[must_use]
    pub fn for_key(ch: char) -> Option<Self> {
        Some(match ch.to_ascii_lowercase() {
            's' => Self::Select,
            'p' => Self::Pencil,
            'b' => Self::Brush,
            'a' => Self::Spray,
            'e' => Self::Eraser,
            'f' => Self::Fill,
            'i' => Self::Eyedropper,
            'l' => Self::Line,
            'r' => Self::Rectangle,
            'o' => Self::Ellipse,
            _ => return None,
        })
    }

    /// Whether a drag with the tool is drawn as a shape that follows the
    /// pointer until it lets go.
    #[must_use]
    pub const fn shaped(self) -> bool {
        matches!(self, Self::Line | Self::Rectangle | Self::Ellipse)
    }

    /// The settings the tool's panel offers.
    const fn settings(self) -> &'static [Setting] {
        match self {
            Self::Select | Self::Pencil | Self::Eyedropper => &[],
            Self::Brush | Self::Eraser | Self::Line => &[Setting::Size, Setting::Smooth],
            Self::Spray => &[Setting::Size, Setting::Flow],
            Self::Fill => &[Setting::Tolerance],
            Self::Rectangle => &[Setting::Size, Setting::Style],
            Self::Ellipse => &[Setting::Size, Setting::Style, Setting::Smooth],
        }
    }
}

/// How a shape is drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Style {
    /// Its outline, in the primary colour.
    Outline,
    /// Filled with the primary colour.
    Filled,
    /// Filled with the secondary colour and outlined in the primary.
    Both,
}

impl Style {
    const ALL: [Self; 3] = [Self::Outline, Self::Filled, Self::Both];

    const fn label(self) -> &'static str {
        match self {
            Self::Outline => "Outline",
            Self::Filled => "Filled",
            Self::Both => "Filled and outlined",
        }
    }
}

/// What every tool is set to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// Brush and line width, spray reach, in pixels: `1..=MAX_SIZE`.
    pub size: u32,
    /// How rectangles and ellipses are drawn.
    pub style: Style,
    /// Whether edges are smoothed where the picture can show part of a pixel.
    pub smooth: bool,
    /// How far a colour may differ from the one filled from and still be
    /// filled, per channel.
    pub tolerance: u8,
    /// How thickly the spray lays paint, in percent.
    pub flow: u8,
}

/// The widest a brush is.
pub const MAX_SIZE: u32 = 64;

impl Default for Options {
    fn default() -> Self {
        Self {
            size: 4,
            style: Style::Outline,
            smooth: true,
            tolerance: 0,
            flow: 30,
        }
    }
}

/// One row of a tool's panel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Setting {
    Size,
    Style,
    Smooth,
    Tolerance,
    Flow,
}

/// `value` of `1..=most` as a slider's permille.
fn permille(value: u32, most: u32) -> u16 {
    let span = most.saturating_sub(1).max(1);
    u16::try_from(value.saturating_sub(1).min(span) * 1000 / span).unwrap_or(1000)
}

/// A slider's `permille` as a value of `1..=most`, the nearest whole one.
#[must_use]
pub fn from_permille(permille: u16, most: u32) -> u32 {
    let span = most.saturating_sub(1).max(1);
    1 + (u32::from(permille.min(1000)) * span + 500) / 1000
}

impl Options {
    /// The panel of settings `tool` offers, set as these are; `smooth_allowed`
    /// is false for a palette picture, whose pixels are whole.
    #[must_use]
    pub fn panel(&self, tool: Tool, smooth_allowed: bool) -> FieldGroup {
        let rows = tool
            .settings()
            .iter()
            .map(|setting| self.row(*setting, smooth_allowed))
            .collect();
        FieldGroup::new(tool.name(), rows)
    }

    fn row(self, setting: Setting, smooth_allowed: bool) -> FieldRow {
        let slider = |value: u32, most: u32| {
            FieldControl::Slider(
                Slider::new(permille(value, most))
                    .with_stops(u16::try_from(most).unwrap_or(1000))
                    .with_steps(permille(2, most), permille(9, most)),
            )
        };
        match setting {
            Setting::Size => FieldRow::new(size_label(self.size), slider(self.size, MAX_SIZE)),
            Setting::Style => FieldRow::new(
                "Style",
                FieldControl::Combo(
                    ComboBox::new(
                        Style::ALL
                            .iter()
                            .map(|style| String::from(style.label()))
                            .collect(),
                    )
                    .with_selected(
                        Style::ALL
                            .iter()
                            .position(|&s| s == self.style)
                            .unwrap_or(0),
                    ),
                ),
            ),
            Setting::Smooth => {
                let row = FieldRow::new(
                    "Smooth edges",
                    FieldControl::Toggle(Toggle::new(
                        "Smooth edges",
                        self.smooth && smooth_allowed,
                    )),
                );
                if smooth_allowed {
                    row
                } else {
                    row.with_description("A palette picture's pixels are one colour each")
                        .with_state(ControlState::disabled())
                }
            }
            Setting::Tolerance => FieldRow::new(
                tolerance_label(self.tolerance),
                slider(u32::from(self.tolerance) + 1, 256),
            ),
            Setting::Flow => {
                FieldRow::new(flow_label(self.flow), slider(u32::from(self.flow), 100))
            }
        }
    }

    /// Adopt the value a panel row's control asked for, answering the label
    /// that row now reads, when it changes with the value.
    pub fn adopt(
        &mut self,
        tool: Tool,
        row: usize,
        action: &tairix_controls::FieldAction,
    ) -> Option<String> {
        use tairix_controls::FieldAction;
        let setting = *tool.settings().get(row)?;
        match (setting, action) {
            (
                Setting::Size,
                FieldAction::SetValue { permille } | FieldAction::Settled { permille },
            ) => {
                self.size = from_permille(*permille, MAX_SIZE);
                Some(size_label(self.size))
            }
            (
                Setting::Tolerance,
                FieldAction::SetValue { permille } | FieldAction::Settled { permille },
            ) => {
                self.tolerance = u8::try_from(from_permille(*permille, 256) - 1).unwrap_or(u8::MAX);
                Some(tolerance_label(self.tolerance))
            }
            (
                Setting::Flow,
                FieldAction::SetValue { permille } | FieldAction::Settled { permille },
            ) => {
                self.flow = u8::try_from(from_permille(*permille, 100)).unwrap_or(100);
                Some(flow_label(self.flow))
            }
            (Setting::Style, FieldAction::Selected { index }) => {
                self.style = *Style::ALL.get(*index)?;
                None
            }
            (Setting::Smooth, FieldAction::Set { on }) => {
                self.smooth = *on;
                None
            }
            _ => None,
        }
    }
}

fn size_label(size: u32) -> String {
    alloc::format!("Size: {size} px")
}

fn tolerance_label(tolerance: u8) -> String {
    alloc::format!("Tolerance: {tolerance}")
}

fn flow_label(flow: u8) -> String {
    alloc::format!("Flow: {flow}%")
}

/// A command the strip offers beside the tools.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ViewCommand {
    /// Magnify less.
    ZoomOut,
    /// Magnify more.
    ZoomIn,
    /// Fit the picture in the window.
    Fit,
    /// A picture pixel to a screen pixel.
    Actual,
    /// Show or hide the grid between pixels.
    Grid,
}

/// The strip's commands after its tools, with their glyphs and tips.
pub const VIEW_COMMANDS: [(IconKind, ViewCommand, &str); 5] = [
    (IconKind::ZoomOut, ViewCommand::ZoomOut, "Zoom out (-)"),
    (IconKind::ZoomIn, ViewCommand::ZoomIn, "Zoom in (+)"),
    (IconKind::ZoomFit, ViewCommand::Fit, "Fit in window"),
    (IconKind::ZoomActual, ViewCommand::Actual, "Actual size (1)"),
    (IconKind::PixelGrid, ViewCommand::Grid, "Pixel grid (G)"),
];

/// What the strip's tool at `index` is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StripItem {
    /// A tool.
    Tool(Tool),
    /// A command.
    Command(ViewCommand),
}

/// The strip's item at `index`.
#[must_use]
pub fn strip_item(index: usize) -> Option<StripItem> {
    Tool::ALL
        .get(index)
        .map(|&tool| StripItem::Tool(tool))
        .or_else(|| {
            VIEW_COMMANDS
                .get(index.checked_sub(Tool::ALL.len())?)
                .map(|&(_, command, _)| StripItem::Command(command))
        })
}

/// The tip the strip's item at `index` carries.
#[must_use]
pub fn strip_tip(index: usize) -> Option<&'static str> {
    Tool::ALL.get(index).map(|tool| tool.label()).or_else(|| {
        VIEW_COMMANDS
            .get(index.checked_sub(Tool::ALL.len())?)
            .map(|&(_, _, tip)| tip)
    })
}

/// The strip: every tool, then the view's commands.
#[must_use]
pub fn strip(active: Tool) -> Toolbar {
    let mut toolbar = Toolbar::new();
    for tool in Tool::ALL {
        toolbar = toolbar.with_icon(IconButton::new(tool.icon(), ControlRole::Neutral), 0);
    }
    for (icon, _, _) in VIEW_COMMANDS {
        toolbar = toolbar.with_icon(IconButton::new(icon, ControlRole::Neutral), 1);
    }
    toolbar.set_active(tool_index(active));
    toolbar
}

/// Where `tool` sits in the strip.
#[must_use]
pub fn tool_index(tool: Tool) -> usize {
    Tool::ALL.iter().position(|&t| t == tool).unwrap_or(0)
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
