use super::*;
use tairix_appconf::{Document, Registry};

#[test]
fn the_default_posture_is_polite() {
    assert_eq!(ProbeSettings::default().posture, Posture::Polite);
    assert_eq!(Posture::default(), Posture::Polite);
}

#[test]
fn each_posture_resolves_to_its_preset() {
    assert_eq!(Posture::Passive.profile(), ProbeProfile::PASSIVE);
    assert_eq!(Posture::Polite.profile(), ProbeProfile::POLITE);
    assert_eq!(Posture::Thorough.profile(), ProbeProfile::THOROUGH);
    assert!(!Posture::Passive.profile().probe, "passive sends nothing");
}

#[test]
fn a_posture_round_trips_through_the_store() {
    for posture in Posture::ALL {
        let doc = Document::parse(&alloc::format!("probe.posture = {}\n", posture.token()))
            .expect("the line parses");
        let (settings, refused) = ProbeSettings::load(&doc);
        assert_eq!(settings.posture, posture);
        assert!(refused.is_empty());
    }
}

#[test]
fn an_unknown_value_keeps_the_default() {
    let doc = Document::parse("probe.posture = aggressive\n").expect("the line parses");
    let (settings, refused) = ProbeSettings::load(&doc);
    assert_eq!(settings.posture, Posture::default(), "the default stands");
    assert_eq!(refused, [ProbeKey::Posture], "the bad value is reported");
}

#[test]
fn the_pane_opens_on_a_posture_and_adopts_a_new_one() {
    use tairix_geometry::{Region, Scale};
    use tairix_theme::ThemeRegistry;

    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut pane = ProbePane::new(Posture::Polite, Scale::ONE, theme);
    let (width, height) = pane.extent();
    assert!(width > 0 && height > 0, "the pane wants a real extent");

    let mut damage = Region::new();
    pane.adopt(Posture::Thorough, &mut damage);
    assert!(!damage.is_empty(), "adopting a new posture repaints");

    let mut again = Region::new();
    pane.adopt(Posture::Thorough, &mut again);
    assert!(again.is_empty(), "adopting the shown posture moves nothing");
}

#[test]
fn tokens_are_distinct_and_recognised() {
    for posture in Posture::ALL {
        assert_eq!(Posture::from_token(posture.token()), Some(posture));
    }
    assert_eq!(Posture::from_token("nonsense"), None);
}
