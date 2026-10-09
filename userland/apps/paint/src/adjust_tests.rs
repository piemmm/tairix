use tairix_colour::Rgb;
use tairix_controls::Keystroke;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::{Theme, ThemeRegistry};

use super::{AdjustOutcome, AdjustPane, Pick, Role};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::filter::Filter;
use crate::histogram::Histogram;
use crate::layout::Faces;
use crate::panel::{Part, Placed};
use crate::tone::{
    Channel, ColourBalance, Curves, HueRange, HueRanges, Levels, Tones, WhiteBalance,
};

const BOUNDS: Rect = Rect::new(600, 100, 216, 900);
const VIEWPORT: Rect = Rect::new(0, 0, 900, 1100);

struct Pane {
    pane: AdjustPane,
    registry: ThemeRegistry,
}

impl Pane {
    fn of(filter: Filter) -> Self {
        Self {
            pane: AdjustPane::setting(filter),
            registry: ThemeRegistry::with_builtins(),
        }
    }

    fn theme(&self) -> &Theme {
        self.registry.active()
    }

    fn context(&self) -> (Faces, Scale, &Theme) {
        (
            Faces::of(self.theme(), Scale::ONE),
            Scale::ONE,
            self.theme(),
        )
    }

    /// Where the part playing `role` is laid out.
    fn placed(&self, role: Role) -> Placed {
        let part = self
            .pane
            .roles
            .iter()
            .position(|&at| at == role)
            .expect("a part plays it");
        let (faces, scale, theme) = self.context();
        self.pane
            .panel
            .place_of(part, BOUNDS, faces, scale, theme)
            .expect("laid out")
    }

    fn pointer(&mut self, event: InputEvent) -> AdjustOutcome {
        let registry = ThemeRegistry::with_builtins();
        let theme = registry.active();
        let faces = Faces::of(theme, Scale::ONE);
        self.pane.on_pointer(
            &event,
            (BOUNDS, VIEWPORT),
            (faces, Scale::ONE, theme),
            &mut Region::new(),
        )
    }

    fn click(&mut self, at: Point) -> AdjustOutcome {
        self.pointer(InputEvent::PointerMoved { to: at });
        let pressed = self.pointer(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        let released = self.pointer(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        if released == AdjustOutcome::Ignored || released == AdjustOutcome::Taken {
            pressed
        } else {
            released
        }
    }

    fn key(&mut self, key: Key) -> AdjustOutcome {
        self.stroke(key, Modifiers::default())
    }

    /// `key` with Ctrl held: Ctrl+A selects a field's text.
    fn chord(&mut self, key: Key) -> AdjustOutcome {
        self.stroke(
            key,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        )
    }

    fn stroke(&mut self, key: Key, modifiers: Modifiers) -> AdjustOutcome {
        let registry = ThemeRegistry::with_builtins();
        let theme = registry.active();
        let faces = Faces::of(theme, Scale::ONE);
        self.pane.on_key(
            Keystroke {
                key,
                modifiers,
                at_ns: 0,
            },
            (BOUNDS, VIEWPORT),
            (faces, Scale::ONE, theme),
            &mut Region::new(),
        )
    }

    /// Type `text` over what the field of `role`'s cell `cell` holds.
    fn type_into(&mut self, role: Role, cell: usize, text: &str) -> AdjustOutcome {
        let field = self.placed(role).controls[cell];
        self.click(field.center());
        self.chord(Key::Char('a'));
        let mut last = AdjustOutcome::Ignored;
        for character in text.chars() {
            last = self.key(Key::Char(character));
        }
        last
    }

    fn levels(&self) -> Levels {
        match self.pane.filter() {
            Some(Filter::Levels(levels)) => levels,
            other => panic!("levels, not {other:?}"),
        }
    }
}

#[test]
fn the_list_opens_the_adjustment_chosen() {
    let mut pane = Pane {
        pane: AdjustPane::choosing(),
        registry: ThemeRegistry::with_builtins(),
    };
    assert_eq!(pane.pane.filter(), None);
    assert_eq!(pane.pane.title(), "Adjustment");
    let field = pane.placed(Role::Choose).controls[0];
    pane.click(field.center());
    assert!(pane.pane.listing());
    for _ in 0..3 {
        pane.key(Key::Named(NamedKey::Down));
    }
    let opened = pane.key(Key::Named(NamedKey::Enter));
    let AdjustOutcome::Open(filter) = opened else {
        panic!("an adjustment opened, not {opened:?}");
    };
    assert!(filter.has_settings());
}

#[test]
fn levels_handles_and_fields_set_the_channel_shown() {
    let mut pane = Pane::of(Filter::Levels(Levels::IDENTITY));
    assert_eq!(pane.pane.title(), "Levels");
    let track = pane.placed(Role::Input).controls[0];
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(track.left() + 4, track.top() + 12),
    });
    pane.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(
            track.left() + tairix_geometry::to_i32(track.width / 4),
            track.top() + 12,
        ),
    });
    let released = pane.pointer(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(released, AdjustOutcome::Changed { settled: true });
    let black = pane.levels().of(Channel::Composite).black;
    assert!((50..80).contains(&black), "{black}");
    assert_eq!(
        pane.type_into(Role::InputFields, 1, "1.5"),
        AdjustOutcome::Changed { settled: false }
    );
    assert_eq!(pane.levels().of(Channel::Composite).gamma, 150);
    let channel = pane.placed(Role::Channel).controls[0];
    pane.click(channel.center());
    pane.key(Key::Named(NamedKey::Down));
    assert_eq!(
        pane.key(Key::Named(NamedKey::Enter)),
        AdjustOutcome::Taken,
        "red shown"
    );
    pane.type_into(Role::OutputFields, 1, "200");
    let levels = pane.levels();
    assert_eq!(levels.of(Channel::Red).out_white, 200);
    assert_eq!(
        levels.of(Channel::Composite).out_white,
        255,
        "the composite as it was"
    );
}

#[test]
fn reset_and_escape_return_the_settings_to_where_they_started() {
    let mut pane = Pane::of(Filter::Levels(Levels::IDENTITY));
    pane.type_into(Role::InputFields, 0, "40");
    assert_ne!(pane.levels(), Levels::IDENTITY);
    let actions = pane.placed(Role::Actions);
    assert_eq!(
        pane.click(actions.controls[0].center()),
        AdjustOutcome::Changed { settled: true }
    );
    assert_eq!(pane.levels(), Levels::IDENTITY);
    pane.type_into(Role::InputFields, 2, "200");
    pane.key(Key::Named(NamedKey::Enter));
    assert_eq!(
        pane.key(Key::Named(NamedKey::Escape)),
        AdjustOutcome::Changed { settled: true }
    );
    assert_eq!(pane.levels(), Levels::IDENTITY);
    assert_eq!(
        pane.click(actions.controls[1].center()),
        AdjustOutcome::Apply
    );
}

#[test]
fn an_eyedropper_is_taken_up_set_from_and_put_down() {
    let mut pane = Pane::of(Filter::Levels(Levels::IDENTITY));
    let pickers = pane.placed(Role::Pickers);
    assert_eq!(
        pane.click(pickers.controls[0].center()),
        AdjustOutcome::Picking
    );
    assert_eq!(pane.pane.picking(), Some(Pick::Black));
    assert_eq!(
        pane.click(pickers.controls[2].center()),
        AdjustOutcome::Picking
    );
    assert_eq!(pane.pane.picking(), Some(Pick::White), "one up at a time");
    assert!(pane.pane.picked(Rgb::new(200, 210, 220)));
    assert_eq!(pane.pane.picking(), None);
    assert_eq!(pane.levels().of(Channel::Blue).white, 220);
    assert!(!pane.pane.picked(Rgb::new(1, 1, 1)), "nothing is up");
    pane.click(pickers.controls[1].center());
    assert!(pane.pane.put_down_pick());
    assert_eq!(pane.pane.picking(), None);
}

fn spread_canvas() -> Canvas {
    let mut built =
        CanvasBuilder::new(256, 1, Kind::Rgba, Sample::Rgba([0, 0, 0, 255])).expect("fits");
    for x in 0..256u32 {
        let level = u8::try_from(x / 2 + 40).expect("a level");
        built.set(x, 0, Sample::Rgba([level, level, level, 255]));
    }
    built.finish()
}

#[test]
fn auto_waits_for_a_histogram_and_then_stretches_each_channel() {
    let mut pane = Pane::of(Filter::Levels(Levels::IDENTITY));
    assert!(pane.pane.reads_histogram());
    let auto = pane.placed(Role::Pickers).controls[3];
    assert_eq!(
        pane.click(auto.center()),
        AdjustOutcome::Taken,
        "withheld without one"
    );
    assert!(pane.pane.set_histogram_ready(true));
    assert_eq!(pane.click(auto.center()), AdjustOutcome::Auto);
    let histogram = Histogram::of(&spread_canvas(), None).expect("room");
    assert!(pane.pane.auto(&histogram));
    let red = *pane.levels().of(Channel::Red);
    assert!(red.black >= 40 && red.white <= 167, "{red:?}");
}

#[test]
fn a_curve_point_added_on_the_graph_is_set_by_its_fields() {
    let mut pane = Pane::of(Filter::Curves(Curves::IDENTITY));
    let graph = pane.placed(Role::Curve).controls[0];
    let added = pane.click(Point::new(
        graph.left() + tairix_geometry::to_i32(graph.width / 4),
        graph.center().y,
    ));
    assert_eq!(added, AdjustOutcome::Changed { settled: true });
    let Some(Filter::Curves(curves)) = pane.pane.filter() else {
        panic!("curves");
    };
    assert_eq!(curves.of(Channel::Composite).points().len(), 3);
    pane.type_into(Role::Point, 1, "30");
    let Some(Filter::Curves(curves)) = pane.pane.filter() else {
        panic!("curves");
    };
    assert_eq!(curves.of(Channel::Composite).points()[1].1, 30);
}

#[test]
fn white_balance_is_set_by_its_numbers_and_its_neutral_picker() {
    let mut pane = Pane::of(Filter::WhiteBalance(WhiteBalance::NEUTRAL));
    pane.type_into(Role::Kelvin, 0, "3200");
    assert_eq!(
        pane.pane.filter(),
        Some(Filter::WhiteBalance(WhiteBalance {
            kelvin: 3200,
            tint: 0
        }))
    );
    let pickers = pane.placed(Role::Pickers);
    assert_eq!(
        pane.click(pickers.controls[0].center()),
        AdjustOutcome::Picking
    );
    assert_eq!(pane.pane.picking(), Some(Pick::Neutral));
    assert!(pane.pane.picked(Rgb::new(150, 128, 100)));
    let Some(Filter::WhiteBalance(balance)) = pane.pane.filter() else {
        panic!("white balance");
    };
    assert!(
        balance.kelvin < 6500,
        "a warm cast read as a warm light: {balance:?}"
    );
}

#[test]
fn hue_and_saturation_set_the_range_shown_alone() {
    let mut pane = Pane::of(Filter::HueSaturation(HueRanges::IDENTITY));
    let range = pane.placed(Role::Range).controls[0];
    pane.click(range.center());
    pane.key(Key::Named(NamedKey::Down));
    pane.key(Key::Named(NamedKey::Down));
    pane.key(Key::Named(NamedKey::Enter));
    pane.type_into(Role::Hue, 0, "45");
    let Some(Filter::HueSaturation(ranges)) = pane.pane.filter() else {
        panic!("hue and saturation");
    };
    assert_eq!(ranges.of(HueRange::Yellows).hue, 45);
    assert_eq!(ranges.of(HueRange::Master).hue, 0);
}

#[test]
fn colour_balance_sets_the_band_shown_and_keeps_luminosity_as_switched() {
    let mut pane = Pane::of(Filter::ColourBalance(ColourBalance::NEUTRAL));
    pane.type_into(Role::Axis(2), 0, "30");
    let keep = pane.placed(Role::KeepLuminosity).controls[0];
    assert_eq!(
        pane.click(keep.center()),
        AdjustOutcome::Changed { settled: true }
    );
    let Some(Filter::ColourBalance(balance)) = pane.pane.filter() else {
        panic!("colour balance");
    };
    assert_eq!(balance.tones[Tones::Midtones.index()][2], 30);
    assert!(!balance.keep_luminosity);
}

#[test]
fn preview_switches_off_and_a_withheld_pane_takes_nothing() {
    let mut pane = Pane::of(Filter::Brightness {
        brightness: 0,
        contrast: 0,
    });
    assert!(pane.pane.previewing());
    let preview = pane.placed(Role::Preview).controls[0];
    assert_eq!(pane.click(preview.center()), AdjustOutcome::Previewed);
    assert!(!pane.pane.previewing());
    pane.pane.set_withheld(true);
    let number = pane.placed(Role::Parameter(0));
    assert_eq!(
        pane.click(number.controls[1].center()),
        AdjustOutcome::Ignored
    );
    assert!(matches!(pane.pane.panel.parts()[0], Part::Number { .. }));
    let (faces, scale, theme) = pane.context();
    assert!(pane.pane.measured_height(BOUNDS.width, faces, scale, theme) < BOUNDS.height);
}
