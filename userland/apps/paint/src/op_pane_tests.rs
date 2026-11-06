use alloc::string::String;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_controls::Keystroke;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_parallel::SERIAL;
use tairix_theme::{Theme, ThemeRegistry};

use super::{OpOutcome, OpPane, Pick, Role, Spot};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::effect::{Distribution, Noise};
use crate::histogram::Histogram;
use crate::layout::{Faces, Fit};
use crate::op::{Family, Op};
use crate::panel::{Part, Placed};
use crate::tone::{
    Channel, ChannelMixer, ColourBalance, Curves, HueRange, HueRanges, Levels, Tones, WhiteBalance,
};

const BOUNDS: Rect = Rect::new(600, 100, 216, 900);
const VIEWPORT: Rect = Rect::new(0, 0, 900, 1100);

struct Pane {
    pane: OpPane,
    registry: ThemeRegistry,
}

impl Pane {
    fn of(family: Family) -> Self {
        Self {
            pane: OpPane::new(family),
            registry: ThemeRegistry::with_builtins(),
        }
    }

    /// The pane with `op` chosen, as the window chooses it once it has left
    /// the operation the selector held.
    fn on(op: Op) -> Self {
        let mut pane = Self::of(op.family());
        assert_eq!(pane.pane.applying(), None, "nothing moved to apply");
        pane.pane.choose(op.place());
        pane
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

    fn pointer(&mut self, event: InputEvent) -> OpOutcome {
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

    fn click(&mut self, at: Point) -> OpOutcome {
        self.pointer(InputEvent::PointerMoved { to: at });
        let pressed = self.pointer(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        let released = self.pointer(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        if released == OpOutcome::Ignored || released == OpOutcome::Taken {
            pressed
        } else {
            released
        }
    }

    fn key(&mut self, key: Key) -> OpOutcome {
        self.stroke(key, Modifiers::default())
    }

    /// `key` with Ctrl held: Ctrl+A selects a field's text.
    fn chord(&mut self, key: Key) -> OpOutcome {
        self.stroke(
            key,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        )
    }

    fn stroke(&mut self, key: Key, modifiers: Modifiers) -> OpOutcome {
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
    fn type_into(&mut self, role: Role, cell: usize, text: &str) -> OpOutcome {
        let field = self.placed(role).controls[cell];
        self.click(field.center());
        self.chord(Key::Char('a'));
        let mut last = OpOutcome::Ignored;
        for character in text.chars() {
            last = self.key(Key::Char(character));
        }
        last
    }

    /// Open the list playing `role`, step `downs` past its choice, and take
    /// the entry stepped to.
    fn choose_in(&mut self, role: Role, downs: usize) -> OpOutcome {
        let list = self.placed(role).controls[0];
        self.click(list.center());
        assert!(self.pane.listing(), "the list opened");
        for _ in 0..downs {
            self.key(Key::Named(NamedKey::Down));
        }
        self.key(Key::Named(NamedKey::Enter))
    }

    fn levels(&self) -> Levels {
        match self.pane.op() {
            Op::Levels(levels) => levels,
            other => panic!("levels, not {other:?}"),
        }
    }
}

#[test]
fn the_selector_lists_its_family_and_answers_the_operation_chosen() {
    for family in Family::ALL {
        let pane = Pane::of(family);
        let Part::Choice { combo, .. } = &pane.pane.panel.parts()[0] else {
            panic!("the selector first");
        };
        let listed: Vec<&str> = combo.choices().iter().map(String::as_str).collect();
        let offered: Vec<&str> = family.ops().iter().map(Op::label).collect();
        assert_eq!(listed, offered);
        assert_eq!(pane.pane.title(), family.label());
        assert_eq!(pane.pane.op(), family.first());
        assert!(!pane.pane.is_open() && !pane.pane.previewing());
    }
    let mut pane = Pane::of(Family::Adjustments);
    let switched = pane.choose_in(Role::Choose, 1);
    assert_eq!(switched, OpOutcome::Switched { to: 1 });
    assert_eq!(
        pane.pane.op(),
        Family::Adjustments.first(),
        "held until the window has left it"
    );
    assert_eq!(
        pane.pane.applying(),
        None,
        "nothing moved, so it is dropped"
    );
    pane.pane.choose(1);
    assert!(matches!(pane.pane.op(), Op::Levels(_)));
    assert!(pane.pane.is_open() && pane.pane.previewing());
}

#[test]
fn a_slider_sample_opens_the_operation_and_its_release_settles_it() {
    let mut pane = Pane::of(Family::Effects);
    assert!(!pane.pane.is_open());
    let slider = pane.placed(Role::Parameter(0)).controls[1];
    let row = slider.center().y;
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(slider.left() + 2, row),
    });
    pane.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    let sample = pane.pointer(InputEvent::PointerMoved {
        to: Point::new(slider.left() + to_i32(slider.width / 2), row),
    });
    assert_eq!(sample, OpOutcome::Changed { settled: false });
    assert!(pane.pane.is_open() && pane.pane.previewing());
    let released = pane.pointer(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(released, OpOutcome::Changed { settled: true });
    let Op::GaussianBlur { radius } = pane.pane.op() else {
        panic!("a blur");
    };
    assert!((20..30).contains(&radius), "{radius}");
    assert!(pane.pane.moved());
}

#[test]
fn leaving_a_moved_operation_answers_it_to_apply_and_one_brought_back_is_dropped() {
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    pane.type_into(Role::InputFields, 0, "40");
    let moved = pane.pane.op();
    assert!(pane.pane.moved());
    let switched = pane.choose_in(Role::Choose, 1);
    assert_eq!(switched, OpOutcome::Switched { to: 2 });
    assert_eq!(
        pane.pane.op(),
        moved,
        "its settings kept until it is applied"
    );
    assert_eq!(pane.pane.applying(), Some(moved));
    pane.pane.applied();
    pane.pane.choose(2);
    assert!(matches!(pane.pane.op(), Op::Curves(_)));
    assert!(pane.pane.is_open() && !pane.pane.moved());
    let mut pane = Pane::of(Family::Adjustments);
    pane.type_into(Role::Parameter(0), 0, "30");
    pane.type_into(Role::Parameter(0), 0, "0");
    assert!(!pane.pane.moved());
    let switched = pane.choose_in(Role::Choose, 1);
    assert_eq!(switched, OpOutcome::Switched { to: 1 });
    assert_eq!(pane.pane.applying(), None);
}

#[test]
fn only_an_operation_moved_to_change_something_is_to_apply() {
    let pane = Pane::on(Op::Invert);
    assert!(pane.pane.is_open());
    assert_eq!(
        pane.pane.applying(),
        None,
        "an operation never moved is dropped"
    );
    let mut pane = Pane::on(Op::Posterise { levels: 4 });
    pane.type_into(Role::Parameter(0), 0, "9");
    assert_eq!(pane.pane.applying(), Some(Op::Posterise { levels: 9 }));
    let mut pane = Pane::of(Family::Effects);
    pane.type_into(Role::Parameter(0), 0, "0");
    assert!(pane.pane.moved());
    assert_eq!(pane.pane.applying(), None, "moved to change nothing");
}

#[test]
fn closing_drops_what_leaving_would_have_applied() {
    let mut pane = Pane::on(Op::Posterise { levels: 4 });
    assert_eq!(pane.pane.applying(), None, "nothing moved");
    pane.type_into(Role::Parameter(0), 0, "9");
    assert_eq!(pane.pane.applying(), Some(Op::Posterise { levels: 9 }));
    assert!(pane.pane.is_open());
    pane.pane.close();
    assert!(!pane.pane.is_open());
    assert_eq!(pane.pane.op(), Op::Posterise { levels: 4 }, "dropped");
    assert_eq!(pane.pane.applying(), None);
}

#[test]
fn the_part_holding_the_keyboard_is_where_a_scrolled_body_is_brought() {
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    assert_eq!(pane.pane.focus_rect(BOUNDS, pane.context()), None);
    assert!(pane.pane.enter_focus(true, BOUNDS, &mut Region::new()));
    let selector = pane.placed(Role::Choose).rect;
    assert_eq!(pane.pane.focus_rect(BOUNDS, pane.context()), Some(selector));
    pane.key(Key::Named(NamedKey::Tab));
    let channel = pane.placed(Role::Channel).rect;
    assert_eq!(pane.pane.focus_rect(BOUNDS, pane.context()), Some(channel));
}

#[test]
fn applied_closes_the_pane_and_draws_noise_afresh() {
    let mut pane = Pane::on(Op::AddNoise(Noise::START));
    pane.type_into(Role::Parameter(0), 0, "60");
    assert!(pane.pane.moved());
    pane.pane.applied();
    assert!(!pane.pane.is_open() && !pane.pane.moved());
    let Op::AddNoise(noise) = pane.pane.op() else {
        panic!("noise");
    };
    assert_eq!(noise.amount, Noise::START.amount);
    assert_ne!(noise.seed, Noise::START.seed, "the next lays new speckle");
}

#[test]
fn noise_and_the_mixer_are_set_by_their_lists_and_switches() {
    let mut pane = Pane::on(Op::AddNoise(Noise::START));
    let spread = pane.choose_in(Role::Distribution, 1);
    assert_eq!(spread, OpOutcome::Changed { settled: true });
    let mono = pane.placed(Role::Monochrome).controls[0];
    assert_eq!(
        pane.click(mono.center()),
        OpOutcome::Changed { settled: true }
    );
    let Op::AddNoise(noise) = pane.pane.op() else {
        panic!("noise");
    };
    assert_eq!(noise.distribution, Distribution::Gaussian);
    assert!(noise.monochrome);
    let mut pane = Pane::on(Op::ChannelMixer(ChannelMixer::IDENTITY));
    assert_eq!(
        pane.choose_in(Role::Mixer, 2),
        OpOutcome::Taken,
        "blue shown"
    );
    pane.type_into(Role::Mix(0), 0, "40");
    let Op::ChannelMixer(mixer) = pane.pane.op() else {
        panic!("the mixer");
    };
    assert_eq!(mixer.rows[2], [40, 0, 100, 0]);
    assert_eq!(mixer.rows[0], [100, 0, 0, 0], "red as it was");
}

#[test]
fn levels_handles_and_fields_set_the_channel_shown() {
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    let track = pane.placed(Role::Input).controls[0];
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(track.left() + 4, track.top() + 12),
    });
    pane.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(track.left() + to_i32(track.width / 4), track.top() + 12),
    });
    let released = pane.pointer(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(released, OpOutcome::Changed { settled: true });
    let black = pane.levels().of(Channel::Composite).black;
    assert!((50..80).contains(&black), "{black}");
    assert_eq!(
        pane.type_into(Role::InputFields, 1, "1.5"),
        OpOutcome::Changed { settled: false }
    );
    assert_eq!(pane.levels().of(Channel::Composite).gamma, 150);
    assert_eq!(
        pane.choose_in(Role::Channel, 1),
        OpOutcome::Taken,
        "red shown"
    );
    pane.type_into(Role::OutputFields, 1, "200");
    let levels = pane.levels();
    assert_eq!(levels.of(Channel::Red).out_white, 200);
    assert_eq!(levels.of(Channel::Composite).out_white, 255, "as it was");
}

/// A live edit leaves the field being typed in as it is: typing `2.5` into the
/// gamma lands 2.50 rather than the first digit respelled under the caret.
#[test]
fn typing_a_decimal_is_not_respelled_under_the_caret() {
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    pane.type_into(Role::InputFields, 1, "2.5");
    assert_eq!(pane.levels().of(Channel::Composite).gamma, 250);
}

#[test]
fn a_typed_setting_repaints_only_what_it_moved() {
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    let fields = pane.placed(Role::InputFields);
    let output = pane.placed(Role::Output);
    pane.click(fields.controls[0].center());
    pane.chord(Key::Char('a'));
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut damage = Region::new();
    pane.pane.on_key(
        Keystroke {
            key: Key::Char('9'),
            modifiers: Modifiers::default(),
            at_ns: 0,
        },
        (BOUNDS, VIEWPORT),
        (Faces::of(theme, Scale::ONE), Scale::ONE, theme),
        &mut damage,
    );
    assert_eq!(pane.levels().of(Channel::Composite).black, 9);
    assert!(damage.intersects(fields.rect), "the fields it was typed in");
    assert!(
        damage.intersects(pane.placed(Role::Input).rect),
        "and the track that shows the same black"
    );
    assert!(!damage.intersects(output.rect), "but not the output levels");
}

#[test]
fn reset_and_escape_return_the_settings_to_where_they_started() {
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    pane.type_into(Role::InputFields, 0, "40");
    assert_ne!(pane.levels(), Levels::IDENTITY);
    let actions = pane.placed(Role::Actions);
    assert_eq!(
        pane.click(actions.controls[0].center()),
        OpOutcome::Changed { settled: true }
    );
    assert_eq!(pane.levels(), Levels::IDENTITY);
    pane.type_into(Role::InputFields, 2, "200");
    pane.key(Key::Named(NamedKey::Enter));
    assert_eq!(
        pane.key(Key::Named(NamedKey::Escape)),
        OpOutcome::Changed { settled: true }
    );
    assert_eq!(pane.levels(), Levels::IDENTITY);
    assert!(pane.pane.is_open(), "reset leaves the operation open");
    assert_eq!(pane.click(actions.controls[1].center()), OpOutcome::Apply);
}

#[test]
fn an_eyedropper_is_taken_up_set_from_and_put_down() {
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    let pickers = pane.placed(Role::Pickers);
    assert_eq!(pane.click(pickers.controls[0].center()), OpOutcome::Picking);
    assert_eq!(pane.pane.picking(), Some(Pick::Black));
    assert_eq!(pane.click(pickers.controls[2].center()), OpOutcome::Picking);
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
    let mut pane = Pane::on(Op::Levels(Levels::IDENTITY));
    assert!(pane.pane.reads_histogram());
    let auto = pane.placed(Role::Pickers).controls[3];
    assert_eq!(
        pane.click(auto.center()),
        OpOutcome::Taken,
        "withheld without one"
    );
    assert!(pane.pane.set_histogram_ready(true));
    assert_eq!(pane.click(auto.center()), OpOutcome::Auto);
    let histogram = Histogram::of(&spread_canvas(), None, &SERIAL, &|| true).expect("room");
    assert!(pane.pane.auto(&histogram));
    let red = *pane.levels().of(Channel::Red);
    assert!(red.black >= 40 && red.white <= 167, "{red:?}");
}

#[test]
fn a_curve_point_added_on_the_graph_is_set_by_its_fields() {
    let mut pane = Pane::on(Op::Curves(Curves::IDENTITY));
    let graph = pane.placed(Role::Curve).controls[0];
    let added = pane.click(Point::new(
        graph.left() + to_i32(graph.width / 4),
        graph.center().y,
    ));
    assert_eq!(added, OpOutcome::Changed { settled: true });
    let Op::Curves(curves) = pane.pane.op() else {
        panic!("curves");
    };
    assert_eq!(curves.of(Channel::Composite).points().len(), 3);
    pane.type_into(Role::Point, 1, "30");
    let Op::Curves(curves) = pane.pane.op() else {
        panic!("curves");
    };
    assert_eq!(curves.of(Channel::Composite).points()[1].1, 30);
}

#[test]
fn white_balance_is_set_by_its_numbers_and_its_neutral_picker() {
    let mut pane = Pane::on(Op::WhiteBalance(WhiteBalance::NEUTRAL));
    pane.type_into(Role::Kelvin, 0, "3200");
    let warm = WhiteBalance {
        kelvin: 3200,
        tint: 0,
    };
    assert_eq!(pane.pane.op(), Op::WhiteBalance(warm));
    let pickers = pane.placed(Role::Pickers);
    assert_eq!(pane.click(pickers.controls[0].center()), OpOutcome::Picking);
    assert_eq!(pane.pane.picking(), Some(Pick::Neutral));
    assert!(pane.pane.picked(Rgb::new(150, 128, 100)));
    let Op::WhiteBalance(balance) = pane.pane.op() else {
        panic!("white balance");
    };
    assert!(
        balance.kelvin < 6500,
        "a warm cast read as a warm light: {balance:?}"
    );
}

#[test]
fn hue_and_saturation_set_the_range_shown_alone() {
    let mut pane = Pane::on(Op::HueSaturation(HueRanges::IDENTITY));
    pane.choose_in(Role::Range, 2);
    pane.type_into(Role::Hue, 0, "45");
    let Op::HueSaturation(ranges) = pane.pane.op() else {
        panic!("hue and saturation");
    };
    assert_eq!(ranges.of(HueRange::Yellows).hue, 45);
    assert_eq!(ranges.of(HueRange::Master).hue, 0);
}

#[test]
fn colour_balance_sets_the_band_shown_and_keeps_luminosity_as_switched() {
    let mut pane = Pane::on(Op::ColourBalance(ColourBalance::NEUTRAL));
    pane.type_into(Role::Axis(2), 0, "30");
    let keep = pane.placed(Role::KeepLuminosity).controls[0];
    assert_eq!(
        pane.click(keep.center()),
        OpOutcome::Changed { settled: true }
    );
    let Op::ColourBalance(balance) = pane.pane.op() else {
        panic!("colour balance");
    };
    assert_eq!(balance.tones[Tones::Midtones.index()][2], 30);
    assert!(!balance.keep_luminosity);
}

#[test]
fn preview_switches_a_withheld_pane_takes_nothing_and_its_least_is_its_selector() {
    let mut pane = Pane::of(Family::Adjustments);
    let preview = pane.placed(Role::Preview).controls[0];
    assert_eq!(pane.click(preview.center()), OpOutcome::Previewed);
    assert!(!pane.pane.is_open(), "turned off, nothing opens");
    assert_eq!(pane.click(preview.center()), OpOutcome::Previewed);
    assert!(
        pane.pane.is_open() && pane.pane.previewing(),
        "turned on, it opens"
    );
    pane.pane.set_withheld(true);
    let number = pane.placed(Role::Parameter(0));
    assert_eq!(pane.click(number.controls[1].center()), OpOutcome::Ignored);
    let (faces, scale, theme) = pane.context();
    let heights = pane.pane.heights(BOUNDS.width, faces, scale, theme);
    assert!(heights.least > 0 && heights.least < heights.natural);
    assert!(heights.natural < BOUNDS.height && heights.fit == Fit::Scrolled);
    assert_eq!(heights.least, pane.placed(Role::Choose).rect.height);
    let levels = Pane::on(Op::Levels(Levels::IDENTITY));
    let (faces, scale, theme) = levels.context();
    let taller = levels
        .pane
        .heights(BOUNDS.width, faces, scale, theme)
        .natural;
    assert!(
        taller > heights.natural,
        "as tall as its operation's settings"
    );
}

#[test]
fn every_spot_a_window_presses_lies_on_the_pane() {
    let levels = Pane::on(Op::Levels(Levels::IDENTITY));
    let (faces, scale, theme) = levels.context();
    let spots = [
        Spot::Selector,
        Spot::Reset,
        Spot::Apply,
        Spot::Preview,
        Spot::Picker(3),
    ];
    for spot in spots {
        let at = levels.pane.spot(spot, BOUNDS, (faces, scale, theme));
        assert!(at.is_some_and(|at| BOUNDS.contains(at)), "{spot:?}");
    }
    let blur = Pane::of(Family::Effects);
    let (faces, scale, theme) = blur.context();
    let along = blur
        .pane
        .spot(Spot::Number(500), BOUNDS, (faces, scale, theme));
    assert!(along.is_some_and(|at| BOUNDS.contains(at)));
}

/// Escape in a pane with nothing open or moved is the window's, which can give
/// the keyboard back to the picture; an open pane takes it to put its settings
/// back.
#[test]
fn escape_in_a_closed_pane_is_left_to_the_window() {
    let mut pane = Pane::of(Family::Effects);
    assert!(pane.pane.enter_focus(true, BOUNDS, &mut Region::new()));
    pane.key(Key::Named(NamedKey::Tab));
    assert!(!pane.pane.is_open());
    assert_eq!(pane.key(Key::Named(NamedKey::Escape)), OpOutcome::Ignored);
    let mut open = Pane::on(Op::Median { radius: 1 });
    assert!(open.pane.enter_focus(true, BOUNDS, &mut Region::new()));
    open.key(Key::Named(NamedKey::Tab));
    assert_eq!(
        open.key(Key::Named(NamedKey::Escape)),
        OpOutcome::Changed { settled: false },
        "an open pane takes it"
    );
}
