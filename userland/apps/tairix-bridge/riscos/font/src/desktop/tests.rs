use alloc::vec::Vec;

use tairix_riscos_kernel::desktop::DesktopFont;

use super::set_variables;
use crate::fixture::{find, fonts};

fn variable(s: &mut tairix_riscos_filing::session::Session, name: &[u8]) -> Option<Vec<u8>> {
    s.kernel.os().variable_text(name).expect("read")
}

#[test]
fn a_session_names_the_desktop_font_in_the_wimp_s_variables_at_no_size_of_its_own() {
    let mut s = fonts();
    assert_eq!(
        variable(&mut s, b"Wimp$Font").as_deref(),
        Some(&b"Desktop.Medium"[..])
    );
    assert_eq!(
        variable(&mut s, b"Wimp$FontSize"),
        None,
        "the Wimp's own twelve points"
    );
    assert_eq!(variable(&mut s, b"Wimp$FontWidth"), None);
}

#[test]
fn a_size_its_host_gives_is_the_wimp_s_height_and_width_and_a_set_variable_stands() {
    let mut s = fonts();
    {
        let mut os = s.kernel.os();
        os.set_string_variable(b"Wimp$Font", b"Homerton.Medium")
            .expect("set");
        os.set_string_variable(b"Wimp$FontSize", b"192")
            .expect("set");
        let given = DesktopFont {
            size: Some(160),
            ..DesktopFont::SHIPPED
        };
        set_variables(&mut os, given).expect("set");
    }
    assert_eq!(
        variable(&mut s, b"Wimp$Font").as_deref(),
        Some(&b"Homerton.Medium"[..]),
        "the user's own"
    );
    assert_eq!(
        variable(&mut s, b"Wimp$FontSize").as_deref(),
        Some(&b"192"[..])
    );
    assert_eq!(
        variable(&mut s, b"Wimp$FontWidth").as_deref(),
        Some(&b"160"[..]),
        "ten points"
    );
}

#[test]
fn the_desktop_font_is_found_among_the_rom_fonts_in_the_session_s_family() {
    let mut s = fonts();
    let service = alloc::rc::Rc::new(core::cell::RefCell::new(crate::build::SquareFaces::capped(
        usize::MAX,
    )));
    let serving = alloc::rc::Rc::clone(&service);
    s.serve_fonts(alloc::boxed::Box::new(move |q, r| {
        serving.borrow_mut().answer(q, r)
    }));
    let handle = find(&mut s, b"Desktop.Medium", 192);
    assert_ne!(handle, 0);
    let families = core::mem::take(&mut service.borrow_mut().families);
    assert!(
        !families.is_empty() && families.iter().all(|&f| f == DesktopFont::SHIPPED.family),
        "{families:?}"
    );
}
