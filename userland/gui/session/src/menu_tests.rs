//! The menu chain's rules, driven without a screen.
//!
//! The chain owns model, state and geometry and touches no compositor, so
//! every rule of `plans/NEW-MENUS.md` §1 is exercised here directly rather
//! than inferred from pixels.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuReason, AppMenuRow, AppMenuSymbol,
    MenuRefusal, APP_MENU_MAX_DEPTH,
};
use tairix_controls::{ChainChild, ChainModel, ChainRow, Fact, FactList, MenuItem, PlatePlacement};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_theme::Theme;
use tairix_wm::{InputEvent, Key, NamedKey, PointerButton};

use tairix_controls::damage::Repaint;

use crate::menu::{
    ChainAction, ChainGeometry, ChainOutcome, ChainOwner, MenuChain, ModelRefused, SurfaceKind,
    SUBMENU_SETTLE_NS,
};

const SCREEN: Rect = Rect::new(0, 0, 1280, 800);

/// An owner whose answers are the window engine's.
const APP: ChainOwner = ChainOwner::Window {
    window_id: 7,
    open_id: 42,
};

/// The ground the desktop opens a chain on: a plate is floating chrome, so the
/// rectangles a row is hit-tested against are resolved against the same form
/// its pixels are painted with.
fn theme() -> Theme {
    Theme::dark().floating()
}

fn geom(theme: &Theme) -> ChainGeometry<'_> {
    ChainGeometry {
        screen: SCREEN,
        scale: Scale::ONE,
        theme,
        epoch: (100, 0),
    }
}

/// The same geometry on a screen of `w` × `h`, for the edge cases.
fn geom_on(theme: &Theme, screen: Rect) -> ChainGeometry<'_> {
    ChainGeometry {
        screen,
        scale: Scale::ONE,
        theme,
        epoch: (100, 0),
    }
}

/// A wire menu with `title` as its root plate's.
fn titled(title: &str) -> AppMenu {
    AppMenu::titled(label(title))
}

/// A bounded wire label.
fn label(text: &str) -> AppMenuLabel {
    AppMenuLabel::new(text).expect("a label within the bound")
}

fn id(raw: u16) -> AppMenuItemId {
    AppMenuItemId::new(raw).expect("a non-zero row id")
}

fn moved(x: i32, y: i32) -> InputEvent {
    InputEvent::PointerMoved {
        to: Point::new(x, y),
    }
}

const PRESS: InputEvent = InputEvent::PointerPressed {
    button: PointerButton::Primary,
};
const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: PointerButton::Primary,
};

fn key(named: NamedKey) -> InputEvent {
    InputEvent::KeyPressed {
        key: Key::Named(named),
        modifiers: tairix_wm::Modifiers::default(),
    }
}

/// A root plate of three chooseable rows.
fn flat_model() -> ChainModel {
    let mut model = ChainModel::new("Plain");
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("One"),
    ));
    model.push(ChainRow::item(
        id(2),
        AppMenuSymbol::Open,
        MenuItem::new("Two"),
    ));
    model.push(ChainRow::item(
        id(3),
        AppMenuSymbol::Open,
        MenuItem::new("Three"),
    ));
    model
}

/// A root plate whose second row opens a submenu of two rows, the first of
/// which opens a submenu of its own — the chain the bar could never render.
fn nested_model() -> ChainModel {
    let mut model = ChainModel::new("Nested");
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("Top"),
    ));
    let parent = model.push(ChainRow::submenu(
        AppMenuSymbol::Settings,
        MenuItem::new("More"),
    ));
    let inner = model
        .push(ChainRow::submenu(AppMenuSymbol::Settings, MenuItem::new("Deeper")).under(parent));
    model.push(ChainRow::item(id(2), AppMenuSymbol::Open, MenuItem::new("Inner")).under(parent));
    model.push(ChainRow::item(id(3), AppMenuSymbol::Open, MenuItem::new("Deepest")).under(inner));
    model
}

/// Open `model` for `owner` at a point anchor, asserting it was accepted.
fn open(
    chain: &mut MenuChain,
    owner: ChainOwner,
    model: ChainModel,
    at: Point,
    g: &ChainGeometry<'_>,
) {
    chain
        .open(
            owner,
            model,
            PlatePlacement::adjacent(Rect::new(at.x, at.y, 0, 0)),
            g,
        )
        .expect("the model opens");
}

/// The plate at `depth`, or the test fails.
fn plate(chain: &MenuChain, depth: usize) -> Rect {
    chain
        .surfaces()
        .into_iter()
        .find_map(|surface| match surface.kind {
            SurfaceKind::Plate(at) if at == depth => Some(surface.rect),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no plate at depth {depth}"))
}

/// The information panel's rectangle, if one hangs off the chain.
fn info(chain: &MenuChain) -> Option<Rect> {
    chain
        .surfaces()
        .into_iter()
        .find(|surface| surface.kind == SurfaceKind::Info)
        .map(|surface| surface.rect)
}

/// What the chain says the surface `kind` has still to be painted.
fn owed(chain: &MenuChain, kind: SurfaceKind) -> Repaint {
    chain
        .surfaces()
        .into_iter()
        .find(|surface| surface.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} surface"))
        .repaint
}

/// Mark every surface the chain lists as painted, as the session does once it
/// has presented them.
fn present_all(chain: &mut MenuChain) {
    for kind in chain
        .surfaces()
        .into_iter()
        .map(|surface| surface.kind)
        .collect::<Vec<_>>()
    {
        chain.presented(kind);
    }
}

/// The region covering `rects`, to compare against what a surface owes.
fn covering(rects: &[Rect]) -> Repaint {
    let mut region = Region::new();
    for rect in rects {
        region.add(*rect);
    }
    Repaint::Parts(region)
}

/// Row `row` of the plate at `depth`, in that plate's own local pixels — the
/// space a surface's repaint is reported in.
fn local_row(chain: &MenuChain, depth: usize, row: usize, g: &ChainGeometry<'_>) -> Rect {
    let plate = plate(chain, depth);
    let rect = chain
        .row_rect(depth, row, g)
        .unwrap_or_else(|| panic!("no row {row} on plate {depth}"));
    Rect::new(
        rect.left() - plate.left(),
        rect.top() - plate.top(),
        rect.width,
        rect.height,
    )
}

/// The plate at `depth` painted whole, as the session presents it.
fn paint(chain: &MenuChain, depth: usize, g: &ChainGeometry<'_>) -> tairix_raster::Surface {
    let rect = plate(chain, depth);
    let mut surface =
        tairix_raster::Surface::new(rect.width, rect.height).expect("a plate surface");
    chain.render_surface(SurfaceKind::Plate(depth), &mut surface, g);
    surface
}

/// How many plates the chain has up.
fn plates(chain: &MenuChain) -> usize {
    chain
        .surfaces()
        .iter()
        .filter(|surface| matches!(surface.kind, SurfaceKind::Plate(_)))
        .count()
}

/// The centre of row `row` of the plate at `depth`, in screen pixels.
///
/// Derived from the plate's own rectangle and the band it reserves, so a test
/// aims at what the chain actually lays out rather than a hand-copied
/// position.
fn row_point(chain: &MenuChain, depth: usize, row: usize, g: &ChainGeometry<'_>) -> Point {
    let rect = chain
        .row_rect(depth, row, g)
        .unwrap_or_else(|| panic!("no row {row} on plate {depth}"));
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).unwrap_or(0),
        rect.top() + i32::try_from(rect.height / 2).unwrap_or(0),
    )
}

/// Settle the pointer on row `row` of the plate at `depth`.
fn settle_on(
    chain: &mut MenuChain,
    depth: usize,
    row: usize,
    g: &ChainGeometry<'_>,
) -> (ChainAction, Point) {
    let at = row_point(chain, depth, row, g);
    let acted = chain.handle(&moved(at.x, at.y), at, g, 0);
    (acted, at)
}

// --- the singleton -------------------------------------------------------

/// The one thing anybody outside this process can learn about a plate: a frame
/// carrying it went out. Once per open, or a consumer counting the record
/// cannot tell one menu from a repainted one.
#[test]
fn a_chain_announces_itself_shown_once_per_open() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();

    let mut seen: Vec<ChainOwner> = Vec::new();
    chain.report_newly_shown(|owner| seen.push(owner.clone()));
    assert!(seen.is_empty(), "a seat with no chain announces nothing");

    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    chain.report_newly_shown(|owner| seen.push(owner.clone()));
    assert_eq!(seen, alloc::vec![APP]);

    chain.report_newly_shown(|owner| seen.push(owner.clone()));
    assert_eq!(
        seen.len(),
        1,
        "a later frame announced the same chain again"
    );

    // A submenu opening is more of the chain, not a new one.
    settle_on(&mut chain, 0, 1, &g);
    assert_eq!(plates(&chain), 2);
    chain.report_newly_shown(|owner| seen.push(owner.clone()));
    assert_eq!(seen.len(), 1, "a deeper plate announced a second chain");

    // The desktop's own menu is announced exactly as an application's is; a
    // fresh chain is a fresh open.
    open(
        &mut chain,
        ChainOwner::Backdrop,
        flat_model(),
        Point::new(200, 60),
        &g,
    );
    chain.report_newly_shown(|owner| seen.push(owner.clone()));
    assert_eq!(seen, alloc::vec![APP, ChainOwner::Backdrop]);

    assert!(chain.dismiss());
    chain.report_newly_shown(|owner| seen.push(owner.clone()));
    assert_eq!(seen.len(), 2, "a chain that has closed announced itself");
}

#[test]
fn a_second_open_closes_the_first_and_answers_it_dismissed() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);
    assert!(chain.take_answers().is_empty(), "an open answers nothing");

    let second = ChainOwner::Window {
        window_id: 9,
        open_id: 43,
    };
    open(
        &mut chain,
        second.clone(),
        flat_model(),
        Point::new(200, 60),
        &g,
    );

    assert_eq!(
        chain.take_answers(),
        alloc::vec![(APP, ChainOutcome::Dismissed)],
        "the displaced chain is answered, and only it"
    );
    assert_eq!(chain.owner(), Some(&second));
    assert_eq!(plates(&chain), 1, "one chain, one root plate");
}

#[test]
fn a_model_with_no_root_rows_opens_nothing_and_leaves_the_chain_alone() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);

    let refused = chain.open(
        ChainOwner::Backdrop,
        ChainModel::new("Empty"),
        PlatePlacement::adjacent(Rect::new(0, 0, 0, 0)),
        &g,
    );
    assert_eq!(refused, Err(ModelRefused::NoRows));
    assert_eq!(
        chain.owner(),
        Some(&APP),
        "the chain that was up is untouched"
    );
    assert!(chain.take_answers().is_empty(), "nothing was answered");
}

// --- placement -----------------------------------------------------------

#[test]
fn a_root_plate_stays_on_screen_at_every_corner() {
    let theme = theme();
    let g = geom(&theme);
    for corner in [
        Point::new(0, 0),
        Point::new(SCREEN.right() - 1, 0),
        Point::new(0, SCREEN.bottom() - 1),
        Point::new(SCREEN.right() - 1, SCREEN.bottom() - 1),
    ] {
        let mut chain = MenuChain::new();
        open(&mut chain, APP, flat_model(), corner, &g);
        let root = plate(&chain, 0);
        assert!(
            root.left() >= SCREEN.left()
                && root.top() >= SCREEN.top()
                && root.right() <= SCREEN.right()
                && root.bottom() <= SCREEN.bottom(),
            "a plate anchored at {corner:?} left the screen: {root:?}"
        );
    }
}

#[test]
fn a_child_hangs_edge_adjacent_to_its_parent_and_flips_at_the_far_edge() {
    let theme = theme();
    let g = geom(&theme);

    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    let (_, _) = settle_on(&mut chain, 0, 1, &g);
    let root = plate(&chain, 0);
    let child = plate(&chain, 1);
    assert_eq!(
        child.left(),
        root.right(),
        "a child with room opens edge-adjacent, leaving no gap to cross"
    );

    // Anchored hard against the right edge there is no room on the trailing
    // side, so the child takes the parent's other one.
    let mut chain = MenuChain::new();
    open(
        &mut chain,
        APP,
        nested_model(),
        Point::new(SCREEN.right() - 1, 40),
        &g,
    );
    let (_, _) = settle_on(&mut chain, 0, 1, &g);
    let root = plate(&chain, 0);
    let child = plate(&chain, 1);
    assert_eq!(
        child.right(),
        root.left(),
        "with no room trailing, the child flips to the parent's other side"
    );
}

#[test]
fn a_submenus_first_row_lies_level_with_the_row_that_opened_it() {
    let themes = [
        theme(),
        tairix_controls::testkit::high_contrast().floating(),
    ];
    for theme in &themes {
        for percent in [100, 125, 200] {
            let scale = Scale::from_percent(percent).expect("a supported scale");
            let g = ChainGeometry {
                screen: SCREEN,
                scale,
                theme,
                epoch: (100, 0),
            };
            let mut chain = MenuChain::new();
            open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
            let row = chain.row_rect(0, 1, &g).expect("the submenu row");
            settle_on(&mut chain, 0, 1, &g);
            let first = chain.row_rect(1, 0, &g).expect("the submenu's first row");
            assert_eq!(
                first.top(),
                row.top(),
                "at {percent}%, {:?}: the first row, not the band, lies beside its parent row",
                theme.contrast()
            );
            assert!(
                plate(&chain, 1).top() < row.top(),
                "the band stands above the row that opened it"
            );
        }
    }
}

#[test]
fn a_child_near_the_bottom_slides_up_to_stay_on_screen() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(
        &mut chain,
        APP,
        nested_model(),
        Point::new(40, SCREEN.bottom() - 1),
        &g,
    );
    settle_on(&mut chain, 0, 1, &g);
    assert!(
        plate(&chain, 1).bottom() <= SCREEN.bottom(),
        "a child near the bottom slides up rather than running off"
    );
}

#[test]
fn a_chain_opens_plates_deeper_than_one_level() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    assert_eq!(plates(&chain), 2);
    settle_on(&mut chain, 1, 0, &g);
    assert_eq!(plates(&chain), 3, "a submenu inside a submenu opens");
    assert_eq!(
        plate(&chain, 2).left(),
        plate(&chain, 1).right(),
        "the grandchild is placed against its own parent"
    );
}

// --- arrival, with no timer ---------------------------------------------

#[test]
fn a_submenu_opens_on_arrival_with_no_click_and_no_timer() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    assert_eq!(plates(&chain), 1);
    let (acted, _) = settle_on(&mut chain, 0, 1, &g);
    assert_eq!(acted, ChainAction::Redraw);
    assert_eq!(plates(&chain), 2, "one motion opened it");
}

#[test]
fn travelling_from_a_parent_row_into_its_own_child_keeps_the_child_open() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    let child = plate(&chain, 1);

    // Leaving the parent row's rectangle for the child's own plate must not
    // close what the pointer is travelling to.
    let into = Point::new(child.left() + 4, child.top() + 4);
    chain.handle(&moved(into.x, into.y), into, &g, 0);
    assert_eq!(plates(&chain), 2, "the child the pointer entered stayed up");
}

#[test]
fn settling_on_a_sibling_row_closes_the_child_and_opens_that_rows_own() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    assert_eq!(plates(&chain), 2);

    settle_on(&mut chain, 0, 0, &g);
    assert_eq!(
        plates(&chain),
        1,
        "settling on a different row of the same plate closed the child"
    );
}

#[test]
fn a_disabled_submenu_row_opens_nothing() {
    let theme = theme();
    let g = geom(&theme);
    let mut model = ChainModel::new("Guarded");
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("One"),
    ));
    let shut = model.push(ChainRow::submenu(
        AppMenuSymbol::Settings,
        MenuItem::new("Shut").with_state(tairix_controls::ControlState::disabled()),
    ));
    model.push(ChainRow::item(id(2), AppMenuSymbol::Open, MenuItem::new("Hidden")).under(shut));

    let mut chain = MenuChain::new();
    open(&mut chain, APP, model, Point::new(40, 40), &g);
    let (acted, _) = settle_on(&mut chain, 0, 1, &g);
    assert_eq!(plates(&chain), 1, "a disabled row opens nothing");
    // It still repaints: the highlight moved onto it, and a disabled row
    // states its reason while it is current.
    assert_eq!(acted, ChainAction::Redraw);
}

/// A submenu row with siblings beneath it, the second behind a group break,
/// so a path aimed at the child crosses both the break and a row.
fn aim_model() -> ChainModel {
    let mut model = ChainModel::new("Aim");
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("First"),
    ));
    let more = model.push(ChainRow::submenu(
        AppMenuSymbol::Settings,
        MenuItem::new("More"),
    ));
    model.push(ChainRow::item(
        id(2),
        AppMenuSymbol::Open,
        MenuItem::new("Below").with_group_break(true),
    ));
    model.push(ChainRow::item(
        id(3),
        AppMenuSymbol::Open,
        MenuItem::new("Last"),
    ));
    for (raw, label) in [(4, "One"), (5, "Two"), (6, "Three")] {
        model.push(ChainRow::item(id(raw), AppMenuSymbol::Open, MenuItem::new(label)).under(more));
    }
    model
}

/// The aim model open with its submenu up, and the point it was opened from.
fn aimed_chain(g: &ChainGeometry<'_>) -> (MenuChain, Point) {
    let mut chain = MenuChain::new();
    open(&mut chain, APP, aim_model(), Point::new(40, 40), g);
    let (_, at) = settle_on(&mut chain, 0, 1, g);
    assert_eq!(plates(&chain), 2, "the submenu is up");
    (chain, at)
}

/// Three quarters of the way from `at` to the child's near edge, at `y`: a
/// step plainly aimed at the child.
fn towards_child(chain: &MenuChain, at: Point, y: i32) -> Point {
    let child = plate(chain, 1);
    Point::new(at.x + 3 * (child.left() - at.x) / 4, y)
}

/// The parent's last column is its rim: on the plate, on no row. Crossing it
/// is crossing into the child, not settling anywhere else.
#[test]
fn the_parents_rim_on_the_way_to_its_child_keeps_the_child_open() {
    let theme = theme();
    let g = geom(&theme);
    let (mut chain, at) = aimed_chain(&g);
    let resting = paint(&chain, 0, &g);
    let rim = Point::new(plate(&chain, 0).right() - 1, at.y);
    chain.handle(&moved(rim.x, rim.y), rim, &g, 1);
    assert_eq!(plates(&chain), 2, "the rim is not a different row");
    assert_eq!(
        paint(&chain, 0, &g).pixels(),
        resting.pixels(),
        "the open row keeps its highlight"
    );
    let into = Point::new(plate(&chain, 1).left(), at.y);
    chain.handle(&moved(into.x, into.y), into, &g, 2);
    assert_eq!(plates(&chain), 2);
}

/// A group break lies between rows, so a pointer in it has settled on none.
#[test]
fn a_group_break_on_the_way_to_the_child_keeps_the_child_open() {
    let theme = theme();
    let g = geom(&theme);
    let (mut chain, at) = aimed_chain(&g);
    let more = chain.row_rect(0, 1, &g).expect("the submenu row");
    let gap = towards_child(&chain, at, more.bottom() + 2);
    chain.handle(&moved(gap.x, gap.y), gap, &g, 1);
    assert_eq!(plates(&chain), 2, "the break is not a row");
}

/// Aiming at the child's lower rows crosses the row beneath the parent row:
/// the crossing waits, the open row keeps its highlight, and reaching the
/// child ends the wait.
#[test]
fn a_path_aimed_at_the_child_may_cross_the_row_beneath() {
    let theme = theme();
    let g = geom(&theme);
    let (mut chain, at) = aimed_chain(&g);
    let resting = paint(&chain, 0, &g);
    let below = row_point(&chain, 0, 2, &g);
    let step = towards_child(&chain, at, below.y);
    chain.handle(&moved(step.x, step.y), step, &g, 10);
    assert_eq!(plates(&chain), 2, "the crossing waits while it heads in");
    assert_eq!(paint(&chain, 0, &g).pixels(), resting.pixels());
    assert_eq!(chain.park_deadline_ns(10, u64::MAX), SUBMENU_SETTLE_NS);

    let into = Point::new(plate(&chain, 1).left() + 4, below.y);
    chain.handle(&moved(into.x, into.y), into, &g, 20);
    assert_eq!(plates(&chain), 2);
    assert_eq!(
        chain.park_deadline_ns(20, u64::MAX),
        u64::MAX,
        "reaching the child ends the wait"
    );
}

/// A step onto a sibling that turns away from the child is a choice at once.
#[test]
fn a_step_onto_a_sibling_away_from_the_child_chooses_it_at_once() {
    let theme = theme();
    let g = geom(&theme);
    let (mut chain, at) = aimed_chain(&g);
    let below = row_point(&chain, 0, 2, &g);
    let away = Point::new(at.x - 8, below.y);
    chain.handle(&moved(away.x, away.y), away, &g, 10);
    assert_eq!(plates(&chain), 1, "turning away chose the row");
    assert_eq!(chain.park_deadline_ns(10, u64::MAX), u64::MAX);
}

/// A pointer that stops on the row it crossed has chosen it, once the wait
/// runs out: the child closes and the highlight moves to that row.
#[test]
fn resting_on_a_crossed_row_chooses_it_when_the_wait_runs_out() {
    let theme = theme();
    let g = geom(&theme);
    let (mut chain, at) = aimed_chain(&g);
    let below = row_point(&chain, 0, 2, &g);
    let step = towards_child(&chain, at, below.y);
    chain.handle(&moved(step.x, step.y), step, &g, 10);

    assert!(
        !chain.tick(10 + SUBMENU_SETTLE_NS - 1, &g),
        "not before its time"
    );
    assert_eq!(plates(&chain), 2);
    assert!(chain.tick(10 + SUBMENU_SETTLE_NS, &g));
    assert_eq!(plates(&chain), 1, "the rest chose the crossed row");
    assert_eq!(chain.park_deadline_ns(0, u64::MAX), u64::MAX);

    let mut direct = MenuChain::new();
    open(&mut direct, APP, aim_model(), Point::new(40, 40), &g);
    settle_on(&mut direct, 0, 2, &g);
    assert_eq!(
        paint(&chain, 0, &g).pixels(),
        paint(&direct, 0, &g).pixels(),
        "the highlight is on the row the pointer rests on"
    );
}

/// With nothing waiting the chain asks for no wake at all.
#[test]
fn a_chain_with_nothing_waiting_asks_for_no_deadline() {
    let theme = theme();
    let g = geom(&theme);
    let (mut chain, _) = aimed_chain(&g);
    assert_eq!(chain.park_deadline_ns(0, 1234), 1234);
    assert!(!chain.tick(u64::MAX, &g));
}

/// Walk the pointer a pixel at a time from `from` towards `to`, a vertical
/// step first wherever the line allows one — a slow hand's path, whose step
/// off the open row is a pure vertical one — and hand each sample to the
/// chain, failing at the first that closed the child.
fn staircase(chain: &mut MenuChain, from: Point, to: Point, g: &ChainGeometry<'_>) {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let (step_x, step_y) = (dx.signum(), dy.signum());
    let mut at = from;
    let mut now = 1;
    while at != to {
        let (done_x, done_y) = ((at.x - from.x).abs(), (at.y - from.y).abs());
        if at.y != to.y && done_y * dx.abs() <= done_x * dy.abs() {
            at.y += step_y;
        } else {
            at.x += step_x;
        }
        chain.handle(&moved(at.x, at.y), at, g, now);
        now += 1;
        assert_eq!(plates(chain), 2, "the child closed at {at:?}");
    }
}

/// A slow hand moves a pixel at a time, so the step that leaves the open row
/// is a pure vertical one. Judged from where the path left the open row it
/// still heads into the child, so the crossing waits all the way in.
#[test]
fn a_one_pixel_staircase_towards_the_child_keeps_it_open() {
    let theme = theme();
    let g = geom(&theme);
    let (mut chain, at) = aimed_chain(&g);
    let below = row_point(&chain, 0, 2, &g);
    let into = Point::new(plate(&chain, 1).left() + 2, below.y);
    staircase(&mut chain, at, into, &g);
    assert_eq!(
        chain.park_deadline_ns(0, u64::MAX),
        u64::MAX,
        "reaching the child ended the wait"
    );
}

/// A child with no room beside its parent opens on the other side, and a path
/// aimed at it there is judged against the edge it actually faces.
#[test]
fn a_path_aimed_at_a_child_opening_leftwards_may_cross_the_row_beneath() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(
        &mut chain,
        APP,
        aim_model(),
        Point::new(SCREEN.right() - 10, 40),
        &g,
    );
    let (_, at) = settle_on(&mut chain, 0, 1, &g);
    assert_eq!(plates(&chain), 2);
    let (parent, child) = (plate(&chain, 0), plate(&chain, 1));
    assert!(child.right() <= parent.left(), "the child opened leftwards");
    let below = row_point(&chain, 0, 2, &g);
    let step = Point::new(at.x - 3 * (at.x - child.right()) / 4, below.y);
    chain.handle(&moved(step.x, step.y), step, &g, 10);
    assert_eq!(plates(&chain), 2, "the crossing waits while it heads in");
    assert_eq!(chain.park_deadline_ns(10, u64::MAX), SUBMENU_SETTLE_NS);
    let into = Point::new(child.right() - 2, below.y);
    staircase(&mut chain, step, into, &g);
}

/// The aim model with a crossing of the row beneath waiting, at `now` 10.
fn waiting_chain(g: &ChainGeometry<'_>) -> MenuChain {
    let (mut chain, at) = aimed_chain(g);
    let below = row_point(&chain, 0, 2, g);
    let step = towards_child(&chain, at, below.y);
    chain.handle(&moved(step.x, step.y), step, g, 10);
    assert_eq!(chain.park_deadline_ns(10, u64::MAX), SUBMENU_SETTLE_NS);
    chain
}

/// The band is no row: a pointer that turned onto it has rested on no
/// crossed row, so the wait ends without choosing anything.
#[test]
fn a_move_onto_the_band_ends_a_wait_without_choosing() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = waiting_chain(&g);
    let parent = plate(&chain, 0);
    let band = Point::new(parent.left() + 20, parent.top() + 2);
    chain.handle(&moved(band.x, band.y), band, &g, 20);
    assert_eq!(chain.park_deadline_ns(20, u64::MAX), u64::MAX);
    assert!(!chain.tick(20 + SUBMENU_SETTLE_NS, &g));
    assert_eq!(plates(&chain), 2, "nothing was chosen");
}

/// A key, a press, and the pointer leaving the chain each end a wait: none of
/// them is the pointer resting on the crossed row.
#[test]
fn a_key_a_press_or_leaving_the_chain_ends_a_wait() {
    let theme = theme();
    let g = geom(&theme);
    let outside = Point::new(SCREEN.right() - 1, SCREEN.bottom() - 1);
    for (what, event) in [
        ("a key", key(NamedKey::Down)),
        ("a press", PRESS),
        ("leaving", moved(outside.x, outside.y)),
    ] {
        let mut chain = waiting_chain(&g);
        let at = if what == "leaving" {
            outside
        } else {
            row_point(&chain, 0, 2, &g)
        };
        chain.handle(&event, at, &g, 20);
        assert_eq!(chain.park_deadline_ns(20, u64::MAX), u64::MAX, "{what}");
    }
}

/// A child the keyboard opened has an apex all the same — where the pointer
/// stood when the key opened it — so the pointer's first move, aimed at the
/// child across a sibling, waits like any other.
#[test]
fn a_child_opened_from_the_keyboard_is_aimed_at_from_where_the_pointer_was() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, aim_model(), Point::new(40, 40), &g);
    let first = row_point(&chain, 0, 0, &g);
    for named in [NamedKey::Home, NamedKey::Down, NamedKey::Right] {
        chain.handle(&key(named), first, &g, 0);
    }
    assert_eq!(plates(&chain), 2, "the keyboard opened the submenu");
    let step = Point::new(first.x + 6, first.y + 4);
    chain.handle(&moved(step.x, step.y), step, &g, 10);
    assert_eq!(plates(&chain), 2, "the first move heads into the child");
    assert_eq!(chain.park_deadline_ns(10, u64::MAX), SUBMENU_SETTLE_NS);
}

/// A row that can open nothing borrows the open row's highlight while the
/// pointer rests on it, so its Authority Mark can be read; the open row takes
/// it back once the pointer moves on to the child.
#[test]
fn a_disabled_sibling_borrows_the_highlight_and_gives_it_back() {
    let theme = theme();
    let g = geom(&theme);
    let mut model = ChainModel::new("Lend");
    let more = model.push(ChainRow::submenu(
        AppMenuSymbol::Settings,
        MenuItem::new("More"),
    ));
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("Shut").with_state(refused_state()),
    ));
    model.push(ChainRow::item(id(2), AppMenuSymbol::Open, MenuItem::new("One")).under(more));
    let mut chain = MenuChain::new();
    open(&mut chain, APP, model, Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 0, &g);
    assert_eq!(plates(&chain), 2);
    let resting = paint(&chain, 0, &g);

    let shut = row_point(&chain, 0, 1, &g);
    let away = Point::new(plate(&chain, 0).left() + 6, shut.y);
    let acted = chain.handle(&moved(away.x, away.y), away, &g, 10);
    assert_eq!(plates(&chain), 2, "a disabled row closes nothing");
    assert_eq!(acted, ChainAction::Redraw, "the highlight moved onto it");
    assert_ne!(paint(&chain, 0, &g).pixels(), resting.pixels());

    let child = plate(&chain, 1);
    let into = Point::new(child.left() + 4, child.bottom() - 4);
    let acted = chain.handle(&moved(into.x, into.y), into, &g, 20);
    assert_eq!(acted, ChainAction::Redraw, "the open row took it back");
    assert_eq!(paint(&chain, 0, &g).pixels(), resting.pixels());
}

/// A row with no child that takes down the quick-entry field has changed the
/// chain, and says so.
#[test]
fn settling_on_a_plain_row_closes_the_entry_field_and_asks_for_a_redraw() {
    let theme = theme();
    let g = geom(&theme);
    let mut model = ChainModel::new("Entry");
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("Plain"),
    ));
    model.push(ChainRow::entry(
        (id(2), id(3)),
        ("name", 0..4),
        AppMenuSymbol::Rename,
        MenuItem::new("Name"),
    ));
    let mut chain = MenuChain::new();
    open(&mut chain, APP, model, Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    let has_entry = |chain: &MenuChain| {
        chain
            .surfaces()
            .iter()
            .any(|surface| surface.kind == SurfaceKind::Entry)
    };
    assert!(has_entry(&chain), "the field opened");
    let plain = row_point(&chain, 0, 0, &g);
    let away = Point::new(plate(&chain, 0).left() + 6, plain.y);
    let acted = chain.handle(&moved(away.x, away.y), away, &g, 10);
    assert!(!has_entry(&chain), "the plain row took the field down");
    assert_eq!(acted, ChainAction::Redraw);
}

// --- the drag ------------------------------------------------------------

#[test]
fn dragging_a_band_moves_that_plate_and_its_descendants_but_not_its_ancestors() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(200, 200), &g);
    settle_on(&mut chain, 0, 1, &g);
    settle_on(&mut chain, 1, 0, &g);
    assert_eq!(plates(&chain), 3);

    let root_before = plate(&chain, 0);
    let child_before = plate(&chain, 1);
    let grandchild_before = plate(&chain, 2);

    // Press the child's band, then cross the drag threshold.
    let band = Point::new(child_before.left() + 20, child_before.top() + 2);
    chain.handle(&moved(band.x, band.y), band, &g, 0);
    chain.handle(&PRESS, band, &g, 0);
    let to = Point::new(band.x + 60, band.y + 40);
    chain.handle(&moved(to.x, to.y), to, &g, 0);
    let further = Point::new(to.x + 10, to.y + 5);
    chain.handle(&moved(further.x, further.y), further, &g, 0);

    assert_eq!(plate(&chain, 0), root_before, "the ancestor stayed put");
    let child_after = plate(&chain, 1);
    assert_ne!(child_after, child_before, "the dragged plate moved");
    assert_ne!(
        plate(&chain, 2),
        grandchild_before,
        "the descendant travelled with it"
    );
    assert_eq!(
        plate(&chain, 2).left(),
        child_after.right(),
        "and stayed edge-adjacent to it"
    );
}

#[test]
fn a_dragged_plate_keeps_its_position_when_the_chain_re_places() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(200, 200), &g);

    let root = plate(&chain, 0);
    let band = Point::new(root.left() + 20, root.top() + 2);
    chain.handle(&moved(band.x, band.y), band, &g, 0);
    chain.handle(&PRESS, band, &g, 0);
    let to = Point::new(band.x + 80, band.y + 30);
    chain.handle(&moved(to.x, to.y), to, &g, 0);
    chain.handle(&RELEASE, to, &g, 0);
    let dragged = plate(&chain, 0);
    assert_ne!(dragged, root);

    // Opening a child re-derives every unpinned plate; the pinned root is not
    // one of them.
    settle_on(&mut chain, 0, 1, &g);
    assert_eq!(
        plate(&chain, 0),
        dragged,
        "a plate the user placed keeps its position"
    );
}

// --- the information sheet -----------------------------------------------

/// The sheet an attested application named `App` states.
fn about_app() -> tairix_controls::AboutSheet {
    tairix_controls::AboutSheet::new(
        "App",
        "1.0",
        Some(String::from("Does one thing well.")),
        FactList::new(alloc::vec![Fact::new("Author", "TAIRiX")]),
        tairix_icon::IconKind::AppBundle,
    )
}

/// A model whose second row opens the desktop's own information sheet.
fn info_model() -> ChainModel {
    let mut model = ChainModel::new("Attested");
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("One"),
    ));
    model.push(ChainRow::info(MenuItem::new("Info"), about_app()));
    model
}

#[test]
fn the_information_panel_hangs_where_a_submenu_would_and_dies_with_the_chain() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, info_model(), Point::new(40, 40), &g);
    let row = chain.row_rect(0, 1, &g).expect("the information row");
    let (acted, _) = settle_on(&mut chain, 0, 1, &g);
    assert_eq!(acted, ChainAction::Redraw);
    let placed = info(&chain).expect("the panel hangs");
    assert_eq!(placed.left(), plate(&chain, 0).right(), "edge-adjacent");

    let mut nested = MenuChain::new();
    open(&mut nested, APP, nested_model(), Point::new(40, 40), &g);
    let submenu_row = nested.row_rect(0, 1, &g).expect("the submenu row");
    settle_on(&mut nested, 0, 1, &g);
    let lift = submenu_row.top() - plate(&nested, 1).top();
    assert_eq!(
        row.top() - placed.top(),
        lift,
        "lifted above its row exactly as a submenu is"
    );

    // Settling on another row of the same plate takes it down with the rest
    // of what hung there.
    settle_on(&mut chain, 0, 0, &g);
    assert!(info(&chain).is_none(), "the panel went with its row");
}

#[test]
fn choosing_the_information_row_answers_nothing_and_keeps_its_panel() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, info_model(), Point::new(40, 40), &g);
    let (_, at) = settle_on(&mut chain, 0, 1, &g);

    chain.handle(&PRESS, at, &g, 0);
    let acted = chain.handle(&RELEASE, at, &g, 0);
    assert_ne!(
        acted,
        ChainAction::Closed,
        "the row states an identity; it names no command to answer with"
    );
    assert!(chain.take_answers().is_empty());
    assert!(info(&chain).is_some(), "and its panel stays up");
}

#[test]
fn clicking_a_submenu_row_keeps_its_plate_rather_than_acting() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    let (_, at) = settle_on(&mut chain, 0, 1, &g);
    chain.handle(&PRESS, at, &g, 0);
    let acted = chain.handle(&RELEASE, at, &g, 0);
    assert_ne!(
        acted,
        ChainAction::Closed,
        "a submenu row opens rather than answering"
    );
    assert!(chain.take_answers().is_empty());
    assert_eq!(plates(&chain), 2);
}

// --- the grab ------------------------------------------------------------

#[test]
fn a_press_outside_the_chain_dismisses_it_and_is_consumed() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);
    let away = Point::new(SCREEN.right() - 5, SCREEN.bottom() - 5);
    assert_eq!(
        chain.handle(&PRESS, away, &g, 0),
        ChainAction::Closed,
        "the press ended the chain"
    );
    assert_eq!(
        chain.take_answers(),
        alloc::vec![(APP, ChainOutcome::Dismissed)]
    );
    assert!(!chain.is_open());
    // Consumed, not delivered: the caller is told the chain closed and never
    // given the press to route at what was behind it.
    assert!(chain.surfaces().is_empty());
}

#[test]
fn a_press_on_the_information_panel_is_claimed_and_acts_on_nothing() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, info_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    let placed = info(&chain).expect("the panel hangs");

    let inside = Point::new(placed.left() + 8, placed.top() + 8);
    let acted = chain.handle(&PRESS, inside, &g, 0);
    assert_eq!(
        acted,
        ChainAction::Consumed,
        "a panel of facts offers no action, so the press is claimed"
    );
    assert!(chain.is_open());
}

// --- keyboard ------------------------------------------------------------

#[test]
fn traversal_moves_within_a_plate_and_in_and_out_of_a_child() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    let at = Point::ORIGIN;

    chain.handle(&key(NamedKey::Down), at, &g, 0);
    chain.handle(&key(NamedKey::Down), at, &g, 0);
    assert_eq!(
        chain.handle(&key(NamedKey::Right), at, &g, 0),
        ChainAction::Redraw
    );
    assert_eq!(
        plates(&chain),
        2,
        "Right entered the highlighted row's child"
    );

    assert_eq!(
        chain.handle(&key(NamedKey::Left), at, &g, 0),
        ChainAction::Redraw
    );
    assert_eq!(plates(&chain), 1, "Left backed out of it");

    chain.handle(&key(NamedKey::Home), at, &g, 0);
    assert_eq!(
        chain.handle(&key(NamedKey::Enter), at, &g, 0),
        ChainAction::Closed
    );
    assert_eq!(
        chain.take_answers(),
        alloc::vec![(APP, ChainOutcome::Chosen(id(1)))],
        "Home landed on the first row and Enter chose it"
    );
}

#[test]
fn escape_closes_the_deepest_child_first_and_then_dismisses() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    settle_on(&mut chain, 1, 0, &g);
    assert_eq!(plates(&chain), 3);
    let at = Point::ORIGIN;

    chain.handle(&key(NamedKey::Escape), at, &g, 0);
    assert_eq!(plates(&chain), 2);
    chain.handle(&key(NamedKey::Escape), at, &g, 0);
    assert_eq!(plates(&chain), 1);
    assert_eq!(
        chain.handle(&key(NamedKey::Escape), at, &g, 0),
        ChainAction::Closed
    );
    assert_eq!(
        chain.take_answers(),
        alloc::vec![(APP, ChainOutcome::Dismissed)],
        "repeated Escape always gets the user out"
    );
}

#[test]
fn escape_closes_the_information_panel_before_the_menu_that_opened_it() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, info_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);

    let acted = chain.handle(&key(NamedKey::Escape), Point::ORIGIN, &g, 0);
    assert_eq!(acted, ChainAction::Redraw);
    assert!(info(&chain).is_none(), "the panel closed first");
    assert!(chain.is_open(), "and the chain it hung on survived");
}

// --- lifetime ------------------------------------------------------------

#[test]
fn the_owners_death_answers_the_chain_dismissed() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);

    assert!(
        !chain.dismiss_owner(1234),
        "another window's death is not this chain's"
    );
    assert!(chain.take_answers().is_empty());

    assert!(chain.dismiss_owner(7));
    assert_eq!(
        chain.take_answers(),
        alloc::vec![(APP, ChainOutcome::Dismissed)]
    );
    assert!(!chain.is_open());
}

#[test]
fn a_mode_change_under_the_gesture_dismisses_rather_than_re_placing() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();

    for changed in [
        geom_on(&theme, Rect::new(0, 0, 1024, 768)),
        ChainGeometry {
            epoch: (200, 0),
            ..geom(&theme)
        },
        ChainGeometry {
            epoch: (100, 1),
            ..geom(&theme)
        },
    ] {
        open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);
        assert!(
            !chain.settle_mode(&g),
            "the mode it was placed at is not a change"
        );
        assert!(chain.settle_mode(&changed), "the ground moved under it");
        assert_eq!(
            chain.take_answers(),
            alloc::vec![(APP, ChainOutcome::Dismissed)]
        );
        assert!(!chain.is_open());
    }
}

#[test]
fn a_closed_chain_lists_no_surfaces_at_any_depth() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    settle_on(&mut chain, 1, 0, &g);
    assert_eq!(chain.surfaces().len(), 3);

    let away = Point::new(SCREEN.right() - 5, SCREEN.bottom() - 5);
    assert_eq!(chain.handle(&PRESS, away, &g, 0), ChainAction::Closed);
    assert!(
        chain.surfaces().is_empty(),
        "the reconcile that takes plates down reads this list, so every depth \
         must leave it at once"
    );
}

// --- the wire model is a bounded subset ----------------------------------

#[test]
fn a_decoded_row_can_never_claim_the_system_lacks_authority() {
    let mut wire = titled("App");
    wire.push(AppMenuRow::Item(AppMenuItem::new(
        id(1),
        label("Do it"),
        AppMenuSymbol::Open,
    )))
    .expect("a row");
    wire.push(AppMenuRow::Item(
        AppMenuItem::new(id(2), label("Denied"), AppMenuSymbol::Open).disabled(),
    ))
    .expect("a row");

    let model = ChainModel::from_app_menu("App", &wire, None);
    for row in model.rows() {
        assert_eq!(
            row.drawn().state().authority,
            tairix_controls::AuthorityState::Allowed,
            "no wire field carries an authority state, so none can be claimed"
        );
    }
}

#[test]
fn a_declared_separator_becomes_the_next_rows_group_break_on_every_plate() {
    let mut wire = titled("App");
    wire.push(AppMenuRow::Item(AppMenuItem::new(
        id(1),
        label("One"),
        AppMenuSymbol::Open,
    )))
    .expect("a row");
    wire.push(AppMenuRow::Separator).expect("a rule");
    wire.push(AppMenuRow::Submenu {
        label: label("More"),
        enabled: true,
        symbol: AppMenuSymbol::Settings,
        reason: AppMenuReason::EMPTY,
    })
    .expect("a submenu");
    let parent = wire.len() - 1;
    wire.push_under(
        AppMenuRow::Item(AppMenuItem::new(id(2), label("Inner"), AppMenuSymbol::Open)),
        parent,
    )
    .expect("a row");
    wire.push_under(AppMenuRow::Separator, parent)
        .expect("a rule");
    wire.push_under(
        AppMenuRow::Item(AppMenuItem::new(id(3), label("After"), AppMenuSymbol::Open)),
        parent,
    )
    .expect("a row");

    let model = ChainModel::from_app_menu("App", &wire, None);
    let breaks: Vec<bool> = model
        .rows()
        .iter()
        .map(|row| row.drawn().is_group_break())
        .collect();
    assert_eq!(
        breaks,
        alloc::vec![false, true, false, true],
        "a separator draws a divider inside a submenu exactly as on the root"
    );
    assert_eq!(
        model.rows().len(),
        4,
        "and takes no row of its own on either plate"
    );
}

#[test]
fn an_information_row_without_an_attested_identity_is_left_out() {
    let mut wire = titled("App");
    wire.push(AppMenuRow::Item(AppMenuItem::new(
        id(1),
        label("One"),
        AppMenuSymbol::Open,
    )))
    .expect("a row");
    wire.push(AppMenuRow::Info).expect("an info row");

    let bare = ChainModel::from_app_menu("App", &wire, None);
    assert_eq!(bare.rows().len(), 1, "no identity, no panel to open");

    let attested = ChainModel::from_app_menu("App", &wire, Some(&about_app()));
    assert_eq!(attested.rows().len(), 2);
    assert!(
        matches!(attested.rows()[1].child(), ChainChild::Info(_),),
        "the session's own panel, from the signed manifest"
    );
}

#[test]
fn a_submenu_on_the_deepest_plate_is_refused_by_the_wire_model() {
    // The chain renders what the model can express; the model's own depth
    // bound is what stops a chevron opening nothing.
    let mut wire = titled("Deep");
    wire.push(AppMenuRow::Submenu {
        label: label("L1"),
        enabled: true,
        symbol: AppMenuSymbol::Settings,
        reason: AppMenuReason::EMPTY,
    })
    .expect("a submenu");
    let mut parent = wire.len() - 1;
    for level in 2..APP_MENU_MAX_DEPTH {
        wire.push_under(
            AppMenuRow::Submenu {
                label: label(&alloc::format!("L{level}")),
                enabled: true,
                symbol: AppMenuSymbol::Settings,
                reason: AppMenuReason::EMPTY,
            },
            parent,
        )
        .expect("a deeper submenu");
        parent = wire.len() - 1;
    }
    assert!(
        wire.push_under(
            AppMenuRow::Submenu {
                label: label("too deep"),
                enabled: true,
                symbol: AppMenuSymbol::Settings,
                reason: AppMenuReason::EMPTY,
            },
            parent,
        )
        .is_err(),
        "a submenu past the bound is refused rather than drawn opening nothing"
    );
    let _ = String::new();
}

// --- a refused menu is an answer, not a death ----------------------------

#[test]
fn a_refusal_answers_the_asking_window_and_leaves_the_chain_alone() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);

    let asking = ChainOwner::Window {
        window_id: 11,
        open_id: 44,
    };
    chain.refuse(asking.clone(), MenuRefusal::SeatBusy);
    assert_eq!(
        chain.take_answers(),
        alloc::vec![(asking, ChainOutcome::Refused(MenuRefusal::SeatBusy))]
    );
    assert_eq!(
        chain.owner(),
        Some(&APP),
        "a refusal is a fact about the seat, not a reason to end a menu the \
         user is already using"
    );
}

#[test]
fn a_chain_the_desktop_cannot_draw_is_refused_no_resources() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    assert!(!chain.exhausted(), "no chain, nothing to refuse");

    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);
    assert!(chain.exhausted());
    assert_eq!(
        chain.take_answers(),
        alloc::vec![(APP, ChainOutcome::Refused(MenuRefusal::NoResources))]
    );
    assert!(!chain.is_open());
    assert!(chain.surfaces().is_empty());
}

// --- what a repaint costs ------------------------------------------------

#[test]
fn a_pointer_travelling_within_one_row_costs_no_repaint() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);

    let row = chain.row_rect(0, 1, &g).expect("the second row");
    let first = Point::new(row.left() + 4, row.top() + 2);
    assert_eq!(
        chain.handle(&moved(first.x, first.y), first, &g, 0),
        ChainAction::Redraw,
        "arriving on the row moved the highlight onto it"
    );
    for step in 1..4 {
        let along = Point::new(first.x + step * 3, first.y + 1);
        assert_eq!(
            chain.handle(&moved(along.x, along.y), along, &g, 0),
            ChainAction::Consumed,
            "a sample inside the row it is already on changes no pixel"
        );
    }
}

#[test]
fn a_highlight_moving_on_one_plate_disturbs_no_other() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    let root = plate(&chain, 0);
    let child = plate(&chain, 1);

    // Move the highlight within the child plate: the chain repaints, and both
    // plates stay exactly where they were.
    let (acted, _) = settle_on(&mut chain, 1, 1, &g);
    assert_eq!(acted, ChainAction::Redraw);
    assert_eq!(plate(&chain, 0), root);
    assert_eq!(plate(&chain, 1), child);
    assert_eq!(plates(&chain), 2, "and no plate opened or closed");
}

/// A surface the session has never drawn owes every pixel of it; one it has
/// just drawn owes none. That pair is what lets a plate be retained chrome
/// rather than a fresh render per pointer sample.
#[test]
fn a_new_surface_owes_every_pixel_and_a_presented_one_owes_none() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    assert_eq!(owed(&chain, SurfaceKind::Plate(0)), Repaint::Whole);

    present_all(&mut chain);
    assert_eq!(owed(&chain, SurfaceKind::Plate(0)), covering(&[]));

    // A child opening is a surface of its own, and it too owes all of itself.
    settle_on(&mut chain, 0, 1, &g);
    assert_eq!(owed(&chain, SurfaceKind::Plate(1)), Repaint::Whole);
}

/// The whole point of M4: a highlight crossing a plate owes the row the mark
/// left and the row it arrived on, in that plate's own pixels — not the plate,
/// which on frosted chrome would mean re-blending all of it.
#[test]
fn moving_the_highlight_owes_the_two_rows_it_moved_between() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);
    present_all(&mut chain);

    settle_on(&mut chain, 0, 0, &g);
    assert_eq!(
        owed(&chain, SurfaceKind::Plate(0)),
        covering(&[local_row(&chain, 0, 0, &g)]),
        "arriving from nowhere lights one row"
    );

    present_all(&mut chain);
    settle_on(&mut chain, 0, 2, &g);
    let moved_between = [local_row(&chain, 0, 0, &g), local_row(&chain, 0, 2, &g)];
    assert_eq!(
        owed(&chain, SurfaceKind::Plate(0)),
        covering(&moved_between),
        "the row the mark left and the row it arrived on"
    );
    let owes = owed(&chain, SurfaceKind::Plate(0));
    let Repaint::Parts(parts) = owes else {
        panic!("a plate on screen owes rectangles, not all of itself");
    };
    assert!(
        parts.bounds().height < plate(&chain, 0).height,
        "and not the plate: {:?} of {:?}",
        parts.bounds(),
        plate(&chain, 0)
    );
}

/// Keyboard traversal is the same two rows: the service owns the mark, so how
/// it was moved cannot change what a move costs.
#[test]
fn traversing_by_keyboard_owes_the_two_rows_too() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(40, 40), &g);
    chain.handle(&key(NamedKey::Down), Point::ORIGIN, &g, 0);
    present_all(&mut chain);

    assert_eq!(
        chain.handle(&key(NamedKey::Down), Point::ORIGIN, &g, 0),
        ChainAction::Redraw
    );
    assert_eq!(
        owed(&chain, SurfaceKind::Plate(0)),
        covering(&[local_row(&chain, 0, 0, &g), local_row(&chain, 0, 1, &g)])
    );
}

/// A pointer moving on a child plate leaves its parent — and the information
/// panel, when one is up — owing nothing. Without that, one highlight would
/// re-render every surface of the chain.
#[test]
fn a_highlight_moving_on_one_plate_owes_no_other_surface_a_pixel() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    settle_on(&mut chain, 1, 0, &g);
    present_all(&mut chain);

    settle_on(&mut chain, 1, 1, &g);
    assert_eq!(
        owed(&chain, SurfaceKind::Plate(1)),
        covering(&[local_row(&chain, 1, 0, &g), local_row(&chain, 1, 1, &g)])
    );
    assert_eq!(
        owed(&chain, SurfaceKind::Plate(0)),
        covering(&[]),
        "the parent plate's pixels did not change"
    );
}

/// A plate rebuilt at a depth another plate held owes all of it: its rows are
/// a different menu, and the session re-uses that depth's window, so anything
/// less would leave the previous submenu's rows on screen.
#[test]
fn a_plate_rebuilt_at_a_depth_owes_every_pixel_of_it() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, nested_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    settle_on(&mut chain, 1, 0, &g);
    present_all(&mut chain);
    assert_eq!(plates(&chain), 3);

    // "Inner" has no child, so the grandchild plate closes; going back to
    // "Deeper" builds a fresh one at that same depth.
    settle_on(&mut chain, 1, 1, &g);
    assert_eq!(plates(&chain), 2);
    settle_on(&mut chain, 1, 0, &g);
    assert_eq!(owed(&chain, SurfaceKind::Plate(2)), Repaint::Whole);
}

/// Dragging moves a plate; it does not repaint one. The plate's pixels are the
/// same pixels somewhere else, so a drag costs the compositor a move and the
/// chain nothing.
#[test]
fn dragging_a_plate_owes_no_repaint() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, flat_model(), Point::new(200, 200), &g);
    present_all(&mut chain);

    let root = plate(&chain, 0);
    let band = Point::new(root.left() + 20, root.top() + 2);
    chain.handle(&moved(band.x, band.y), band, &g, 0);
    chain.handle(&PRESS, band, &g, 0);
    let to = Point::new(band.x + 60, band.y + 40);
    chain.handle(&moved(to.x, to.y), to, &g, 0);

    assert_ne!(plate(&chain, 0), root, "the plate moved");
    assert_eq!(
        owed(&chain, SurfaceKind::Plate(0)),
        covering(&[]),
        "and not one pixel of it was redrawn"
    );
}

/// The information panel states facts that never change, so it owes its pixels
/// once and nothing after that.
#[test]
fn the_information_panel_owes_its_pixels_once() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, info_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    assert_eq!(owed(&chain, SurfaceKind::Info), Repaint::Whole);

    present_all(&mut chain);
    let inside = {
        let panel = info(&chain).expect("the panel hangs");
        Point::new(panel.left() + 8, panel.top() + 8)
    };
    chain.handle(&moved(inside.x, inside.y), inside, &g, 0);
    assert_eq!(
        owed(&chain, SurfaceKind::Info),
        covering(&[]),
        "a pointer over facts changes nothing"
    );
}

/// A partial repaint lands exactly the pixels a whole one would. The session
/// paints a plate's damaged rectangles into the buffer it already holds, so
/// anything else would leave the highlight smeared across the rows it crossed.
#[test]
fn repainting_a_plates_damaged_rows_lands_what_a_whole_paint_would() {
    let theme = theme();
    let g = geom(&theme);
    let paint_whole = |chain: &MenuChain| paint(chain, 0, &g);

    // Two chains driven identically: one repainted whole at every step, the
    // other keeping its buffer and painting only what it said had changed.
    let mut whole = MenuChain::new();
    open(&mut whole, APP, flat_model(), Point::new(40, 40), &g);
    let mut partial = MenuChain::new();
    open(&mut partial, APP, flat_model(), Point::new(40, 40), &g);
    let mut retained = paint_whole(&partial);
    present_all(&mut partial);

    for row in [0, 2, 1] {
        settle_on(&mut whole, 0, row, &g);
        settle_on(&mut partial, 0, row, &g);
        let Repaint::Parts(parts) = owed(&partial, SurfaceKind::Plate(0)) else {
            panic!("a plate on screen owes rectangles, not all of itself");
        };
        assert!(!parts.is_empty(), "settling on row {row} moved the mark");
        // Through the shared clip-and-clear walk the session paints with, so
        // this is the production path rather than a second copy of it.
        tairix_controls::damage::paint_parts(&mut retained, parts.rects(), |surface| {
            partial.render_surface(SurfaceKind::Plate(0), surface, &g);
        });
        present_all(&mut partial);
        assert_eq!(
            retained,
            paint_whole(&whole),
            "the plate after settling on row {row}"
        );
    }
}

// --- what a plate is painted with -----------------------------------------

/// A plate lays the raised ground, at the opacity floating chrome is drawn at,
/// and repaints with the theme.
///
/// The pixels are what no state test can state. A plate is the desktop's
/// floating chrome like the bar and its popups, so its ground takes the
/// palette's `chrome_alpha` through the one shared rule rather than a weight of
/// its own — and what it opened over reads through it
/// (`tests::every_menu_chain_surface_frosts_what_is_behind_it` holds the other
/// half of that pair: the compositor window asks for the theme's chrome blur,
/// without which the rows would sit on detail).
#[test]
fn a_plate_lays_the_raised_ground_and_follows_the_theme() {
    let dark = Theme::dark().floating();
    let light = Theme::light().floating();
    let mut chain = MenuChain::new();

    let painted = |chain: &MenuChain, theme: &Theme| -> tairix_raster::Surface {
        let g = geom(theme);
        let plate = chain.surfaces().first().expect("the root plate").rect;
        let mut surface =
            tairix_raster::Surface::new(plate.width, plate.height).expect("a plate surface");
        chain.render_surface(SurfaceKind::Plate(0), &mut surface, &g);
        surface
    };

    open(
        &mut chain,
        APP,
        flat_model(),
        Point::new(40, 40),
        &geom(&dark),
    );
    let on_dark = painted(&chain, &dark);
    let fill = tairix_controls::ground_fill(
        &dark,
        dark.palette().surface_raised,
        tairix_controls::ChromeLayer::Ground,
    );
    let ground = tairix_raster::Color::from(fill).premultiply();
    assert!(
        on_dark.pixels().contains(&ground),
        "a plate lays the raised ground"
    );
    assert_eq!(
        ground.a,
        dark.palette().chrome_alpha,
        "and lets what it opened over read through it"
    );
    assert!(ground.a < 255, "a plate that covers frosts nothing");

    let on_light = painted(&chain, &light);
    assert_eq!(
        (on_dark.width(), on_dark.height()),
        (on_light.width(), on_light.height()),
        "the same model, so the same plate"
    );
    assert_ne!(
        on_dark.pixels(),
        on_light.pixels(),
        "a theme switch repaints the plate"
    );
}

#[test]
fn a_plate_is_wide_enough_for_its_own_title_not_only_its_rows() {
    // A plate is chrome the desktop sizes to its content, so a title wider
    // than every row widens the plate rather than being elided to fit rows
    // that happen to be short.
    const TITLE: &str = "A rather longer plate title than any of its rows";
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    let mut model = ChainModel::new(TITLE);
    for (index, label) in ["Cut", "Copy"].iter().enumerate() {
        let id = AppMenuItemId::for_index(index).expect("row id");
        model.push(ChainRow::item(
            id,
            AppMenuSymbol::Open,
            MenuItem::new(*label),
        ));
    }
    chain
        .open(
            APP,
            model,
            PlatePlacement::adjacent(Rect::new(0, 0, 1, 1)),
            &g,
        )
        .expect("open");
    let plate = chain
        .surfaces()
        .into_iter()
        .find(|s| s.kind == SurfaceKind::Plate(0))
        .expect("root plate")
        .rect;

    let mut band = tairix_controls::TitleBar::plate();
    band.set_title(TITLE);
    assert!(
        plate.width >= band.preferred_band_width(g.scale, g.theme),
        "the plate seats the width its band asked for"
    );
    let band_rect = Rect::new(
        0,
        0,
        plate.width,
        tairix_controls::TitleBar::band_height(g.scale, g.theme),
    );
    let title_box = band.layout(band_rect, g.scale, g.theme).title;
    let font = tairix_font::BitmapFont::for_role(
        g.theme.fonts(),
        tairix_theme::TextRole::SectionHeader,
        g.scale,
    );
    let (_, marked) = font.elide_to_width(TITLE, title_box.width);
    assert!(!marked, "and draws the title without eliding it");
}

// --- a row's explanation is a tip --------------------------------------

/// A root plate whose second row is refused and explains why, and whose
/// third is refused and says nothing.
fn explained_model() -> ChainModel {
    let refused = || MenuItem::new("Open With\u{2026}").with_state(refused_state());
    let mut model = ChainModel::new("Files");
    model.push(ChainRow::item(
        id(1),
        AppMenuSymbol::Open,
        MenuItem::new("Open"),
    ));
    model.push(
        ChainRow::item(id(2), AppMenuSymbol::Open, refused())
            .explained("only a file opens with an application"),
    );
    model.push(ChainRow::item(
        id(3),
        AppMenuSymbol::Open,
        MenuItem::new("Paste").with_state(refused_state()),
    ));
    model
}

/// The state a row the desktop will not let be chosen is drawn in.
fn refused_state() -> tairix_controls::ControlState {
    tairix_controls::ControlState::default().with_enabled(false)
}

#[test]
fn the_hovered_row_reports_its_own_screen_rect_and_its_explanation() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, explained_model(), Point::new(40, 40), &g);

    assert_eq!(
        chain.hovered_tip(&g),
        None,
        "a chain nothing is hovering on explains nothing"
    );

    settle_on(&mut chain, 0, 1, &g);
    let (rect, why) = chain.hovered_tip(&g).expect("the refused row explains");
    assert_eq!(why, "only a file opens with an application");
    assert_eq!(
        rect,
        chain.row_rect(0, 1, &g).expect("the row is laid out"),
        "the region is the row's own screen rectangle, because the chain \
         placed the plate itself"
    );

    // A row with nothing to explain answers nothing, refused or not.
    settle_on(&mut chain, 0, 2, &g);
    assert_eq!(chain.hovered_tip(&g), None);
    settle_on(&mut chain, 0, 0, &g);
    assert_eq!(chain.hovered_tip(&g), None);
}

#[test]
fn a_closed_chain_explains_nothing() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();
    open(&mut chain, APP, explained_model(), Point::new(40, 40), &g);
    settle_on(&mut chain, 0, 1, &g);
    assert!(chain.hovered_tip(&g).is_some());

    chain.handle(&key(NamedKey::Escape), row_point(&chain, 0, 1, &g), &g, 0);
    assert_eq!(
        chain.hovered_tip(&g),
        None,
        "a dismissed chain has no row under the pointer to explain"
    );
}

/// The pointer works on the deepest plate; an ancestor keeps its highlight to
/// show the path it came down, not to say where the pointer is. So it is the
/// child's row that answers a tip, and the child's rectangle.
#[test]
fn the_deepest_plate_is_the_one_that_explains_the_pointer() {
    let theme = theme();
    let g = geom(&theme);
    let mut chain = MenuChain::new();

    let mut model = ChainModel::new("Nested");
    let parent = model.push(ChainRow::submenu(
        AppMenuSymbol::Settings,
        MenuItem::new("Open With\u{2026}"),
    ));
    model.push(
        ChainRow::item(
            id(2),
            AppMenuSymbol::Open,
            MenuItem::new("View").with_state(refused_state()),
        )
        .under(parent)
        .explained("that application cannot open this file"),
    );
    open(&mut chain, APP, model, Point::new(40, 40), &g);

    // Resting on the parent opens its child, which becomes the deepest plate
    // with nothing hovered on it yet. The parent is offered, so there is
    // nothing to explain either way.
    settle_on(&mut chain, 0, parent, &g);
    assert_eq!(plates(&chain), 2, "the child plate is up");
    assert_eq!(chain.hovered_tip(&g), None);

    settle_on(&mut chain, 1, 0, &g);
    let (rect, why) = chain.hovered_tip(&g).expect("the child's refused row");
    assert_eq!(why, "that application cannot open this file");
    assert_eq!(
        rect,
        chain.row_rect(1, 0, &g).expect("the child row is laid out"),
        "and its own rectangle on its own plate"
    );
}
