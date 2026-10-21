//! The bar's menu *subjects*: what each of its four menus offers, and how a
//! chosen row is read back (`plans/NEW-MENUS.md` M3.4).
//!
//! The bar describes and the desktop's one menu service decides. Each builder
//! here answers with a [`MenuRequest`] — a [`ChainModel`], where it hangs, and
//! which menu it is — and the inverse reads a chosen row back into the typed
//! [`TaskbarResponse`] the embedder carries out
//! ([`Taskbar::menu_chosen`](crate::Taskbar::menu_chosen)). The plate, the
//! title band, the placement, the grab, keyboard traversal and dismissal are
//! the chain's; nothing here draws a menu pixel or keeps a menu shell.
//!
//! Rows are built from one ordered list per subject and read back against that
//! same list, so a reordering cannot re-map what a row does. A row's id is its
//! *command's* position in that list rather than its position on the plate, so
//! a menu that leaves a row out — the system menu without *Switch User…* —
//! shifts no other row's meaning.
//!
//! An application's own menu is the one subject whose rows are not the bar's:
//! the application declared them over the window channel, and
//! [`ChainModel::from_app_menu`] decodes exactly what it declared. The one row
//! the bar owns inside such a menu is the information row, whose child is the
//! desktop-drawn panel of the bundle's **signed** manifest, so an application
//! cannot state an identity that is not its own.

use alloc::string::String;
use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, AppMenuSymbol};
use tairix_abi::ProcId;

use tairix_controls::{
    AboutSheet, ChainModel, ChainRow, Fact, FactList, MenuItem, PlatePlacement, PlateSide, RowIcon,
    INFO_ROW_LABEL,
};
use tairix_geometry::Rect;
use tairix_icon::IconKind;
use tairix_proglib::{Edit, EntryId};

use crate::apps::AppIdentity;
use crate::clock_menu::{self, ClockPermits};
use crate::edge::Edge;
use crate::input::TaskbarResponse;
use crate::network_menu;
use crate::system::{self, SystemPermits};

/// One row a bar menu's own table produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BarMenuRow {
    /// The row's position in its table, which numbers its id.
    pub index: usize,
    /// The function the row's picture shows.
    pub symbol: AppMenuSymbol,
    /// What the row draws.
    pub item: MenuItem,
    /// Why it cannot be chosen: the seat's tooltip text, shown on dwell —
    /// never drawn beside the label, which is what used to make a plate as
    /// wide as its longest excuse.
    pub why: Option<&'static str>,
}

/// Which of the bar's menus a chain belongs to, and what a chosen row of it
/// acts on.
///
/// The address the desktop answers the chain at, and the least that answer
/// needs: the rows the plate was built from belong to the chain, and the bar
/// keeps no copy of them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuSubject {
    /// The menu an application declared. A chosen row is the application's
    /// own id, relayed straight back to the process that declared it — named
    /// by its identity, never by a strip place another application may have
    /// taken by the time the row is chosen.
    App {
        /// The process that declared the menu.
        owner: ProcId,
    },
    /// The context menu on this program-library entry's row.
    Entry {
        /// The catalog entry the row names.
        entry: EntryId,
    },
    /// The desktop's system quick actions.
    System,
    /// The bar's clock.
    Clock,
    /// The network signal.
    Network,
}

impl MenuSubject {
    /// What choosing the row `item` of this menu asks the embedder for, or
    /// `None` for an id the menu never declared (fail closed — never a
    /// guessed command).
    ///
    /// The bar interprets no application row: it hands the id straight back to
    /// the process that declared it.
    #[must_use]
    pub(crate) fn chosen(&self, item: AppMenuItemId) -> Option<TaskbarResponse> {
        match self {
            Self::App { owner } => Some(TaskbarResponse::AppMenuChosen {
                owner: *owner,
                item,
            }),
            Self::Entry { entry } => Some(EntryRow::at(item.index())?.response(entry)),
            Self::System => system::response_at(item.index()),
            Self::Clock => clock_menu::response_at(item.index()),
            Self::Network => network_menu::response_at(item.index()),
        }
    }

    /// Whether choosing row `item` of this menu closes the program-library
    /// popup.
    ///
    /// Only an entry row that acts somewhere the popup would stand between the
    /// user and the result does — launching, or putting a shortcut on the
    /// desktop. Keeping or arranging a favourite changes the popup itself,
    /// which stays open to show it.
    #[must_use]
    pub(crate) fn closes_library(&self, item: AppMenuItemId) -> bool {
        matches!(self, Self::Entry { .. })
            && EntryRow::at(item.index()).is_some_and(EntryRow::acts_elsewhere)
    }
}

/// What the bar asks the desktop to open: which menu, its rows, and where it
/// hangs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuRequest {
    /// Which menu, and what a chosen row acts on.
    pub subject: MenuSubject,
    /// The rows to draw, their ids, and the root plate's title.
    pub model: ChainModel,
    /// The slot or row the press landed on, and the side the plate opens on.
    pub placement: PlatePlacement,
}

/// One row of a program-library entry's context menu.
///
/// What the launcher can do to a program that its own click cannot. The bar
/// performs none of it — each becomes a typed response the session carries out
/// under its own authority.
///
/// Public because aiming *at* a row is the same fact as reading one back: an
/// embedder's own test, or a QEMU pointer script, finds the row by the label
/// [`EntryRow::label`] gives it rather than by restating its position.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EntryRow {
    /// Launch the entry's bundle.
    Open,
    /// Launch it as an account that may do more, once that account's
    /// password is given at the console's authorisation prompt.
    RunAsAdministrator,
    /// Keep it among the account's favourites.
    Favourite,
    /// Stop keeping it among the favourites.
    Unfavourite,
    /// Move a favourite one place earlier.
    MoveEarlier,
    /// Move a favourite one place later.
    MoveLater,
    /// Forget how it has been used, so it leaves the frequently used.
    Forget,
    /// Forget how every program has been used.
    ForgetAll,
    /// Create a symbolic link to the entry's bundle in the user's own
    /// `Desktop` folder.
    Shortcut,
}

/// Every row a program-library entry's context menu may offer, in row order.
///
/// The one definition of that menu: a row's id is its place here, so a menu
/// that leaves rows out — a program that is not a favourite has nothing to
/// move — shifts no other row's meaning, and a chosen id reads back through
/// this same list.
const ENTRY_ROWS: [EntryRow; 9] = [
    EntryRow::Open,
    EntryRow::RunAsAdministrator,
    EntryRow::Favourite,
    EntryRow::Unfavourite,
    EntryRow::MoveEarlier,
    EntryRow::MoveLater,
    EntryRow::Forget,
    EntryRow::ForgetAll,
    EntryRow::Shortcut,
];

/// What a program-library entry's menu offers depends on: the account's own
/// record of the entry, and whether the console can authorise an account.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EntryState {
    /// Where the entry stands among the favourites, if it is one.
    pub(crate) favourite: Option<Neighbours>,
    /// The account has a record of using it.
    pub(crate) used: bool,
    /// The console has an elevation broker to authorise an account.
    pub(crate) elevation: bool,
}

/// Whether a favourite has others before and after it, so it can move.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Neighbours {
    /// A favourite stands before it.
    pub(crate) earlier: bool,
    /// A favourite stands after it.
    pub(crate) later: bool,
}

impl EntryRow {
    /// The label this row draws with.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::RunAsAdministrator => "Run as Administrator\u{2026}",
            Self::Favourite => "Add to Favourites",
            Self::Unfavourite => "Remove from Favourites",
            Self::MoveEarlier => "Move Earlier",
            Self::MoveLater => "Move Later",
            Self::Forget => "Remove from Frequently Used",
            Self::ForgetAll => "Clear Frequently Used",
            Self::Shortcut => "Create Desktop Shortcut",
        }
    }

    /// The function this row's picture shows.
    const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::Open => AppMenuSymbol::Open,
            Self::RunAsAdministrator => AppMenuSymbol::RunAsAdministrator,
            Self::Favourite => AppMenuSymbol::Favourite,
            Self::Unfavourite | Self::Forget => AppMenuSymbol::Remove,
            Self::MoveEarlier => AppMenuSymbol::MoveUp,
            Self::MoveLater => AppMenuSymbol::MoveDown,
            Self::ForgetAll => AppMenuSymbol::Clear,
            Self::Shortcut => AppMenuSymbol::Shortcut,
        }
    }

    /// Whether an entry in `state` offers this row.
    const fn offered(self, state: EntryState) -> bool {
        match (self, state.favourite) {
            (Self::Open | Self::Shortcut, _) => true,
            (Self::RunAsAdministrator, _) => state.elevation,
            (Self::Favourite, place) => place.is_none(),
            (Self::Unfavourite, place) => place.is_some(),
            (Self::MoveEarlier, Some(place)) => place.earlier,
            (Self::MoveLater, Some(place)) => place.later,
            (Self::MoveEarlier | Self::MoveLater, None) => false,
            (Self::Forget | Self::ForgetAll, _) => state.used,
        }
    }

    /// Whether the row acts somewhere the launcher would stand in front of, so
    /// choosing it closes the launcher.
    const fn acts_elsewhere(self) -> bool {
        matches!(self, Self::Open | Self::RunAsAdministrator | Self::Shortcut)
    }

    /// Whether the row starts the next group on its plate.
    const fn begins_group(self) -> bool {
        matches!(self, Self::Favourite | Self::Unfavourite | Self::Shortcut)
    }

    /// What choosing this row on `entry` asks the embedder for.
    fn response(self, entry: &EntryId) -> TaskbarResponse {
        let entry = entry.clone();
        match self {
            Self::Open => TaskbarResponse::LibraryLaunch { entry },
            Self::RunAsAdministrator => TaskbarResponse::LibraryLaunchElevated { entry },
            Self::Favourite => TaskbarResponse::LibraryEdit(Edit::Favourite(entry)),
            Self::Unfavourite => TaskbarResponse::LibraryEdit(Edit::Unfavourite(entry)),
            Self::MoveEarlier => TaskbarResponse::LibraryEdit(Edit::MoveEarlier(entry)),
            Self::MoveLater => TaskbarResponse::LibraryEdit(Edit::MoveLater(entry)),
            Self::Forget => TaskbarResponse::LibraryEdit(Edit::Forget(entry)),
            Self::ForgetAll => TaskbarResponse::LibraryEdit(Edit::ForgetAll),
            Self::Shortcut => TaskbarResponse::CreateDesktopShortcut { entry },
        }
    }

    /// The row at `index` of the menu, or `None` for an index it never drew.
    fn at(index: usize) -> Option<Self> {
        ENTRY_ROWS.get(index).copied()
    }
}

/// The model a program-library entry's context menu opens with, titled with
/// the entry's name, offering what `state` allows, and closing with the *Info*
/// sheet of its signed manifest where `identity` is known.
#[must_use]
pub(crate) fn entry_menu(
    name: &str,
    state: EntryState,
    identity: Option<&AppIdentity>,
    bundle: &str,
) -> ChainModel {
    let mut model = ChainModel::new(name);
    for (index, row) in ENTRY_ROWS.into_iter().enumerate() {
        if !row.offered(state) {
            continue;
        }
        if let Some(id) = AppMenuItemId::for_index(index) {
            let item = MenuItem::new(row.label()).with_group_break(row.begins_group());
            model.push(ChainRow::item(id, row.symbol(), item));
        }
    }
    if let Some(identity) = identity {
        model.push(ChainRow::info(
            MenuItem::new(INFO_ROW_LABEL).with_group_break(true),
            about_sheet(identity),
        ));
        model.set_info_source(&RowIcon::Bundle(String::from(bundle)));
    }
    model
}

/// The model the menu the application `name` declared opens with, titled from
/// its **signed** manifest and stating `identity` in its information panel.
///
/// The wire model decodes into the one service model, so what the plate draws
/// is exactly what the application declared — and nothing it declared can
/// claim the system lacks authority for a row, because the wire carries no
/// field for that.
#[must_use]
pub(crate) fn app_menu(name: &str, declared: &AppMenu, identity: &AppIdentity) -> ChainModel {
    ChainModel::from_app_menu(name, declared, Some(&about_sheet(identity)))
}

/// The title the desktop's system quick-actions plate carries.
const SYSTEM_TITLE: &str = "System";

/// The model the system quick-actions menu opens with, for what `permits`
/// offers.
#[must_use]
pub(crate) fn system_menu(permits: SystemPermits) -> ChainModel {
    rows_from(SYSTEM_TITLE, system::rows(permits))
}

/// The title the clock's plate carries.
const CLOCK_TITLE: &str = "Clock";

/// The model the clock's menu opens with, for what `permits` offers.
#[must_use]
pub(crate) fn clock_menu(permits: &ClockPermits) -> ChainModel {
    rows_from(CLOCK_TITLE, clock_menu::rows(permits))
}

/// The title the network menu's plate carries.
const NETWORK_TITLE: &str = "Network";

/// The model the network signal's menu opens with.
#[must_use]
pub(crate) fn network_menu(settings_installed: bool) -> ChainModel {
    rows_from(NETWORK_TITLE, network_menu::rows(settings_installed))
}

/// A flat model titled `title` from rows a subject's own table produced, each
/// carrying the id of the command's position in that table.
///
/// The one place the bar's three own menus turn a table position into a row
/// id, so the numbering and [`MenuSubject::chosen`]'s inverse cannot drift.
fn rows_from(title: &str, rows: alloc::vec::Vec<BarMenuRow>) -> ChainModel {
    let mut model = ChainModel::new(title);
    for BarMenuRow {
        index,
        symbol,
        item,
        why,
    } in rows
    {
        let Some(id) = AppMenuItemId::for_index(index) else {
            continue;
        };
        let row = ChainRow::item(id, symbol, item);
        model.push(match why {
            Some(why) => row.explained(why),
            None => row,
        });
    }
    model
}

/// Where a menu anchored at `anchor` opens for a bar on `edge`, with `gap`
/// pixels of clearance.
///
/// A bar's own menu opens away from the bar, so the plate never covers the
/// slot the user pressed; which side it lands on when that would leave the
/// screen is the shared placement rule's answer, not a second one here.
#[must_use]
pub(crate) const fn placement(anchor: Rect, edge: Edge, gap: u32) -> PlatePlacement {
    PlatePlacement {
        anchor,
        side: match edge {
            Edge::Bottom => PlateSide::Above,
            Edge::Top => PlateSide::Below,
            Edge::Left => PlateSide::Trailing,
            Edge::Right => PlateSide::Leading,
        },
        gap,
    }
}

/// The sheet an application's *Info* row opens: its name and version always
/// (a signed manifest carries both), and its purpose and author only where the
/// manifest states them rather than as blank lines. Its picture is the
/// session's to give, at the sheet's own side
/// ([`ChainModel::set_info_artwork`]).
#[must_use]
pub fn about_sheet(identity: &AppIdentity) -> AboutSheet {
    let facts: alloc::vec::Vec<Fact> = identity
        .author
        .iter()
        .map(|author| Fact::new("Author", author.clone()))
        .collect();
    AboutSheet::new(
        identity.name.clone(),
        identity.version.clone(),
        identity.purpose.clone(),
        FactList::new(facts).with_separators(true),
        IconKind::AppBundle,
    )
}
