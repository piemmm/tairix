use super::*;
use crate::cpu::gpr;
use tairix_controls::{ControlDisposition, FieldGroupAction};

fn regs() -> Regs {
    let mut regs = Regs::default();
    regs.set_sreg(Seg::Cs, 0xF000);
    regs.set_sreg(Seg::Ds, 0x1234);
    regs.gpr[gpr::SI] = 0x0080;
    regs
}

/// The dialog with kind `kind` chosen, `at` and `detail` typed.
fn typed(kind: usize, at: &str, detail: &str) -> Adding {
    let mut adding = Adding::new(regs());
    let choose = FieldGroupAction {
        row: row::KIND,
        action: FieldAction::Selected { index: kind },
    };
    adding.take(&choose, Rect::EMPTY, &mut Region::new());
    for (n, text) in [(row::AT, at), (row::DETAIL, detail)] {
        if let Some(FieldControl::Text(field)) = adding
            .group
            .rows_mut()
            .get_mut(n)
            .map(FieldRow::control_mut)
        {
            field.set_text(text);
        }
    }
    adding
}

#[test]
fn each_kind_makes_its_breakpoint_from_what_was_typed() {
    let entry = |kind, at, detail| typed(kind, at, detail).entry().unwrap();
    assert_eq!(
        entry(0, "E05B", ""),
        Entry {
            breakpoint: Breakpoint::Code(0xF_E05B),
            at: String::from("F000:E05B"),
        },
        "an offset alone is in CS"
    );
    assert_eq!(
        entry(1, "SI", "2"),
        Entry {
            breakpoint: Breakpoint::Memory {
                first: 0x1_23C0,
                len: 2,
                access: Access::Write,
            },
            at: String::from("1234:0080"),
        },
        "memory's in DS"
    );
    assert_eq!(
        entry(2, "B800:0000", "").breakpoint,
        Breakpoint::Memory {
            first: 0xB_8000,
            len: 1,
            access: Access::Any,
        },
        "a byte unless more are asked"
    );
    assert_eq!(
        entry(5, "3F8h", "3FF").breakpoint,
        Breakpoint::Port {
            first: 0x3F8,
            last: 0x3FF,
            direction: Direction::Both,
        }
    );
    assert_eq!(
        entry(3, "60", "").breakpoint,
        Breakpoint::Port {
            first: 0x60,
            last: 0x60,
            direction: Direction::In,
        }
    );
    assert_eq!(
        entry(6, "21", "4C").breakpoint,
        Breakpoint::Interrupt {
            vector: 0x21,
            ah: Some(0x4C),
        }
    );
    assert_eq!(
        entry(6, "8", "").breakpoint,
        Breakpoint::Interrupt {
            vector: 0x08,
            ah: None,
        }
    );
}

#[test]
fn what_names_no_breakpoint_is_said_and_nothing_is_added() {
    for (kind, at, detail) in [
        (0, "nowhere", ""),
        (1, "0", "0"),
        (1, "0", "65537"),
        (2, "FFFFF", "2"),
        (3, "10000", ""),
        (5, "64", "60"),
        (6, "100", ""),
        (6, "21", "100"),
    ] {
        assert!(
            typed(kind, at, detail).entry().is_err(),
            "{kind} {at:?} {detail:?}"
        );
    }
}

#[test]
fn choosing_a_kind_relabels_its_fields_and_one_with_no_detail_disables_it() {
    let disabled = |adding: &Adding| {
        adding.group.rows()[row::DETAIL].state().disposition()
            == ControlDisposition::DisabledByState
    };
    assert!(disabled(&typed(0, "", "")), "an instruction has no detail");
    assert!(!disabled(&typed(1, "", "")));
}

#[test]
fn the_kind_list_stays_open_under_the_pointer_and_takes_the_kind_clicked() {
    tairix_font::install_test_transport();
    let (scale, theme) = (Scale::ONE, Theme::light());
    let mut adding = Adding::new(regs());
    let feed = |adding: &mut Adding, event: InputEvent| {
        adding.on_input(&event, 0, (scale, &theme), &mut Region::new());
    };
    let primary = tairix_input::PointerButton::Primary;
    let (_, layout) = adding.layout(scale, &theme);
    let bounds = adding
        .group
        .row_rect(row::KIND, layout, scale, &theme)
        .unwrap();
    let field = adding.group.rows()[row::KIND]
        .control_rect(FieldLayout { bounds, ..layout }, scale, &theme)
        .unwrap()
        .center();
    for event in [
        InputEvent::PointerMoved { to: field },
        InputEvent::PointerPressed { button: primary },
        InputEvent::PointerReleased { button: primary },
    ] {
        feed(&mut adding, event);
    }
    let (_, layout) = adding.layout(scale, &theme);
    let FieldControl::Combo(combo) = adding.group.rows()[row::KIND].control() else {
        panic!("the kind row is a choice");
    };
    let port = 3;
    let choice = combo
        .menu()
        .row_rect(port, layout.popup, scale, &theme)
        .unwrap()
        .center();
    feed(&mut adding, InputEvent::PointerMoved { to: choice });
    assert!(adding.listing(), "moving onto the list leaves it open");
    for event in [
        InputEvent::PointerPressed { button: primary },
        InputEvent::PointerReleased { button: primary },
    ] {
        feed(&mut adding, event);
    }
    assert!(!adding.listing());
    assert_eq!(adding.kind, KINDS[port].0);
}
