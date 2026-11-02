use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{AppMenuItemId, AppMenuRowView};
use tairix_geometry::{Point, Region, Scale};
use tairix_image::{SpriteMode, SpriteName, SpritePalette, Unkept};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_sandbox::imagerender::ViewFormat;
use tairix_theme::ThemeRegistry;
use tairix_window::docapp::Request as HostRequest;
use tairix_window::document::{Access, SavedDocument};

use tairix_raster::{Pixel, Surface};
use tairix_window::docapp::Relayout;

use super::{render_area, AreaLayout, AreaView, SpriteOpening, OPEN_CELLS, SPRITES_AT_ONCE};
use crate::canvas::{Canvas, Kind, Sample};
use crate::document::{Document, Entry, EntryId, Kept, Origin, Picture, SpriteInfo};
use crate::layout::Faces;
use crate::save::SaveSettings;
use crate::view::{compute, Action, Compute, Computed, MenuKind, Outcome, Own, RENAME_ENTRY};

const WINDOW: (u32, u32) = (420, 320);
const DOUBLE: Duration64 = Duration64::from_millis(500);

fn named(name: &str) -> SpriteName {
    SpriteName::new(name).expect("a sprite name")
}

fn sprite(name: &str, colour: [u8; 4]) -> Entry {
    let canvas = Canvas::new(8, 4, Kind::Rgba, Sample::Rgba(colour)).expect("a canvas");
    let mut picture = Picture::plain(canvas);
    picture.sprite = Some(SpriteInfo {
        name: named(name),
        mode: SpriteMode::truecolour((1, 1), false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    Entry::Picture(picture)
}

fn area(names: &[&str]) -> Document {
    let entries = names
        .iter()
        .enumerate()
        .map(|(at, name)| sprite(name, [u8::try_from(at * 40).unwrap_or(0), 0, 0, 255]))
        .collect();
    Document::of(
        entries,
        Origin::Read(ViewFormat::Sprite),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("a document")
}

/// A sprite file window on `document`, laid out.
struct Window {
    view: AreaView,
    layout: AreaLayout,
    registry: ThemeRegistry,
    now: u64,
}

impl Window {
    fn new(document: Document) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let view = AreaView::new(
            document,
            String::from("Sprites"),
            None,
            Access::Writable,
            DOUBLE,
        );
        let mut window = Self {
            layout: view.layout(
                WINDOW.0,
                WINDOW.1,
                registry.active(),
                Scale::ONE,
                Faces::of(registry.active(), Scale::ONE),
            ),
            view,
            registry,
            now: 1_000_000_000,
        };
        window.settle();
        window
    }

    fn of(names: &[&str]) -> Self {
        Self::new(area(names))
    }

    /// Lay the window out again, as the host does after an outcome asking it.
    fn settle(&mut self) {
        let theme = self.registry.active();
        self.layout = self.view.layout(
            WINDOW.0,
            WINDOW.1,
            theme,
            Scale::ONE,
            Faces::of(theme, Scale::ONE),
        );
        self.view.settle(&self.layout, &mut Region::new());
    }

    fn feed(&mut self, input: InputEvent) -> Outcome {
        self.now += 1_000_000;
        let theme = self.registry.active();
        let outcome = self.view.input(
            &input,
            self.now,
            &self.layout,
            Scale::ONE,
            theme,
            &mut Region::new(),
        );
        self.settle();
        outcome
    }

    fn press_cell(&mut self, index: usize, modifiers: Modifiers) -> Outcome {
        let cell = self.layout.cell_rect(index, self.view.offset());
        self.feed(InputEvent::ModifiersChanged { modifiers });
        self.feed(InputEvent::PointerMoved {
            to: Point::new(cell.left() + 4, cell.top() + 4),
        });
        let outcome = self.feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        self.feed(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        outcome
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> Outcome {
        self.feed(InputEvent::KeyPressed { key, modifiers })
    }

    fn act(&mut self, action: Action) -> Outcome {
        let item = AppMenuItemId::new(action.id()).expect("a row");
        let outcome = self.view.chosen_row(item, &self.layout, &mut Region::new());
        self.settle();
        outcome
    }

    fn names(&self) -> Vec<String> {
        self.view
            .document()
            .entries()
            .iter()
            .map(|entry| {
                entry
                    .name()
                    .map_or_else(String::new, |name| alloc::format!("{name}"))
            })
            .collect()
    }

    fn id(&self, index: usize) -> EntryId {
        self.view.document().ids()[index]
    }
}

fn opened(outcome: Outcome) -> Option<Vec<EntryId>> {
    match outcome.request {
        Some(HostRequest::Own(Own::OpenSprites(ids))) => Some(ids),
        _ => None,
    }
}

const CTRL: Modifiers = Modifiers {
    shift: false,
    ctrl: true,
    alt: false,
    meta: false,
};

const SHIFT: Modifiers = Modifiers {
    shift: true,
    ctrl: false,
    alt: false,
    meta: false,
};

#[test]
fn a_sprite_file_opens_with_its_first_sprite_chosen() {
    let window = Window::of(&["one", "two", "three"]);
    assert_eq!(window.view.chosen(), [window.id(0)]);
    assert_eq!(window.view.cursor(), Some(window.id(0)));
    assert!(!window.view.is_modified());
}

#[test]
fn a_double_click_opens_the_sprite_in_its_own_window() {
    let mut window = Window::of(&["one", "two"]);
    assert!(opened(window.press_cell(1, Modifiers::default())).is_none());
    assert_eq!(window.view.chosen(), [window.id(1)]);
    let ids = opened(window.press_cell(1, Modifiers::default())).expect("opened");
    assert_eq!(ids, [window.id(1)]);
}

#[test]
fn ctrl_adds_and_takes_sprites_and_shift_runs_to_one() {
    let mut window = Window::of(&["a", "b", "c", "d"]);
    window.press_cell(2, CTRL);
    assert_eq!(window.view.chosen(), [window.id(0), window.id(2)]);
    // A Ctrl press moves the anchor to what it pressed, as a file manager's.
    window.press_cell(3, SHIFT);
    assert_eq!(window.view.chosen(), [window.id(2), window.id(3)]);
    window.press_cell(2, CTRL);
    assert_eq!(window.view.chosen(), [window.id(3)]);
}

#[test]
fn the_keyboard_walks_the_sprites() {
    let mut window = Window::of(&["a", "b", "c"]);
    window.key(Key::Named(NamedKey::Right), Modifiers::default());
    assert_eq!(window.view.chosen(), [window.id(1)]);
    window.key(Key::Named(NamedKey::End), Modifiers::default());
    assert_eq!(window.view.cursor(), Some(window.id(2)));
    window.key(Key::Named(NamedKey::Home), SHIFT);
    assert_eq!(
        window.view.chosen().len(),
        3,
        "Shift runs back to the first"
    );
    let ids = opened(window.key(Key::Named(NamedKey::Enter), Modifiers::default()))
        .expect("Enter opens what is chosen");
    assert_eq!(ids.len(), 3);
}

#[test]
fn deleting_sprites_is_undone_with_their_identities() {
    let mut window = Window::of(&["a", "b", "c"]);
    let doomed = window.id(1);
    window.press_cell(1, Modifiers::default());
    let outcome = window.act(Action::DeleteEntry);
    assert_eq!(outcome.relayout, tairix_window::docapp::Relayout::Whole);
    assert_eq!(window.names(), ["a", "c"]);
    assert!(window.view.is_modified());
    window.act(Action::Undo);
    assert_eq!(window.names(), ["a", "b", "c"]);
    assert_eq!(window.id(1), doomed, "the same sprite came back");
}

#[test]
fn the_last_sprites_are_never_all_deleted() {
    let mut window = Window::of(&["a", "b"]);
    window.act(Action::SelectAll);
    window.act(Action::DeleteEntry);
    assert_eq!(window.names(), ["a", "b"]);
    assert!(window.view.message().is_some(), "the refusal is said");
}

#[test]
fn a_rename_changes_the_name_alone_and_is_undone_alone() {
    let mut window = Window::of(&["a", "b"]);
    let item = AppMenuItemId::new(RENAME_ENTRY).expect("a field");
    window
        .view
        .entered(item, "  apple ", &window.layout, &mut Region::new());
    assert_eq!(window.names(), ["apple", "b"]);
    window
        .view
        .entered(item, "b", &window.layout, &mut Region::new());
    assert_eq!(window.names(), ["apple", "b"], "a name taken is refused");
    window.act(Action::Undo);
    assert_eq!(window.names(), ["a", "b"]);
}

#[test]
fn a_kept_sprite_keeps_its_name_and_opens_nothing() {
    let kept = Entry::Kept(Kept {
        name: named("raw"),
        reason: tairix_sandbox::imageedit::KeptReason::UnknownMode,
        bytes: alloc::sync::Arc::new(alloc::vec![0; 44]),
    });
    let document = Document::of(
        alloc::vec![kept, sprite("pic", [0, 0, 0, 255])],
        Origin::Read(ViewFormat::Sprite),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("a document");
    let mut window = Window::new(document);
    let item = AppMenuItemId::new(RENAME_ENTRY).expect("a field");
    window
        .view
        .entered(item, "other", &window.layout, &mut Region::new());
    assert_eq!(window.names(), ["raw", "pic"]);
    assert!(opened(window.act(Action::EditEntry)).is_none());
}

#[test]
fn a_copy_takes_a_free_name_beside_its_original() {
    let mut window = Window::of(&["leaf", "tree"]);
    window.act(Action::DuplicateEntry);
    assert_eq!(window.names(), ["leaf", "leaf1", "tree"]);
    assert_eq!(window.view.chosen(), [window.id(1)], "the copy is chosen");
}

#[test]
fn moving_a_sprite_carries_its_identity() {
    let mut window = Window::of(&["a", "b", "c"]);
    let first = window.id(0);
    window.act(Action::EntryDown);
    assert_eq!(window.names(), ["b", "a", "c"]);
    assert_eq!(window.id(1), first);
}

#[test]
fn thumbnails_are_asked_for_one_job_at_a_time_and_land_on_their_sprites() {
    let mut window = Window::of(&["a", "b"]);
    let Some(HostRequest::Own(Own::Compute { job, work })) = window.view.wanted() else {
        panic!("thumbnails asked for");
    };
    assert!(window.view.wanted().is_none(), "one job at a time");
    let Compute::Thumbnails { side, ref pictures } = work else {
        panic!("a thumbnail job");
    };
    assert_eq!(side, window.layout.thumb());
    assert_eq!(pictures.len(), 2);
    let answer = compute(work);
    let outcome = window
        .view
        .computed(job, answer, &window.layout, &mut Region::new());
    assert!(outcome.request.is_none(), "nothing left to draw");
    let thumb = window
        .view
        .thumbnail(window.id(0), window.layout.thumb())
        .expect("drawn");
    assert_eq!((thumb.width(), thumb.height()), (side, side));
    assert!(window.view.wanted().is_none());
}

#[test]
fn an_answer_to_a_job_given_up_lands_nowhere() {
    let mut window = Window::of(&["a"]);
    let _ = window.view.wanted();
    let stale = Computed::Thumbnails {
        side: 4,
        drawn: alloc::vec![(window.id(0), 0, Some(alloc::vec![0; 64]))],
    };
    window
        .view
        .computed(99, stale, &window.layout, &mut Region::new());
    assert!(window
        .view
        .thumbnail(window.id(0), window.layout.thumb())
        .is_none());
}

#[test]
fn an_edit_handed_back_keeps_the_files_name_and_is_drawn_again() {
    let mut window = Window::of(&["a", "b"]);
    let Some(HostRequest::Own(Own::Compute { job, work })) = window.view.wanted() else {
        panic!("thumbnails asked for");
    };
    let answer = compute(work);
    window
        .view
        .computed(job, answer, &window.layout, &mut Region::new());
    let Entry::Picture(edited) = sprite("stale", [0, 200, 0, 255]) else {
        unreachable!("a picture");
    };
    let id = window.id(1);
    let outcome = window
        .view
        .adopt_edited(id, edited, &window.layout, &mut Region::new());
    assert_eq!(window.names(), ["a", "b"], "the file's name stands");
    assert!(window.view.is_modified());
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "no step of the file's"
    );
    let Some(HostRequest::Own(Own::Compute {
        work: Compute::Thumbnails { pictures, .. },
        ..
    })) = outcome.request
    else {
        panic!("its thumbnail is drawn again");
    };
    assert_eq!(
        pictures.iter().map(|(id, ..)| *id).collect::<Vec<_>>(),
        [id]
    );
}

#[test]
fn cells_reflow_to_the_window_and_scroll_when_they_overflow() {
    let names: Vec<String> = (0..40).map(|i| alloc::format!("s{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let window = Window::of(&refs);
    assert!(!window.layout.bar().is_empty(), "forty sprites overflow");
    let first = window.layout.cell_rect(0, 0);
    let second = window.layout.cell_rect(1, 0);
    assert!(second.left() > first.left() && second.top() == first.top());
    let at = Point::new(second.left() + 2, second.top() + 2);
    assert_eq!(window.layout.index_at(at, 0, 40), Some(1));
    let few = Window::of(&["a"]);
    assert!(few.layout.bar().is_empty(), "one sprite shows whole");
}

#[test]
fn closing_asks_only_when_the_file_changed() {
    let mut window = Window::of(&["a", "b"]);
    let close = |window: &mut Window| {
        window
            .view
            .close_requested(&window.layout, &mut Region::new())
            .request
    };
    assert!(matches!(close(&mut window), Some(HostRequest::Close)));
    window.act(Action::EntryDown);
    assert!(matches!(close(&mut window), Some(HostRequest::AskToClose)));
}

#[test]
fn the_menu_offers_what_the_choice_allows() {
    let window = Window::of(&["a", "b"]);
    let menu = window.view.menu(MenuKind::Window);
    let enabled = |wanted: Action| {
        menu.rows().any(|(row, _)| match row {
            AppMenuRowView::Item(item) => item.id.get() == wanted.id() && item.enabled,
            _ => false,
        })
    };
    assert!(enabled(Action::EditEntry) && enabled(Action::DuplicateEntry));
    assert!(
        enabled(Action::EntryDown) && !enabled(Action::EntryUp),
        "the first sprite"
    );
    assert!(!enabled(Action::Undo), "nothing to undo yet");
}

/// A sprite added where the view already showed as many asks for its
/// thumbnail, as does a new sprite's form offering a free name.
#[test]
fn a_sprite_added_is_drawn_and_a_new_one_is_offered_a_free_name() {
    let names: Vec<String> = (0..40).map(|i| alloc::format!("s{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut window = Window::of(&refs);
    while let Some(HostRequest::Own(Own::Compute { job, work })) = window.view.wanted() {
        let answer = compute(work);
        window
            .view
            .computed(job, answer, &window.layout, &mut Region::new());
    }
    assert!(
        window.view.wanted().is_none(),
        "every sprite near the view is drawn"
    );
    window.act(Action::DuplicateEntry);
    let copy = window.id(1);
    let Some(HostRequest::Own(Own::Compute {
        work: Compute::Thumbnails { pictures, .. },
        ..
    })) = window.view.wanted()
    else {
        panic!("the copy is drawn");
    };
    assert!(pictures.iter().any(|(id, ..)| *id == copy));
    window.act(Action::NewEntry);
    let form = window.view.modal().expect("the new sprite form shows");
    let offered = form.new_sprite_answer().expect("the offer stands");
    assert_eq!(offered.name.as_bytes(), b"sprite");
}

/// Draw every thumbnail `window` asks for, on this thread.
fn draw_thumbnails(window: &mut Window) {
    while let Some(HostRequest::Own(Own::Compute { job, work })) = window.view.wanted() {
        let answer = compute(work);
        window
            .view
            .computed(job, answer, &window.layout, &mut Region::new());
    }
}

#[test]
fn the_new_sprite_form_is_drawn_where_its_dialog_shows_it() {
    let mut window = Window::of(&["a"]);
    window.act(Action::NewEntry);
    let theme = window.registry.active();
    let dialog = window
        .view
        .dialog_window(&window.layout, Scale::ONE, theme)
        .expect("its dialog");
    let rect = dialog.rect;
    let right = u32::try_from(rect.right()).expect("on the drawing");
    let bottom = u32::try_from(rect.bottom()).expect("on the drawing");
    let mut surface = Surface::new(right.max(WINDOW.0), bottom).expect("a surface");
    render_area(
        &mut surface,
        &window.view,
        &window.layout,
        theme,
        Faces::of(theme, Scale::ONE),
        Scale::ONE,
    );
    let (left, top) = (
        u32::try_from(rect.left()).expect("on the drawing"),
        u32::try_from(rect.top()).expect("on the drawing"),
    );
    let drawn =
        (top..bottom).any(|y| (left..right).any(|x| surface.get(x, y) != Some(Pixel::TRANSPARENT)));
    assert!(
        drawn,
        "the form is drawn into the dialog's part of the drawing"
    );
}

#[test]
fn a_new_sprite_confirmed_is_laid_out_and_opened() {
    let mut window = Window::of(&["a"]);
    window.act(Action::NewEntry);
    let theme = window.registry.active();
    let outcome = window.view.dialog_input(
        &InputEvent::KeyPressed {
            key: Key::Named(NamedKey::Enter),
            modifiers: Modifiers::default(),
        },
        window.now,
        &window.layout,
        (Scale::ONE, theme),
        &mut Region::new(),
    );
    assert_eq!(outcome.relayout, Relayout::Whole, "its cell is laid out");
    assert!(opened(outcome).is_some());
    assert_eq!(window.names(), ["a", "sprite"]);
}

#[test]
fn a_choice_of_sprites_opens_no_more_than_so_many_and_says_so_once() {
    let mut opening = SpriteOpening::of(SPRITES_AT_ONCE + 4);
    let mut opened = 0;
    while opening.goes_on() {
        opening.opened();
        opened += 1;
    }
    assert_eq!(opened, SPRITES_AT_ONCE);
    assert_eq!(
        opening.said().as_deref(),
        Some("16 sprites open at once: 4 more chosen stay shut")
    );
    let mut refused = SpriteOpening::of(5);
    refused.opened();
    refused.refused();
    assert!(!refused.goes_on(), "none after the desktop refuses one");
    assert_eq!(refused.said(), None, "the desktop's alert said why");
    let mut short = SpriteOpening::of(3);
    short.opened();
    short.short();
    short.short();
    assert_eq!(
        short.said().as_deref(),
        Some("There is not enough memory to open 2 sprites")
    );
    let mut all = SpriteOpening::of(2);
    all.opened();
    all.opened();
    assert_eq!(all.said(), None);
}

#[test]
fn a_thumbnail_of_another_side_waits_for_its_own() {
    let mut window = Window::of(&["a"]);
    draw_thumbnails(&mut window);
    let id = window.id(0);
    let side = window.layout.thumb();
    assert!(window.view.thumbnail(id, side).is_some());
    let theme = window.registry.active();
    let double = Scale::from_percent(200).expect("a scale");
    let layout = window
        .view
        .layout(WINDOW.0, WINDOW.1, theme, double, Faces::of(theme, double));
    window.view.settle(&layout, &mut Region::new());
    assert_ne!(layout.thumb(), side);
    assert!(
        window.view.thumbnail(id, layout.thumb()).is_none(),
        "the old side is not drawn at the new"
    );
    assert!(window.view.wanted().is_some(), "its own is asked for");
}

#[test]
fn a_new_sprite_file_opens_its_sprites_once() {
    let mut view = AreaView::new(
        area(&["a", "b"]),
        String::from("Untitled"),
        None,
        Access::Untitled,
        DOUBLE,
    )
    .opening_its_sprites();
    assert_eq!(view.take_pending().len(), 2);
    assert!(view.take_pending().is_empty());
}

#[test]
fn its_sprite_windows_follow_a_change_a_save_and_a_rename() {
    let mut window = Window::of(&["a"]);
    assert!(window.view.members_moved(), "asked first");
    assert!(!window.view.members_moved());
    window.act(Action::DuplicateEntry);
    assert!(window.view.members_moved(), "changed");
    let generation = window.view.document().generation();
    SavedDocument::saved(&mut window.view, generation);
    assert!(window.view.members_moved(), "saved, it holds no changes");
    SavedDocument::rename(&mut window.view, String::from("Leaves"));
    assert!(window.view.members_moved(), "renamed");
    assert!(!window.view.members_moved());
}

#[test]
fn what_the_window_said_goes_at_the_next_act() {
    let mut window = Window::of(&["a", "b"]);
    window.act(Action::Deselect);
    window.act(Action::EntryUp);
    assert_eq!(window.view.message(), Some("Choose one sprite to move"));
    window.press_cell(1, Modifiers::default());
    assert_eq!(window.view.message(), None);
    window.act(Action::Deselect);
    window.act(Action::EntryUp);
    window.key(Key::Named(NamedKey::Right), Modifiers::default());
    assert_eq!(window.view.message(), None);
}

#[test]
fn a_sprite_file_opens_with_room_for_its_sprites_and_its_band() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    // Text set large, as a theme may set it: twice the reference size.
    let faces = Faces::of(theme, Scale::from_percent(200).expect("a scale"));
    let most = (OPEN_CELLS.0 * OPEN_CELLS.1) as usize;
    let names: Vec<String> = (0..most).map(|i| alloc::format!("s{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let view = AreaView::new(
        area(&refs),
        String::from("Sprites"),
        None,
        Access::Writable,
        DOUBLE,
    );
    let (width, height) = AreaView::open_size(most, theme, faces);
    let layout = view.layout(width, height, theme, Scale::ONE, faces);
    assert!(layout.bar().is_empty(), "every sprite shows");
    assert_eq!(layout.shown(0, most), 0..most);
    let last = layout.cell_rect(most - 1, 0);
    assert!(last.bottom() <= layout.status().top(), "above its band");
}
