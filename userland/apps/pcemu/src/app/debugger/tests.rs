use super::*;
use crate::cpu::Regs;
use crate::debug::Activity;
use crate::link::debug::Listing;
use tairix_geometry::Scale;
use tairix_input::Modifiers;
use tairix_raster::Pixel;

const SIZE: (u32, u32) = (1100, 800);

fn theme() -> Theme {
    tairix_font::install_test_transport();
    Theme::light()
}

/// Whether `pixel` is the colour of channels `rgb`.
fn is(pixel: Option<Pixel>, rgb: (u8, u8, u8)) -> bool {
    pixel.is_some_and(|p| (p.r, p.g, p.b) == rgb)
}

fn registers(cs: u16, ip: u16) -> Registers {
    let mut regs = Regs::default();
    regs.set_sreg(Seg::Cs, cs);
    regs.set_sreg(Seg::Ds, 0x0040);
    regs.ip = ip;
    Registers {
        regs,
        cycles: 0,
        activity: Activity::Executing,
        emulating: false,
        coprocessor: None,
    }
}

fn press(debugger: &mut Debugger, at: Point, looks: (Scale, &Theme)) -> Option<Ask> {
    let mut damage = Region::new();
    debugger.on_input(&InputEvent::PointerMoved { to: at }, 0, looks, &mut damage);
    let pressed = InputEvent::PointerPressed {
        button: PointerButton::Primary,
    };
    let ask = debugger.on_input(&pressed, 0, looks, &mut damage);
    let released = InputEvent::PointerReleased {
        button: PointerButton::Primary,
    };
    ask.or_else(|| debugger.on_input(&released, 0, looks, &mut damage))
}

fn key(debugger: &mut Debugger, key: Key, shift: bool, looks: (Scale, &Theme)) -> Option<Ask> {
    let modifiers = Modifiers {
        shift,
        ..Modifiers::default()
    };
    let event = InputEvent::KeyPressed { key, modifiers };
    debugger.on_input(&event, 0, looks, &mut Region::new())
}

fn function(number: u8) -> Key {
    Key::Named(NamedKey::Function { number })
}

/// The stopped window's look answered with lines at each of `ips`, the
/// first `before` of them ahead of the code address.
fn answer(debugger: &mut Debugger, ips: &[u16], before: u8, looks: (Scale, &Theme)) -> Option<Ask> {
    let lines: Vec<crate::debug::disasm::Line> = ips
        .iter()
        .map(|&ip| crate::debug::disasm::Line {
            ip,
            bytes: alloc::vec![0x90],
            text: String::from("NOP"),
        })
        .collect();
    let (before, after, encoded) = Listing::encode(&lines, usize::from(before));
    let seen = Seen {
        number: 0,
        listing: Listing::new(before, after, &encoded),
        memory: &[0; 32],
        stack: &[0; 4],
    };
    debugger.seen(&seen, looks, &mut Region::new())
}

#[test]
fn a_stop_asks_for_what_the_panes_show_and_a_second_waits_for_the_first() {
    let theme = theme();
    let looks = (Scale::ONE, &theme);
    let mut debugger = Debugger::new(false, SIZE);
    assert!(debugger.running());
    let Some(Ask::Look(look)) = debugger.stopped(
        Reason::Break,
        registers(0xF000, 0xE05B),
        looks,
        &mut Region::new(),
    ) else {
        panic!("no look asked");
    };
    assert_eq!(look.code, (0xF000, 0xE05B));
    assert_eq!(look.memory, (0x0040, 0), "DS:0000 first");
    assert!(
        look.lines.0 > 0 && look.lines.1 > look.lines.0,
        "most lines after"
    );
    assert!(look.bytes > 0 && look.bytes % ROW_BYTES == 0 && look.stack > 0);
    assert_eq!(
        debugger.stopped(
            Reason::Stepped,
            registers(0xF000, 0xE05C),
            looks,
            &mut Region::new()
        ),
        None,
        "asked again once the first is answered"
    );
    let Some(Ask::Look(again)) = answer(&mut debugger, &[0xE05B], 0, looks) else {
        panic!("the wanted look was not asked");
    };
    assert_eq!(again.code, (0xF000, 0xE05C));
    assert_eq!(answer(&mut debugger, &[0xE05C], 0, looks), None);
}

#[test]
fn the_controls_send_the_machine_on_only_from_a_stop() {
    let theme = theme();
    let looks = (Scale::ONE, &theme);
    let mut debugger = Debugger::new(false, SIZE);
    assert_eq!(
        key(&mut debugger, function(10), false, looks),
        None,
        "running"
    );
    assert_eq!(
        key(&mut debugger, function(5), false, looks),
        Some(Ask::Break)
    );
    debugger.stopped(Reason::Break, registers(0, 0), looks, &mut Region::new());
    assert_eq!(
        key(&mut debugger, function(11), true, looks),
        Some(Ask::Go(Go::StepOut))
    );
    assert!(debugger.running(), "sent on");
    debugger.stopped(Reason::Stepped, registers(0, 1), looks, &mut Region::new());
    let layout = debugger.layout(looks);
    let centre = |rect: Rect| Point::new(rect.left() + 4, rect.top() + 4);
    assert_eq!(
        press(&mut debugger, centre(layout.buttons[1]), looks),
        None,
        "Break is off"
    );
    assert_eq!(
        press(&mut debugger, centre(layout.buttons[3]), looks),
        Some(Ask::Go(Go::StepOver))
    );
    assert_eq!(
        press(&mut debugger, centre(layout.buttons[5]), looks),
        Some(Ask::AddBreakpoint)
    );
}

#[test]
fn a_press_in_the_gutter_toggles_a_breakpoint_on_its_line() {
    let theme = theme();
    let looks = (Scale::ONE, &theme);
    let mut debugger = Debugger::new(false, SIZE);
    debugger.stopped(
        Reason::Break,
        registers(0xF000, 0xE05B),
        looks,
        &mut Region::new(),
    );
    answer(&mut debugger, &[0xE05A, 0xE05B, 0xE05C], 1, looks);
    let layout = debugger.layout(looks);
    let second = layout.row(layout.code, 1);
    let gutter = Point::new(second.left() + 1, second.top() + 1);
    let breakpoint = Breakpoint::Code(0xF_E05B);
    assert_eq!(
        press(&mut debugger, gutter, looks),
        Some(Ask::Add(breakpoint))
    );
    assert_eq!(debugger.entries()[0].at, "F000:E05B");
    assert_eq!(
        press(&mut debugger, gutter, looks),
        Some(Ask::Remove(breakpoint))
    );
    assert!(debugger.entries().is_empty());
    assert_eq!(
        key(&mut debugger, function(9), false, looks),
        Some(Ask::Add(breakpoint))
    );
}

#[test]
fn the_wheel_moves_the_listing_and_memory_by_lines() {
    let theme = theme();
    let looks = (Scale::ONE, &theme);
    let mut debugger = Debugger::new(false, SIZE);
    debugger.stopped(
        Reason::Break,
        registers(0x1000, 0x0100),
        looks,
        &mut Region::new(),
    );
    answer(
        &mut debugger,
        &[0xFE, 0xFF, 0x100, 0x101, 0x102, 0x103, 0x104],
        2,
        looks,
    );
    let layout = debugger.layout(looks);
    let mut damage = Region::new();
    let over = |rect: Rect| InputEvent::PointerMoved {
        to: Point::new(rect.left() + 40, rect.top() + 40),
    };
    let detent = tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    debugger.on_input(&over(layout.code), 0, looks, &mut damage);
    let down = InputEvent::PointerScrolled { dx: 0, dy: detent };
    let Some(Ask::Look(look)) = debugger.on_input(&down, 0, looks, &mut damage) else {
        panic!("no look");
    };
    assert_eq!(look.code, (0x1000, 0x103), "three lines on");
    answer(&mut debugger, &[0x101, 0x102, 0x103, 0x104], 2, looks);
    debugger.on_input(&over(layout.memory), 0, looks, &mut damage);
    let up = InputEvent::PointerScrolled { dx: 0, dy: -detent };
    let Some(Ask::Look(look)) = debugger.on_input(&up, 0, looks, &mut damage) else {
        panic!("no look");
    };
    assert_eq!(look.memory, (0x0040, 0u16.wrapping_sub(3 * ROW_BYTES)));
}

#[test]
fn an_address_typed_is_where_its_pane_reads_from() {
    let theme = theme();
    let looks = (Scale::ONE, &theme);
    let mut debugger = Debugger::new(false, SIZE);
    debugger.stopped(
        Reason::Break,
        registers(0x1000, 0x0100),
        looks,
        &mut Region::new(),
    );
    answer(&mut debugger, &[0x100], 0, looks);
    let layout = debugger.layout(looks);
    let field = layout.fields[1];
    press(
        &mut debugger,
        Point::new(field.left() + 2, field.top() + 2),
        looks,
    );
    for ch in "b800:10".chars() {
        key(&mut debugger, Key::Char(ch), false, looks);
    }
    let Some(Ask::Look(look)) = key(&mut debugger, Key::Named(NamedKey::Enter), false, looks)
    else {
        panic!("no look");
    };
    assert_eq!(look.memory, (0xB800, 0x0010));
    press(
        &mut debugger,
        Point::new(field.left() + 2, field.top() + 2),
        looks,
    );
    for ch in "nowhere".chars() {
        key(&mut debugger, Key::Char(ch), false, looks);
    }
    assert_eq!(
        key(&mut debugger, Key::Named(NamedKey::Enter), false, looks),
        None
    );
}

#[test]
fn a_stop_s_state_paints_and_what_a_change_damages_is_painted_alone() {
    let theme = theme();
    let looks = (Scale::ONE, &theme);
    let mut debugger = Debugger::new(true, SIZE);
    let mut stopped = registers(0x1000, 0x0100);
    stopped.coprocessor = Some(crate::debug::Coprocessor {
        control: 0x037F,
        status: 0,
        tags: 0xFFFF,
        ip: 0,
        opcode: 0,
        dp: 0,
        stack: [tairix_inline::ArrayString::from_str_truncating("Empty"); 8],
    });
    debugger.stopped(Reason::Break, stopped, looks, &mut Region::new());
    answer(&mut debugger, &[0x100, 0x101], 0, looks);
    let mut surface = Surface::new(SIZE.0, SIZE.1).unwrap();
    let mut whole = Region::new();
    whole.add(Rect::new(0, 0, SIZE.0, SIZE.1));
    debugger.render(&mut surface, &whole, looks);
    let layout = debugger.layout(looks);
    let plate = theme.palette().surface_raised;
    let plate = (plate.r, plate.g, plate.b);
    let corner = |surface: &Surface, rect: Rect| {
        let (x, y) = (rect.right() - 1, rect.bottom() - 1);
        surface.get(u32::try_from(x).unwrap(), u32::try_from(y).unwrap())
    };
    assert!(
        is(corner(&surface, layout.stack), plate),
        "the stack's plate painted"
    );
    let mut blank = Surface::new(SIZE.0, SIZE.1).unwrap();
    let mut only = Region::new();
    only.add(layout.breakpoints);
    debugger.render(&mut blank, &only, looks);
    assert!(is(corner(&blank, layout.breakpoints), plate));
    assert!(
        !is(corner(&blank, layout.code), plate),
        "the code pane left alone"
    );
}

#[test]
fn a_reset_lets_go_of_the_look_out_at_the_stop() {
    let theme = theme();
    let looks = (Scale::ONE, &theme);
    let mut debugger = Debugger::new(false, SIZE);
    let first = debugger.stopped(
        Reason::Break,
        registers(0xF000, 0xE05B),
        looks,
        &mut Region::new(),
    );
    assert!(matches!(first, Some(Ask::Look(_))));
    debugger.runs(&mut Region::new());
    assert!(debugger.running());
    let again = debugger.stopped(
        Reason::Break,
        registers(0xFFFF, 0),
        looks,
        &mut Region::new(),
    );
    assert!(
        matches!(again, Some(Ask::Look(_))),
        "asked at once: the first goes unanswered"
    );
}
