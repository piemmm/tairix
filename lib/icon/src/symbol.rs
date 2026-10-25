//! The symbols the settings categories and panes are drawn with.
//!
//! A symbol is authored as geometry, not pixels: SVG path data and circles on
//! a [`SYMBOL_GRID`]-unit square, filled or stroked. Nothing here flattens a
//! curve or outlines a stroke itself — `lib/svg`'s one flattener and one
//! stroker do, and `lib/svg`'s own placement puts the result on the integer
//! design grid a layer holds — so a symbol and a decoded SVG asset are the
//! same kind of drawing to the rasteriser.
//!
//! The same marks serve two pictures: a category's colour badge, where the
//! symbol stands small in white on its plate ([`crate::badge()`]), and the
//! category's tintable glyph, where it fills the grid on its own.

use alloc::vec::Vec;

use tairix_raster::{Affine, Color, FillRule, Layer, Paint};
use tairix_svg::geom::{place, LineCap, LineJoin, StrokeStyle, SubPath};
use tairix_svg::pathdata::{flatten_ellipse_arc, parse_path_data};
use tairix_svg::stroke::stroke_outline;
use tairix_util::mathf::round;

use crate::fit::{GridFit, Phase};
use crate::glyph::IconKind;
use crate::vector::VectorIcon;

/// The side of the square every symbol is authored on, in symbol units.
pub(crate) const SYMBOL_GRID: f64 = 24.0;

/// Design units per symbol unit: fine enough that rounding a flattened point
/// onto the grid moves it a hundredth of a pixel at the largest slot side.
pub(crate) const UNIT: f64 = 128.0;

/// The design grid a symbol, and the badge it stands on, is drawn over.
pub(crate) const DESIGN: u32 = 3072;

const _: () = assert!(DESIGN as f64 == SYMBOL_GRID * UNIT);

/// The furthest a glyph's flattened curve departs from the true one, in symbol
/// units: a tenth of a pixel at a 480-pixel slot, so one glyph serves every
/// side.
const GLYPH_FLATNESS: f64 = 0.005;

/// The furthest a curve drawn at a known side departs from the true one, in
/// pixels of that side.
const FLATNESS_PX: f64 = 0.05;

/// The coarsest any curve is flattened, in symbol units, however small the
/// side it is drawn at.
const MAX_FLATNESS: f64 = 0.25;

/// The most points one mark may flatten to.
///
/// A containment bound, not a capacity: the authored marks flatten to a few
/// hundred points, so this only stops a defect in the table from allocating
/// without end.
const MAX_MARK_POINTS: usize = 16_384;

/// One closed or open contour of a mark.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Outline {
    /// SVG path data, in symbol units.
    Path(&'static str),
    /// A whole circle.
    Circle {
        /// Its centre, in symbol units.
        centre: (f64, f64),
        /// Its radius, in symbol units.
        radius: f64,
    },
}

/// One painted region of a symbol.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Mark {
    /// The area `outlines` enclose under `rule`: even-odd is how a mark cuts
    /// a hole (a keyhole, a key, a lamp) out of itself.
    Fill {
        /// The contours, filled together.
        outlines: &'static [Outline],
        /// Which points they enclose.
        rule: FillRule,
    },
    /// The area a stroke `width` symbol units wide along `outlines` covers.
    Stroke {
        /// The contours stroked.
        outlines: &'static [Outline],
        /// The stroke width, in symbol units.
        width: f64,
        /// How an open contour ends.
        cap: LineCap,
        /// How a contour turns a corner.
        join: LineJoin,
    },
}

/// Where a symbol lands: `scale` design units per symbol unit, offset by
/// `origin` design units.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Placement {
    /// Design units per symbol unit.
    pub scale: f64,
    /// Where the symbol grid's origin lands, in design units.
    pub origin: (f64, f64),
}

impl Placement {
    fn to_design(self) -> Affine {
        Affine::scale(self.scale, self.scale).then(Affine::translate(self.origin.0, self.origin.1))
    }

    /// A symbol filling the whole grid, as a glyph does.
    pub(crate) const WHOLE: Self = Self {
        scale: UNIT,
        origin: (0.0, 0.0),
    };

    /// A symbol drawn `fraction` of its own size, centred on the grid.
    pub(crate) fn centred(fraction: f64) -> Self {
        let inset = SYMBOL_GRID * (1.0 - fraction) / 2.0 * UNIT;
        Self {
            scale: UNIT * fraction,
            origin: (inset, inset),
        }
    }
}

/// `kind`'s symbol as a glyph filling the grid in `color`, or `None` for a
/// kind drawn with a glyph of its own.
pub(crate) fn glyph(kind: IconKind, color: Color) -> Option<VectorIcon> {
    let layers = layers(
        marks(kind)?,
        Placement::WHOLE,
        &Paint::Solid(color),
        GLYPH_FLATNESS,
    )?;
    Some(VectorIcon::new(DESIGN, layers))
}

/// `kind`'s symbol as a glyph filling a `side`-pixel square in `color`, fitted
/// to that side's pixel grid, or `None` for a kind drawn with a glyph of its
/// own.
pub(crate) fn glyph_at(kind: IconKind, color: Color, side: u32) -> Option<VectorIcon> {
    let layers = fitted_layers(marks(kind)?, Placement::WHOLE, &Paint::Solid(color), side)?;
    Some(VectorIcon::new(DESIGN, layers))
}

/// The flatness, in symbol units, a picture `px_per_unit` pixels to the symbol
/// unit needs: finer as it is drawn larger, so a small icon costs few vertices
/// and a large one shows no facets.
pub(crate) fn flatness_at(px_per_unit: f64) -> f64 {
    (FLATNESS_PX / px_per_unit).clamp(GLYPH_FLATNESS, MAX_FLATNESS)
}

/// `marks` placed through `placement` and painted with `paint`, their curves
/// flattened to within `flatness` symbol units.
///
/// `None` when a mark cannot be built. The table is compiled in and tested
/// whole, so that is a defect in it, and the caller falls back rather than
/// drawing part of a symbol.
pub(crate) fn layers(
    marks: &[Mark],
    placement: Placement,
    paint: &Paint,
    flatness: f64,
) -> Option<Vec<Layer>> {
    let to_design = placement.to_design();
    marks
        .iter()
        .map(|mark| {
            let (subpaths, rule) = match *mark {
                Mark::Fill { outlines, rule } => (flatten(outlines, flatness)?, rule),
                Mark::Stroke {
                    outlines,
                    width,
                    cap,
                    join,
                } => {
                    let style = StrokeStyle {
                        width,
                        cap,
                        join,
                        ..StrokeStyle::default()
                    };
                    let area = stroke_outline(
                        &flatten(outlines, flatness)?,
                        &style,
                        flatness,
                        MAX_MARK_POINTS,
                    )
                    .ok()?;
                    (area, FillRule::NonZero)
                }
            };
            Some(Layer::filled(
                paint.clone(),
                rule,
                place(&subpaths, to_design),
            ))
        })
        .collect()
}

/// How a mark's contours are drawn once fitted.
enum Drawn {
    /// Filled under its rule.
    Fill(FillRule),
    /// Stroked a whole number of pixels wide.
    Stroke(StrokeStyle),
}

impl Drawn {
    /// Where the edges of contours drawn this way land on the pixel grid.
    fn phase(&self) -> Phase {
        match self {
            Self::Fill(_) => Phase::Boundary,
            Self::Stroke(style) => Phase::of_stroke(style.width),
        }
    }
}

/// [`layers`] for a picture `side` pixels square, fitted to its pixel grid:
/// every stroke a whole number of pixels wide, at least one, and every edge of
/// every mark on the grid, the marks fitted together so each still meets the
/// next where it did.
///
/// `None` for a zero side, or as [`layers`] answers it.
pub(crate) fn fitted_layers(
    marks: &[Mark],
    placement: Placement,
    paint: &Paint,
    side: u32,
) -> Option<Vec<Layer>> {
    if side == 0 {
        return None;
    }
    let px_per_design = f64::from(side) / f64::from(DESIGN);
    let px_per_unit = placement.scale * px_per_design;
    let to_pixels = placement
        .to_design()
        .then(Affine::scale(px_per_design, px_per_design));
    let flatness = flatness_at(px_per_unit);
    let mut parts = Vec::with_capacity(marks.len());
    for mark in marks {
        let (outlines, drawn) = match *mark {
            Mark::Fill { outlines, rule } => (outlines, Drawn::Fill(rule)),
            Mark::Stroke {
                outlines,
                width,
                cap,
                join,
            } => (
                outlines,
                Drawn::Stroke(StrokeStyle {
                    width: round(width * px_per_unit).max(1.0),
                    cap,
                    join,
                    ..StrokeStyle::default()
                }),
            ),
        };
        let contours: Vec<SubPath> = flatten(outlines, flatness)?
            .iter()
            .map(|contour| contour.mapped(to_pixels))
            .collect();
        parts.push((contours, drawn));
    }
    let fit = GridFit::new(
        side,
        parts
            .iter()
            .map(|(contours, drawn)| (contours.as_slice(), drawn.phase())),
    );
    let to_design = Affine::scale(1.0 / px_per_design, 1.0 / px_per_design);
    parts
        .into_iter()
        .map(|(mut contours, drawn)| {
            fit.apply(&mut contours);
            let (area, rule) = match drawn {
                Drawn::Fill(rule) => (contours, rule),
                Drawn::Stroke(style) => (
                    stroke_outline(&contours, &style, FLATNESS_PX, MAX_MARK_POINTS).ok()?,
                    FillRule::NonZero,
                ),
            };
            Some(Layer::filled(paint.clone(), rule, place(&area, to_design)))
        })
        .collect()
}

/// Every contour of `outlines`, flattened to within `flatness` symbol units.
pub(crate) fn flatten(outlines: &[Outline], flatness: f64) -> Option<Vec<SubPath>> {
    let mut subpaths = Vec::new();
    for outline in outlines {
        match *outline {
            Outline::Path(data) => {
                subpaths.extend(parse_path_data(data, flatness, MAX_MARK_POINTS, None).ok()?);
            }
            Outline::Circle { centre, radius } => {
                // Four equal quarters, so the polygon is as symmetric as the
                // circle at any flatness: one sweep from angle zero can end on
                // an odd count, which leans a centred symbol to one side.
                let quarter = core::f64::consts::FRAC_PI_2;
                let mut points = Vec::new();
                points.push((centre.0 + radius, centre.1));
                for turn in [0.0, 1.0, 2.0, 3.0] {
                    flatten_ellipse_arc(
                        centre,
                        (radius, radius),
                        0.0,
                        turn * quarter,
                        quarter,
                        flatness,
                        &mut points,
                    );
                }
                subpaths.push(SubPath::closed(points));
            }
        }
    }
    Some(subpaths)
}

/// The symbol `kind` is drawn with, or `None` for a kind drawn with a glyph
/// of its own.
pub(crate) fn marks(kind: IconKind) -> Option<&'static [Mark]> {
    Some(match kind {
        IconKind::Settings => GEAR,
        IconKind::Appearance => APPEARANCE,
        IconKind::Wallpaper => WALLPAPER,
        IconKind::Display => DISPLAY,
        IconKind::LockScreen => LOCK,
        IconKind::Screensaver => SCREENSAVER,
        IconKind::Power => POWER,
        IconKind::Networking => GLOBE,
        IconKind::Bluetooth => BLUETOOTH,
        IconKind::Sound => SPEAKER,
        IconKind::Notifications => BELL,
        IconKind::Keyboard => KEYBOARD,
        IconKind::Mouse => MOUSE,
        IconKind::Trackpad => TRACKPAD,
        IconKind::Touchscreen => TOUCHSCREEN,
        IconKind::Printer => PRINTER,
        IconKind::Accessibility => ACCESSIBILITY,
        IconKind::Language => LANGUAGE,
        IconKind::Sharing => SHARING,
        IconKind::Users => USERS,
        IconKind::Storage => STORAGE,
        IconKind::About => ABOUT,
        IconKind::Startup => STARTUP,
        IconKind::Caching => CHIP,
        IconKind::BoardFan => BOARD,
        IconKind::DateTime => CLOCK,
        IconKind::Ethernet => PLUG,
        IconKind::WiFi => WIFI,
        IconKind::Dns => SIGNPOST,
        IconKind::TcpIp => EXCHANGE,
        IconKind::SoundDevices => FADERS,
        IconKind::SoundEffects => NOTES,
        IconKind::Theme => PALETTE,
        IconKind::Computer => COMPUTER,
        IconKind::Laptop => LAPTOP,
        IconKind::Server => SERVER,
        IconKind::Router => ROUTER,
        IconKind::NetworkSwitch => NETWORK_SWITCH,
        IconKind::Phone => PHONE,
        IconKind::Nas => NAS,
        IconKind::IotDevice => SMART_BULB,
        IconKind::DeviceUnknown => UNKNOWN_DEVICE,
        IconKind::Subnet => SEGMENT,
        IconKind::OsLinux => PENGUIN,
        IconKind::OsWindows => PANED_WINDOW,
        IconKind::OsApple => LOOPED_SQUARE,
        IconKind::OsAndroid => ROBOT_HEAD,
        IconKind::AlertError => CROSS,
        IconKind::AlertWarning => EXCLAMATION,
        IconKind::AlertInformation => LETTER_I,
        IconKind::AlertQuestion => QUESTION,
        _ => return crate::command::marks(kind),
    })
}

pub(crate) const fn fill(outlines: &'static [Outline]) -> Mark {
    Mark::Fill {
        outlines,
        rule: FillRule::NonZero,
    }
}

/// A fill whose inner contours are holes.
pub(crate) const fn cut(outlines: &'static [Outline]) -> Mark {
    Mark::Fill {
        outlines,
        rule: FillRule::EvenOdd,
    }
}

pub(crate) const fn stroke(outlines: &'static [Outline], width: f64) -> Mark {
    Mark::Stroke {
        outlines,
        width,
        cap: LineCap::Round,
        join: LineJoin::Round,
    }
}

pub(crate) const fn circle(x: f64, y: f64, radius: f64) -> Outline {
    Outline::Circle {
        centre: (x, y),
        radius,
    }
}

/// An eight-toothed cog around a bored hub.
const GEAR: &[Mark] = &[cut(&[
    Outline::Path(
        "M9.685 4.656L10.105 1.774A10.4 10.4 0 0 1 13.895 1.774L14.315 4.656\
         A7.7 7.7 0 0 1 15.555 5.17L17.891 3.429A10.4 10.4 0 0 1 20.571 6.109\
         L18.83 8.445A7.7 7.7 0 0 1 19.344 9.685L22.226 10.105\
         A10.4 10.4 0 0 1 22.226 13.895L19.344 14.315A7.7 7.7 0 0 1 18.83 15.555\
         L20.571 17.891A10.4 10.4 0 0 1 17.891 20.571L15.555 18.83\
         A7.7 7.7 0 0 1 14.315 19.344L13.895 22.226A10.4 10.4 0 0 1 10.105 22.226\
         L9.685 19.344A7.7 7.7 0 0 1 8.445 18.83L6.109 20.571\
         A10.4 10.4 0 0 1 3.429 17.891L5.17 15.555A7.7 7.7 0 0 1 4.656 14.315\
         L1.774 13.895A10.4 10.4 0 0 1 1.774 10.105L4.656 9.685\
         A7.7 7.7 0 0 1 5.17 8.445L3.429 6.109A10.4 10.4 0 0 1 6.109 3.429\
         L8.445 5.17A7.7 7.7 0 0 1 9.685 4.656Z",
    ),
    circle(12.0, 12.0, 3.5),
])];

/// A ring with its leading half filled: light against dark.
const APPEARANCE: &[Mark] = &[
    cut(&[circle(12.0, 12.0, 9.4), circle(12.0, 12.0, 7.4)]),
    fill(&[Outline::Path("M12 4.4A7.6 7.6 0 0 0 12 19.6Z")]),
];

/// A framed landscape under a sun.
const WALLPAPER: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M5.6 4.6H18.4A2.6 2.6 0 0 1 21 7.2V16.8A2.6 2.6 0 0 1 18.4 19.4H5.6\
             A2.6 2.6 0 0 1 3 16.8V7.2A2.6 2.6 0 0 1 5.6 4.6Z",
        )],
        2.2,
    ),
    fill(&[
        Outline::Path("M5.2 17.4L9.6 12.3L12.6 15.4L14.6 13.4L18.8 17.4Z"),
        circle(15.6, 9.2, 1.8),
    ]),
];

/// A screen on a pedestal.
const DISPLAY: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M5.2 3.8H18.8A2.2 2.2 0 0 1 21 6V14.2A2.2 2.2 0 0 1 18.8 16.4H5.2\
             A2.2 2.2 0 0 1 3 14.2V6A2.2 2.2 0 0 1 5.2 3.8Z",
        )],
        2.2,
    ),
    fill(&[Outline::Path(
        "M10.9 16.4H13.1L13.5 19.2H15.8A0.9 0.9 0 0 1 15.8 21H8.2\
         A0.9 0.9 0 0 1 8.2 19.2H10.5Z",
    )]),
];

/// A padlock with a keyhole.
const LOCK: &[Mark] = &[
    Mark::Stroke {
        outlines: &[Outline::Path("M8.2 10.6V8.2A3.8 3.8 0 0 1 15.8 8.2V10.6")],
        width: 2.2,
        cap: LineCap::Butt,
        join: LineJoin::Round,
    },
    cut(&[
        Outline::Path(
            "M7.4 10.2H16.6A2.2 2.2 0 0 1 18.8 12.4V18.6A2.2 2.2 0 0 1 16.6 20.8H7.4\
             A2.2 2.2 0 0 1 5.2 18.6V12.4A2.2 2.2 0 0 1 7.4 10.2Z",
        ),
        Outline::Path(
            "M12 13.2A1.5 1.5 0 0 1 12.8 15.96L13.1 17.9H10.9L11.2 15.96\
             A1.5 1.5 0 0 1 12 13.2Z",
        ),
    ]),
];

/// A crescent moon beside two stars.
const SCREENSAVER: &[Mark] = &[fill(&[
    Outline::Path("M9.613 4.918A8.2 8.2 0 1 0 18.931 15.084A7.2 7.2 0 0 1 9.613 4.918Z"),
    Outline::Path("M17.6 2.9Q17.95 4.85 19.9 5.2Q17.95 5.55 17.6 7.5Q17.25 5.55 15.3 5.2Q17.25 4.85 17.6 2.9Z"),
    Outline::Path("M20.4 8.5Q20.62 9.78 21.9 10Q20.62 10.22 20.4 11.5Q20.18 10.22 18.9 10Q20.18 9.78 20.4 8.5Z"),
])];

/// The power mark: a broken ring and the bar through its gap.
const POWER: &[Mark] = &[stroke(
    &[
        Outline::Path("M16.952 7.501A7.4 7.4 0 1 1 7.048 7.501"),
        Outline::Path("M12 3.4V11.6"),
    ],
    2.3,
)];

/// A globe: its rim, a meridian and the equator.
const GLOBE: &[Mark] = &[stroke(
    &[
        circle(12.0, 12.0, 9.1),
        Outline::Path(
            "M12 2.9C9.7 5.3 8.4 8.5 8.4 12C8.4 15.5 9.7 18.7 12 21.1\
             C14.3 18.7 15.6 15.5 15.6 12C15.6 8.5 14.3 5.3 12 2.9Z",
        ),
        Outline::Path("M2.9 12H21.1"),
    ],
    1.9,
)];

/// The Bluetooth rune.
const BLUETOOTH: &[Mark] = &[stroke(
    &[Outline::Path(
        "M7 7.8L16.6 16.1L12 20.4V3.6L16.6 7.9L7 16.2",
    )],
    2.0,
)];

/// A speaker sounding.
const SPEAKER: &[Mark] = &[
    fill(&[Outline::Path(
        "M4.6 9.2H6.6L10.4 5.8C10.9 5.4 11.6 5.7 11.6 6.4V17.6C11.6 18.3 10.9 18.6 \
         10.4 18.2L6.6 14.8H4.6C3.9 14.8 3.4 14.3 3.4 13.6V10.4C3.4 9.7 3.9 9.2 4.6 9.2Z",
    )]),
    stroke(
        &[
            Outline::Path("M14.028 9.172A4 4 0 0 1 14.028 14.828"),
            Outline::Path("M16.085 6.178A7.6 7.6 0 0 1 16.085 17.822"),
        ],
        2.0,
    ),
];

/// A bell over its clapper.
const BELL: &[Mark] = &[fill(&[
    Outline::Path(
        "M12 2.8C8.7 2.8 6.6 5.4 6.6 8.8V12.9L4.9 15.7C4.5 16.4 5 17.2 5.8 17.2H18.2\
         C19 17.2 19.5 16.4 19.1 15.7L17.4 12.9V8.8C17.4 5.4 15.3 2.8 12 2.8Z",
    ),
    Outline::Path("M9.5 18.4H14.5A2.5 2.5 0 0 1 9.5 18.4Z"),
])];

/// A keyboard: two rows of keys over a space bar.
const KEYBOARD: &[Mark] = &[cut(&[
    Outline::Path(
        "M4.2 5.8H19.8A2.2 2.2 0 0 1 22 8V16A2.2 2.2 0 0 1 19.8 18.2H4.2\
         A2.2 2.2 0 0 1 2 16V8A2.2 2.2 0 0 1 4.2 5.8Z",
    ),
    Outline::Path(
        "M3.8 8h3.2v2.4h-3.2zM8.2 8h3.2v2.4h-3.2zM12.6 8h3.2v2.4h-3.2zM17 8h3.2v2.4h-3.2z",
    ),
    Outline::Path(
        "M3.8 11.4h3.2v2.4h-3.2zM8.2 11.4h3.2v2.4h-3.2zM12.6 11.4h3.2v2.4h-3.2z\
         M17 11.4h3.2v2.4h-3.2z",
    ),
    Outline::Path("M7.6 14.8h8.8v1.6h-8.8z"),
])];

/// A mouse and its wheel.
const MOUSE: &[Mark] = &[cut(&[
    Outline::Path(
        "M12 2.6C15.9 2.6 18.2 5.2 18.2 9.4V14.6C18.2 18.8 15.9 21.4 12 21.4\
         C8.1 21.4 5.8 18.8 5.8 14.6V9.4C5.8 5.2 8.1 2.6 12 2.6Z",
    ),
    Outline::Path("M12 5.6A1 1 0 0 1 13 6.6V9.4A1 1 0 0 1 11 9.4V6.6A1 1 0 0 1 12 5.6Z"),
])];

/// A trackpad with its click band.
const TRACKPAD: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M5.4 4.8H18.6A2.4 2.4 0 0 1 21 7.2V16.8A2.4 2.4 0 0 1 18.6 19.2H5.4\
             A2.4 2.4 0 0 1 3 16.8V7.2A2.4 2.4 0 0 1 5.4 4.8Z",
        )],
        2.1,
    ),
    Mark::Stroke {
        outlines: &[Outline::Path("M3.8 15.3H20.2")],
        width: 1.5,
        cap: LineCap::Butt,
        join: LineJoin::Round,
    },
];

/// A touch on an upright screen.
const TOUCHSCREEN: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M7.6 2.4H16.4A2.6 2.6 0 0 1 19 5V19A2.6 2.6 0 0 1 16.4 21.6H7.6\
             A2.6 2.6 0 0 1 5 19V5A2.6 2.6 0 0 1 7.6 2.4Z",
        )],
        2.0,
    ),
    fill(&[circle(12.0, 12.0, 1.9)]),
    stroke(&[circle(12.0, 12.0, 4.1)], 1.4),
];

/// A printer feeding a sheet.
const PRINTER: &[Mark] = &[
    cut(&[
        Outline::Path(
            "M5 7.6H19A2.2 2.2 0 0 1 21.2 9.8V14.8A2.2 2.2 0 0 1 19 17H18.2V13.2H5.8V17H5\
             A2.2 2.2 0 0 1 2.8 14.8V9.8A2.2 2.2 0 0 1 5 7.6Z",
        ),
        circle(17.9, 10.4, 0.9),
    ]),
    fill(&[
        Outline::Path("M7.4 3H16.6V6.8H7.4Z"),
        Outline::Path("M7 14.4H17V20.6A0.6 0.6 0 0 1 16.4 21.2H7.6A0.6 0.6 0 0 1 7 20.6Z"),
    ]),
];

/// A figure with arms open.
const ACCESSIBILITY: &[Mark] = &[
    fill(&[circle(12.0, 5.2, 2.1)]),
    stroke(
        &[
            Outline::Path("M5.6 9.2L12 10.6L18.4 9.2"),
            Outline::Path("M12 10.6V14.4"),
            Outline::Path("M12 14.4L9.2 20.2M12 14.4L14.8 20.2"),
        ],
        2.2,
    ),
];

/// A speech bubble carrying a letter.
const LANGUAGE: &[Mark] = &[cut(&[
    Outline::Path(
        "M5.8 3H18.2C20.4 3 21.8 4.4 21.8 6.6V14C21.8 16.2 20.4 17.6 18.2 17.6H11.4\
         L6.9 21.2C6.5 21.5 5.9 21.2 5.9 20.7V17.6H5.8C3.6 17.6 2.2 16.2 2.2 14V6.6\
         C2.2 4.4 3.6 3 5.8 3Z",
    ),
    Outline::Path("M12 4.9L16.9 15.8H14.4L13.5 13.6H10.5L9.6 15.8H7.1Z"),
    Outline::Path("M12 8.6L12.85 11.5H11.15Z"),
])];

/// Three joined nodes: one thing reaching two others.
const SHARING: &[Mark] = &[
    fill(&[
        circle(17.4, 5.8, 2.7),
        circle(6.6, 12.0, 2.7),
        circle(17.4, 18.2, 2.7),
    ]),
    stroke(&[Outline::Path("M6.6 12L17.4 5.8M6.6 12L17.4 18.2")], 1.8),
];

/// Two people, one before the other.
const USERS: &[Mark] = &[fill(&[
    circle(9.2, 8.4, 3.3),
    Outline::Path(
        "M2.8 19.4C2.8 15.5 5.4 13.1 9.2 13.1C13 13.1 15.6 15.5 15.6 19.4\
         C15.6 20 15.2 20.4 14.6 20.4H3.8C3.2 20.4 2.8 20 2.8 19.4Z",
    ),
    circle(16.4, 7.6, 2.7),
    Outline::Path(
        "M17.1 11.7C19.7 11.9 21.4 14 21.4 17.2C21.4 17.7 21 18.1 20.5 18.1H17.3\
         C17 15.6 15.9 13.6 14.1 12.4C15 11.9 16 11.6 17.1 11.7Z",
    ),
])];

/// Two stacked drives, each with its lamp.
const STORAGE: &[Mark] = &[cut(&[
    Outline::Path(
        "M5 4H19A2 2 0 0 1 21 6V9.6A2 2 0 0 1 19 11.6H5A2 2 0 0 1 3 9.6V6A2 2 0 0 1 5 4Z",
    ),
    circle(17.6, 7.8, 1.15),
    Outline::Path(
        "M5 12.8H19A2 2 0 0 1 21 14.8V18.4A2 2 0 0 1 19 20.4H5A2 2 0 0 1 3 18.4V14.8\
         A2 2 0 0 1 5 12.8Z",
    ),
    circle(17.6, 16.6, 1.15),
])];

/// A lower-case `i` in a ring.
const ABOUT: &[Mark] = &[
    stroke(&[circle(12.0, 12.0, 9.1)], 1.9),
    fill(&[circle(12.0, 7.3, 1.45)]),
    stroke(&[Outline::Path("M12 11.2V17")], 2.4),
];

/// A bold cross: the error mark.
const CROSS: &[Mark] = &[stroke(
    &[Outline::Path("M6.6 6.6L17.4 17.4M17.4 6.6L6.6 17.4")],
    3.4,
)];

/// A bold exclamation mark: the warning mark.
const EXCLAMATION: &[Mark] = &[
    stroke(&[Outline::Path("M12 3.6V14")], 3.6),
    fill(&[circle(12.0, 19.5, 2.1)]),
];

/// A bold lower-case `i`: the information mark.
const LETTER_I: &[Mark] = &[
    fill(&[circle(12.0, 4.6, 2.1)]),
    stroke(&[Outline::Path("M12 10.2V20.2")], 3.6),
];

/// A bold question mark.
const QUESTION: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M7.4 8.2C7.4 5.3 9.5 3.4 12.2 3.4C15 3.4 17 5.2 17 7.8C17 10.1 15.6 11.2 14.2 12.2\
             C13 13.1 12.2 13.9 12.2 15.2",
        )],
        3.2,
    ),
    fill(&[circle(12.2, 20.0, 2.0)]),
];

/// An arrow passing into a door frame.
const STARTUP: &[Mark] = &[stroke(
    &[
        Outline::Path("M13.6 4H17.6A2.4 2.4 0 0 1 20 6.4V17.6A2.4 2.4 0 0 1 17.6 20H13.6"),
        Outline::Path("M3.8 12H13.4"),
        Outline::Path("M9.8 8.2L13.6 12L9.8 15.8"),
    ],
    2.1,
)];

/// A memory chip: its body, the window over its die, and a row of pins down
/// either side.
const CHIP: &[Mark] = &[
    cut(&[
        Outline::Path(
            "M8.2 4.4H15.8A1.8 1.8 0 0 1 17.6 6.2V17.8A1.8 1.8 0 0 1 15.8 19.6H8.2\
             A1.8 1.8 0 0 1 6.4 17.8V6.2A1.8 1.8 0 0 1 8.2 4.4Z",
        ),
        Outline::Path(
            "M10.7 9.8H13.3A0.9 0.9 0 0 1 14.2 10.7V13.3A0.9 0.9 0 0 1 13.3 14.2H10.7\
             A0.9 0.9 0 0 1 9.8 13.3V10.7A0.9 0.9 0 0 1 10.7 9.8Z",
        ),
    ]),
    stroke(
        &[Outline::Path(
            "M3.6 8.4H6.4M3.6 12H6.4M3.6 15.6H6.4M17.6 8.4H20.4M17.6 12H20.4M17.6 15.6H20.4",
        )],
        1.6,
    ),
];

/// A board seen from above: its header's pins along one edge, and a fan
/// over its processor.
const BOARD: &[Mark] = &[
    cut(&[
        Outline::Path(
            "M4.6 4.2H19.4A1.8 1.8 0 0 1 21.2 6V18A1.8 1.8 0 0 1 19.4 19.8H4.6\
             A1.8 1.8 0 0 1 2.8 18V6A1.8 1.8 0 0 1 4.6 4.2Z",
        ),
        circle(5.6, 6.9, 0.7),
        circle(7.8, 6.9, 0.7),
        circle(10.0, 6.9, 0.7),
        circle(12.2, 6.9, 0.7),
        circle(14.4, 6.9, 0.7),
        circle(16.6, 6.9, 0.7),
        circle(18.8, 6.9, 0.7),
        circle(13.0, 13.6, 4.4),
    ]),
    fill(&[circle(13.0, 13.6, 1.2)]),
    stroke(
        &[Outline::Path(
            "M13 12.4C13 10.6 14.2 9.9 15.2 10.4M14.1 14.2C15.7 15.1 15.6 16.5 14.7 17.1\
             M11.9 14.2C10.3 15.1 9.3 14.2 9.3 13.1",
        )],
        1.2,
    ),
];

/// A clock face and its two hands.
const CLOCK: &[Mark] = &[
    stroke(&[circle(12.0, 12.0, 9.1)], 1.9),
    stroke(&[Outline::Path("M12 6.6V12L15.6 14.2")], 2.1),
];

/// A network plug seen end on: its contacts, the latch step and the cable.
const PLUG: &[Mark] = &[cut(&[
    Outline::Path(
        "M6.8 3.5H17.2A1.8 1.8 0 0 1 19 5.3V12.5A1.8 1.8 0 0 1 17.2 14.3H15.4V16.9H13.6V20.5\
         H10.4V16.9H8.6V14.3H6.8A1.8 1.8 0 0 1 5 12.5V5.3A1.8 1.8 0 0 1 6.8 3.5Z",
    ),
    Outline::Path("M7.85 5.9H9.35V9.9H7.85ZM11.25 5.9H12.75V9.9H11.25ZM14.65 5.9H16.15V9.9H14.65Z"),
])];

/// The wireless fan: three widening arcs over the point they spread from.
const WIFI: &[Mark] = &[
    stroke(
        &[
            Outline::Path("M9.313 14.613A3.8 3.8 0 0 1 14.687 14.613"),
            Outline::Path("M6.697 11.997A7.5 7.5 0 0 1 17.303 11.997"),
            Outline::Path("M4.08 9.38A11.2 11.2 0 0 1 19.92 9.38"),
        ],
        2.0,
    ),
    fill(&[circle(12.0, 17.3, 1.5)]),
];

/// A signpost: two boards pointing opposite ways from one post.
const SIGNPOST: &[Mark] = &[fill(&[
    Outline::Path("M11 3.2H13V21.2H11Z"),
    Outline::Path(
        "M5.4 4.6H16.4L19.4 7.3L16.4 10H5.4A0.8 0.8 0 0 1 4.6 9.2V5.4\
         A0.8 0.8 0 0 1 5.4 4.6Z",
    ),
    Outline::Path(
        "M18.6 11.8H7.6L4.6 14.5L7.6 17.2H18.6A0.8 0.8 0 0 0 19.4 16.4V12.6\
         A0.8 0.8 0 0 0 18.6 11.8Z",
    ),
])];

/// Traffic both ways: an arrow up beside an arrow down.
const EXCHANGE: &[Mark] = &[stroke(
    &[
        Outline::Path("M8.6 19.4V5.2"),
        Outline::Path("M4.9 8.9L8.6 5.2L12.3 8.9"),
        Outline::Path("M15.4 4.6V18.8"),
        Outline::Path("M11.7 15.1L15.4 18.8L19.1 15.1"),
    ],
    2.1,
)];

/// Three faders on their tracks, each at its own level.
const FADERS: &[Mark] = &[
    stroke(
        &[
            Outline::Path("M6 4V20"),
            Outline::Path("M12 4V20"),
            Outline::Path("M18 4V20"),
        ],
        1.6,
    ),
    fill(&[
        Outline::Path("M3.6 13.2H8.4V16.4H3.6Z"),
        Outline::Path("M9.6 6.8H14.4V10H9.6Z"),
        Outline::Path("M15.6 10.6H20.4V13.8H15.6Z"),
    ]),
];

/// Two notes joined by their beam.
const NOTES: &[Mark] = &[fill(&[
    circle(6.0, 17.6, 2.4),
    circle(15.8, 15.6, 2.4),
    Outline::Path("M7.6 17.6V6L19.4 3.4V15.6H17.8V6.4L9.2 8.3V17.6Z"),
])];

/// A painter's palette, its thumb notch and four wells of paint.
const PALETTE: &[Mark] = &[cut(&[
    Outline::Path(
        "M12 3.2C17.1 3.2 21 6.6 21 10.9C21 13.9 18.9 15.6 16.4 15.6H14.9\
         C13.8 15.6 13.1 16.3 13.1 17.2C13.1 17.8 13.4 18.2 13.6 18.6\
         C13.9 19.2 13.6 20.8 11.8 20.8C7 20.8 3 16.9 3 12C3 7.1 7 3.2 12 3.2Z",
    ),
    circle(7.4, 12.2, 1.55),
    circle(8.9, 7.9, 1.55),
    circle(13.1, 6.4, 1.55),
    circle(17.1, 9.3, 1.55),
])];

/// A monitor on its stand, beside a tower with a drive slot and power button.
const COMPUTER: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M5 5.2H12.9A1.6 1.6 0 0 1 14.5 6.8V12.4A1.6 1.6 0 0 1 12.9 14H5\
             A1.6 1.6 0 0 1 3.4 12.4V6.8A1.6 1.6 0 0 1 5 5.2Z",
        )],
        1.8,
    ),
    fill(&[Outline::Path(
        "M7.9 14H10L10.3 16.6H11.9A0.8 0.8 0 0 1 11.9 18.2H6A0.8 0.8 0 0 1 6 16.6H7.6Z",
    )]),
    cut(&[
        Outline::Path(
            "M17.6 3.6H20.4A1.2 1.2 0 0 1 21.6 4.8V19.2A1.2 1.2 0 0 1 20.4 20.4H17.6\
             A1.2 1.2 0 0 1 16.4 19.2V4.8A1.2 1.2 0 0 1 17.6 3.6Z",
        ),
        Outline::Path("M17.8 6.2H20.2V7.4H17.8Z"),
        circle(19.0, 17.4, 0.85),
    ]),
];

/// An open lid over a keyboard deck with its finger notch.
const LAPTOP: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M6.2 4.4H17.8A1.6 1.6 0 0 1 19.4 6V15H4.6V6A1.6 1.6 0 0 1 6.2 4.4Z",
        )],
        1.8,
    ),
    cut(&[
        Outline::Path(
            "M1.8 16.4H22.2L21.4 18.8A1.6 1.6 0 0 1 19.9 19.8H4.1A1.6 1.6 0 0 1 2.6 18.8Z",
        ),
        Outline::Path("M10.2 16.4H13.8V17.3H10.2Z"),
    ]),
];

/// Three stacked rack units, each with a drive slot and a status light.
const SERVER: &[Mark] = &[cut(&[
    Outline::Path(
        "M4.8 2.6H19.2A1.4 1.4 0 0 1 20.6 4V6.8A1.4 1.4 0 0 1 19.2 8.2H4.8\
         A1.4 1.4 0 0 1 3.4 6.8V4A1.4 1.4 0 0 1 4.8 2.6Z",
    ),
    Outline::Path("M6.2 4.8H12V6H6.2Z"),
    circle(17.4, 5.4, 0.85),
    Outline::Path(
        "M4.8 9.2H19.2A1.4 1.4 0 0 1 20.6 10.6V13.4A1.4 1.4 0 0 1 19.2 14.8H4.8\
         A1.4 1.4 0 0 1 3.4 13.4V10.6A1.4 1.4 0 0 1 4.8 9.2Z",
    ),
    Outline::Path("M6.2 11.4H12V12.6H6.2Z"),
    circle(17.4, 12.0, 0.85),
    Outline::Path(
        "M4.8 15.8H19.2A1.4 1.4 0 0 1 20.6 17.2V20A1.4 1.4 0 0 1 19.2 21.4H4.8\
         A1.4 1.4 0 0 1 3.4 20V17.2A1.4 1.4 0 0 1 4.8 15.8Z",
    ),
    Outline::Path("M6.2 18H12V19.2H6.2Z"),
    circle(17.4, 18.6, 0.85),
])];

/// A low box of status lights and a port, under two splayed antennas.
const ROUTER: &[Mark] = &[
    stroke(
        &[Outline::Path("M6.2 11.6L4.8 4.4M17.8 11.6L19.2 4.4")],
        1.8,
    ),
    cut(&[
        Outline::Path(
            "M4.4 11.6H19.6A1.8 1.8 0 0 1 21.4 13.4V17.6A1.8 1.8 0 0 1 19.6 19.4H4.4\
             A1.8 1.8 0 0 1 2.6 17.6V13.4A1.8 1.8 0 0 1 4.4 11.6Z",
        ),
        circle(6.6, 15.5, 0.85),
        circle(9.4, 15.5, 0.85),
        circle(12.2, 15.5, 0.85),
        Outline::Path("M15.4 14.9H18.6V16.1H15.4Z"),
    ]),
];

/// A flat box of five ports, between traffic passing one way above it and the
/// other way below.
const NETWORK_SWITCH: &[Mark] = &[
    stroke(
        &[Outline::Path(
            "M6.8 4.6H17.2M15 2.6L17.2 4.6L15 6.6M17.2 19.4H6.8M9 17.4L6.8 19.4L9 21.4",
        )],
        1.6,
    ),
    cut(&[
        Outline::Path(
            "M3.2 8.6H20.8A1.6 1.6 0 0 1 22.4 10.2V13.8A1.6 1.6 0 0 1 20.8 15.4H3.2\
             A1.6 1.6 0 0 1 1.6 13.8V10.2A1.6 1.6 0 0 1 3.2 8.6Z",
        ),
        Outline::Path(
            "M4.1 10.6H6.3V13.4H4.1ZM7.5 10.6H9.7V13.4H7.5ZM10.9 10.6H13.1V13.4H10.9Z\
             M14.3 10.6H16.5V13.4H14.3ZM17.7 10.6H19.9V13.4H17.7Z",
        ),
    ]),
];

/// A handset: its screen and home button cut from a rounded body.
const PHONE: &[Mark] = &[cut(&[
    Outline::Path(
        "M8.8 2.2H15.2A2.4 2.4 0 0 1 17.6 4.6V19.4A2.4 2.4 0 0 1 15.2 21.8H8.8\
         A2.4 2.4 0 0 1 6.4 19.4V4.6A2.4 2.4 0 0 1 8.8 2.2Z",
    ),
    Outline::Path("M8.2 4.8H15.8V17.4H8.2Z"),
    circle(12.0, 19.6, 0.95),
])];

/// A storage box: two drive bays over a pair of activity lights.
const NAS: &[Mark] = &[cut(&[
    Outline::Path(
        "M6.4 2.6H17.6A1.8 1.8 0 0 1 19.4 4.4V19.6A1.8 1.8 0 0 1 17.6 21.4H6.4\
         A1.8 1.8 0 0 1 4.6 19.6V4.4A1.8 1.8 0 0 1 6.4 2.6Z",
    ),
    Outline::Path("M6.8 5H17.2V8.6H6.8ZM6.8 10.4H17.2V14H6.8Z"),
    circle(8.4, 17.6, 0.9),
    circle(11.4, 17.6, 0.9),
])];

/// A smart bulb: the glass with its filament cut through, on a two-ring base.
const SMART_BULB: &[Mark] = &[
    cut(&[
        Outline::Path("M9.2 17.2V15.6A6.4 6.4 0 1 1 14.8 15.6V17.2Z"),
        Outline::Path("M11.3 15.6V12.6L10.2 11.2H13.8L12.7 12.6V15.6Z"),
    ]),
    fill(&[
        Outline::Path("M9.4 18.4H14.6V19.6H9.4Z"),
        Outline::Path("M10.2 20.6H13.8A0.6 0.6 0 0 1 13.8 21.8H10.2A0.6 0.6 0 0 1 10.2 20.6Z"),
    ]),
];

/// A question mark in an empty device outline.
const UNKNOWN_DEVICE: &[Mark] = &[
    stroke(
        &[
            Outline::Path(
                "M5.6 3.6H18.4A2 2 0 0 1 20.4 5.6V18.4A2 2 0 0 1 18.4 20.4H5.6\
                 A2 2 0 0 1 3.6 18.4V5.6A2 2 0 0 1 5.6 3.6Z",
            ),
            Outline::Path("M9.4 9.6A2.7 2.7 0 1 1 13.4 11.9L12.6 12.5A1.5 1.5 0 0 0 12 13.7V14.2"),
        ],
        1.8,
    ),
    fill(&[circle(12.0, 17.0, 1.05)]),
];

/// Three hosts tapped onto one shared line.
const SEGMENT: &[Mark] = &[
    stroke(
        &[Outline::Path("M3 12H21M6.5 12V8.4M12 12V15.6M17.5 12V8.4")],
        1.7,
    ),
    fill(&[
        Outline::Path(
            "M5.3 4.4H7.7A0.8 0.8 0 0 1 8.5 5.2V7.6A0.8 0.8 0 0 1 7.7 8.4H5.3\
             A0.8 0.8 0 0 1 4.5 7.6V5.2A0.8 0.8 0 0 1 5.3 4.4Z",
        ),
        Outline::Path(
            "M10.8 15.6H13.2A0.8 0.8 0 0 1 14 16.4V18.8A0.8 0.8 0 0 1 13.2 19.6H10.8\
             A0.8 0.8 0 0 1 10 18.8V16.4A0.8 0.8 0 0 1 10.8 15.6Z",
        ),
        Outline::Path(
            "M16.3 4.4H18.7A0.8 0.8 0 0 1 19.5 5.2V7.6A0.8 0.8 0 0 1 18.7 8.4H16.3\
             A0.8 0.8 0 0 1 15.5 7.6V5.2A0.8 0.8 0 0 1 16.3 4.4Z",
        ),
    ]),
];

/// A penguin standing front on: its face and belly white, its eyes, beak and
/// feet dark.
const PENGUIN: &[Mark] = &[
    cut(&[
        Outline::Path(
            "M12 2.4C14.7 2.4 16.4 4.5 16.4 7.2C16.4 8.6 17 9.6 17.9 10.9\
             C19.1 12.6 19.7 14.7 19.3 16.9C18.9 19.2 16.6 20.7 12 20.7\
             C7.4 20.7 5.1 19.2 4.7 16.9C4.3 14.7 4.9 12.6 6.1 10.9\
             C7 9.6 7.6 8.6 7.6 7.2C7.6 4.5 9.3 2.4 12 2.4Z",
        ),
        Outline::Path(
            "M12 4.5C13.5 4.5 14.6 5.6 14.6 7C14.6 8.7 13.4 9.9 12 10.5\
             C10.6 9.9 9.4 8.7 9.4 7C9.4 5.6 10.5 4.5 12 4.5Z",
        ),
        Outline::Path(
            "M12 11.4C14.4 11.4 15.9 13.6 15.9 16C15.9 18.3 14.3 19.5 12 19.5\
             C9.7 19.5 8.1 18.3 8.1 16C8.1 13.6 9.6 11.4 12 11.4Z",
        ),
    ]),
    fill(&[
        circle(10.8, 6.6, 0.8),
        circle(13.2, 6.6, 0.8),
        Outline::Path("M10.6 8.2H13.4L12 9.8Z"),
        Outline::Path(
            "M6.6 20.3C6.6 19.3 8.2 18.8 10 19.1C11.4 19.4 11.6 20.8 10.6 21.3\
             C9.4 21.9 6.6 21.6 6.6 20.3Z",
        ),
        Outline::Path(
            "M17.4 20.3C17.4 19.3 15.8 18.8 14 19.1C12.6 19.4 12.4 20.8 13.4 21.3\
             C14.6 21.9 17.4 21.6 17.4 20.3Z",
        ),
    ]),
];

/// A window of four panes in its frame.
const PANED_WINDOW: &[Mark] = &[stroke(
    &[
        Outline::Path(
            "M5.4 3.6H18.6A1.8 1.8 0 0 1 20.4 5.4V18.6A1.8 1.8 0 0 1 18.6 20.4H5.4\
             A1.8 1.8 0 0 1 3.6 18.6V5.4A1.8 1.8 0 0 1 5.4 3.6Z",
        ),
        Outline::Path("M12 3.6V20.4M3.6 12H20.4"),
    ],
    1.9,
)];

/// The looped square: four loops joined by the square their lines cross in.
const LOOPED_SQUARE: &[Mark] = &[stroke(
    &[Outline::Path(
        "M8.7 6A2.7 2.7 0 1 0 6 8.7H18A2.7 2.7 0 1 0 15.3 6V18\
         A2.7 2.7 0 1 0 18 15.3H6A2.7 2.7 0 1 0 8.7 18Z",
    )],
    1.9,
)];

/// A robot's domed head with its two eyes and splayed antennas.
const ROBOT_HEAD: &[Mark] = &[
    cut(&[
        Outline::Path("M3.8 17.2A8.2 8.2 0 0 1 20.2 17.2Z"),
        circle(8.7, 13.4, 1.15),
        circle(15.3, 13.4, 1.15),
    ]),
    stroke(
        &[Outline::Path("M8.3 10.8L6.6 7.9M15.7 10.8L17.4 7.9")],
        1.4,
    ),
];
