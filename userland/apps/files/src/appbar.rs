//! The file manager's icon-bar declaration: what its slot on the desktop's
//! bar offers.
//!
//! # Why this is its own module
//!
//! The `Run` binary around it is a freestanding program — it only exists when
//! the crate is built for a bare-metal target — so nothing inside it can be
//! reached by a host test. What the slot offers is worth testing: which places
//! become rows, in what order, which are refused, what happens at the row cap,
//! and how a chosen row maps back to the place it names. All of that is a pure
//! function of the [`Places`] model, so it lives here, compiles on the host,
//! and is covered by the tests beside it.
//!
//! # A component's declaration
//!
//! The file manager is a component of the desktop, and its slot is the user's
//! way *into* the filesystem rather than a handle on one window. So it offers
//! neither of the shared convention's rows — a component states no identity
//! panel of its own, and the desktop's file manager never quits — and instead
//! lists the places the browser's own rail lists: the user's `Home` and `Desktop` folders, then the storage
//! attached right now, each beside the picture the rail draws it with — the
//! house, a folder, or the drive a volume is on. Choosing one opens a window
//! there, and a primary click on the slot opens one among the user's own files
//! ([`DESKTOP_SLOT_CLICK`]). A volume the user may eject — a removable
//! device's, or a disk image or share they attached — opens a plate holding
//! *Eject*, and one they may retitle — a disk image of theirs, or a device's
//! volume, whose format takes a title — holds *Title*, whose field opens on the
//! title as it stands. *Network*, which opens a
//! window browsing the storage the network offers, follows beneath a rule,
//! and *Trash* comes last beneath a rule of its own: it opens the user's
//! Trash, and with no home to keep one in it says so.
//!
//! # The row cap is a display cap, and it is stated
//!
//! An icon-bar menu holds at most [`APP_MENU_MAX_ROWS`] rows on a plate, and
//! only so much text in all, which the fixed places and a busy machine's
//! volumes can exceed. A place that does not fit is **dropped** whole — its
//! row with its plate, and never at the closing rows' cost — and how many is
//! reported to the caller so it can say so: a truncated list that claims to
//! be the whole one would be worse than a short one that admits it.
//!
//! # An answer names the declaration it answers
//!
//! A menu is copied as it opens, so it may be answered after the places were
//! declared afresh. Each declaration is made under a revision of its own, and
//! every answer names the one its menu was copied from; [`Declarations`]
//! keeps what the rows of the last few the session accepted stood for, so an
//! answer acts on what its own menu showed, and one naming a declaration no
//! longer held is refused saying why.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::{VOLUME_ID_LEN, VOLUME_ID_UNPUBLISHED};
use tairix_abi::window_ipc::{
    AppBar, AppBarClick, AppMenu, AppMenuEntry, AppMenuEntryText, AppMenuItem, AppMenuItemId,
    AppMenuLabel, AppMenuReason, AppMenuRow, AppMenuSymbol, IconAsset, APP_MENU_MAX_ROWS,
    APP_MENU_MAX_TOTAL_ROWS,
};
use tairix_abi::Errno;
use tairix_browse::chrome::REASON_NOT_EJECTABLE;
use tairix_browse::mount::{title_refusal, EjectAsked, MountAct, MountRefusal};
use tairix_browse::{ContextCommand, Place, PlaceKind, Places, Retitling};

/// What a primary click on the component's slot does.
///
/// Every click asks for a window among the user's own files rather than
/// raising whichever window is most recent. A component may hold no window at
/// all, so there is often nothing to raise; when one already shows those
/// files, it is the one brought forward.
pub const DESKTOP_SLOT_CLICK: AppBarClick = AppBarClick::Open;

/// What a row of the slot's menu, or the field one opens, names.
///
/// A row's id carries what it does and, for a place's rows, the place's own
/// index into [`Places::rows`] as it was declared, so a place that had to be
/// skipped shifts nothing; [`Declarations`] resolves the index against what
/// the declaration the answer names showed, since the places may have been
/// re-read before the menu is answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BarRow {
    /// Open a window at the place at this index.
    Place(usize),
    /// Eject the volume at this index.
    Eject(usize),
    /// The *Title* row of the volume at this index, which only opens its
    /// field.
    Title(usize),
    /// The title committed in that row's field.
    Retitled(usize),
    /// Open a window on the network.
    Network,
    /// Open a window on the user's Trash.
    Trash,
}

/// How many of the latest declarations the session accepted an answer may
/// name: one from a menu copied further back is refused rather than guessed
/// at.
pub const DECLARATIONS_HELD: usize = 4;

/// What the declarations of the slot the session accepted stood for — the
/// latest [`DECLARATIONS_HELD`] — and the revision the next is made under.
#[derive(Debug, Default)]
pub struct Declarations {
    held: VecDeque<Declared>,
    next: u32,
}

impl Declarations {
    /// The revision to make the next declaration under: one no earlier
    /// declaration was made under, whether or not the session accepted it.
    pub fn next_revision(&mut self) -> u32 {
        let revision = self.next;
        self.next = self.next.wrapping_add(1);
        revision
    }

    /// Keep what the rows of the declaration made from `places` under
    /// `revision` stand for, now the session has accepted it, letting the
    /// oldest held go.
    pub fn accepted(&mut self, revision: u32, places: &Places) {
        if self.held.len() >= DECLARATIONS_HELD {
            self.held.pop_front();
        }
        self.held.push_back(Declared::of(revision, places));
    }

    /// What choosing `item` on the menu copied from the declaration made under
    /// `revision` asks of `viewer`, the rail standing as `places` do now.
    #[must_use]
    pub fn chosen(
        &self,
        (item, revision): (AppMenuItemId, u32),
        places: &Places,
        viewer: Option<u32>,
    ) -> BarAnswer<'_> {
        match BarRow::of(item) {
            // The closing rows mean the same in every declaration.
            Some(BarRow::Network) => BarAnswer::Network,
            Some(BarRow::Trash) => BarAnswer::Trash,
            Some(BarRow::Place(index)) => match self.declared(revision) {
                Ok(declared) => declared
                    .location(index)
                    .map_or(BarAnswer::Nothing, BarAnswer::Open),
                Err(refusal) => BarAnswer::Refused(refusal),
            },
            Some(BarRow::Eject(index)) => match self
                .declared(revision)
                .and_then(|declared| declared.eject(index, places, viewer))
            {
                Ok(Some(eject)) => BarAnswer::Mount(eject),
                Ok(None) => BarAnswer::Nothing,
                Err(refusal) => BarAnswer::Refused(refusal),
            },
            // The *Title* row only opens its field.
            Some(BarRow::Title(_) | BarRow::Retitled(_)) | None => BarAnswer::Nothing,
        }
    }

    /// What the title `text`, committed in the *Title* field of the place at
    /// `index` of the declaration made under `revision`, asks of `viewer`, the
    /// rail standing as `places` do now: nothing for the title the volume
    /// records already, else its retitle.
    ///
    /// # Errors
    ///
    /// Why it is refused: the declaration is no longer held, the volume has
    /// gone, or the one check refuses it ([`MountAct::retitle`]).
    pub fn retitled(
        &self,
        (index, revision): (usize, u32),
        places: &Places,
        viewer: Option<u32>,
        text: &str,
    ) -> Result<Option<MountAct>, MountRefusal> {
        self.declared(revision)?
            .retitled(index, places, viewer, text)
    }

    /// The declaration made under `revision`, or why an answer naming it is
    /// refused.
    fn declared(&self, revision: u32) -> Result<&Declared, MountRefusal> {
        self.held
            .iter()
            .find(|declared| declared.revision == revision)
            .ok_or_else(|| MountRefusal {
                headline: String::from(OUTDATED_HEADLINE),
                reason: OUTDATED,
            })
    }
}

/// What an answer from the slot's menu asks of the file manager.
#[derive(Debug, Eq, PartialEq)]
pub enum BarAnswer<'a> {
    /// Open a window at this place.
    Open(&'a [String]),
    /// Open a window on the network.
    Network,
    /// Open a window on the user's Trash.
    Trash,
    /// Carry this eject out.
    Mount(MountAct),
    /// Say this was not done, and why.
    Refused(MountRefusal),
    /// Nothing: a row that only opens its field, or an id no declaration
    /// carried that way.
    Nothing,
}

/// What each place row of one accepted declaration stood for: its place's
/// location, name and volume, by the place's index.
///
/// An eject or a retitle acts on the volume its row showed, wherever the rail
/// lists it now, or on nothing — never on whatever slid into its index.
#[derive(Debug)]
struct Declared {
    revision: u32,
    places: Vec<Stood>,
}

/// What one place row stood for.
#[derive(Debug)]
struct Stood {
    location: Vec<String>,
    label: String,
    /// The volume it showed, by the identity a detach names it by: `None` for
    /// a folder, or a volume no detach could name.
    volume_id: Option<[u8; VOLUME_ID_LEN]>,
}

impl Declared {
    /// What the rows of the declaration made from `places` under `revision`
    /// stand for.
    fn of(revision: u32, places: &Places) -> Self {
        Self {
            revision,
            places: places
                .rows()
                .iter()
                .map(|place| Stood {
                    location: place.components().to_vec(),
                    label: String::from(place.label()),
                    volume_id: place
                        .facts()
                        .map(|facts| facts.volume_id)
                        .filter(|id| *id != VOLUME_ID_UNPUBLISHED),
                })
                .collect(),
        }
    }

    /// Where the place row `index` stood for is.
    fn location(&self, index: usize) -> Option<&[String]> {
        self.places
            .get(index)
            .map(|stood| stood.location.as_slice())
    }

    /// The volume row `index` showed, with what the row stood for.
    fn volume(&self, index: usize) -> Option<(&Stood, [u8; VOLUME_ID_LEN])> {
        let stood = self.places.get(index)?;
        Some((stood, stood.volume_id?))
    }

    /// The eject the *Eject* row at `index` asks of `viewer`: of the volume it
    /// showed, as `places` list it now.
    fn eject(
        &self,
        index: usize,
        places: &Places,
        viewer: Option<u32>,
    ) -> Result<Option<MountAct>, MountRefusal> {
        let Some((stood, volume_id)) = self.volume(index) else {
            return Ok(None);
        };
        let refused = |reason: &'static str| {
            MountAct::Eject {
                volume_id,
                name: stood.label.clone(),
                asked: EjectAsked::ByUser,
            }
            .refused(reason)
        };
        let place = listed(places, volume_id).ok_or_else(|| refused(VOLUME_GONE))?;
        MountAct::eject_row(place, viewer)
            .map(Some)
            .ok_or_else(|| refused(REASON_NOT_EJECTABLE))
    }

    /// What the title `text`, committed in the field of the *Title* row at
    /// `index`, asks of `viewer`: what committing it asks of the volume the
    /// row showed, as `places` list it now.
    fn retitled(
        &self,
        index: usize,
        places: &Places,
        viewer: Option<u32>,
        text: &str,
    ) -> Result<Option<MountAct>, MountRefusal> {
        let Some((stood, volume_id)) = self.volume(index) else {
            return Ok(None);
        };
        let facts = listed(places, volume_id)
            .and_then(Place::facts)
            .ok_or_else(|| title_refusal(&stood.label, VOLUME_GONE))?;
        MountAct::retitle(facts, &stood.label, viewer, text)
    }
}

/// The row `places` lists for the volume published as `volume_id`.
fn listed(places: &Places, volume_id: [u8; VOLUME_ID_LEN]) -> Option<&Place> {
    places.rows().iter().find(|place| {
        place
            .facts()
            .is_some_and(|facts| facts.volume_id == volume_id)
    })
}

/// What an answer naming a declaration no longer held is refused as: what
/// its rows stood for went with it, so it can name no place.
const OUTDATED_HEADLINE: &str = "What you chose on the icon bar was not done";

/// Why an answer naming a declaration no longer held is refused.
const OUTDATED: &str = "the volumes changed while the menu was open";

/// Why an eject or a title chosen on the bar was refused: the volume its row
/// showed is no longer listed.
const VOLUME_GONE: &str = "it is no longer attached";

/// How far apart the id ranges of a place's rows lie: past any index a
/// declaration can carry, since one holds at most [`APP_MENU_MAX_ROWS`]
/// places.
const VERB_STRIDE: u16 = 0x1000;

/// How many id ranges a place's rows answer in: one per [`BarRow`] verb that
/// carries an index.
const VERBS: u16 = 4;

const _: () = assert!(APP_MENU_MAX_ROWS < VERB_STRIDE as usize);
const _: () = assert!(VERBS * VERB_STRIDE < TRASH_ROW);

/// The id the *Network* row answers with: past every place's.
const NETWORK_ROW: u16 = u16::MAX;

/// The id the *Trash* row answers with: the lowest of the closing rows',
/// still clear of every place's.
const TRASH_ROW: u16 = u16::MAX - 1;

impl BarRow {
    /// What the chosen row or committed field `item` names, or `None` for an
    /// id no declaration carries (fail closed — a menu outcome is never
    /// guessed at).
    #[must_use]
    pub fn of(item: AppMenuItemId) -> Option<Self> {
        match item.get() {
            NETWORK_ROW => Some(Self::Network),
            TRASH_ROW => Some(Self::Trash),
            raw => {
                let index = usize::from(raw % VERB_STRIDE).checked_sub(1)?;
                match raw / VERB_STRIDE {
                    0 => Some(Self::Place(index)),
                    1 => Some(Self::Eject(index)),
                    2 => Some(Self::Title(index)),
                    3 => Some(Self::Retitled(index)),
                    _ => None,
                }
            }
        }
    }

    /// The id this row answers with.
    fn id(self) -> Result<AppMenuItemId, Errno> {
        let (verb, index) = match self {
            Self::Network => return AppMenuItemId::new(NETWORK_ROW),
            Self::Trash => return AppMenuItemId::new(TRASH_ROW),
            Self::Place(index) => (0, index),
            Self::Eject(index) => (1, index),
            Self::Title(index) => (2, index),
            Self::Retitled(index) => (3, index),
        };
        let index = u16::try_from(index.saturating_add(1)).map_err(|_| Errno::OutOfRange)?;
        if index >= VERB_STRIDE {
            return Err(Errno::OutOfRange);
        }
        AppMenuItemId::new(verb * VERB_STRIDE + index)
    }
}

/// The *Network* row's label.
const NETWORK_LABEL: &str = "Network";

/// The *Trash* row's label, and what a Trash that could not be opened is
/// called.
pub const TRASH_LABEL: &str = "Trash";

/// Why there is no Trash: there is no home folder to keep it in.
pub const NO_HOME: &str = "there is no home folder, so there is no Trash";

/// The *Title* row's label.
const TITLE_LABEL: &str = "Title";

/// The most rows a place's own plate holds: *Eject* and *Title*.
const PLATE_ROWS: usize = 2;

const _: () = assert!(APP_MENU_MAX_ROWS * (1 + PLATE_ROWS) <= APP_MENU_MAX_TOTAL_ROWS);

/// The *Network* row.
fn network_row() -> Result<AppMenuRow, Errno> {
    Ok(AppMenuRow::Item(AppMenuItem::new(
        BarRow::Network.id()?,
        AppMenuLabel::new(NETWORK_LABEL)?,
        AppMenuSymbol::Network,
    )))
}

/// The *Trash* row: refused saying why unless `homed`, since the Trash is
/// kept in the home.
fn trash_row(homed: bool) -> Result<AppMenuRow, Errno> {
    let row = AppMenuItem::new(
        BarRow::Trash.id()?,
        AppMenuLabel::new(TRASH_LABEL)?,
        AppMenuSymbol::Trash,
    );
    Ok(AppMenuRow::Item(if homed {
        row
    } else {
        row.with_reason(AppMenuReason::new(NO_HOME)?).disabled()
    }))
}

/// Put under `menu`'s row `parent` the plate of `place`, at `index`, as
/// `viewer` may act on it: *Eject* where they may eject it, and *Title* where
/// they may retitle it (`tairix_browse::VolumeFacts::retitling_by`), whose
/// field opens on its title with the caret at its end — greyed, saying why,
/// for one of theirs not to be retitled as it is mounted. Nothing for a place
/// with neither.
fn push_plate(
    menu: &mut AppMenu,
    parent: usize,
    (place, index): (&Place, usize),
    viewer: Option<u32>,
) -> Result<(), Errno> {
    let Some(facts) = place.facts() else {
        return Ok(());
    };
    if viewer.is_some_and(|uid| facts.ejectable_by(uid)) {
        let eject = AppMenuItem::new(
            BarRow::Eject(index).id()?,
            AppMenuLabel::new(ContextCommand::Eject.label())?,
            ContextCommand::Eject.symbol(),
        );
        menu.push_under(AppMenuRow::Item(eject), parent)?;
    }
    let retitling = facts.retitling_by(viewer);
    if matches!(retitling, Retitling::Refused(_)) {
        return Ok(());
    }
    let row = AppMenuItem::new(
        BarRow::Title(index).id()?,
        AppMenuLabel::new(TITLE_LABEL)?,
        AppMenuSymbol::Rename,
    );
    let title = match retitling.refusal() {
        Some(why) => row.with_reason(AppMenuReason::new(why)?).disabled(),
        None => row.with_entry(
            AppMenuEntry::new(
                BarRow::Retitled(index).id()?,
                AppMenuEntryText::new(&facts.title)?,
            )
            .opened_by_choosing(),
        ),
    };
    menu.push_under(AppMenuRow::Item(title), parent)?;
    Ok(())
}

/// The component's declaration for `places`, as `viewer` may act on them,
/// addressed to its own `endpoint` and made under `revision`, together with
/// the number of places that are not shown.
///
/// One row per place in rail order, each with the class picture the rail draws
/// it with and the plate [`push_plate`] gives it, and a rule where the user's
/// own places give way to the mounted volumes, so the menu reads as two groups
/// rather than one long list; then a rule and *Network*, and a rule and
/// *Trash*, which every place leaves room for. What a chosen row
/// names is [`BarRow::of`]'s.
///
/// A place is shown whole or not at all. One whose label the menu's own
/// bounds refuse is skipped rather than truncated — a row reading almost like
/// a volume's name would navigate somewhere the user did not ask for — and
/// one whose rows the root plate's cap or the menu's text cannot hold is
/// dropped. Each is counted in the returned total, so the caller states one
/// honest "and some are not shown".
///
/// # Errors
///
/// Any [`Errno`] the bounded menu builder refuses the closing rows. A place
/// is admitted only where they still fit, so a refusal can only mean the
/// shared bounds changed under this declaration; the caller reports it and
/// keeps the bar it had rather than showing one it could not describe.
pub fn component_declaration(
    endpoint: u64,
    places: &Places,
    viewer: Option<u32>,
    revision: u32,
) -> Result<(AppBar, usize), Errno> {
    // The Trash is kept in the home the rail's own places hang off.
    let homed = places
        .rows()
        .iter()
        .any(|place| place.kind() == PlaceKind::Home);
    let closing = closing_text(homed)?;
    let mut menu = AppMenu::EMPTY;
    let (mut root, mut shown, mut skipped) = (0, 0, 0);
    let mut ruled = false;
    for (index, place) in places.rows().iter().enumerate() {
        // The rule opens the volume group, before the first volume row that
        // actually makes it in — so a menu whose volumes were all skipped
        // never ends on a dangling divider. It costs a row of the cap, so the
        // pair is admitted together or not at all.
        let rule = !ruled && shown > 0 && place.kind() == PlaceKind::Volume;
        let rows = 1 + usize::from(rule);
        if root + rows > APP_MENU_MAX_ROWS - CLOSING_ROWS {
            skipped += 1;
            continue;
        }
        // Built on a copy, so a place that does not fit leaves none of its
        // rows behind.
        let mut grown = menu;
        if push_place(&mut grown, (place, index), rule, viewer).is_err()
            || grown.text_remaining() < closing
        {
            skipped += 1;
            continue;
        }
        menu = grown;
        root += rows;
        ruled |= rule;
        shown += 1;
    }
    push_closing(&mut menu, shown > 0, homed)?;
    Ok((
        AppBar {
            event_endpoint: endpoint,
            click: DESKTOP_SLOT_CLICK,
            menu,
            revision,
        },
        skipped,
    ))
}

/// Append to `menu` the row of `place`, at `index`, and its plate, after the
/// rule opening the volumes where `rule`.
fn push_place(
    menu: &mut AppMenu,
    (place, index): (&Place, usize),
    rule: bool,
    viewer: Option<u32>,
) -> Result<(), Errno> {
    let mut row = AppMenuItem::new(
        BarRow::Place(index).id()?,
        AppMenuLabel::new(place.label())?,
        place_symbol(place.kind()),
    );
    if let Ok(asset) = IconAsset::new(place.icon().asset_id()) {
        row = row.with_icon_asset(asset);
    }
    if rule {
        menu.push(AppMenuRow::Separator)?;
    }
    let at = menu.len();
    menu.push(AppMenuRow::Item(if place.is_available() {
        row
    } else {
        row.disabled()
    }))?;
    push_plate(menu, at, (place, index), viewer)
}

/// The rows the component's menu closes with: a rule and *Network*, then a
/// rule and *Trash*.
const CLOSING_ROWS: usize = 4;

/// Close `menu` with *Network*, beneath a rule where places precede it, and
/// *Trash* beneath a rule of its own, refused saying why unless `homed`.
fn push_closing(menu: &mut AppMenu, ruled: bool, homed: bool) -> Result<(), Errno> {
    if ruled {
        menu.push(AppMenuRow::Separator)?;
    }
    menu.push(network_row()?)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(trash_row(homed)?)
}

/// How much of a menu's text the closing rows take, as the builder counts
/// it, with a home or without one.
fn closing_text(homed: bool) -> Result<usize, Errno> {
    let mut closing = AppMenu::EMPTY;
    push_closing(&mut closing, true, homed)?;
    Ok(AppMenu::EMPTY
        .text_remaining()
        .saturating_sub(closing.text_remaining()))
}

/// What a row opening a place of `kind` does.
const fn place_symbol(kind: PlaceKind) -> AppMenuSymbol {
    match kind {
        PlaceKind::Home => AppMenuSymbol::Home,
        PlaceKind::UserFolder => AppMenuSymbol::Folder,
        PlaceKind::Volume => AppMenuSymbol::Drive,
    }
}

#[cfg(test)]
#[path = "appbar_tests.rs"]
mod tests;
