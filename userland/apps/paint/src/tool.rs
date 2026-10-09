//! The tools, what each is set to, and the tool box and view strip that
//! choose them.

use tairix_controls::{ControlRole, IconButton, ScrollOrientation, Toolbar};
use tairix_icon::IconKind;

use crate::brush::Tip;
use crate::mask::Combine;

/// Something the pointer does to the picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tool {
    /// Mark out a selection, and move what it holds.
    Select,
    /// Set single pixels, exactly.
    Pencil,
    /// Paint with a round brush.
    Brush,
    /// Paint that builds up the longer it is held.
    Airbrush,
    /// Rub paint out.
    Eraser,
    /// Paint with what lies at a distance from the brush.
    Clone,
    /// Fill the area joined to a pixel, or every pixel like it.
    Fill,
    /// Lay a blend from one colour to the other.
    Gradient,
    /// Take a pixel's colour.
    Eyedropper,
    /// Set text into the picture.
    Text,
    /// Draw a straight line.
    Line,
    /// Draw a rectangle.
    Rectangle,
    /// Draw an ellipse.
    Ellipse,
    /// Draw a polygon a corner at a time.
    Polygon,
    /// Cut the picture down to a part of it.
    Crop,
    /// Drag the view about.
    Hand,
    /// Magnify where it is pressed.
    Zoom,
}

impl Tool {
    /// Every tool, in the order the tool box shows them.
    pub const ALL: [Self; 17] = [
        Self::Select,
        Self::Pencil,
        Self::Brush,
        Self::Airbrush,
        Self::Eraser,
        Self::Clone,
        Self::Fill,
        Self::Gradient,
        Self::Eyedropper,
        Self::Text,
        Self::Line,
        Self::Rectangle,
        Self::Ellipse,
        Self::Polygon,
        Self::Crop,
        Self::Hand,
        Self::Zoom,
    ];

    /// The glyph the tool box draws.
    #[must_use]
    pub const fn icon(self) -> IconKind {
        match self {
            Self::Select => IconKind::ToolSelect,
            Self::Pencil => IconKind::ToolPencil,
            Self::Brush => IconKind::ToolBrush,
            Self::Airbrush => IconKind::ToolSpray,
            Self::Eraser => IconKind::ToolEraser,
            Self::Clone => IconKind::ToolClone,
            Self::Fill => IconKind::ToolFill,
            Self::Gradient => IconKind::ToolGradient,
            Self::Eyedropper => IconKind::ToolEyedropper,
            Self::Text => IconKind::ToolText,
            Self::Line => IconKind::ToolLine,
            Self::Rectangle => IconKind::ToolRectangle,
            Self::Ellipse => IconKind::ToolEllipse,
            Self::Polygon => IconKind::ToolPolygon,
            Self::Crop => IconKind::ToolCrop,
            Self::Hand => IconKind::ToolHand,
            Self::Zoom => IconKind::ToolZoom,
        }
    }

    /// What the tool is called, with the key that chooses it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "Select (S)",
            Self::Pencil => "Pencil (P)",
            Self::Brush => "Brush (B)",
            Self::Airbrush => "Airbrush (A)",
            Self::Eraser => "Eraser (E)",
            Self::Clone => "Clone (C)",
            Self::Fill => "Fill (F)",
            Self::Gradient => "Gradient (D)",
            Self::Eyedropper => "Eyedropper (I)",
            Self::Text => "Text (T)",
            Self::Line => "Line (L)",
            Self::Rectangle => "Rectangle (R)",
            Self::Ellipse => "Ellipse (O)",
            Self::Polygon => "Polygon (Y)",
            Self::Crop => "Crop (K)",
            Self::Hand => "Hand (H)",
            Self::Zoom => "Zoom (Z)",
        }
    }

    /// The name alone, as the tool-controls bar is headed.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Pencil => "Pencil",
            Self::Brush => "Brush",
            Self::Airbrush => "Airbrush",
            Self::Eraser => "Eraser",
            Self::Clone => "Clone",
            Self::Fill => "Fill",
            Self::Gradient => "Gradient",
            Self::Eyedropper => "Eyedropper",
            Self::Text => "Text",
            Self::Line => "Line",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Polygon => "Polygon",
            Self::Crop => "Crop",
            Self::Hand => "Hand",
            Self::Zoom => "Zoom",
        }
    }

    /// The tool the letter key `ch` chooses.
    #[must_use]
    pub fn for_key(ch: char) -> Option<Self> {
        Some(match ch.to_ascii_lowercase() {
            's' => Self::Select,
            'p' => Self::Pencil,
            'b' => Self::Brush,
            'a' => Self::Airbrush,
            'e' => Self::Eraser,
            'c' => Self::Clone,
            'f' => Self::Fill,
            'd' => Self::Gradient,
            'i' => Self::Eyedropper,
            't' => Self::Text,
            'l' => Self::Line,
            'r' => Self::Rectangle,
            'o' => Self::Ellipse,
            'y' => Self::Polygon,
            'k' => Self::Crop,
            'h' => Self::Hand,
            'z' => Self::Zoom,
            _ => return None,
        })
    }

    /// Whether a drag with the tool is drawn as a shape that follows the
    /// pointer until it lets go.
    #[must_use]
    pub const fn shaped(self) -> bool {
        matches!(self, Self::Line | Self::Rectangle | Self::Ellipse)
    }

    /// Whether it paints with a round tip laid as dabs.
    #[must_use]
    pub const fn tipped(self) -> bool {
        matches!(
            self,
            Self::Brush | Self::Airbrush | Self::Eraser | Self::Clone
        )
    }

    /// The settings the tool-controls bar offers for the tool, left to
    /// right.
    #[must_use]
    pub const fn settings(self) -> &'static [Setting] {
        match self {
            Self::Select => &[
                Setting::Marquee,
                Setting::Combine,
                Setting::Feather,
                Setting::Tolerance,
                Setting::Smooth,
            ],
            Self::Pencil | Self::Eyedropper | Self::Crop | Self::Hand | Self::Zoom => &[],
            Self::Brush | Self::Airbrush | Self::Eraser | Self::Clone => &[
                Setting::Size,
                Setting::Hardness,
                Setting::Opacity,
                Setting::Flow,
                Setting::Spacing,
                Setting::Smooth,
            ],
            Self::Line => &[Setting::Size, Setting::Smooth],
            Self::Fill => &[Setting::Tolerance, Setting::Contiguous],
            Self::Gradient => &[Setting::Gradient],
            Self::Text => &[Setting::TextSize, Setting::Smooth],
            Self::Rectangle => &[
                Setting::Size,
                Setting::Style,
                Setting::Corners,
                Setting::Smooth,
            ],
            Self::Ellipse | Self::Polygon => &[Setting::Size, Setting::Style, Setting::Smooth],
        }
    }
}

/// The most settings any tool offers: the room a placement of the
/// tool-controls bar keeps.
pub const MOST_SETTINGS: usize = most_settings();

const fn most_settings() -> usize {
    let mut most = 0;
    let mut at = 0;
    while at < Tool::ALL.len() {
        let count = Tool::ALL[at].settings().len();
        if count > most {
            most = count;
        }
        at += 1;
    }
    most
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
    /// Every style, in the order the choice lists them.
    pub const ALL: [Self; 3] = [Self::Outline, Self::Filled, Self::Both];

    /// What the choice calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Outline => "Outline",
            Self::Filled => "Filled",
            Self::Both => "Filled and outlined",
        }
    }
}

/// What the select tool marks out.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Marquee {
    /// A rectangle, dragged corner to corner.
    #[default]
    Rectangle,
    /// The ellipse inside a dragged rectangle.
    Ellipse,
    /// What a path drawn freehand encloses.
    Lasso,
    /// What corners clicked in turn enclose.
    Polygon,
    /// The pixels joined to the one clicked through colours like it.
    Wand,
}

impl Marquee {
    /// Every marquee, in the order the choice lists them.
    pub const ALL: [Self; 5] = [
        Self::Rectangle,
        Self::Ellipse,
        Self::Lasso,
        Self::Polygon,
        Self::Wand,
    ];

    /// What the choice calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Lasso => "Lasso",
            Self::Polygon => "Polygon",
            Self::Wand => "Magic wand",
        }
    }
}

/// How a gradient spreads from where its drag began.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum GradientShape {
    /// In bands across the drag.
    #[default]
    Linear,
    /// In rings about where the drag began.
    Radial,
}

impl GradientShape {
    /// Every shape, in the order the choice lists them.
    pub const ALL: [Self; 2] = [Self::Linear, Self::Radial];

    /// What the choice calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Radial => "Radial",
        }
    }
}

/// What every tool is set to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// Line and outline width, in pixels: `1..=MAX_SIZE`.
    pub size: u32,
    /// How rectangles and ellipses are drawn.
    pub style: Style,
    /// Whether edges are smoothed where the picture can show part of a pixel.
    pub smooth: bool,
    /// How far a colour may differ from the one filled from and still be
    /// filled, per channel.
    pub tolerance: u8,
    /// Whether a fill reaches only the pixels joined to the one pressed,
    /// rather than every pixel like it.
    pub contiguous: bool,
    /// How a gradient spreads.
    pub gradient: GradientShape,
    /// Text height, in pixels: `MIN_TEXT..=MAX_TEXT`.
    pub text_size: u32,
    /// A rectangle's corner radius, in pixels: `0..=MAX_CORNERS`.
    pub corners: u32,
    /// The brush's tip.
    pub brush: Tip,
    /// The airbrush's tip.
    pub airbrush: Tip,
    /// The eraser's tip.
    pub eraser: Tip,
    /// The clone tool's tip.
    pub clone: Tip,
    /// What the select tool marks out.
    pub marquee: Marquee,
    /// How a selection marked out meets the one held.
    pub combine: Combine,
    /// How far a selection's edge is softened, in pixels: `0..=MAX_FEATHER`.
    pub feather: u32,
}

/// The widest a brush is.
pub const MAX_SIZE: u32 = 64;

/// The farthest a selection's edge is softened.
pub const MAX_FEATHER: u32 = 100;

/// The shortest text is set, in pixels.
pub const MIN_TEXT: u32 = 6;
/// The tallest text is set, in pixels.
pub const MAX_TEXT: u32 = 400;

/// The roundest a rectangle's corners are, in pixels.
pub const MAX_CORNERS: u32 = 200;

impl Default for Options {
    fn default() -> Self {
        Self {
            size: 4,
            style: Style::Outline,
            smooth: true,
            tolerance: 0,
            contiguous: true,
            gradient: GradientShape::Linear,
            text_size: 24,
            corners: 0,
            brush: Tip {
                size: 4,
                hardness: 100,
                opacity: 100,
                flow: 100,
                spacing: 10,
            },
            airbrush: Tip {
                size: 16,
                hardness: 0,
                opacity: 100,
                flow: 10,
                spacing: 10,
            },
            eraser: Tip {
                size: 8,
                hardness: 100,
                opacity: 100,
                flow: 100,
                spacing: 10,
            },
            clone: Tip {
                size: 16,
                hardness: 50,
                opacity: 100,
                flow: 100,
                spacing: 10,
            },
            marquee: Marquee::Rectangle,
            combine: Combine::Replace,
            feather: 0,
        }
    }
}

impl Options {
    /// The tip `tool` paints with, if it paints with one.
    #[must_use]
    pub const fn tip(&self, tool: Tool) -> Option<&Tip> {
        match tool {
            Tool::Brush => Some(&self.brush),
            Tool::Airbrush => Some(&self.airbrush),
            Tool::Eraser => Some(&self.eraser),
            Tool::Clone => Some(&self.clone),
            _ => None,
        }
    }

    fn tip_mut(&mut self, tool: Tool) -> Option<&mut Tip> {
        match tool {
            Tool::Brush => Some(&mut self.brush),
            Tool::Airbrush => Some(&mut self.airbrush),
            Tool::Eraser => Some(&mut self.eraser),
            Tool::Clone => Some(&mut self.clone),
            _ => None,
        }
    }

    /// Whether switch setting `setting` is on; `None` for a setting that is
    /// not a switch.
    #[must_use]
    pub const fn switch(&self, setting: Setting) -> Option<bool> {
        match setting {
            Setting::Smooth => Some(self.smooth),
            Setting::Contiguous => Some(self.contiguous),
            _ => None,
        }
    }

    /// Turn switch setting `setting` on or off; a setting that is not a
    /// switch is left alone.
    pub fn set_switch(&mut self, setting: Setting, on: bool) {
        match setting {
            Setting::Smooth => self.smooth = on,
            Setting::Contiguous => self.contiguous = on,
            _ => {}
        }
    }

    /// The value number setting `setting` holds for `tool`, whose own tip
    /// holds its size and how it lays paint; `None` for a setting that is
    /// not a number, or a tip setting of a tool with no tip.
    #[must_use]
    pub fn number(&self, tool: Tool, setting: Setting) -> Option<i32> {
        let tip = self.tip(tool);
        match setting {
            Setting::Size => i32::try_from(tip.map_or(self.size, |tip| tip.size)).ok(),
            Setting::Hardness => tip.map(|tip| i32::from(tip.hardness)),
            Setting::Opacity => tip.map(|tip| i32::from(tip.opacity)),
            Setting::Flow => tip.map(|tip| i32::from(tip.flow)),
            Setting::Spacing => tip.map(|tip| i32::from(tip.spacing)),
            Setting::Tolerance => Some(i32::from(self.tolerance)),
            Setting::Feather => i32::try_from(self.feather).ok(),
            Setting::TextSize => i32::try_from(self.text_size).ok(),
            Setting::Corners => i32::try_from(self.corners).ok(),
            Setting::Style
            | Setting::Smooth
            | Setting::Contiguous
            | Setting::Marquee
            | Setting::Combine
            | Setting::Gradient => None,
        }
    }

    /// Which of its choices choice setting `setting` holds; `None` for a
    /// setting that is not a choice.
    #[must_use]
    pub fn choice(&self, setting: Setting) -> Option<usize> {
        match setting {
            Setting::Style => Style::ALL.iter().position(|&style| style == self.style),
            Setting::Marquee => Marquee::ALL
                .iter()
                .position(|&marquee| marquee == self.marquee),
            Setting::Combine => Combine::ALL
                .iter()
                .position(|&combine| combine == self.combine),
            Setting::Gradient => GradientShape::ALL
                .iter()
                .position(|&shape| shape == self.gradient),
            _ => None,
        }
    }

    /// Hold choice `index` of choice setting `setting`, answering whether it
    /// changed; one past its choices, or of a setting that is not a choice,
    /// changes nothing.
    pub fn set_choice(&mut self, setting: Setting, index: usize) -> bool {
        let before = *self;
        match setting {
            Setting::Style => {
                if let Some(&style) = Style::ALL.get(index) {
                    self.style = style;
                }
            }
            Setting::Marquee => {
                if let Some(&marquee) = Marquee::ALL.get(index) {
                    self.marquee = marquee;
                }
            }
            Setting::Combine => {
                if let Some(&combine) = Combine::ALL.get(index) {
                    self.combine = combine;
                }
            }
            Setting::Gradient => {
                if let Some(&shape) = GradientShape::ALL.get(index) {
                    self.gradient = shape;
                }
            }
            _ => {}
        }
        *self != before
    }

    /// Set number setting `setting` of `tool` to `value`, held to its
    /// bounds; a setting that is not a number, or a tip setting of a tool
    /// with no tip, is left alone.
    pub fn set_number(&mut self, tool: Tool, setting: Setting, value: i32) {
        let Some((least, most)) = setting.bounds() else {
            return;
        };
        let value = value.clamp(least, most);
        let whole = u32::try_from(value).unwrap_or(0);
        let part = u8::try_from(value).unwrap_or(u8::MAX);
        match (setting, self.tip_mut(tool)) {
            (Setting::Size, Some(tip)) => tip.size = whole,
            (Setting::Size, None) => self.size = whole,
            (Setting::Hardness, Some(tip)) => tip.hardness = part,
            (Setting::Opacity, Some(tip)) => tip.opacity = part,
            (Setting::Flow, Some(tip)) => tip.flow = part,
            (Setting::Spacing, Some(tip)) => tip.spacing = part,
            (Setting::Tolerance, _) => self.tolerance = part,
            (Setting::Feather, _) => self.feather = whole,
            (Setting::TextSize, _) => self.text_size = whole,
            (Setting::Corners, _) => self.corners = whole,
            _ => {}
        }
    }
}

/// One setting of the tool-controls bar.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Setting {
    /// [`Options::size`], a number.
    Size,
    /// [`Options::style`], a choice.
    Style,
    /// [`Options::smooth`], a switch.
    Smooth,
    /// [`Options::tolerance`], a number.
    Tolerance,
    /// How hard a tip's edge is ([`Tip::hardness`]), a number.
    Hardness,
    /// How much one stroke lays ([`Tip::opacity`]), a number.
    Opacity,
    /// How much each dab lays ([`Tip::flow`]), a number.
    Flow,
    /// How far apart dabs fall ([`Tip::spacing`]), a number.
    Spacing,
    /// [`Options::marquee`], a choice.
    Marquee,
    /// [`Options::combine`], a choice.
    Combine,
    /// [`Options::feather`], a number.
    Feather,
    /// [`Options::contiguous`], a switch.
    Contiguous,
    /// [`Options::gradient`], a choice.
    Gradient,
    /// [`Options::text_size`], a number.
    TextSize,
    /// [`Options::corners`], a number.
    Corners,
}

impl Setting {
    /// What it is called: the words before a number's field or a choice,
    /// and a switch's own label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Size | Self::TextSize => "Size",
            Self::Style => "Style",
            Self::Smooth => "Smooth edges",
            Self::Tolerance => "Tolerance",
            Self::Hardness => "Hardness",
            Self::Opacity => "Opacity",
            Self::Flow => "Flow",
            Self::Spacing => "Spacing",
            Self::Marquee => "Select",
            Self::Combine => "Mode",
            Self::Feather => "Feather",
            Self::Contiguous => "Joined pixels only",
            Self::Gradient => "Shape",
            Self::Corners => "Corners",
        }
    }

    /// Whether it is a switch: a checkbox naming itself.
    #[must_use]
    pub const fn is_switch(self) -> bool {
        matches!(self, Self::Smooth | Self::Contiguous)
    }

    /// Whether it sets how much of a pixel paint lays, which a palette
    /// picture, its pixels one colour each, holds off.
    #[must_use]
    pub const fn lays_part(self) -> bool {
        matches!(
            self,
            Self::Smooth | Self::Hardness | Self::Opacity | Self::Flow
        )
    }

    /// A choice setting's choices, as its list names them; none for a
    /// number or a switch.
    #[must_use]
    pub fn choices(self) -> &'static [&'static str] {
        const STYLES: [&str; 3] = [
            Style::ALL[0].label(),
            Style::ALL[1].label(),
            Style::ALL[2].label(),
        ];
        const MARQUEES: [&str; 5] = [
            Marquee::ALL[0].label(),
            Marquee::ALL[1].label(),
            Marquee::ALL[2].label(),
            Marquee::ALL[3].label(),
            Marquee::ALL[4].label(),
        ];
        const COMBINES: [&str; 4] = [
            Combine::ALL[0].label(),
            Combine::ALL[1].label(),
            Combine::ALL[2].label(),
            Combine::ALL[3].label(),
        ];
        const GRADIENTS: [&str; 2] = [GradientShape::ALL[0].label(), GradientShape::ALL[1].label()];
        match self {
            Self::Style => &STYLES,
            Self::Marquee => &MARQUEES,
            Self::Combine => &COMBINES,
            Self::Gradient => &GRADIENTS,
            _ => &[],
        }
    }

    /// The unit after a number's field, if it has one.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Size | Self::Feather | Self::TextSize | Self::Corners => "px",
            Self::Hardness | Self::Opacity | Self::Flow | Self::Spacing => "%",
            Self::Style
            | Self::Smooth
            | Self::Contiguous
            | Self::Tolerance
            | Self::Marquee
            | Self::Combine
            | Self::Gradient => "",
        }
    }

    /// The least and most a number setting holds; `None` for a choice or a
    /// switch.
    #[must_use]
    pub fn bounds(self) -> Option<(i32, i32)> {
        match self {
            Self::Size => Some((1, i32::try_from(MAX_SIZE).unwrap_or(i32::MAX))),
            Self::Tolerance => Some((0, i32::from(u8::MAX))),
            Self::Hardness => Some((0, 100)),
            Self::Opacity | Self::Flow | Self::Spacing => Some((1, 100)),
            Self::Feather => Some((0, i32::try_from(MAX_FEATHER).unwrap_or(i32::MAX))),
            Self::TextSize => Some((
                i32::try_from(MIN_TEXT).unwrap_or(1),
                i32::try_from(MAX_TEXT).unwrap_or(i32::MAX),
            )),
            Self::Corners => Some((0, i32::try_from(MAX_CORNERS).unwrap_or(i32::MAX))),
            Self::Style
            | Self::Smooth
            | Self::Contiguous
            | Self::Marquee
            | Self::Combine
            | Self::Gradient => None,
        }
    }

    /// How far a number setting steps: a line for Up, Down and a wheel
    /// detent, a page for Page Up and Page Down.
    #[must_use]
    pub const fn steps(self) -> (i32, i32) {
        match self {
            Self::Size | Self::TextSize | Self::Corners => (1, 8),
            Self::Tolerance => (1, 16),
            Self::Hardness | Self::Opacity | Self::Flow | Self::Spacing | Self::Feather => (1, 10),
            Self::Style
            | Self::Smooth
            | Self::Contiguous
            | Self::Marquee
            | Self::Combine
            | Self::Gradient => (0, 0),
        }
    }

    /// What the tip over it says.
    #[must_use]
    pub const fn tip(self) -> &'static str {
        match self {
            Self::Size => "How wide a stroke, line or outline is, in pixels",
            Self::Style => "How a rectangle or an ellipse is drawn",
            Self::Smooth => "Smooth edges where the picture can show part of a pixel",
            Self::Tolerance => {
                "How far a colour may differ from the one filled or chosen from and still be taken"
            }
            Self::Hardness => "How much of the tip is solid before its edge fades away",
            Self::Opacity => "The most paint one stroke lays",
            Self::Flow => "How much paint each dab lays; dabs build up to the opacity",
            Self::Spacing => "How far apart dabs fall, as a share of the tip's width",
            Self::Marquee => "What a drag or a click marks out",
            Self::Combine => {
                "How a selection marked out meets the one held: Shift adds, Alt takes away"
            }
            Self::Feather => "How far a selection's edge is softened, in pixels",
            Self::Contiguous => {
                "Fill only the pixels joined to the one pressed, rather than every pixel like it"
            }
            Self::Gradient => "How the blend spreads from where the drag began",
            Self::TextSize => "How tall the text is, in pixels",
            Self::Corners => "How round a rectangle's corners are, in pixels",
        }
    }
}

/// Why smoothing is held off on a palette picture, as its tip says.
pub const WHOLE_PIXELS: &str = "A palette picture's pixels are one colour each";

/// The tool box: every tool, [`TOOL_BOX_LANES`] to a line down the window's
/// side, the one in use marked.
#[must_use]
pub fn tool_box(active: Tool) -> Toolbar {
    let mut toolbar = Toolbar::new()
        .with_orientation(ScrollOrientation::Vertical)
        .with_lanes(TOOL_BOX_LANES);
    for tool in Tool::ALL {
        toolbar = toolbar.with_icon(IconButton::new(tool.icon(), ControlRole::Neutral), 0);
    }
    toolbar.set_active(tool_index(active));
    toolbar
}

/// How many tools stand side by side down the tool box.
pub const TOOL_BOX_LANES: u16 = 2;

/// Where `tool` sits in the tool box.
#[must_use]
pub fn tool_index(tool: Tool) -> usize {
    Tool::ALL.iter().position(|&t| t == tool).unwrap_or(0)
}

/// A command the view strip offers.
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
    PixelGrid,
}

/// The view strip's commands, in order, with their glyphs and tips.
pub const VIEW_COMMANDS: [(IconKind, ViewCommand, &str); 5] = [
    (IconKind::ZoomOut, ViewCommand::ZoomOut, "Zoom out (-)"),
    (IconKind::ZoomIn, ViewCommand::ZoomIn, "Zoom in (+)"),
    (IconKind::ZoomFit, ViewCommand::Fit, "Fit in window"),
    (IconKind::ZoomActual, ViewCommand::Actual, "Actual size (1)"),
    (
        IconKind::PixelGrid,
        ViewCommand::PixelGrid,
        "Pixel grid (G)",
    ),
];

/// The view strip: the view's own commands, the zoom's apart from the
/// grid's, the grid's marked while `grid` shows.
#[must_use]
pub fn view_strip(grid: bool) -> Toolbar {
    let mut toolbar = Toolbar::new();
    for (icon, command, _) in VIEW_COMMANDS {
        let group = u16::from(command == ViewCommand::PixelGrid);
        toolbar = toolbar.with_icon(IconButton::new(icon, ControlRole::Neutral), group);
    }
    mark_pixel_grid(&mut toolbar, grid);
    toolbar
}

/// Mark the view strip's grid command while the grid shows, and nothing
/// while it does not.
pub fn mark_pixel_grid(strip: &mut Toolbar, grid: bool) {
    let command = VIEW_COMMANDS
        .iter()
        .position(|&(_, command, _)| command == ViewCommand::PixelGrid)
        .filter(|_| grid);
    strip.set_active(command.unwrap_or(usize::MAX));
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
