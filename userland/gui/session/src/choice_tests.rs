//! The shared two-button prompt: fitted to a screen too small for it, and
//! answered only by its own two buttons.

use tairix_abi::PowerAction;
use tairix_geometry::Scale;
use tairix_wm::{Point, Rect};

use super::{Choice, ChoicePrompt};
use crate::confirm::{build_dialog, WIN_HEIGHT, WIN_WIDTH};
use crate::tests::headless_desktop;

/// A prompt drawn at 200% on a 640-pixel desktop: twice its logical width is
/// wider than the screen.
#[test]
fn a_prompt_wider_than_the_screen_is_fitted_to_it_and_strands_no_button() {
    let (mut shell, mut comp) = headless_desktop();
    let scale = Scale::from_percent(200).expect("200% is in range");
    assert!(comp.set_scale(scale));
    let screen = comp.screen_rect();
    assert!(scale.scale_length(WIN_WIDTH) > screen.width);

    let prompt = ChoicePrompt::open(
        build_dialog(PowerAction::Restart),
        (WIN_WIDTH, WIN_HEIGHT),
        "Restart",
        &mut shell,
        &mut comp,
    )
    .expect("the prompt opens");
    let window = comp.window(prompt.wm()).expect("the prompt's window");
    let (width, height) = window.client_size();
    let placed = Rect::new(window.origin().x, window.origin().y, width, height);
    assert!(
        placed.left() >= 0
            && placed.right() <= screen.right()
            && placed.bottom() <= screen.bottom(),
        "{placed:?} is not on {screen:?}"
    );
    let theme = shell.session().active_theme();
    for button in build_dialog(PowerAction::Restart).action_rects(
        Rect::new(0, 0, width, height),
        scale,
        theme,
    ) {
        assert!(button.width > 0);
        assert!(
            button.left() >= 0 && button.right() <= i32::try_from(width).expect("a width"),
            "{button:?} is outside a {width}-pixel prompt"
        );
    }
}

#[test]
fn only_its_two_buttons_answer_a_prompt() {
    let (mut shell, mut comp) = headless_desktop();
    let mut prompt = ChoicePrompt::open(
        build_dialog(PowerAction::PowerOff),
        (WIN_WIDTH, WIN_HEIGHT),
        "Shut Down",
        &mut shell,
        &mut comp,
    )
    .expect("the prompt opens");
    assert_eq!(prompt.click(Point::new(4, 4), &shell, &comp), None);
    let rects = build_dialog(PowerAction::PowerOff).action_rects(
        Rect::new(0, 0, WIN_WIDTH, WIN_HEIGHT),
        Scale::ONE,
        shell.session().active_theme(),
    );
    let centre = |rect: Rect| {
        Point::new(
            rect.origin.x + i32::try_from(rect.width / 2).expect("a width"),
            rect.origin.y + i32::try_from(rect.height / 2).expect("a height"),
        )
    };
    assert_eq!(
        prompt.click(centre(rects[1]), &shell, &comp),
        Some(Choice::Accepted)
    );
    assert_eq!(
        prompt.click(centre(rects[0]), &shell, &comp),
        Some(Choice::Declined)
    );
}
