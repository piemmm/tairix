//! The shared two-button prompt: an alert of the desktop's own, fitted to a
//! screen too small for it, armed against a click meant for what was there,
//! and answered only by its own two buttons.

use tairix_abi::PowerAction;
use tairix_geometry::Scale;
use tairix_wm::Point;

use super::{Choice, ChoicePrompt, ACCEPT, DECLINE};
use crate::alert::ALERT_ARM_NS;
use crate::confirm::build_spec;
use crate::tests::headless_desktop;

/// A prompt drawn at 200% on a small desktop is fitted to it whole: every
/// button reachable and no edge off the screen.
#[test]
fn a_prompt_on_a_small_dense_screen_is_wholly_on_it() {
    let (mut shell, mut comp) = headless_desktop();
    let scale = Scale::from_percent(200).expect("200% is in range");
    assert!(comp.set_scale(scale));
    let screen = comp.screen_rect();
    let spec = build_spec(PowerAction::Restart).expect("the prompt's text fits an alert");
    let prompt = ChoicePrompt::open(&spec, None, &mut shell, &mut comp).expect("the prompt opens");
    let bounds = comp
        .window(prompt.wm())
        .expect("the prompt's window")
        .bounds();
    assert!(
        bounds.left() >= screen.left()
            && bounds.top() >= screen.top()
            && bounds.right() <= screen.right()
            && bounds.bottom() <= screen.bottom(),
        "{bounds:?} is not on {screen:?}"
    );
    let client = comp.window_client_rect(prompt.wm()).expect("a client");
    for index in [DECLINE, ACCEPT] {
        let at = prompt.button_centre(index, &shell, &comp);
        assert!(
            at.x >= 0 && at.y >= 0 && at.x < client.right() && at.y < client.bottom(),
            "button {index} at {at:?} is outside its {client:?} client"
        );
    }
}

#[test]
fn only_its_two_buttons_answer_a_prompt_once_it_has_armed() {
    let (mut shell, mut comp) = headless_desktop();
    let spec = build_spec(PowerAction::PowerOff).expect("the prompt's text fits an alert");
    let mut prompt = ChoicePrompt::open(&spec, None, &mut shell, &mut comp).expect("opens");
    prompt.stamp(0);
    let accept = prompt.button_centre(ACCEPT, &shell, &comp);
    let decline = prompt.button_centre(DECLINE, &shell, &comp);
    assert_eq!(
        prompt.click(accept, ALERT_ARM_NS - 1, &shell, &comp),
        None,
        "a click just after it appeared was meant for what was there"
    );
    assert_eq!(
        prompt.click(Point::new(4, 4), ALERT_ARM_NS, &shell, &comp),
        None
    );
    assert_eq!(
        prompt.click(accept, ALERT_ARM_NS, &shell, &comp),
        Some(Choice::Accepted)
    );
    assert_eq!(
        prompt.click(decline, ALERT_ARM_NS, &shell, &comp),
        Some(Choice::Declined)
    );
}
