//! Host tests of the pictures a pane offers: what each chooser lists and
//! writes, which picture is asked for next, what is kept and what let go,
//! and what a rebuild keeps.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{PreviewSubject, WINDOW_PREVIEW_MAX_SIDE};
use tairix_controls::{FieldGroup, FieldLayout, PictureChoice};
use tairix_geometry::{to_i32, Rect, Scale};
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_wallpaper::{
    wallpaper_path, Backdrop, CatalogItem, DesktopSettings, Rgb, ScreensaverKind, WallpaperChoice,
    WallpaperPath,
};

use super::{
    options_caption, Chooser, Offer, Offered, PictureWanted, Pictured, Pictures, NONE_LABEL,
};

fn subjects(offered: &[Offered]) -> Vec<Option<PreviewSubject>> {
    offered.iter().map(|held| held.subject).collect()
}

/// What choosing each picture sets.
fn offers(offered: &[Offered]) -> Vec<&Offer> {
    offered.iter().map(|held| &held.offer).collect()
}

fn item(category: &str, file: &str) -> CatalogItem {
    CatalogItem {
        category: String::from(category),
        file: String::from(file),
    }
}

fn catalog() -> Vec<CatalogItem> {
    vec![
        item("Abstract", "a.jpg"),
        item("Nature", "n0.jpg"),
        item("Nature", "n1.jpg"),
        item("Space", "s.jpg"),
    ]
}

fn showing(path: &str) -> DesktopSettings {
    DesktopSettings {
        wallpaper: WallpaperChoice::Image(WallpaperPath::new(path).expect("a path")),
        ..DesktopSettings::default()
    }
}

fn titles(choice: &PictureChoice) -> Vec<&str> {
    choice
        .sections()
        .iter()
        .map(tairix_controls::PictureSection::title)
        .collect()
}

fn labels(choice: &PictureChoice) -> Vec<&str> {
    (0..choice.len())
        .filter_map(|index| choice.item(index))
        .map(tairix_controls::PictureItem::label)
        .collect()
}

#[test]
fn the_wallpaper_chooser_offers_no_picture_then_each_category_in_turn() {
    let settings = showing(&wallpaper_path("Nature", "n1.jpg"));
    let (choice, offered) = Chooser::Wallpaper.offer(&settings, &catalog());
    assert_eq!(titles(&choice), ["", "Abstract", "Nature", "Space"]);
    assert_eq!(
        labels(&choice),
        [NONE_LABEL, "a.jpg", "n0.jpg", "n1.jpg", "s.jpg"]
    );
    assert_eq!(choice.selected(), Some(3));
    assert_eq!(
        subjects(&offered),
        [
            None,
            Some(PreviewSubject::Wallpaper(0)),
            Some(PreviewSubject::Wallpaper(1)),
            Some(PreviewSubject::Wallpaper(2)),
            Some(PreviewSubject::Wallpaper(3)),
        ]
    );
    let none = choice.item(0).expect("no picture");
    assert!(!none.takes_art(), "the backdrop colour is its own picture");
}

/// A picture in effect the catalog does not hold is never hidden: it is
/// listed beside the others of its category, or under the directory it sits
/// in, and chosen.
#[test]
fn a_picture_in_effect_the_catalog_lacks_is_listed_where_it_belongs() {
    let removed = showing(&wallpaper_path("Nature", "gone.jpg"));
    let (choice, offered) = Chooser::Wallpaper.offer(&removed, &catalog());
    assert_eq!(titles(&choice), ["", "Abstract", "Nature", "Space"]);
    assert_eq!(
        labels(&choice),
        [NONE_LABEL, "a.jpg", "n0.jpg", "n1.jpg", "gone.jpg", "s.jpg"]
    );
    assert_eq!(choice.selected(), Some(4));
    assert_eq!(
        offered[4].subject, None,
        "a render names a catalog position"
    );

    let own = showing("/Users/ann/Pictures/me.png");
    let (choice, _) = Chooser::Wallpaper.offer(&own, &catalog());
    assert_eq!(
        titles(&choice),
        ["", "Abstract", "Nature", "Space", "Pictures"]
    );
    assert_eq!(choice.selected(), Some(5));
}

/// Each picture sets exactly the picture it shows — the one outside the
/// catalog included — and nothing else.
#[test]
fn each_picture_sets_the_picture_it_shows() {
    let own = showing("/Users/ann/Pictures/me.png");
    let (_, offered) = Chooser::Wallpaper.offer(&own, &catalog());
    let set = |offer: &Offer| {
        let mut settings = own.clone();
        offer.apply(&mut settings);
        settings
    };
    let chosen: Vec<DesktopSettings> = offers(&offered).into_iter().map(set).collect();
    assert_eq!(chosen[0].wallpaper, WallpaperChoice::None);
    assert_eq!(chosen[2], showing(&wallpaper_path("Nature", "n0.jpg")));
    assert_eq!(chosen[5], own, "the one in effect, by its own position");
    assert!(
        chosen.iter().all(|settings| DesktopSettings {
            wallpaper: own.wallpaper.clone(),
            ..settings.clone()
        } == own),
        "a picture writes the wallpaper alone"
    );
}

#[test]
fn the_no_picture_choice_follows_the_backdrop() {
    for backdrop in [Backdrop::Theme, Backdrop::Colour(Rgb::new(1, 2, 3))] {
        let settings = DesktopSettings {
            backdrop,
            wallpaper: WallpaperChoice::None,
            ..DesktopSettings::default()
        };
        let (choice, _) = Chooser::Wallpaper.offer(&settings, &[]);
        assert_eq!(labels(&choice), [NONE_LABEL]);
        assert_eq!(choice.selected(), Some(0));
    }
}

#[test]
fn the_screensaver_chooser_offers_every_kind_with_its_preview() {
    let settings = DesktopSettings {
        screensaver: ScreensaverKind::Starfield,
        ..DesktopSettings::default()
    };
    let (choice, offered) = Chooser::Screensaver.offer(&settings, &catalog());
    assert_eq!(
        labels(&choice),
        [
            "Black",
            "Dimmed desktop",
            "Slideshow",
            "Clock",
            "Minimal Clock",
            "Starfield",
            "Game of Life",
            "Ray Tracer"
        ]
    );
    assert_eq!(titles(&choice), [""]);
    assert_eq!(choice.selected(), Some(5));
    let expected: Vec<_> = ScreensaverKind::ALL
        .iter()
        .map(|kind| Some(PreviewSubject::Screensaver(*kind)))
        .collect();
    assert_eq!(subjects(&offered), expected);
    let mut chosen = settings.clone();
    offered[6].offer.apply(&mut chosen);
    assert_eq!(chosen.screensaver, ScreensaverKind::Life);
    assert_eq!(options_caption(ScreensaverKind::Life), "GAME OF LIFE");
    let mut chosen = settings.clone();
    offered[4].offer.apply(&mut chosen);
    assert_eq!(chosen.screensaver, ScreensaverKind::Ribbon);
    assert_eq!(options_caption(ScreensaverKind::Ribbon), "MINIMAL CLOCK");
    let mut chosen = settings;
    offered[7].offer.apply(&mut chosen);
    assert_eq!(chosen.screensaver, ScreensaverKind::Raytrace);
    assert_eq!(options_caption(ScreensaverKind::Raytrace), "RAY TRACER");
}

/// `count` shipped pictures, all in one category.
fn nature(count: usize) -> Vec<CatalogItem> {
    (0..count)
        .map(|at| item("Nature", &alloc::format!("{at}.jpg")))
        .collect()
}

/// A wallpaper chooser showing no picture, laid out 600 wide, with its
/// bookkeeping.
struct Laid {
    groups: Vec<FieldGroup>,
    pictures: Pictures,
    layout: FieldLayout,
    theme: Theme,
}

impl Laid {
    fn new(catalog: &[CatalogItem]) -> Self {
        let settings = DesktopSettings {
            wallpaper: WallpaperChoice::None,
            ..DesktopSettings::default()
        };
        let (choice, offered) = Chooser::Wallpaper.offer(&settings, catalog);
        let theme = Theme::dark();
        let group = FieldGroup::new("DESKTOP PICTURE", Vec::new()).with_pictures(choice);
        let height = group.measured_height(600, 0, Scale::ONE, &theme);
        let mut pictures = Pictures::default();
        pictures.adopt(
            vec![Pictured {
                chooser: Chooser::Wallpaper,
                group: 0,
                pictures: offered,
            }],
            &mut [],
            alloc::collections::BTreeMap::default(),
        );
        Self {
            groups: vec![group],
            pictures,
            layout: FieldLayout::new(Rect::new(0, 0, 600, height), 0),
            theme,
        }
    }

    fn round(&mut self, seen: Rect, roomy: bool) -> Option<PictureWanted> {
        self.round_passing(seen, roomy, |_| false)
    }

    fn round_passing(
        &mut self,
        seen: Rect,
        roomy: bool,
        mut asked: impl FnMut(PreviewSubject) -> bool,
    ) -> Option<PictureWanted> {
        self.pictures.round(
            &mut self.groups,
            &[(0, self.layout)],
            (seen, roomy),
            (Scale::ONE, &self.theme),
            &mut asked,
        )
    }

    /// Answer every picture the rounds ask for over `seen`.
    fn fill(&mut self, seen: Rect, roomy: bool) {
        while let Some(next) = self.round(seen, roomy) {
            assert!(self.answer(next));
        }
    }

    fn choice(&self) -> &PictureChoice {
        self.groups[0].pictures().expect("a chooser")
    }

    fn tile(&self, index: usize) -> Rect {
        let bounds = self.groups[0]
            .row_rect(0, self.layout, Scale::ONE, &self.theme)
            .expect("the chooser");
        self.choice()
            .item_rect(index, bounds, Scale::ONE, &self.theme)
            .expect("a tile")
    }

    /// Answer `wanted` with opaque pixels of the size it asked for.
    fn answer(&mut self, wanted: PictureWanted) -> bool {
        let pixels = vec![0xFF; wanted.bytes()];
        self.pictures
            .land(&mut self.groups, wanted, &pixels)
            .is_some()
    }

    fn has_art(&self, index: usize) -> bool {
        self.choice()
            .item(index)
            .is_some_and(|item| item.art().is_some())
    }
}

/// The band a test sees: the first `height` pixels of the chooser.
fn top(height: u32) -> Rect {
    Rect::new(0, 0, 600, height)
}

#[test]
fn a_picture_already_asked_for_is_passed_over_for_the_next_nearest() {
    let mut laid = Laid::new(&nature(40));
    let seen = Rect::new(0, 0, 600, laid.tile(1).bottom().unsigned_abs());
    let nearest = laid.round(seen, false).expect("a picture on screen");
    let next = laid
        .round_passing(seen, false, |subject| subject == nearest.subject)
        .expect("another picture on screen");
    assert_ne!(next.subject, nearest.subject);
    assert_eq!(laid.round_passing(seen, false, |_| true), None);
}

#[test]
fn the_pictures_on_screen_are_asked_for_first_then_those_within_reach() {
    let mut laid = Laid::new(&nature(40));
    let seen = Rect::new(0, 0, 600, laid.tile(1).bottom().unsigned_abs());
    let first = laid.round(seen, false).expect("a picture on screen");
    assert_eq!(first.subject, PreviewSubject::Wallpaper(0));
    let (width, height) = laid.choice().picture_size(Scale::ONE, &laid.theme);
    assert_eq!(
        (u32::from(first.width), u32::from(first.height)),
        (width, height)
    );
    assert!(laid.answer(first));
    // Everything on the first line, then nothing more once memory is short.
    let mut asked = vec![first.subject];
    while let Some(next) = laid.round(seen, false) {
        assert!(laid.answer(next));
        asked.push(next.subject);
    }
    let shown = (1..=40)
        .take_while(|index| laid.tile(*index).top() < seen.bottom())
        .count();
    assert_eq!(asked.len(), shown, "only what is on screen: {asked:?}");
    // With memory to spare, the lines just beneath follow, nearest first.
    let next = laid.round(seen, true).expect("one within reach");
    assert_eq!(
        next.subject,
        PreviewSubject::Wallpaper(u16::try_from(shown).expect("small"))
    );
}

/// Scrolling a long chooser to its far end and back never costs the desktop a
/// second render while memory is plentiful: a picture handed over stays for
/// the life of the pane, however far off screen it is scrolled.
#[test]
fn while_memory_is_roomy_every_picture_handed_over_is_kept_however_far_the_pane_scrolls() {
    let mut laid = Laid::new(&nature(40));
    let screen = laid.tile(1).bottom().unsigned_abs();
    let first = Rect::new(0, 0, 600, screen);
    laid.fill(first, true);
    let held: Vec<usize> = (1..=40).filter(|index| laid.has_art(*index)).collect();
    assert!(
        !held.is_empty() && !laid.has_art(40),
        "the premise: the top is rendered and the far end is out of reach"
    );

    let far = laid.tile(40);
    let last = Rect::new(0, far.bottom() - to_i32(screen), 600, screen);
    laid.fill(last, true);
    assert!(laid.has_art(40), "the far end was rendered on the way down");
    assert!(
        held.iter().all(|index| laid.has_art(*index)),
        "nothing rendered at the top was let go on the way down"
    );
    assert_eq!(
        laid.round(first, true),
        None,
        "scrolling back to the top asks the desktop for nothing"
    );
}

#[test]
fn once_memory_is_short_pictures_off_screen_are_let_go_and_asked_for_again_on_return() {
    let mut laid = Laid::new(&nature(40));
    let everything = top(laid.layout.bounds.height);
    laid.fill(everything, false);
    assert!(laid.has_art(40));
    let seen = Rect::new(0, 0, 600, laid.tile(1).bottom().unsigned_abs());
    assert_eq!(
        laid.round(seen, false),
        None,
        "everything on screen is in hand"
    );
    assert!(laid.has_art(1), "on screen is kept");
    assert!(!laid.has_art(40), "the far end is let go");
    let back = laid.round(everything, false).expect("asked for again");
    assert_ne!(back.subject, PreviewSubject::Wallpaper(0));
}

/// Plentiful memory keeps what the chooser draws, not a picture of another
/// size: one rendered before the scale moved is let go all the same.
#[test]
fn a_picture_of_a_size_the_chooser_no_longer_draws_is_let_go_and_asked_for_again() {
    let mut laid = Laid::new(&nature(3));
    let everything = top(laid.layout.bounds.height);
    assert!(laid.groups[0]
        .pictures_mut()
        .expect("a chooser")
        .set_art(1, Surface::new(4, 4).expect("art")));
    let again = laid
        .round(everything, true)
        .expect("the stale one is wanted");
    assert_eq!(again.subject, PreviewSubject::Wallpaper(0));
    assert!(!laid.has_art(1));
}

#[test]
fn a_refused_picture_is_not_asked_for_again_and_a_short_answer_is_a_refusal() {
    let mut laid = Laid::new(&nature(2));
    let everything = top(laid.layout.bounds.height);
    let first = laid.round(everything, false).expect("wanted");
    assert!(laid.pictures.refuse(first.subject));
    assert!(!laid.pictures.refuse(first.subject), "once");
    let second = laid.round(everything, false).expect("the other");
    assert_ne!(second.subject, first.subject);
    assert_eq!(laid.pictures.land(&mut laid.groups, second, &[0; 8]), None);
    assert!(!laid.has_art(2), "nothing of the wrong shape is drawn");
    assert_eq!(laid.round(everything, false), None, "both refused");
    // An answer for a subject no chooser shows changes nothing.
    let stray = PictureWanted {
        subject: PreviewSubject::Screensaver(ScreensaverKind::Clock),
        ..second
    };
    assert!(!laid.answer(stray));
}

#[test]
fn rendered_pictures_survive_a_rebuild_on_whichever_picture_they_render() {
    let mut catalog = nature(3);
    catalog.push(item("Space", "s.jpg"));
    let mut laid = Laid::new(&catalog);
    laid.fill(top(laid.layout.bounds.height), true);
    let carried = laid.pictures.take(&mut laid.groups);
    assert_eq!(carried.len(), 4);
    assert!(!laid.has_art(1), "taken, not copied");
    // Rebuilt over the same catalog with a picture in effect it lacks, listed
    // among its category's and so moving the picture after it along by one.
    let (choice, offered) =
        Chooser::Wallpaper.offer(&showing(&wallpaper_path("Nature", "zz.jpg")), &catalog);
    laid.groups = vec![FieldGroup::new("DESKTOP PICTURE", Vec::new()).with_pictures(choice)];
    laid.pictures.adopt(
        vec![Pictured {
            chooser: Chooser::Wallpaper,
            group: 0,
            pictures: offered,
        }],
        &mut laid.groups,
        carried,
    );
    assert!([1, 2, 3, 5].iter().all(|index| laid.has_art(*index)));
    assert!(!laid.has_art(4), "the one in effect has nothing to carry");
}

/// Every picture a chooser draws is one the desktop agrees to render, at
/// every scale a desktop may be set to.
#[test]
fn every_picture_a_chooser_asks_for_is_one_the_desktop_accepts_at_every_scale() {
    let (choice, _) = Chooser::Screensaver.offer(&DesktopSettings::default(), &[]);
    for theme in [Theme::dark(), Theme::light()] {
        for percent in (Scale::MIN_PERCENT..=Scale::MAX_PERCENT).step_by(25) {
            let scale = Scale::from_percent(percent).expect("a scale");
            let (width, height) = choice.picture_size(scale, &theme);
            assert!(
                width <= u32::from(WINDOW_PREVIEW_MAX_SIDE)
                    && height <= u32::from(WINDOW_PREVIEW_MAX_SIDE),
                "{width}x{height} at {percent}%"
            );
        }
    }
}
