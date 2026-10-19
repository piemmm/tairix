//! Each walk names exactly the pictures its paint asks for, at the sides it
//! asks for them and inside the window: what a landing repaints is then where
//! the paint drew, and nothing drawn is missed.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::blkio::{BlkMedium, BlkTransport};
use tairix_abi::fs::FileId;
use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::time::Time64;
use tairix_abi::Errno;
use tairix_controls::text::TextField;
use tairix_geometry::{Rect, Scale};
use tairix_icon::{IconArtwork, IconKind, IconPicture, IconRequest};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::{chooser_art, manager_art, properties_art, ShownArt};
use crate::browser::Browser;
use crate::chrome::{ManagerToolModel, ToolbarBand, MANAGER_TOOLS};
use crate::column::ScrollColumn;
use crate::entry::{Entry, EntryKind};
use crate::layout::ViewMode;
use crate::open_with::{AppAssociation, OpenWithChooser};
use crate::places::{Places, Volume, VolumeFacts};
use crate::render::{
    draw_open_with_chooser, draw_properties_window, render_into, Identity, ManagerChrome,
    PermsPanel, PropertiesControls, PropertiesFrame, PropertiesView,
};
use crate::source::{DirectorySource, Listing};

/// A source every folder of which lists the same entries.
struct Fixed(Vec<Entry>);

impl DirectorySource for Fixed {
    fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
        Ok(Listing::Ready(self.0.clone()))
    }
}

/// Records each picture a paint asks for, answering none.
#[derive(Default)]
struct Asked(Vec<(String, u32)>);

impl IconArtwork for Asked {
    fn artwork(&mut self, request: IconRequest<'_>, side: u32) -> Option<IconPicture<'_>> {
        self.0.push((format!("{request:?}"), side));
        None
    }
}

/// Each picture a walk names, checked to lie inside `window`.
fn walked(window: Rect, walk: impl FnOnce(&mut dyn FnMut(ShownArt<'_>))) -> Vec<(String, u32)> {
    let mut named = Vec::new();
    walk(&mut |art: ShownArt<'_>| {
        assert!(!art.at.is_empty(), "a picture was named where none shows");
        assert_eq!(art.at.intersection(&window), art.at, "outside the window");
        named.push((format!("{:?}", art.request), art.side));
    });
    named
}

/// A folder of every kind of entry a picture is drawn for: bundles, pictures,
/// folders, plain files.
fn entries() -> Vec<Entry> {
    let id = FileId {
        volume: [3; 16],
        node: 9,
    };
    let mut entries: Vec<Entry> = (0..12)
        .map(|n| {
            Entry::new(
                format!("App {n:02}.app"),
                EntryKind::Bundle,
                0,
                Time64::UNIX_EPOCH,
            )
            .with_id(id)
        })
        .collect();
    entries.extend((0..6).map(|n| {
        Entry::new(
            format!("photo-{n}.png"),
            EntryKind::File,
            512,
            Time64::UNIX_EPOCH,
        )
        .with_id(id)
    }));
    entries.extend((0..4).map(|n| Entry::directory(format!("Folder {n}"))));
    entries.push(Entry::new(
        "notes.txt",
        EntryKind::File,
        3,
        Time64::UNIX_EPOCH,
    ));
    entries
}

fn places() -> Places {
    Places::new(
        &["Users".to_string(), "ann".to_string()],
        &[Volume {
            label: String::from("Backup"),
            target: String::from("/Storage/Backup"),
            medium: Some(BlkMedium::of_transport(BlkTransport::Nvme)),
            facts: VolumeFacts {
                filesystem: String::from("arxfs"),
                free: None,
                availability: MountAvailability::Available,
            },
        }],
    )
}

/// The manager window's walk names what its paint asks for, in every view and
/// in a window too short to show every entry.
#[test]
fn the_manager_walk_names_what_its_paint_asks_for() {
    let theme = Theme::dark();
    let places = places();
    let chrome = ManagerChrome {
        tools: MANAGER_TOOLS,
        tool_model: ManagerToolModel::none(),
        sidebar: Some(&places),
        toolbar: ToolbarBand::Shown,
        renaming: None,
    };
    for view in [ViewMode::LargeIcons, ViewMode::SmallIcons, ViewMode::List] {
        for window in [Rect::new(0, 0, 640, 480), Rect::new(0, 0, 360, 220)] {
            let mut browser = Browser::open_root(Fixed(entries())).expect("the root lists");
            browser.set_view_mode(view);
            let mut painted = Asked::default();
            let mut surface = Surface::new(window.width, window.height).expect("surface");
            render_into(
                &mut surface,
                &browser,
                Scale::ONE,
                &theme,
                window,
                &chrome,
                &mut painted,
            );

            let named = walked(window, |visit| {
                manager_art(&browser, (Scale::ONE, &theme), window, &chrome, visit);
            });
            assert!(
                named.len() > places.len(),
                "{view:?} {window:?} named too little"
            );
            assert_eq!(named, painted.0, "{view:?} in {window:?}");
        }
    }
}

/// The Properties window's walk names its identity's picture, as its paint
/// asks for it.
#[test]
fn the_properties_walk_names_what_its_paint_asks_for() {
    let theme = Theme::dark();
    let window = Rect::new(0, 0, 420, 520);
    let bar = ScrollColumn::new();
    let editor = TextField::new();
    let perms = PermsPanel::default();
    let controls = PropertiesControls {
        identity: Identity {
            name: "One.app",
            detail: "Application",
            art: IconRequest::bundle(IconKind::AppBundle, "/Storage/B/One.app"),
        },
        can_chown: false,
        owner: None,
        attribute: &editor,
        scrollbar: bar.scrollbar(),
        perms: &perms,
        drive: None,
    };
    let mut painted = Asked::default();
    let mut surface = Surface::new(window.width, window.height).expect("surface");
    draw_properties_window(
        &mut surface,
        PropertiesFrame::Reading,
        PropertiesView::default(),
        controls,
        Scale::ONE,
        &theme,
        window,
        &mut painted,
    );

    let named = walked(window, |visit| {
        properties_art(&controls.identity, (Scale::ONE, &theme), window, visit);
    });
    assert_eq!(named.len(), 1);
    assert_eq!(named, painted.0);
}

/// The chooser's walk names the file's type and every candidate row it shows,
/// as its paint asks for them, in a popup too short to show them all.
#[test]
fn the_chooser_walk_names_what_its_paint_asks_for() {
    let theme = Theme::dark();
    let apps: Vec<AppAssociation> = (0..9)
        .map(|n| {
            AppAssociation::new(
                format!("Viewer {n}"),
                format!("/System/Applications/Viewer{n}.app"),
                vec![String::from("image/png")],
            )
        })
        .collect();
    let offered: Vec<&AppAssociation> = apps.iter().collect();
    let chooser =
        OpenWithChooser::new(&offered, "/Users/ann/Home/a.png", "a.png").expect("candidates");
    for viewport in [Rect::new(0, 0, 420, 480), Rect::new(0, 0, 420, 220)] {
        let mut painted = Asked::default();
        let mut surface = Surface::new(viewport.width, viewport.height).expect("surface");
        draw_open_with_chooser(
            &mut surface,
            &chooser,
            Scale::ONE,
            &theme,
            viewport,
            &mut painted,
        );

        let named = walked(viewport, |visit| {
            chooser_art(&chooser, (Scale::ONE, &theme), viewport, visit);
        });
        assert!(named.len() > 1, "{viewport:?} named no candidate");
        assert_eq!(named, painted.0, "in {viewport:?}");
    }
}
