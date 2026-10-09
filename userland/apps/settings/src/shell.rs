//! The [`Shell`]: the whole client content of the settings window.
//!
//! It owns the navigation and nothing else. Every pixel is a shared
//! `lib/controls` control — the vertical `Tabs` strip, the `SearchField`, the
//! `Breadcrumb`, the `ScrollBar`, the `Menu` the shed strip becomes — and the
//! pane on show is drawn by the one statement renderer. The shell holds no
//! capability, performs no I/O, and reads nothing but the registry table and
//! the desktop it was handed.
//!
//! Input updates state and asks for a paint; the paint is produced from that
//! state afterwards, so a burst of pointer motion costs one frame rather than
//! one per sample.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::elevate::ElevateArgv;
use tairix_abi::font_ipc::FamilyEntry;
use tairix_abi::net_ipc::NetServerAddr;
use tairix_abi::window_ipc::PreviewSubject;
use tairix_abi::BundleId;
use tairix_controls::{
    ground_fill, plate_rect, Breadcrumb, BreadcrumbAction, ChromeLayer, CredentialAction,
    CredentialSheet, Crumb, DisclosureSet, FieldGroup, Keystroke, Menu, MenuAction, MenuItem,
    PlatePlacement, PlateSide, ScrollAction, ScrollBar, ScrollModel, ScrollOrientation, ScrollPart,
    ScrollRange, ScrollView, SearchField, Tab, Tabs, TabsAction, TabsOrientation, TextAction,
    CREDENTIAL_REFUSED_REASON,
};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::{Color, Surface};
use tairix_sysconfig::SystemConfig;
use tairix_theme::{CursorSetId, Fonts, Grounds, Theme};
use tairix_users::Salt;
use tairix_wallpaper::{CatalogItem, DesktopSettings};

use crate::accounts::{AccountFacts, Roster};
use crate::body::{self, Body};
use crate::facts::MachineFacts;
use crate::footer::{Footer, FooterAction, Standing};
use crate::form::{
    Action, Composition, Form, FormOutcome, FormPlace, Posture, Setting, TextChoices,
};
use crate::frame::{resolve_frame, Actions, Overflow, ShellFrame};
use crate::network::{Addressing, NetworkFacts};
use crate::pictures::{Chooser, PictureWanted};
use crate::registry::{
    strip_rows, Category, CategoryRow, Location, Pane, PaneContent, PaneRow, StripRow, CATEGORIES,
};
use crate::volumes::VolumeReading;

/// The trail's leading crumb: the surface itself, and — once the strip is
/// shed — the way back to the category list.
const ROOT_CRUMB: &str = "Settings";

/// A reading the caller takes for this window.
///
/// Each is wanted from the moment the pane that states it comes on show
/// until its answer lands, and none is ever awaited: the pane draws
/// whatever has arrived and rebuilds when the rest does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Reading {
    /// The machine's boot-time configuration store.
    Config,
    /// The mount table.
    Volumes,
    /// The stack's resolver set.
    Resolvers,
    /// The caller's own account, the two public directories, and a salt.
    Accounts,
    /// The sources the desktop says have notified.
    NotifySources,
}

impl Reading {
    /// This reading's bit in a [`Wanted`] set.
    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Which readings the caller should take for this window.
///
/// One set rather than a field per reading: every one obeys the same
/// rule — armed when its pane comes on show, cleared when its answer
/// lands — so a field apiece would be four copies of it to keep in step.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Wanted(u8);

impl Wanted {
    /// Ask for `reading`.
    fn arm(&mut self, reading: Reading) {
        self.0 |= reading.bit();
    }

    /// Record that `reading`'s answer has landed.
    fn landed(&mut self, reading: Reading) {
        self.0 &= !reading.bit();
    }

    /// Whether `reading` is still wanted.
    const fn holds(self, reading: Reading) -> bool {
        self.0 & reading.bit() != 0
    }
}

/// Which region of the shell holds the keyboard cursor.
///
/// A region the frame did not seat is not on the ring, so `Tab` never lands
/// somewhere the reader cannot see.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Focus {
    /// The search field above the strip.
    Search,
    /// The location trail.
    Trail,
    /// The category and pane strip.
    Strip,
    /// The pane column, whose keyboard is its scrollbar's while the pane
    /// composes no controls of its own.
    Content,
    /// The pane's own action band, where it has one.
    Footer,
}

/// Which region of the window a pointer move lands in, and so which one a
/// move out of it must reach.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Under {
    /// The search field above the strip.
    Search,
    /// The category and pane strip.
    Strip,
    /// The strip's scrollbar.
    StripBar,
    /// The location trail.
    Trail,
    /// The pane column.
    Pane,
    /// The pane column's scrollbar.
    PaneBar,
    /// The pane's action band.
    Band,
}

/// The region of `frame` at `point`, in the order a press is routed.
fn region_at(frame: &ShellFrame, point: Point) -> Option<Under> {
    let inside = |rect: Option<Rect>| rect.is_some_and(|rect| rect.contains(point));
    [
        (frame.footer, Under::Band),
        (frame.scrollbar, Under::PaneBar),
        (Some(frame.content), Under::Pane),
        (frame.search, Under::Search),
        (frame.sidebar, Under::Strip),
        (frame.strip_scrollbar, Under::StripBar),
        (Some(frame.breadcrumb), Under::Trail),
    ]
    .into_iter()
    .find_map(|(rect, region)| inside(rect).then_some(region))
}

/// What the caller does with the program an offered credential runs.
///
/// Three, because three things are wanted of an elevated run and each is a
/// different exchange with the supervisor: a store write is over in
/// moments and its exit code is the whole answer, an application the user
/// then works in is started and left running, and a store *read* is waited
/// for and answered with what it printed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RunMode {
    /// Wait for it; the exit code is the answer.
    Wait,
    /// Start it and leave it running.
    Leave,
    /// Wait for it; what it printed is the answer.
    Capture,
}

/// What an elevated run came to.
///
/// The caller reports this back verbatim; the shell decides what it means
/// for the pane that asked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Elevated {
    /// The run finished with this exit code.
    Finished(i32),
    /// The run finished with this exit code, having printed these bytes.
    Printed(i32, Vec<u8>),
    /// The run happened, but printed more than the supervisor's reply
    /// carries, so nothing came back. Not a refusal: whatever it did, it
    /// did.
    Overran,
    /// Nothing ran, and why.
    Refused(ElevateRefusal),
}

/// The program an offered credential will run, and the arguments to hand
/// it.
///
/// Built by the shell and carried out by the caller: this application holds
/// no authority and starts nothing. What the caller does with the run is
/// the shell's to say, in `mode`.
#[derive(Clone, Eq, PartialEq)]
pub struct Elevation {
    /// The account the reader named.
    pub account: String,
    /// The password they offered, as bytes.
    ///
    /// A secret, and bytes rather than text so its holder can zero it in
    /// place through the one shared eraser when the exchange resolves — a
    /// `String`'s contents cannot be overwritten without `unsafe`, and a
    /// credential left in a freed block is the leak the charter makes the
    /// holder responsible for.
    pub password: Vec<u8>,
    /// The absolute path of the program to run.
    pub program: &'static str,
    /// The arguments to hand it, already in the order the program's own
    /// command line takes them.
    pub argv: Vec<String>,
    /// What the caller does with the run.
    pub mode: RunMode,
}

impl Elevation {
    /// Erase the offered secret in place.
    ///
    /// The one eraser, run by [`Drop`] and by any holder that outlives the
    /// exchange and so must erase before it is dropped — a worker desk
    /// keeps the job it was handed until the next one replaces it, which
    /// would be the whole time between two authentications.
    pub fn erase(&mut self) {
        tairix_util::secret::wipe(&mut self.password);
    }
}

impl Drop for Elevation {
    /// Erase the offered secret.
    ///
    /// Every copy ends here — the one the routing carried out of the
    /// window and the one the worker was handed — so no path out of the
    /// exchange, taken or refused or cancelled or unwound, can leave a
    /// password in a freed block.
    fn drop(&mut self) {
        self.erase();
    }
}

impl core::fmt::Debug for Elevation {
    /// Redacts the secret: a derived `Debug` would print an offered
    /// password into whatever rendered it.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Elevation")
            .field("account", &self.account)
            .field("password", &"<redacted>")
            .field("program", &self.program)
            .field("argv", &self.argv)
            .field("mode", &self.mode)
            .finish()
    }
}

/// What a captured run answers, and therefore which pane it is for.
///
/// Carried explicitly rather than inferred from the pane on show: the
/// desktop can send the window to another pane while a run is in flight,
/// and a privileged listing routed to whichever pane happens to be showing
/// when it lands would be a reading installed for a surface that never
/// asked for it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Captures {
    /// The machine's configured addressing, for the networking panes.
    Addressing,
    /// The account and group listing, for the Users pane.
    Roster,
}

impl Captures {
    /// Whether `composition` is the pane this capture was asked for.
    const fn asked_by(self, composition: Composition) -> bool {
        match self {
            Self::Addressing => composition.reads_addressing(),
            Self::Roster => composition.reads_roster(),
        }
    }
}

/// A credential question standing over the window, and what it is for.
struct Asking {
    sheet: CredentialSheet,
    program: &'static str,
    argv: Vec<String>,
    mode: RunMode,
    /// What a captured run answers, and `None` for a run whose exit code
    /// is the whole answer.
    captures: Option<Captures>,
}

/// What one routed event concluded, for a caller that must act outside the
/// window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellOutcome {
    /// Nothing on screen changed.
    Idle,
    /// The shell changed and must be re-presented.
    Changed,
    /// The reader chose a setting: the rendered document the caller posts to
    /// the desktop session, which decides whether to adopt it.
    ///
    /// The shell holds no capability and performs no I/O: it never writes
    /// the desktop's settings, it says what was asked for.
    Apply(String),
    /// The reader offered an account for a change this application may not
    /// make: the run to ask the console's elevation broker for.
    ///
    /// The shell authenticates nobody and spawns nothing. It carries the
    /// offer and renders the verdict it is told.
    Elevate(Elevation),
    /// The reader asked for the screen to be locked now: the request the
    /// caller makes of the desktop session, whose lock it is.
    LockScreen,
    /// The reader asked to see the screensaver now: the screensaver keys'
    /// document as the pane shows them, for the caller to hand the desktop
    /// session, which previews it and keeps none of it.
    PreviewScreensaver(String),
}

impl ShellOutcome {
    /// `Changed` when `acted`, else `Idle`.
    const fn of(acted: bool) -> Self {
        if acted {
            Self::Changed
        } else {
            Self::Idle
        }
    }

    /// Whether the shell must be re-presented.
    #[must_use]
    pub const fn changed(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// The document this outcome asks the session to adopt, if any.
    #[must_use]
    pub fn document(&self) -> Option<&str> {
        match self {
            Self::Apply(document) => Some(document),
            Self::Idle
            | Self::Changed
            | Self::Elevate(_)
            | Self::LockScreen
            | Self::PreviewScreensaver(_) => None,
        }
    }
}

/// The settings window's client content.
pub struct Shell {
    /// Where the surface is.
    location: Location,
    /// Which disclosing categories show their panes in the strip.
    open: DisclosureSet<Category>,
    /// The strip's rows, in the order they are drawn — the one list the
    /// paint, the hit test and the cursor read.
    rows: Vec<StripRow>,
    strip: Tabs,
    search: SearchField,
    trail: Breadcrumb,
    /// The strip's own scroll: the list of categories is longer than a short
    /// window's column, and a row the reader cannot reach is a category they
    /// cannot open.
    strip_scroll: ScrollBar,
    /// The pane column's scroll.
    scroll: ScrollBar,
    /// The category list the shed strip becomes, while it is open.
    categories: Option<Menu>,
    focus: Focus,
    /// The last pointer position, for routing a press to the region under it;
    /// `None` until the window has seen one.
    pointer: Option<Point>,
    /// The region the last move landed in, which the next move out of it
    /// reaches too.
    under: Option<Under>,
    /// How many times the columns have been measured, so a round can tell
    /// that what lies under a still pointer was laid out afresh.
    layouts: u64,
    /// The desktop settings every composed pane's rows are built from.
    settings: DesktopSettings,
    /// What the pane on show draws.
    body: Body,
    /// The shipped pictures the desktop answered, empty until it has.
    catalog: Vec<CatalogItem>,
    /// The cursor sets the desktop offers besides the built-in one, empty
    /// until it has answered.
    cursor_sets: Vec<CursorSetId>,
    /// The font store's families and the theme's own text, once listed.
    text: Option<TextChoices>,
    /// The mounted volumes the caller last read for the Storage pane, empty
    /// until it has.
    volumes: Vec<VolumeReading>,
    /// The machine's boot-time configuration, or `None` while the read has
    /// not landed.
    config: Option<SystemConfig>,
    /// The machine readings the About and Date & Time panes state.
    machine: MachineFacts,
    /// The network readings the DNS pane states.
    network: NetworkFacts,
    /// The account readings the Users pane states.
    accounts: AccountFacts,
    /// The sources the desktop said have notified, or `None` while it has not
    /// said — or would not.
    notify_sources: Option<Vec<BundleId>>,
    /// Why the desktop last would not lock the screen, held here so a refusal
    /// landing while another pane is on show is stated once the pane that
    /// asked is shown again.
    lock_refusal: Option<tairix_abi::Errno>,
    /// Why it last would not show the screensaver, held for the same reason.
    preview_refusal: Option<tairix_abi::Errno>,
    /// A fresh salt the caller drew, held so a password can be hashed
    /// without the event loop waiting on a read.
    ///
    /// Consumed on use and never reused: an apply takes it and the caller
    /// draws another. With none held a password apply is refused rather
    /// than salted with something predictable.
    salt: Option<Salt>,
    /// Which readings the caller should take for this window.
    wanted: Wanted,
    /// The action band beneath the pane on show, for a pane that has one.
    footer: Option<Footer>,
    /// The credential question standing over the window, and what it will
    /// ask the broker to run once an account is offered.
    asking: Option<Asking>,
    /// Moved whenever what the pane's pictures want may have: the pane, what
    /// it holds, or an answer landing.
    pictures_epoch: u64,
    /// The last question about pictures that found nothing to ask for, so
    /// the event loop asking after every event costs nothing until the
    /// answer could differ.
    pictures_settled: Option<PictureQuestion>,
}

/// Everything the answer to "which picture next" depends on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct PictureQuestion {
    epoch: u64,
    layouts: u64,
    offset: u64,
    viewport: Rect,
    scale: Scale,
    roomy: bool,
    asked: u64,
}

impl Shell {
    /// The shell as the window opens on `settings`: the first category, its
    /// first pane, and the cursor on the strip.
    ///
    /// `settings` is the desktop's own published document, read by the
    /// caller — the shell performs no I/O. An empty registry would leave
    /// nothing to show, so it answers `None` rather than inventing a
    /// location; the registry's own test rules it out.
    #[must_use]
    pub fn new(settings: DesktopSettings) -> Option<Self> {
        let location = Location::opening()?;
        let mut open = DisclosureSet::closed();
        list_pane_of(&mut open, location);
        let rows = strip_rows(&open, "");
        let mut shell = Self {
            location,
            open,
            strip: strip_of(&rows, location),
            rows,
            search: SearchField::new().with_placeholder("Search settings"),
            trail: Breadcrumb::new(Vec::new()),
            strip_scroll: ScrollBar::new(ScrollOrientation::Vertical, empty_scroll()),
            scroll: ScrollBar::new(ScrollOrientation::Vertical, empty_scroll()),
            categories: None,
            focus: Focus::Strip,
            pointer: None,
            under: None,
            layouts: 0,
            settings,
            body: Body::Statement,
            catalog: Vec::new(),
            cursor_sets: Vec::new(),
            text: None,
            volumes: Vec::new(),
            wanted: Wanted::default(),
            config: None,
            machine: MachineFacts::default(),
            network: NetworkFacts::default(),
            accounts: AccountFacts::default(),
            notify_sources: None,
            lock_refusal: None,
            preview_refusal: None,
            salt: None,
            footer: None,
            asking: None,
            pictures_epoch: 0,
            pictures_settled: None,
        };
        shell.restate_trail();
        shell.restate_body();
        Some(shell)
    }

    /// Adopt the cursor sets the desktop answered.
    ///
    /// Which sets exist is what the desktop's read-only store holds, and
    /// this application may not read it: the session lists it once and
    /// answers, so the pointer row's choice space arrives here. Until it
    /// does the row offers the built-in set alone, plus whatever the
    /// document already names.
    pub fn adopt_cursor_sets(&mut self, sets: Vec<CursorSetId>) {
        self.cursor_sets = sets;
        self.restate_body();
    }

    /// Adopt the families the font store lists, and `theme`, the fonts the
    /// theme was registered with, which the Text rows' defaults name.
    ///
    /// Until they arrive the Font row offers its default and whatever the
    /// document already names.
    pub fn adopt_text_choices(&mut self, families: Vec<FamilyEntry>, theme: Fonts) {
        self.text = Some(TextChoices { families, theme });
        self.restate_body();
    }

    /// Adopt the shipped picture catalog the desktop answered.
    ///
    /// Requested, never awaited: the Wallpaper pane opens on whatever has
    /// arrived — nothing at all, at first — and rebuilds when it lands.
    pub fn adopt_catalog(&mut self, catalog: Vec<CatalogItem>) {
        self.catalog = catalog;
        if self
            .body
            .composition()
            .is_some_and(Composition::reads_catalog)
        {
            self.restate_body();
        }
    }

    /// Whether the caller should read the machine's boot-time configuration
    /// for this window.
    ///
    /// Set when a pane that stages a machine setting comes on show and
    /// cleared when a read is answered, so the rows are as current as the
    /// last time the reader looked without the pane ever waiting on a round
    /// trip.
    #[must_use]
    pub const fn config_wanted(&self) -> bool {
        self.wanted.holds(Reading::Config)
    }

    /// Adopt the machine's boot-time configuration the caller read.
    ///
    /// `None` is a read that was refused or did not decode, which leaves
    /// the rows unmeasured rather than showing defaults the reader could
    /// not have set; a store that simply does not exist decodes as the
    /// documented defaults and is a perfectly good reading.
    pub fn adopt_config(&mut self, config: Option<SystemConfig>) {
        self.config = config;
        self.wanted.landed(Reading::Config);
        if let Some(form) = self.body.form_mut() {
            form.adopt_config(self.config.as_ref());
        }
        self.restate_footer();
    }

    /// Adopt the machine readings the caller took.
    pub fn adopt_machine(&mut self, machine: MachineFacts) {
        self.machine = machine;
        if matches!(self.body, Body::Facts(_)) {
            self.restate_body();
        }
    }

    /// Adopt a staged edit: what each plate and the band now say about it,
    /// and the column's extent where a plate learning it has changed moved
    /// one.
    ///
    /// Only where it moved: a badge appears once per plate, while an entry
    /// reports every keystroke, and repainting the whole column for each of
    /// them would hand the frame rate to the keyboard.
    fn restate_staged(
        &mut self,
        frame: &ShellFrame,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let column = pane_band(frame, viewport).width;
        let before = self
            .body
            .form()
            .map_or(0, |form| form.measured_height(column, scale, theme));
        if let Some(form) = self.body.form_mut() {
            form.restate_badges();
        }
        self.restate_footer();
        if let Some(rect) = frame.footer {
            damage.add(rect);
        }
        let after = self
            .body
            .form()
            .map_or(0, |form| form.measured_height(column, scale, theme));
        if before != after {
            self.measure(viewport, scale, theme);
            damage.add(pane_band(frame, viewport));
        }
    }

    /// Bring the action band up to date with what the pane now holds.
    fn restate_footer(&mut self) {
        let standing = self.standing();
        if let Some(band) = self.footer.as_mut() {
            band.state(standing);
        }
    }

    /// Whether the caller should read the mount table for this window.
    ///
    /// Set when the Storage pane comes on show and cleared when a walk is
    /// answered, so the volumes are as current as the last time the reader
    /// looked at them without the pane ever waiting on a round trip.
    #[must_use]
    pub const fn volumes_wanted(&self) -> bool {
        self.wanted.holds(Reading::Volumes)
    }

    /// Adopt the mounted volumes the caller walked the mount table for.
    ///
    /// Requested, never awaited: the Storage pane opens on whatever has
    /// arrived — nothing at all, at first — and rebuilds when it lands.
    pub fn adopt_volumes(&mut self, volumes: Vec<VolumeReading>) {
        self.volumes = volumes;
        self.wanted.landed(Reading::Volumes);
        if matches!(self.body, Body::Volumes(_)) {
            self.restate_body();
        }
    }

    /// Whether the caller should read the network stack's resolver set for
    /// this window.
    ///
    /// Set when a pane that states a network reading comes on show and
    /// cleared when one is answered, exactly as the mount walk is: the set
    /// moves as leases come and go, so a pane the reader returns to shows
    /// what the stack holds now rather than what it held last time.
    #[must_use]
    pub const fn network_wanted(&self) -> bool {
        self.wanted.holds(Reading::Resolvers)
    }

    /// Adopt the network readings the caller took.
    ///
    /// Requested, never awaited: the pane opens on whatever has arrived —
    /// nothing at all, at first — and rebuilds when it lands.
    ///
    /// Only the resolver set, because that is the only network reading a
    /// desk takes: the configured addressing is answered by an
    /// authenticated run and would be thrown away by a reading that
    /// replaced the whole record.
    pub fn adopt_resolvers(&mut self, resolvers: Option<Vec<NetServerAddr>>) {
        self.network.resolvers = resolvers;
        self.wanted.landed(Reading::Resolvers);
        // Into the pane that states it, and only its rows: rebuilding the
        // pane would discard a change the reader has staged because an
        // unrelated reading happened to land.
        if !self.states_resolvers() {
            return;
        }
        let resolvers = self.network.resolvers_slice();
        if let Some(form) = self.body.form_mut() {
            form.adopt_resolvers(resolvers);
        }
    }

    /// Whether the caller should read the ungated account readings for
    /// this window.
    ///
    /// Set when the Users pane comes on show and cleared when a read is
    /// answered, exactly as the mount walk and the resolver set are: the
    /// directories move as accounts and groups come and go, so a pane the
    /// reader returns to shows what the machine holds now.
    #[must_use]
    pub const fn accounts_wanted(&self) -> bool {
        self.wanted.holds(Reading::Accounts)
    }

    /// Whether the caller should draw a fresh salt for this window.
    ///
    /// One at a time: a password apply consumes the one held, so the
    /// reserve is refilled rather than drawn at the instant it is wanted —
    /// the loop owes the reader a frame and must not wait on a read.
    #[must_use]
    pub fn salt_wanted(&self) -> bool {
        self.salt.is_none() && self.shows_accounts()
    }

    /// Adopt the ungated account readings the caller took.
    ///
    /// Requested, never awaited: the pane opens on whatever has arrived —
    /// nothing at all, at first — and rebuilds when it lands. The
    /// authenticated listing is *not* replaced: it cost a password, and a
    /// public reading landing must not throw it away.
    pub fn adopt_accounts(&mut self, accounts: AccountFacts) {
        self.accounts.adopt_public(accounts);
        self.wanted.landed(Reading::Accounts);
        if let Some(form) = self
            .body
            .form_mut()
            .filter(|form| form.composition().reads_roster())
        {
            form.adopt_accounts(&self.accounts);
        }
        self.restate_footer();
    }

    /// Adopt a fresh salt the caller drew, or record that the draw failed.
    ///
    /// `None` leaves the reserve empty, which refuses a password apply
    /// rather than reaching for a predictable salt.
    pub fn adopt_salt(&mut self, salt: Option<Salt>) {
        self.salt = salt;
    }

    /// Whether the pane the shell is *on* is discovered from the account
    /// listing.
    ///
    /// The location rather than the body on show, for the same reason the
    /// addressing capture asks it of the location: this decides whether the
    /// listing survives long enough to build the next body.
    fn shows_accounts(&self) -> bool {
        matches!(
            self.pane_row().and_then(PaneRow::content),
            Some(PaneContent::Form(composition)) if composition.reads_roster()
        )
    }

    /// Whether the body on show is discovered from the account listing.
    fn states_accounts(&self) -> bool {
        self.body
            .composition()
            .is_some_and(Composition::reads_roster)
    }

    /// Whether the body on show states the live resolver set.
    fn states_resolvers(&self) -> bool {
        self.body
            .composition()
            .is_some_and(Composition::reads_resolvers)
    }

    /// Whether the pane on show lists the sources that have notified.
    fn lists_notify_sources(&self) -> bool {
        self.body
            .composition()
            .is_some_and(Composition::reads_notify_sources)
    }

    /// Whether the caller should ask the desktop which sources have
    /// notified.
    ///
    /// Set when the pane that lists them comes on show, so a program that
    /// notified since the reader last looked is listed.
    #[must_use]
    pub const fn notify_sources_wanted(&self) -> bool {
        self.wanted.holds(Reading::NotifySources)
    }

    /// Adopt what the desktop answered when asked to lock the screen: `None`
    /// once it has, or its refusal, which the pane states.
    pub fn adopt_lock_answer(&mut self, answer: Result<(), tairix_abi::Errno>) {
        self.lock_refusal = answer.err();
        if let Some(form) = self
            .body
            .form_mut()
            .filter(|form| form.offers(Action::LockNow))
        {
            form.adopt_lock_refusal(self.lock_refusal);
        }
    }

    /// Adopt the sources the desktop said have notified, or `None` when it
    /// did not say.
    pub fn adopt_notify_sources(&mut self, sources: Option<Vec<BundleId>>) {
        self.notify_sources = sources;
        self.wanted.landed(Reading::NotifySources);
        if !self.lists_notify_sources() {
            return;
        }
        let sources = self.notify_sources.as_deref();
        if let Some(form) = self.body.form_mut() {
            form.adopt_notify_sources(sources);
        }
    }

    /// Whether the pane the shell is *on* is discovered from the
    /// addressing capture.
    ///
    /// The location rather than the body on show, because this decides
    /// whether the capture survives long enough to build the next body.
    fn shows_addressing(&self) -> bool {
        matches!(
            self.pane_row().and_then(PaneRow::content),
            Some(PaneContent::Form(composition)) if composition.reads_addressing()
        )
    }

    /// The next picture the caller should ask the desktop to render for the
    /// pane on show in `viewport`, or `None` when there is nothing to ask
    /// for.
    ///
    /// The pictures on screen come first, then those up to a screen's height
    /// either side while `roomy` — memory is plentiful — and none beyond.
    /// While `roomy` every picture handed over is kept for the life of the
    /// pane, so scrolling back never asks the desktop again; once memory is
    /// short only what is on screen is kept.
    ///
    /// Cheap to ask after every event: a question whose answer cannot have
    /// changed since it last found nothing — the same pane, layout, scroll,
    /// screen and band, and the same pictures `asked`, which `asked_changes`
    /// stands for — is answered at once.
    pub fn next_picture_wanted(
        &mut self,
        viewport: Rect,
        (scale, theme): (Scale, &Theme),
        roomy: bool,
        (asked_changes, asked): (u64, impl Fn(PreviewSubject) -> bool),
    ) -> Option<PictureWanted> {
        let question = PictureQuestion {
            epoch: self.pictures_epoch,
            layouts: self.layouts,
            offset: self.scroll.model().offset(),
            viewport,
            scale,
            roomy,
            asked: asked_changes,
        };
        if self.pictures_settled == Some(question) {
            return None;
        }
        let wanted = self.picture_round(viewport, (scale, theme), roomy, &mut |subject| {
            asked(subject)
        });
        if wanted.is_none() {
            self.pictures_settled = Some(question);
        }
        wanted
    }

    /// Let go of the pictures [`next_picture_wanted`](Self::next_picture_wanted)
    /// would no longer keep, at once: what memory pressure asks for, whether
    /// or not a render is outstanding.
    pub fn trim_pictures(&mut self, viewport: Rect, (scale, theme): (Scale, &Theme), roomy: bool) {
        // The next picture is asked for when a render may be requested.
        let _ = self.picture_round(viewport, (scale, theme), roomy, &mut |_| false);
        self.pictures_settled = None;
    }

    /// One round over the pane's pictures in `viewport`: those the band no
    /// longer keeps let go, and the nearest still wanted and not `asked`
    /// answered.
    fn picture_round(
        &mut self,
        viewport: Rect,
        (scale, theme): (Scale, &Theme),
        roomy: bool,
        asked: &mut dyn FnMut(PreviewSubject) -> bool,
    ) -> Option<PictureWanted> {
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        let content = frame.content;
        let seen = Rect::new(
            content.left(),
            content.top().saturating_add(to_i32(view.offset())),
            content.width,
            content.height,
        );
        self.body
            .form_mut()
            .and_then(|form| form.picture_round(spot, (seen, roomy), asked))
    }

    /// Adopt the pixels the desktop rendered for `wanted`, reporting where the
    /// window shows the picture they fill.
    pub fn set_picture(
        &mut self,
        wanted: PictureWanted,
        pixels: &[u8],
        (viewport, scale, theme): (Rect, Scale, &Theme),
        damage: &mut Region,
    ) {
        self.pictures_moved();
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        let landed = self
            .body
            .form_mut()
            .and_then(|form| form.land_picture(wanted, pixels, spot));
        if let Some(shown) = landed.and_then(|tile| view.to_window(tile)) {
            damage.add(shown);
        }
    }

    /// Record that the desktop would not render `subject`, so it keeps its
    /// glyph and is not asked for again.
    pub fn mark_picture_refused(&mut self, subject: PreviewSubject) {
        self.pictures_moved();
        if let Some(form) = self.body.form_mut() {
            form.refuse_picture(subject);
        }
    }

    /// Record that the desktop had no memory to render `subject`, so it keeps
    /// its glyph until memory may have been freed.
    pub fn mark_picture_unavailable(&mut self, subject: PreviewSubject) {
        self.pictures_moved();
        if let Some(form) = self.body.form_mut() {
            form.picture_unavailable(subject);
        }
    }

    /// Memory may have been freed: offer the pictures that were short of it
    /// again.
    pub fn retry_unavailable_pictures(&mut self) {
        if self
            .body
            .form_mut()
            .is_some_and(crate::form::Form::retry_unavailable_pictures)
        {
            self.pictures_moved();
        }
    }

    /// Record that what the pane's pictures want may have changed.
    fn pictures_moved(&mut self) {
        self.pictures_epoch = self.pictures_epoch.wrapping_add(1);
    }

    /// Adopt what the desktop answered when asked to show the screensaver:
    /// nothing once it has, or its refusal, which the pane states.
    pub fn adopt_preview_answer(&mut self, answer: Result<(), tairix_abi::Errno>) {
        self.preview_refusal = answer.err();
        if let Some(form) = self
            .body
            .form_mut()
            .filter(|form| form.offers(Action::PreviewScreensaver))
        {
            form.adopt_preview_refusal(self.preview_refusal);
        }
    }

    /// Show the pane `name` identifies, answering whether it named one.
    ///
    /// The desktop hands this over when it sends the reader here to change
    /// a particular setting. It confers nothing and names nothing outside
    /// the closed registry, so an unknown name leaves the window on the
    /// pane it already showed.
    pub fn go_to_pane(
        &mut self,
        name: &str,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let Some(pane) = Pane::named(name) else {
            return false;
        };
        self.go_to(pane, viewport, scale, theme, damage)
    }

    /// Show `pane`, answering whether the registry could place it.
    pub fn go_to(
        &mut self,
        pane: Pane,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let Some((category, row)) = pane.locate() else {
            return false;
        };
        let location = Location {
            category,
            pane: row.pane,
        };
        if location == self.location {
            return true;
        }
        self.show(location, viewport, scale, theme, damage);
        true
    }

    /// Adopt the desktop settings the session now holds.
    ///
    /// What the window does when an apply is answered: every composed row
    /// shows what the store actually holds, so a refused apply reverts rather
    /// than leaving a value on screen the next login would not restore.
    pub fn adopt_settings(&mut self, settings: DesktopSettings) {
        // Against what the *form* shows, not against the last answer: a
        // choice the reader made moved the rows on screen without moving
        // this, so an answer equal to the last one is exactly the refusal
        // that has to put them back.
        let shown = match self.body.form() {
            Some(form) => form.settings() == &settings,
            None => self.settings == settings,
        };
        self.settings = settings;
        if shown {
            return;
        }
        if self.body.form().is_some() {
            self.body.adopt(&self.settings);
            self.pictures_moved();
        } else {
            self.restate_body();
        }
    }

    /// Put every composed row back to what the store last answered: what a
    /// choice refused before it reached the desktop comes to.
    pub fn revert_settings(&mut self) {
        self.adopt_settings(self.settings.clone());
    }

    /// Build what the pane on show draws.
    fn restate_body(&mut self) {
        let listed = matches!(self.body, Body::Volumes(_));
        let resolved = self.states_resolvers();
        // A pane that is not discovered from the capture drops it before
        // it is built: the machine's address book is a privileged reading,
        // and one held while the reader browses elsewhere is one this
        // application had no business keeping. Both networking panes are
        // discovered from it, so moving between them costs no second
        // authentication.
        if !self.shows_addressing() {
            self.network.addressing = Addressing::Unasked;
        }
        // And the account listing, for exactly the same reason: it names
        // every account's home, shell, lock state and capability ceiling,
        // which is a privileged reading this application had no business
        // keeping while the reader browses elsewhere.
        if !self.shows_accounts() {
            self.accounts.roster = Roster::Unasked;
        }
        let rostered = self.states_accounts();
        let listed_sources = self.lists_notify_sources();
        let staged = self.body.staged().to_vec();
        let staged_accounts = self.body.staged_accounts().to_vec();
        let answered = body::Answered {
            settings: &self.settings,
            cursor_sets: &self.cursor_sets,
            catalog: &self.catalog,
            text: self.text.as_ref(),
            volumes: &self.volumes,
            config: self.config.as_ref(),
            machine: &self.machine,
            network: &self.network,
            staged: &staged,
            accounts: &self.accounts,
            staged_accounts: &staged_accounts,
            notify_sources: self.notify_sources.as_deref(),
            lock_refusal: self.lock_refusal,
            preview_refusal: self.preview_refusal,
        };
        self.body = match self.location.rows() {
            Some((_, pane)) => Body::of(pane, &answered),
            None => Body::Statement,
        };
        // Asked for when the pane that lists them *comes* on show, so the
        // table is current each time the reader looks. Not on every rebuild:
        // adopting an answered walk rebuilds too, and re-arming there would
        // ask again for the table just handed over.
        if !listed && matches!(self.body, Body::Volumes(_)) {
            self.wanted.arm(Reading::Volumes);
        }
        // The resolver set is live state the stack changes on its own, so
        // it is re-read when its pane *comes* on show for the same reason
        // the mount table is, and not on the rebuild that adopting one
        // causes.
        if !resolved && self.states_resolvers() {
            self.wanted.arm(Reading::Resolvers);
        }
        // The directories move on their own too, so they are re-read when
        // the pane that lists them *comes* on show — not on the rebuild
        // that adopting one causes, which would ask again for the readings
        // just handed over.
        if !rostered && self.states_accounts() {
            self.wanted.arm(Reading::Accounts);
        }
        // A program may notify at any moment, so the list is asked for each
        // time its pane comes on show.
        if !listed_sources && self.lists_notify_sources() {
            self.wanted.arm(Reading::NotifySources);
        }
        // Rebuilt rather than kept: a band belongs to the pane that offers
        // it, and one carried across a navigation would state the last
        // pane's standing under this pane's rows.
        // Asked for when a pane that stages a machine setting comes on
        // show, for the same reason the mount table is: the store moves,
        // and a stale reading is a value the reader cannot account for.
        if self.body.stages_machine_settings() && self.config.is_none() {
            self.wanted.arm(Reading::Config);
        }
        self.footer = self.band();
        self.pictures_moved();
    }

    /// The action band the pane on show offers, or `None` for a pane that
    /// offers none.
    ///
    /// A pane with a working copy offers Revert and Apply over it. One
    /// without offers the single command the registry names for it — the
    /// application that owns its subject, or the authenticated reading its
    /// rows cannot exist without.
    fn band(&self) -> Option<Footer> {
        if self.stageable() {
            let mut band = Footer::staged();
            band.state(self.standing());
            return Some(band);
        }
        self.pane_row()
            .and_then(PaneRow::action)
            .map(Footer::command)
    }

    /// Whether the pane on show has a working copy to stage a change
    /// against.
    ///
    /// A machine composition always has one: an unread store leaves its
    /// rows unmeasured, but the pane is still that store's editor and the
    /// reading arrives without the reader doing anything. A networking
    /// composition has one only once a capture has landed, because until
    /// then there is no document — and getting one is what its band's
    /// command is for.
    fn stageable(&self) -> bool {
        let Some(form) = self.body.form() else {
            return false;
        };
        if form.posture() != Posture::Staged {
            return false;
        }
        let composition = form.composition();
        if composition.reads_roster() {
            return !form.roster().accounts().is_empty();
        }
        !composition.reads_addressing() || form.addressing().document().is_some()
    }

    /// What the pane on show has to say about the change it is holding.
    ///
    /// A pane with no working copy holds no change: its band offers one
    /// named command and has nothing to count.
    fn standing(&self) -> Standing {
        let Some(form) = self.body.form().filter(|_| self.stageable()) else {
            return Standing::Offered;
        };
        match (form.refused(), form.changes()) {
            (0, 0) => Standing::Unchanged,
            (0, count) => Standing::Changed(count),
            (refused, _) => Standing::Refusing(refused),
        }
    }

    /// Whether the pane on show has an action band beneath its column.
    const fn actions(&self) -> Actions {
        match self.footer {
            Some(_) => Actions::Band,
            None => Actions::None,
        }
    }

    /// The pane row on show, whose statement a stated absence draws from.
    fn pane_row(&self) -> Option<&'static PaneRow> {
        self.location.rows().map(|(_, pane)| pane)
    }

    /// Where the surface is.
    #[must_use]
    pub fn location(&self) -> Location {
        self.location
    }

    /// The strip's rows, in drawn order.
    #[must_use]
    pub fn rows(&self) -> &[StripRow] {
        &self.rows
    }

    /// The window's title: the pane on show, as a file manager's window is
    /// titled with its folder.
    #[must_use]
    pub fn title(&self) -> &'static str {
        self.pane_row().map_or(ROOT_CRUMB, |pane| pane.title)
    }

    /// Which strip row is drawn selected: the pane on show's own row where
    /// the strip lists it, else its category's row, which stands for it.
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        row_on_show(&self.rows, self.location)
    }

    /// Where the strip draws row `index` in `viewport`, or `None` when the
    /// strip is shed or shows none of that row.
    ///
    /// The rectangle a press on that row is hit-tested against, so a caller
    /// aiming at a row — a test, or the QEMU vertical's script — aims where
    /// the strip drew it.
    #[must_use]
    pub fn strip_row_rect(
        &self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let sidebar = self.frame(viewport, scale, theme).sidebar?;
        let (column, view) = self.strip_view(sidebar, scale, theme);
        view.to_window(self.strip.tab_area(index, column, scale, theme)?)
    }

    /// Where the strip's scrollbar draws `part` in `viewport`, or `None` when
    /// the strip needs no scrollbar or draws none of that part.
    #[must_use]
    pub fn strip_scroll_rect(
        &self,
        part: ScrollPart,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let bar = self.frame(viewport, scale, theme).strip_scrollbar?;
        self.strip_scroll.part_rect(part, bar, scale, theme)
    }

    /// Where the pane on show draws `setting`'s control in `viewport`, or
    /// `None` when it shows no row for it or the column shows none of it.
    #[must_use]
    pub fn setting_rect(
        &self,
        setting: Setting,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        view.to_window(self.body.form()?.control_rect(setting, spot)?)
    }

    /// Where the open choice list draws choice `index` in `viewport`, or
    /// `None` while no list is open.
    #[must_use]
    pub fn choice_rect(
        &self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        view.to_window(self.body.form()?.choice_rect(index, spot)?)
    }

    /// The category list drawn over the content while the shed strip is open.
    #[must_use]
    pub fn category_list_open(&self) -> bool {
        self.categories.is_some()
    }

    /// The frame this shell is drawn with in `viewport`.
    ///
    /// Cheap enough for the input path: whether the pane scrolls is the
    /// scroll range's own answer, laid in by [`lay_out`](Self::lay_out),
    /// rather than a statement re-measured per event.
    #[must_use]
    pub fn frame(&self, viewport: Rect, scale: Scale, theme: &Theme) -> ShellFrame {
        resolve_frame(
            viewport,
            scale,
            theme,
            Overflow {
                strip: self.strip_scroll.model().range().is_scrollable(),
                pane: self.scroll.model().range().is_scrollable(),
            },
            self.actions(),
        )
    }

    /// What adopting an answer can redraw in `viewport`: the pane's column
    /// and everything beside and beneath it, its scrollbar and action band.
    /// No answer moves the strip, the search field or the trail.
    #[must_use]
    pub fn pane_region(&self, viewport: Rect, scale: Scale, theme: &Theme) -> Rect {
        let content = self.frame(viewport, scale, theme).content;
        let span = |from: i32, to: i32| u32::try_from(to.saturating_sub(from)).unwrap_or(0);
        Rect::new(
            content.left(),
            content.top(),
            span(content.left(), viewport.right()),
            span(content.top(), viewport.bottom()),
        )
    }

    /// Measure the pane for `viewport`, adopt the scroll range it implies, and
    /// re-derive the hover from where the pointer rests over what moved.
    ///
    /// For a caller outside an input round, which afterwards redraws all a
    /// layout can move: what the hover changed needs no reporting of its own.
    pub fn lay_out(&mut self, viewport: Rect, scale: Scale, theme: &Theme) {
        self.measure(viewport, scale, theme);
        self.rehover(viewport, scale, theme, &mut tairix_controls::damage::sink());
    }

    /// Measure both columns for `viewport` and adopt the scroll ranges they
    /// imply.
    ///
    /// Called whenever what the pane holds or how wide it is can have changed
    /// — the window opening, a navigation, a resize, a desktop change — and
    /// never for a pointer sample alone: measuring a wrapped statement is a
    /// pass over its every word.
    fn measure(&mut self, viewport: Rect, scale: Scale, theme: &Theme) {
        self.measure_pane(viewport, scale, theme);
        self.measure_strip(viewport, scale, theme);
    }

    /// Measure the pane for `viewport` and adopt the scroll range it implies.
    ///
    /// The column's width decides how the statement wraps and the wrap decides
    /// its height, while a column that needs a scrollbar is the narrower for
    /// it. So it measures without one, and again at the narrowed width when
    /// one turns out to be needed.
    fn measure_pane(&mut self, viewport: Rect, scale: Scale, theme: &Theme) {
        self.layouts = self.layouts.wrapping_add(1);
        let bare = resolve_frame(viewport, scale, theme, Overflow::default(), self.actions());
        // The strip's bar is carved from the strip's own column, so whether it
        // has one never moves the pane's.
        let overflow = Overflow {
            strip: false,
            pane: self.content_height(bare.content.width, scale, theme) > bare.content.height,
        };
        // A column that needs a bar is the narrower for it, and a narrower
        // pane column wraps its statement into more lines — so the column the
        // range is set from is the one a bar has already been taken out of.
        let frame = resolve_frame(viewport, scale, theme, overflow, self.actions());
        let offset = self.scroll.model().offset();
        let pane = self.pane_row().map_or_else(empty_scroll, |pane| {
            self.body
                .scroll_model(pane, frame.content, (scale, theme, offset))
        });
        self.scroll.set_model(pane);
        // Measured now, so a rebuild before this is laid out already.
        if let Some(form) = self.body.form_mut() {
            form.take_reshaped();
        }
    }

    /// Measure the strip for `viewport` and adopt the scroll range it implies:
    /// all a change to the strip alone — a section disclosed, the search
    /// edited — needs, since neither moves the pane.
    fn measure_strip(&mut self, viewport: Rect, scale: Scale, theme: &Theme) {
        self.layouts = self.layouts.wrapping_add(1);
        let sidebar =
            resolve_frame(viewport, scale, theme, Overflow::default(), self.actions()).sidebar;
        let (extent, seen) = sidebar.map_or((0, 0), |rect| {
            (
                u64::from(self.strip.measured_height(scale, theme)),
                u64::from(rect.height),
            )
        });
        self.strip_scroll.set_model(ScrollModel::in_pixels(
            self.strip_scroll.model().range().resize(extent, seen),
            body::line_step(scale, theme),
        ));
    }

    /// Scroll the strip the least that shows row `index`, answering whether
    /// it moved and reporting the strip and its bar when it did.
    fn reveal_row(
        &mut self,
        index: usize,
        frame: &ShellFrame,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let Some(sidebar) = frame.sidebar else {
            return false;
        };
        let (column, _) = self.strip_view(sidebar, scale, theme);
        let Some(area) = self.strip.tab_area(index, column, scale, theme) else {
            return false;
        };
        let moved = reveal(&mut self.strip_scroll, column, area);
        if moved {
            damage.add(sidebar);
            if let Some(bar) = frame.strip_scrollbar {
                damage.add(bar);
            }
        }
        moved
    }

    /// Run `act` on the strip laid out unscrolled down `sidebar`, with the
    /// view it shows through, reporting what it drew where that shows.
    fn in_strip<R>(
        &mut self,
        sidebar: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
        act: impl FnOnce(&mut Tabs, ScrollView, Rect, &mut Region) -> R,
    ) -> R {
        let (column, view) = self.strip_view(sidebar, scale, theme);
        let mut drew = tairix_controls::damage::sink();
        let acted = act(&mut self.strip, view, column, &mut drew);
        view.report(&drew, damage);
        acted
    }

    /// Run `act` on the pane's form laid out unscrolled, with the view it is
    /// hit through, reporting what it drew where that shows; `None` for a
    /// pane that composes no form.
    ///
    /// While a choice list is open, before the event or after it, the view is
    /// the whole client's: the list hangs out of the column and holds the
    /// pointer until it resolves.
    fn in_form(
        &mut self,
        frame: &ShellFrame,
        (viewport, scale, theme): (Rect, Scale, &Theme),
        damage: &mut Region,
        act: impl FnOnce(&mut Form, ScrollView, FormPlace<'_>, &mut Region) -> FormOutcome,
    ) -> Option<FormOutcome> {
        let (spot, view) = self.pane_view(frame, viewport, scale, theme);
        let form = self.body.form_mut()?;
        let listed = form.is_listing();
        let hit = if listed {
            view.confined_to(viewport)
        } else {
            view
        };
        let mut drew = tairix_controls::damage::sink();
        let acted = act(form, hit, spot, &mut drew);
        let shown = if listed || form.is_listing() {
            view.confined_to(viewport)
        } else {
            view
        };
        shown.report(&drew, damage);
        Some(acted)
    }

    /// The strip laid out unscrolled down `sidebar`, and the view it shows
    /// through.
    fn strip_view(&self, sidebar: Rect, scale: Scale, theme: &Theme) -> (Rect, ScrollView) {
        let column = Rect::new(
            sidebar.left(),
            sidebar.top(),
            sidebar.width,
            self.strip.measured_height(scale, theme).max(sidebar.height),
        );
        let view = ScrollView::new(
            ScrollOrientation::Vertical,
            sidebar,
            self.strip_scroll.model().offset(),
        );
        (column, view)
    }

    /// The pane laid out unscrolled — its column, and the client a choice
    /// list must fit inside, both in the column's own layout — and the view
    /// it shows through.
    fn pane_view<'a>(
        &self,
        frame: &ShellFrame,
        viewport: Rect,
        scale: Scale,
        theme: &'a Theme,
    ) -> (FormPlace<'a>, ScrollView) {
        let content = frame.content;
        let model = self.scroll.model();
        let extent = u32::try_from(model.range().content_extent()).unwrap_or(u32::MAX);
        let column = Rect::new(
            content.left(),
            content.top(),
            content.width,
            extent.max(content.height),
        );
        let view = ScrollView::new(ScrollOrientation::Vertical, content, model.offset());
        let client = Rect::new(
            viewport.left(),
            viewport.top().saturating_add(to_i32(view.offset())),
            viewport.width,
            viewport.height,
        );
        (place(column, client, scale, theme), view)
    }

    /// The pane's own height in a column `width` pixels wide.
    fn content_height(&self, width: u32, scale: Scale, theme: &Theme) -> u32 {
        self.pane_row().map_or(0, |pane| {
            self.body.measured_height(pane, width, scale, theme)
        })
    }

    /// Draw the shell into `surface` filling `viewport`: the window's own
    /// ground and everything on it with `grounds.window`, and what the shell
    /// opens over that content with `grounds.popups`.
    pub fn render(
        &self,
        surface: &mut Surface,
        viewport: Rect,
        scale: Scale,
        grounds: Grounds<'_>,
        artwork: &mut dyn IconArtwork,
    ) {
        let theme = grounds.window;
        let ground = ground_fill(theme, theme.palette().surface, ChromeLayer::Ground);
        surface.fill_rect(0, 0, viewport.width, viewport.height, Color::from(ground));
        let frame = self.frame(viewport, scale, theme);
        self.trail.render(surface, frame.breadcrumb, scale, theme);
        if let Some(panel) = frame.panel {
            // A group's own plate, so the field and the strip read as one
            // object beside the pane's groups.
            FieldGroup::paint_plate(surface, panel, scale, theme);
        }
        if let Some(rect) = frame.search {
            self.search.render(surface, rect, scale, theme);
        }
        if let Some(rect) = frame.sidebar {
            let (column, view) = self.strip_view(rect, scale, theme);
            view.paint(surface, |strip| {
                self.strip.render(strip, column, scale, theme, artwork);
            });
        }
        if let Some(rect) = frame.strip_scrollbar {
            self.strip_scroll.render(surface, rect, scale, theme);
        }
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        if let Some(pane) = self.pane_row() {
            view.paint(surface, |column| {
                self.body.render(column, pane, spot, artwork);
            });
        }
        if let Some(rect) = frame.scrollbar {
            self.scroll.render(surface, rect, scale, theme);
        }
        if let (Some(band), Some(rect)) = (self.footer.as_ref(), frame.footer) {
            band.render(surface, rect, scale, theme);
        }
        // An open choice list hangs over the band and the bar beside the
        // column as well as the plates beneath it.
        let popups = grounds.popups;
        view.confined_to(viewport).paint(surface, |client| {
            self.body.render_popup(
                client,
                FormPlace {
                    theme: popups,
                    ..spot
                },
            );
        });
        // The category list stands over everything it was opened from.
        if let Some(menu) = &self.categories {
            menu.render(
                surface,
                Self::list_rect(menu, &frame, viewport, scale, popups),
                scale,
                popups,
            );
        }
        // And the credential question stands over even that: while it is
        // up it holds the keyboard, and nothing behind it can be reached.
        if let Some(asking) = &self.asking {
            asking.sheet.render(
                surface,
                CredentialSheet::centred_in(viewport, scale),
                scale,
                popups,
            );
        }
    }

    /// Scroll the pane the least that shows the form's keyboard cursor,
    /// reporting the column and its bar when it moved.
    ///
    /// A row the cursor reached but the column does not show is a control
    /// the reader cannot use, which is the same correctness property the
    /// strip's own gutter exists for.
    fn reveal_cursor(
        &mut self,
        frame: &ShellFrame,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let (spot, _) = self.pane_view(frame, viewport, scale, theme);
        let seen = frame.content.height;
        let Some(cursor) = self
            .body
            .form()
            .and_then(|form| form.cursor_reveal(spot, seen))
        else {
            return;
        };
        if reveal(&mut self.scroll, spot.bounds, cursor) {
            damage.add(frame.content);
            if let Some(bar) = frame.scrollbar {
                damage.add(bar);
            }
        }
    }

    /// Route one pointer event.
    ///
    /// A round that repainted anything changed the screen, whatever else it
    /// concluded, so a hover moving along the strip is presented like a
    /// press is.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        let before = self.placement();
        let mut drew = tairix_controls::damage::sink();
        let outcome = self.route_pointer(event, viewport, scale, theme, &mut drew);
        if self.placement() != before {
            self.rehover(viewport, scale, theme, &mut drew);
        }
        adopt_drawn(outcome, &drew, damage)
    }

    /// Route one pointer event to the region it concerns.
    fn route_pointer(
        &mut self,
        event: &InputEvent,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = Some(*to);
        }
        // The credential question is modal: while it is up nothing behind
        // it can be pressed, so a stray click cannot change a pane the
        // reader is about to authenticate for.
        if self.asking.is_some() {
            return self.asked(viewport, scale, damage, |sheet, bounds, damage| {
                sheet.on_pointer(event, bounds, scale, theme, damage)
            });
        }
        let frame = self.frame(viewport, scale, theme);

        // Both lists stand over the bands they hang across and hold the
        // pointer until they resolve, so they are asked before anything
        // drawn beneath them.
        if let Some(menu) = &mut self.categories {
            let rect = Self::list_rect(menu, &frame, viewport, scale, theme);
            let acted = menu.on_pointer(event, rect, scale, theme, damage);
            return self.list_acted(acted, viewport, scale, theme, damage);
        }
        if self.body.is_listing() {
            return self.pressed_pane(event, &frame, viewport, scale, theme, damage);
        }

        // Each region learns the pointer has left it, so nothing there stays
        // lit for a pointer that is elsewhere.
        if matches!(event, InputEvent::PointerMoved { .. }) {
            let now = self.pointer.and_then(|at| region_at(&frame, at));
            if let Some(left) = self.under.filter(|left| Some(*left) != now) {
                self.left(left, event, &frame, viewport, (scale, theme), damage);
            }
            self.under = now;
        }

        if let Some(outcome) = self.pressed_band(event, &frame, viewport, scale, theme, damage) {
            return outcome;
        }
        if let Some(rect) = frame.scrollbar {
            if self.points_into(rect) || self.scroll.is_pressing() {
                let acted = self.scroll.on_pointer(event, rect, scale, theme, damage);
                return ShellOutcome::of(scrolled(frame.content, acted, damage));
            }
        }
        if self.points_into(frame.content) {
            if let InputEvent::PointerScrolled { dx, dy } = event {
                let Some(bar) = frame.scrollbar else {
                    return ShellOutcome::Idle;
                };
                let acted = self.scroll.wheel(*dx, *dy, scale, bar, damage);
                return ShellOutcome::of(scrolled(frame.content, acted, damage));
            }
            return self.pressed_pane(event, &frame, viewport, scale, theme, damage);
        }
        self.pressed_chrome(event, &frame, viewport, scale, theme, damage)
    }

    /// Show `region` the move that took the pointer out of it.
    ///
    /// A bar holding a drag is left alone: its grab is still routed every
    /// move.
    fn left(
        &mut self,
        region: Under,
        event: &InputEvent,
        frame: &ShellFrame,
        viewport: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        match region {
            Under::Search => {
                if let Some(rect) = frame.search {
                    self.search.on_pointer(event, rect, scale, theme, damage);
                }
            }
            Under::Strip => {
                if let Some(rect) = frame.sidebar {
                    self.in_strip(rect, (scale, theme), damage, |strip, view, column, drew| {
                        strip.on_pointer(&view.event_in_layout(event), column, scale, theme, drew)
                    });
                }
            }
            Under::StripBar => {
                if let Some(rect) = frame
                    .strip_scrollbar
                    .filter(|_| !self.strip_scroll.is_pressing())
                {
                    self.strip_scroll
                        .on_pointer(event, rect, scale, theme, damage);
                }
            }
            Under::Trail => {
                self.trail
                    .on_pointer(event, frame.breadcrumb, scale, theme, damage);
            }
            Under::Pane => {
                self.pressed_pane(event, frame, viewport, scale, theme, damage);
            }
            Under::PaneBar => {
                if let Some(rect) = frame.scrollbar.filter(|_| !self.scroll.is_pressing()) {
                    self.scroll.on_pointer(event, rect, scale, theme, damage);
                }
            }
            Under::Band => {
                if let (Some(band), Some(rect)) = (self.footer.as_mut(), frame.footer) {
                    band.on_pointer(event, rect, scale, theme, damage);
                }
            }
        }
    }

    /// Route a press into the pane column's form.
    fn pressed_pane(
        &mut self,
        event: &InputEvent,
        frame: &ShellFrame,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        let cursor = self.body.form().and_then(Form::cursor);
        let acted = self.in_form(
            frame,
            (viewport, scale, theme),
            damage,
            |form, view, spot, drew| form.on_pointer(&view.event_in_layout(event), spot, drew),
        );
        // A press that moved the form's cursor took the keyboard into the pane.
        let took = self.body.form().and_then(Form::cursor) != cursor;
        let acted = acted.filter(|acted| !matches!(acted, FormOutcome::Idle));
        if took || (acted.is_some() && is_press(event)) {
            self.focus_on(Focus::Content, viewport, scale, theme, damage);
        }
        let Some(acted) = acted else {
            return ShellOutcome::of(took);
        };
        if matches!(acted, FormOutcome::Staged) {
            self.restate_staged(frame, viewport, scale, theme, damage);
        }
        self.refit(viewport, scale, theme, damage);
        outcome_of(acted)
    }

    /// Lay the pane out again where a choice rebuilt its form — another
    /// screensaver brings its own options — reporting the whole pane band,
    /// since the rows beneath the choice are new and the column may have
    /// grown or shrunk.
    fn refit(&mut self, viewport: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        if !self
            .body
            .form_mut()
            .is_some_and(crate::form::Form::take_reshaped)
        {
            return;
        }
        self.measure_pane(viewport, scale, theme);
        self.pictures_moved();
        damage.add(pane_band(&self.frame(viewport, scale, theme), viewport));
    }

    /// Route a press into the chrome around the pane: the search field,
    /// the strip and its gutter, and the location trail.
    fn pressed_chrome(
        &mut self,
        event: &InputEvent,
        frame: &ShellFrame,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        if let Some(rect) = frame.search {
            if self.points_into(rect) {
                let acted = self.search.on_pointer(event, rect, scale, theme, damage);
                if is_press(event) {
                    self.focus_on(Focus::Search, viewport, scale, theme, damage);
                }
                return self.searched(acted, viewport, scale, theme, damage);
            }
        }
        if let Some(rect) = frame.sidebar {
            if self.points_into(rect) {
                if let InputEvent::PointerScrolled { dx, dy } = event {
                    let Some(bar) = frame.strip_scrollbar else {
                        return ShellOutcome::Idle;
                    };
                    let acted = self.strip_scroll.wheel(*dx, *dy, scale, bar, damage);
                    return ShellOutcome::of(scrolled(rect, acted, damage));
                }
                let acted =
                    self.in_strip(rect, (scale, theme), damage, |strip, view, column, drew| {
                        strip.on_pointer(&view.event_in_layout(event), column, scale, theme, drew)
                    });
                if is_press(event) {
                    self.focus_on(Focus::Strip, viewport, scale, theme, damage);
                }
                if let Some(TabsAction::Selected { index }) = acted {
                    return ShellOutcome::of(self.choose(index, viewport, scale, theme, damage));
                }
                return ShellOutcome::Idle;
            }
        }
        if let (Some(rect), Some(sidebar)) = (frame.strip_scrollbar, frame.sidebar) {
            if self.points_into(rect) || self.strip_scroll.is_pressing() {
                let acted = self
                    .strip_scroll
                    .on_pointer(event, rect, scale, theme, damage);
                return ShellOutcome::of(scrolled(sidebar, acted, damage));
            }
        }
        if self.points_into(frame.breadcrumb) {
            let acted = self
                .trail
                .on_pointer(event, frame.breadcrumb, scale, theme, damage);
            if is_press(event) {
                self.focus_on(Focus::Trail, viewport, scale, theme, damage);
            }
            return ShellOutcome::of(self.navigated(acted, viewport, scale, theme, damage));
        }
        ShellOutcome::Idle
    }

    /// Route one key press, presented whenever it repainted anything.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        let before = self.placement();
        let mut drew = tairix_controls::damage::sink();
        let outcome = self.route_key(stroke, viewport, scale, theme, &mut drew);
        if self.placement() != before {
            self.rehover(viewport, scale, theme, &mut drew);
        }
        adopt_drawn(outcome, &drew, damage)
    }

    /// When a password marker on show next moves its dots — the credential
    /// question's, or a masked entry's on the pane — on the clock the
    /// keystrokes were taken by.
    #[must_use]
    pub fn secret_deadline_ns(&self) -> Option<u64> {
        let asking = self
            .asking
            .as_ref()
            .and_then(|asking| asking.sheet.deadline_ns());
        let pane = self.body.form().and_then(Form::secret_deadline_ns);
        asking.into_iter().chain(pane).min()
    }

    /// Step every password marker on show to `now_ns`, reporting what moved.
    pub fn advance_secrets(
        &mut self,
        now_ns: u64,
        viewport: Rect,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        if let Some(asking) = self.asking.as_mut() {
            let bounds = CredentialSheet::centred_in(viewport, scale);
            asking.sheet.advance(now_ns, bounds, scale, damage);
        }
        let pane_due = self.body.form().and_then(Form::secret_deadline_ns);
        if pane_due.is_none_or(|due| due > now_ns) {
            return;
        }
        let frame = self.frame(viewport, scale, theme);
        let _ = self.in_form(
            &frame,
            (viewport, scale, theme),
            damage,
            |form, _, spot, drew| {
                form.advance_secrets(now_ns, spot, drew);
                FormOutcome::Idle
            },
        );
    }

    /// Where the columns are scrolled to and how often they have been laid
    /// out: what moves the content under a pointer that did not move.
    fn placement(&self) -> (u64, u64, u64) {
        (
            self.scroll.model().offset(),
            self.strip_scroll.model().offset(),
            self.layouts,
        )
    }

    /// Replay the resting pointer as a move, so every region lights what now
    /// lies under it rather than what lay there before its content moved.
    ///
    /// A move neither presses nor takes the keyboard cursor anywhere, so the
    /// replay re-derives only what follows the pointer.
    fn rehover(&mut self, viewport: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        let Some(to) = self.pointer else {
            return;
        };
        self.route_pointer(
            &InputEvent::PointerMoved { to },
            viewport,
            scale,
            theme,
            damage,
        );
    }

    /// Whether the pointer rests inside `rect`.
    fn points_into(&self, rect: Rect) -> bool {
        self.pointer.is_some_and(|at| rect.contains(at))
    }

    /// Route one key press to the region holding the keyboard cursor.
    fn route_key(
        &mut self,
        stroke: Keystroke,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        if self.asking.is_some() {
            return self.asked(viewport, scale, damage, |sheet, bounds, damage| {
                sheet.on_key(stroke, bounds, scale, theme, damage)
            });
        }
        let Keystroke { key, modifiers, .. } = stroke;
        let frame = self.frame(viewport, scale, theme);

        if let Some(menu) = &mut self.categories {
            let rect = Self::list_rect(menu, &frame, viewport, scale, theme);
            let acted = menu.on_key(key, rect, scale, theme, damage);
            return self.list_acted(acted, viewport, scale, theme, damage);
        }
        if key == Key::Named(NamedKey::Tab) {
            self.step_focus(!modifiers.shift, viewport, scale, theme, damage);
            return ShellOutcome::Changed;
        }
        match self.focus {
            Focus::Search => {
                let Some(rect) = frame.search else {
                    return ShellOutcome::Idle;
                };
                let acted = self.search.on_key(key, modifiers, rect, damage);
                self.searched(acted, viewport, scale, theme, damage)
            }
            Focus::Trail => {
                let acted = self
                    .trail
                    .on_key(key, frame.breadcrumb, scale, theme, damage);
                ShellOutcome::of(self.navigated(acted, viewport, scale, theme, damage))
            }
            Focus::Strip => {
                let Some(rect) = frame.sidebar else {
                    return ShellOutcome::Idle;
                };
                let before = self.strip.current();
                let acted =
                    self.in_strip(rect, (scale, theme), damage, |strip, _, column, drew| {
                        strip.on_key(key, column, scale, theme, drew)
                    });
                let revealed = match self.strip.current() {
                    Some(cursor) => self.reveal_row(cursor, &frame, scale, theme, damage),
                    None => false,
                };
                let applied = match acted {
                    Some(TabsAction::Selected { index }) => {
                        self.choose(index, viewport, scale, theme, damage)
                    }
                    Some(TabsAction::Disclose { index, open }) => match self.rows.get(index) {
                        Some(&StripRow::Category(category)) => {
                            self.disclose(category, open, viewport, scale, theme, damage)
                        }
                        Some(StripRow::Pane(..)) | None => false,
                    },
                    None => self.strip.current() != before,
                };
                ShellOutcome::of(applied || revealed)
            }
            Focus::Content => {
                let acted = self.in_form(
                    &frame,
                    (viewport, scale, theme),
                    damage,
                    |form, _, spot, drew| form.on_key(stroke, spot, drew),
                );
                if let Some(acted) = acted.filter(|acted| !matches!(acted, FormOutcome::Idle)) {
                    if matches!(acted, FormOutcome::Staged) {
                        self.restate_staged(&frame, viewport, scale, theme, damage);
                    }
                    self.refit(viewport, scale, theme, damage);
                    // A staged edit or a rebuilt pane can change the column's
                    // extent, and so the frame, before the cursor is scrolled
                    // to.
                    let frame = self.frame(viewport, scale, theme);
                    self.reveal_cursor(&frame, viewport, scale, theme, damage);
                    return outcome_of(acted);
                }
                let Some(rect) = frame.scrollbar else {
                    return ShellOutcome::Idle;
                };
                let acted = self.scroll.on_key(key, rect, damage);
                ShellOutcome::of(scrolled(frame.content, acted, damage))
            }
            Focus::Footer => {
                let acted = self.footer.as_mut().and_then(|band| band.on_key(key));
                self.commanded(acted, viewport, scale, theme, damage)
            }
        }
    }

    /// Route a press in the pane's action band, or answer `None` when the
    /// pointer is not over one.
    fn pressed_band(
        &mut self,
        event: &InputEvent,
        frame: &ShellFrame,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<ShellOutcome> {
        let rect = frame.footer.filter(|rect| self.points_into(*rect))?;
        let acted = self
            .footer
            .as_mut()
            .and_then(|band| band.on_pointer(event, rect, scale, theme, damage));
        Some(self.commanded(acted, viewport, scale, theme, damage))
    }

    /// Route one event into the credential question standing over the
    /// window, `answer` being what the sheet made of it at the bounds it is
    /// drawn in.
    ///
    /// A cancellation takes it down and changes nothing anywhere. An offer
    /// is handed to the caller as the one run to ask the broker for, and
    /// the question stays up: only the caller's verdict takes it down, so a
    /// refusal can be corrected without retyping the account.
    fn asked(
        &mut self,
        viewport: Rect,
        scale: Scale,
        damage: &mut Region,
        answer: impl FnOnce(&mut CredentialSheet, Rect, &mut Region) -> Option<CredentialAction>,
    ) -> ShellOutcome {
        let bounds = CredentialSheet::centred_in(viewport, scale);
        let Some(asking) = self.asking.as_mut() else {
            return ShellOutcome::Idle;
        };
        match answer(&mut asking.sheet, bounds, damage) {
            Some(CredentialAction::Cancelled) => {
                self.asking = None;
                damage.add(viewport);
                ShellOutcome::Changed
            }
            // The sheet refuses an unofferable password itself, so this only
            // ever elevates with one.
            Some(CredentialAction::Offered) => {
                asking
                    .sheet
                    .secret()
                    .map_or(ShellOutcome::Changed, |secret| {
                        ShellOutcome::Elevate(Elevation {
                            account: String::from(asking.sheet.account()),
                            password: secret.as_bytes().to_vec(),
                            program: asking.program,
                            argv: asking.argv.clone(),
                            mode: asking.mode,
                        })
                    })
            }
            None => ShellOutcome::Changed,
        }
    }

    /// Adopt what the elevated run came to.
    ///
    /// Persist-then-adopt: the rows showed a working copy, and the durable
    /// value is whatever the store answers on the next read, which the
    /// caller takes as soon as a run succeeds. A refusal leaves the working
    /// copy exactly as it was so the reader can correct it rather than
    /// retype it, and states why.
    pub fn adopt_elevation(&mut self, outcome: Elevated) {
        match outcome {
            Elevated::Finished(0) => {
                self.asking = None;
                self.settled();
            }
            Elevated::Finished(_) => self.refuse(String::from(
                "The command ran but did not accept the change.",
            )),
            Elevated::Printed(0, output) => {
                let captures = self.asking.as_ref().and_then(|asking| asking.captures);
                self.asking = None;
                match captures {
                    Some(Captures::Addressing) => {
                        self.read_addressing(Addressing::from_listing(&output));
                    }
                    Some(Captures::Roster) => {
                        self.read_roster(Roster::from_capture(&output));
                    }
                    None => {}
                }
            }
            Elevated::Printed(..) => self.refuse(String::from(
                "The command ran but could not read the configuration.",
            )),
            // The run happened; the reply could not carry what it printed.
            // Stated in the pane rather than as a refusal of the question,
            // because asking again would answer the same.
            Elevated::Overran => {
                let captures = self.asking.as_ref().and_then(|asking| asking.captures);
                self.asking = None;
                match captures {
                    Some(Captures::Addressing) => self.read_addressing(Addressing::Overran),
                    Some(Captures::Roster) => self.read_roster(Roster::Overran),
                    None => {}
                }
            }
            Elevated::Refused(ElevateRefusal::Credentials) => {
                self.refuse(String::from(CREDENTIAL_REFUSED_REASON));
            }
            Elevated::Refused(ElevateRefusal::NotRun(reason)) => self.refuse(reason),
        }
    }

    /// Adopt what a run that wrote a store came to.
    ///
    /// The machine's store is re-read, because that reading is free and a
    /// working copy declared durable without one would be a guess. The
    /// network store cannot be re-read without another password, so the
    /// pane records the document it asked for and says so: `configure`
    /// writes every named pair or none, and both sides render through the
    /// same engine, so a clean exit wrote exactly what was staged.
    fn settled(&mut self) {
        if self.body.stages_machine_settings() {
            self.wanted.arm(Reading::Config);
        }
        // An account change went in whole or was refused whole, so the
        // listing moves on to hold it rather than being dropped and
        // re-read — re-reading costs another password, and the reader is
        // left where they were for the next change. The public directories
        // are free, so they are re-read at once.
        if self
            .body
            .composition()
            .is_some_and(Composition::reads_roster)
        {
            self.wanted.arm(Reading::Accounts);
            if let Some(form) = self.body.form_mut() {
                form.adopt_applied();
            }
            if let Some(moved) = self.body.form().map(|form| form.roster().clone()) {
                self.accounts.roster = moved;
            }
        }
        let written = self
            .body
            .form()
            .filter(|form| form.composition().reads_addressing())
            .and_then(Form::proposal)
            .and_then(Result::ok);
        if let Some(written) = written {
            self.network.addressing = Addressing::Listed(written.clone());
            if let Some(form) = self.body.form_mut() {
                form.adopt_written(written);
            }
        }
        if let Some(band) = self.footer.as_mut() {
            band.settled();
        }
    }

    /// Adopt what a capture of the machine's addressing came to.
    ///
    /// A capture that lands for a pane the window has since left is
    /// **dropped**: the desktop can send the window elsewhere while a run
    /// is in flight, and a privileged reading installed for a surface that
    /// is no longer showing is one this application has no business
    /// holding.
    fn read_addressing(&mut self, addressing: Addressing) {
        if !self.captured_for(Captures::Addressing) {
            return;
        }
        self.network.addressing = addressing;
        if let Some(form) = self.body.form_mut() {
            form.adopt_addressing(&self.network.addressing);
        }
        // The pane has a working copy where it had none, so its band is a
        // different band: Revert and Apply rather than the reading.
        self.footer = self.band();
    }

    /// Adopt what a capture of the account listing came to, on exactly the
    /// terms the addressing capture is adopted on.
    fn read_roster(&mut self, roster: Roster) {
        if !self.captured_for(Captures::Roster) {
            return;
        }
        self.accounts.roster = roster;
        if let Some(form) = self.body.form_mut() {
            form.adopt_roster(self.accounts.roster.clone());
        }
        self.footer = self.band();
    }

    /// Whether the pane on show is the one `captures` was asked for.
    fn captured_for(&self, captures: Captures) -> bool {
        self.body
            .composition()
            .is_some_and(|composition| captures.asked_by(composition))
    }

    /// State a refusal on the question that is up, or in the band when the
    /// question has already gone.
    fn refuse(&mut self, reason: String) {
        if let Some(asking) = self.asking.as_mut() {
            asking.sheet.refuse(&reason);
        }
        if let Some(band) = self.footer.as_mut() {
            band.state(Standing::Refused(reason));
        }
    }

    /// Whether a credential question is standing over the window.
    #[must_use]
    pub const fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Adopt what the pane's action band reported.
    fn commanded(
        &mut self,
        acted: Option<FooterAction>,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        match acted {
            Some(FooterAction::Revert) => {
                if let Some(form) = self.body.form_mut() {
                    form.revert();
                }
                self.restate_footer();
                self.measure(viewport, scale, theme);
                damage.add(viewport);
                ShellOutcome::Changed
            }
            Some(FooterAction::Apply) => {
                self.ask_to_apply();
                damage.add(viewport);
                ShellOutcome::Changed
            }
            None => ShellOutcome::Changed,
        }
    }

    /// Put the credential question up for whatever the pane's command
    /// needs an account for.
    ///
    /// A staged pane's is the one `configure` run that writes every row
    /// that differs, together, so the document is rendered once and a
    /// partly-applied store is not a state this can reach. A reading's is
    /// the application that owns its subject, started and left running.
    fn ask_to_apply(&mut self) {
        let asking = if self.stageable() {
            self.staging()
        } else {
            self.reading()
        };
        if let Some(asking) = asking {
            self.asking = Some(asking);
        }
    }

    /// The run that applies what the pane on show has staged.
    ///
    /// A networking pane's document is checked whole first: an interface
    /// half-moved off a static address is a change the store would refuse,
    /// and saying so here names what is wrong rather than leaving the
    /// reader with a run that failed.
    fn staging(&mut self) -> Option<Asking> {
        let composition = self.body.composition()?;
        if composition.reads_roster() {
            return self.staging_account();
        }
        let reads_addressing = composition.reads_addressing();
        if reads_addressing {
            if let Some(Err(err)) = self.body.form().and_then(Form::proposal) {
                self.refuse(alloc::format!("{err}"));
                return None;
            }
        }
        let argv = self.body.form().map(configure_argv).unwrap_or_default();
        if argv.is_empty() {
            return None;
        }
        let purpose = if reads_addressing {
            SET_ADDRESSING_PURPOSE
        } else {
            SET_MACHINE_PURPOSE
        };
        self.carried(CONFIGURE_RUN_PATH, argv, purpose)
    }

    /// The run that applies what the Users pane has staged.
    ///
    /// One account and one tool, or nothing: a change spanning two
    /// accounts, or a password together with the fields beside it, needs
    /// two runs — and half of a change made durable is exactly the state
    /// the one-invocation rule exists to prevent. The salt is *consumed*
    /// here, so the next password is hashed under a fresh one.
    fn staging_account(&mut self) -> Option<Asking> {
        let run = match self.body.form()?.account_run(self.salt)? {
            Ok(run) => run,
            Err(err) => {
                self.refuse(String::from(err.reason()));
                return None;
            }
        };
        self.salt = None;
        self.carried(run.program, run.argv, SET_ACCOUNT_PURPOSE)
    }

    /// The credential question for a run of `program` with `argv`, or a
    /// stated refusal where the seam could not carry it.
    ///
    /// Checked against the seam's own bound rather than a second copy of
    /// it, and before the reader has typed a password: a change too large
    /// for one request is never split across two runs.
    fn carried(
        &mut self,
        program: &'static str,
        argv: Vec<String>,
        purpose: &'static str,
    ) -> Option<Asking> {
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        if ElevateArgv::new(&borrowed).is_err() {
            self.refuse(String::from(TOO_MANY_CHANGES));
            return None;
        }
        Some(Asking {
            sheet: CredentialSheet::new(ASK_TITLE, purpose),
            program,
            argv,
            mode: RunMode::Wait,
            captures: None,
        })
    }

    /// The run behind the one command a pane with no working copy offers.
    fn reading(&self) -> Option<Asking> {
        match self.pane_row()?.content()? {
            PaneContent::Clock => Some(Asking {
                sheet: CredentialSheet::new(ASK_TITLE, SET_CLOCK_PURPOSE),
                program: DATETIME_RUN_PATH,
                argv: Vec::new(),
                mode: RunMode::Leave,
                captures: None,
            }),
            // A read, not a write: the same tool, run with no operand, and
            // what it prints is the answer. This application may not read
            // that store itself and never will, so the authenticated run
            // is the only way the pane can state it.
            PaneContent::Form(composition) if composition.reads_addressing() => Some(Asking {
                sheet: CredentialSheet::new(ASK_TITLE, SHOW_ADDRESSING_PURPOSE),
                program: CONFIGURE_RUN_PATH,
                argv: Vec::new(),
                mode: RunMode::Capture,
                captures: Some(Captures::Addressing),
            }),
            // A read on the same terms: the whole account registry is
            // gated, so what an authenticated listing printed is the only
            // way this pane can state another account's fields at all. The
            // interactive session cannot serve it — a captured run has its
            // standard input closed — so the non-interactive listing is
            // what is asked for.
            PaneContent::Form(composition) if composition.reads_roster() => Some(Asking {
                sheet: CredentialSheet::new(ASK_TITLE, SHOW_ACCOUNTS_PURPOSE),
                program: USERS_RUN_PATH,
                argv: alloc::vec![String::from("-l")],
                mode: RunMode::Capture,
                captures: Some(Captures::Roster),
            }),
            PaneContent::Form(_) | PaneContent::About | PaneContent::Volumes => None,
        }
    }

    /// Adopt a search edit: the strip is rebuilt from the query, and a
    /// submitted one shows the first pane it reached, so a search always lands
    /// on something it matched.
    ///
    /// The first *pane*, not the first row: a category reached through one of
    /// its panes heads the list, and the pane beneath it is what matched.
    fn searched(
        &mut self,
        acted: Option<TextAction>,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        let Some(action) = acted else {
            return ShellOutcome::Idle;
        };
        match action {
            TextAction::Cancelled => self.search.set_text(""),
            TextAction::Edited | TextAction::Submitted => {}
        }
        self.restate_strip(None, viewport, scale, theme, damage);
        if matches!(action, TextAction::Submitted) {
            if let Some(location) = self.rows.iter().find_map(|row| row.destination()) {
                self.show(location, viewport, scale, theme, damage);
            }
        }
        ShellOutcome::Changed
    }

    /// Adopt a trail activation: a crumb other than the trailing one goes
    /// back, and the leading crumb opens the category list once the strip has
    /// been shed and there is no strip to walk.
    fn navigated(
        &mut self,
        acted: Option<BreadcrumbAction>,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let frame = self.frame(viewport, scale, theme);
        let Some(BreadcrumbAction::Activate { index }) = acted else {
            return false;
        };
        if index == 0 {
            if frame.sidebar.is_some() {
                // The strip is on screen, so the category list would be a
                // second way to the same rows.
                self.focus_on(Focus::Strip, viewport, scale, theme, damage);
                return true;
            }
            self.open_category_list(&frame, (viewport, scale, theme), damage);
            return true;
        }
        // The middle crumb is the category on show: going to it shows that
        // category's first pane.
        let Some(location) = self
            .location
            .category
            .row()
            .and_then(CategoryRow::first_pane)
            .map(|pane| Location {
                category: self.location.category,
                pane: pane.pane,
            })
        else {
            return false;
        };
        self.show(location, viewport, scale, theme, damage);
        true
    }

    /// Adopt the strip row at `index`: show its pane, or — for a category that
    /// discloses its panes — open or close their list, leaving every other
    /// list as it was.
    ///
    /// A search lists a category's matches whatever is open, so while one is
    /// in force there is no list to close, and the category's row goes to the
    /// first of the matches listed beneath it instead.
    fn choose(
        &mut self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let Some(&row) = self.rows.get(index) else {
            return false;
        };
        if let Some(location) = row.destination() {
            self.show(location, viewport, scale, theme, damage);
            return true;
        }
        let StripRow::Category(category) = row else {
            return false;
        };
        if self.search.text().is_empty() {
            let open = !self.open.is_open(&category);
            return self.disclose(category, open, viewport, scale, theme, damage);
        }
        let first_match =
            first_listed_pane(&self.rows, index, category).and_then(StripRow::destination);
        let Some(location) = first_match else {
            return false;
        };
        self.show(location, viewport, scale, theme, damage);
        true
    }

    /// Show or hide `category`'s panes in the strip, answering whether the
    /// strip changed; the keyboard cursor stays on the category's row.
    ///
    /// A search lists matches whatever is open, so it is a no-op while one is
    /// in force rather than a change the reader could not see.
    fn disclose(
        &mut self,
        category: Category,
        open: bool,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        if !self.search.text().is_empty() || !self.open.set(category, open) {
            return false;
        }
        self.restate_strip(
            Some(StripRow::Category(category)),
            viewport,
            scale,
            theme,
            damage,
        );
        true
    }

    /// Show `location`: its category's panes listed, the strip restated, the
    /// trail rewritten, the pane re-measured, and its scroll reset to the top.
    ///
    /// The whole pane band is reported rather than the column alone, because a
    /// pane of a different height may have gained or lost the scrollbar
    /// beside it.
    fn show(
        &mut self,
        location: Location,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let frame = self.frame(viewport, scale, theme);
        self.location = location;
        list_pane_of(&mut self.open, location);
        self.scroll.set_model(self.scroll.model().scroll_to(0));
        self.restate_body();
        self.restate_trail();
        self.measure_pane(viewport, scale, theme);
        self.restate_strip(None, viewport, scale, theme, damage);
        damage.add(frame.breadcrumb);
        damage.add(pane_band(&frame, viewport));
    }

    /// Rebuild the strip from the registry for the sections open and the
    /// current query, and scroll to the row a reader expects to see: `cursor`
    /// while the strip still lists it, else the row on show — the row the
    /// keyboard cursor is put on too, while the strip holds it.
    fn restate_strip(
        &mut self,
        cursor: Option<StripRow>,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.rows = strip_rows(&self.open, self.search.text());
        self.strip.restate(strip_of(&self.rows, self.location));
        self.measure_strip(viewport, scale, theme);
        let frame = self.frame(viewport, scale, theme);
        let at = cursor
            .and_then(|kept| self.rows.iter().position(|row| *row == kept))
            .or_else(|| self.selected_row());
        if let (Focus::Strip, Some(rect)) = (self.focus, frame.sidebar) {
            self.in_strip(rect, (scale, theme), damage, |strip, _, column, drew| {
                strip.set_current(at, column, scale, theme, drew);
            });
        }
        if let Some(index) = at {
            self.reveal_row(index, &frame, scale, theme, damage);
        }
        if let Some(rect) = frame.sidebar {
            damage.add(rect);
        }
        if let Some(rect) = frame.strip_scrollbar {
            damage.add(rect);
        }
    }

    /// Rewrite the location trail for where the surface is.
    fn restate_trail(&mut self) {
        let mut crumbs = Vec::with_capacity(3);
        crumbs.push(Crumb::new(ROOT_CRUMB));
        if let Some((category, pane)) = self.location.rows() {
            crumbs.push(Crumb::new(category.label));
            // A category holding one pane shares its name, and a trail that
            // said it twice would read as two places.
            if category.discloses() {
                crumbs.push(Crumb::new(pane.title));
            }
        }
        self.trail = Breadcrumb::new(crumbs);
    }

    /// Open the category list the shed strip becomes, grouped as the strip
    /// is, reporting the plate it opens into.
    fn open_category_list(
        &mut self,
        frame: &ShellFrame,
        (viewport, scale, theme): (Rect, Scale, &Theme),
        damage: &mut Region,
    ) {
        let mut above = None;
        let mut menu = Menu::new(
            CATEGORIES
                .iter()
                .map(|row| {
                    let item = MenuItem::new(row.label).with_group_break(row.breaks_from(above));
                    above = Some(row);
                    item
                })
                .collect(),
        );
        menu.adopt_current(
            CATEGORIES
                .iter()
                .position(|row| row.category == self.location.category),
        );
        damage.add(Self::list_rect(&menu, frame, viewport, scale, theme));
        self.categories = Some(menu);
    }

    /// Adopt what the open category list reported.
    fn list_acted(
        &mut self,
        acted: Option<MenuAction>,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ShellOutcome {
        // The list's own rectangle, resolved while it is still open, so
        // closing it reports the pixels it actually covered.
        let frame = self.frame(viewport, scale, theme);
        let rect = self.categories.as_ref().map_or(Rect::EMPTY, |menu| {
            Self::list_rect(menu, &frame, viewport, scale, theme)
        });
        match acted {
            Some(MenuAction::Activated { index }) => {
                self.categories = None;
                damage.add(rect);
                let chosen = CATEGORIES
                    .get(index)
                    .and_then(|row| StripRow::Category(row.category).location());
                match chosen {
                    Some(location) => {
                        self.show(location, viewport, scale, theme, damage);
                        ShellOutcome::Changed
                    }
                    None => ShellOutcome::Changed,
                }
            }
            Some(MenuAction::Dismissed) => {
                self.categories = None;
                damage.add(rect);
                ShellOutcome::Changed
            }
            Some(MenuAction::OpenSubmenu { .. }) | None => ShellOutcome::Idle,
        }
    }

    /// Where the category list is drawn: hanging off the trail's leading
    /// crumb, placed by the one shared plate rule so it never leaves the
    /// client.
    fn list_rect(
        menu: &Menu,
        frame: &ShellFrame,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Rect {
        let anchor = Rect::new(
            frame.breadcrumb.left(),
            frame.breadcrumb.top(),
            frame.breadcrumb.height,
            frame.breadcrumb.height,
        );
        plate_rect(
            menu.preferred_width(scale, theme),
            menu.preferred_height(scale, theme),
            PlatePlacement {
                anchor,
                side: PlateSide::Below,
                gap: 0,
            },
            viewport,
        )
    }

    /// The focus ring for `frame`: every region the frame actually seated, in
    /// Tab order.
    fn ring(&self, frame: &ShellFrame) -> Vec<Focus> {
        let mut ring = Vec::with_capacity(5);
        if frame.search.is_some() {
            ring.push(Focus::Search);
        }
        ring.push(Focus::Trail);
        if frame.sidebar.is_some() {
            ring.push(Focus::Strip);
        }
        // A pane composing controls is reachable whether or not it is long
        // enough to scroll; one that only scrolls is reachable only when
        // there is something to scroll.
        if self.body.composes_controls() || frame.scrollbar.is_some() {
            ring.push(Focus::Content);
        }
        if frame.footer.is_some() {
            ring.push(Focus::Footer);
        }
        ring
    }

    /// Move the cursor one step round the ring.
    fn step_focus(
        &mut self,
        forward: bool,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let ring = self.ring(&self.frame(viewport, scale, theme));
        if ring.is_empty() {
            return;
        }
        let at = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if forward {
            (at + 1) % ring.len()
        } else {
            (at + ring.len() - 1) % ring.len()
        };
        let Some(&focus) = ring.get(next) else {
            return;
        };
        self.focus_on(focus, viewport, scale, theme, damage);
    }

    /// Put the cursor on `focus`, so exactly one region reads as focused.
    fn focus_on(
        &mut self,
        focus: Focus,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let frame = self.frame(viewport, scale, theme);
        if !self.ring(&frame).contains(&focus) || self.focus == focus {
            return;
        }
        self.focus = focus;
        self.search.set_focused(focus == Focus::Search);
        self.trail.adopt_focus((focus == Focus::Trail).then_some(0));
        self.scroll
            .set_focused(focus == Focus::Content && !self.body.composes_controls());
        if let Some(form) = self.body.form_mut() {
            form.set_focused(focus == Focus::Content);
            damage.add(frame.content);
        }
        if let Some(rect) = frame.sidebar {
            let cursor = (focus == Focus::Strip)
                .then(|| self.selected_row())
                .flatten();
            self.in_strip(rect, (scale, theme), damage, |strip, _, column, drew| {
                strip.set_current(cursor, column, scale, theme, drew);
            });
        }
        if let Some(rect) = frame.search {
            damage.add(rect);
        }
        damage.add(frame.breadcrumb);
        if let Some(rect) = frame.scrollbar {
            damage.add(rect);
        }
        if let Some(band) = self.footer.as_mut() {
            band.set_focused(focus == Focus::Footer);
        }
        if let Some(rect) = frame.footer {
            damage.add(rect);
        }
    }

    /// The readings the pane on show states, for a test that asks what a
    /// reader would see rather than comparing pixels.
    #[cfg(test)]
    pub(crate) fn facts_for_test(&self) -> Option<&crate::facts::Facts> {
        match &self.body {
            Body::Facts(facts) => Some(facts),
            Body::Statement | Body::Form(_) | Body::Volumes(_) => None,
        }
    }

    /// Where the pane's action band draws each of its commands, so a test
    /// presses the command the band actually placed rather than arithmetic
    /// of its own.
    #[cfg(test)]
    pub(crate) fn action_rects(&self, viewport: Rect, scale: Scale, theme: &Theme) -> Vec<Rect> {
        let frame = self.frame(viewport, scale, theme);
        match (self.footer.as_ref(), frame.footer) {
            (Some(band), Some(rect)) => band.command_rects(rect, scale, theme),
            _ => Vec::new(),
        }
    }

    /// The location trail's labels, in order.
    #[cfg(test)]
    pub(crate) fn trail_labels(&self) -> Vec<&str> {
        self.trail.crumbs().iter().map(Crumb::label).collect()
    }

    /// The pane's own height in a column `width` pixels wide.
    #[cfg(test)]
    pub(crate) fn pane_height(&self, width: u32, scale: Scale, theme: &Theme) -> u32 {
        self.content_height(width, scale, theme)
    }

    /// How far the pane column is scrolled, in physical pixels.
    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> u64 {
        self.scroll.model().offset()
    }

    /// How far the strip is scrolled, in physical pixels.
    #[cfg(test)]
    pub(crate) fn strip_offset_for_test(&self) -> u64 {
        self.strip_scroll.model().offset()
    }

    /// Where the strip's keyboard cursor is.
    #[cfg(test)]
    pub(crate) fn strip_cursor_for_test(&self) -> Option<usize> {
        self.strip.current()
    }

    /// Scroll row `index` into view.
    #[cfg(test)]
    pub(crate) fn reveal_for_test(
        &mut self,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) {
        let frame = self.frame(viewport, scale, theme);
        self.reveal_row(
            index,
            &frame,
            scale,
            theme,
            &mut tairix_controls::damage::sink(),
        );
    }

    /// Where the open category list is drawn, or `None` with none open.
    #[cfg(test)]
    pub(crate) fn category_list_rect_for_test(
        &self,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let frame = self.frame(viewport, scale, theme);
        self.categories
            .as_ref()
            .map(|menu| Self::list_rect(menu, &frame, viewport, scale, theme))
    }

    /// The strip, for a test that asks it where it seated a row.
    #[cfg(test)]
    pub(crate) fn strip_for_test(&self) -> &Tabs {
        &self.strip
    }

    /// Show `location`, as choosing its strip row would.
    #[cfg(test)]
    pub(crate) fn go_to_for_test(
        &mut self,
        location: Location,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.show(location, viewport, scale, theme, damage);
    }

    /// The form the pane on show composes, for a test that asks what it
    /// composed.
    #[cfg(test)]
    pub(crate) fn form_for_test(&self) -> Option<&crate::form::Form> {
        self.body.form()
    }

    /// The form the pane on show composes, for a test that drives it.
    #[cfg(test)]
    pub(crate) fn form_mut_for_test(&mut self) -> Option<&mut crate::form::Form> {
        self.body.form_mut()
    }

    /// The volume cards the pane on show draws, for a test that asks what
    /// the machine reported.
    #[cfg(test)]
    pub(crate) fn readings_for_test(&self) -> Option<&crate::volumes::Readings> {
        match &self.body {
            Body::Volumes(readings) => Some(readings),
            Body::Statement | Body::Form(_) | Body::Facts(_) => None,
        }
    }

    /// What the window shows of the control group `group`'s row `row` draws
    /// in `viewport`, or `None` when none of it shows.
    #[cfg(test)]
    pub(crate) fn row_control_rect_for_test(
        &self,
        (group, row): (usize, usize),
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        view.to_window(self.body.form()?.control_rect_for_test(group, row, spot)?)
    }

    /// What the window shows of the control group `group`'s row `row` in
    /// `viewport`, or `None` when none of it shows.
    #[cfg(test)]
    pub(crate) fn row_rect_for_test(
        &self,
        (group, row): (usize, usize),
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        view.to_window(self.body.form()?.row_rect_for_test(group, row, spot)?)
    }

    /// What the window shows of `chooser`'s picture `index` in `viewport`, or
    /// `None` when none of it shows.
    #[must_use]
    pub fn picture_rect(
        &self,
        chooser: Chooser,
        index: usize,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        view.to_window(self.body.form()?.picture_rect(chooser, index, spot)?)
    }

    /// What the search field holds.
    #[cfg(test)]
    pub(crate) fn search_text_for_test(&self) -> &str {
        self.search.text()
    }

    /// What the window shows of the form's keyboard cursor row in
    /// `viewport`, or `None` when none of it shows.
    #[cfg(test)]
    pub(crate) fn cursor_row_rect_for_test(
        &self,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let frame = self.frame(viewport, scale, theme);
        let (spot, view) = self.pane_view(&frame, viewport, scale, theme);
        let form = self.body.form()?;
        let (group, row) = form.cursor()?;
        view.to_window(form.row_rect_for_test(group, row, spot)?)
    }

    /// Which group and row the form's keyboard cursor is on.
    #[cfg(test)]
    pub(crate) fn form_group_cursor_for_test(&self) -> Option<(usize, usize)> {
        self.body.form().and_then(crate::form::Form::cursor)
    }

    /// The settings the shell is showing.
    #[cfg(test)]
    pub(crate) fn settings_for_test(&self) -> &DesktopSettings {
        &self.settings
    }

    /// Choose the value at `index` for the pane's group `group` row `row`,
    /// and bring the action band up to date with it exactly as a committed
    /// choice does.
    #[cfg(test)]
    pub(crate) fn choose_for_test(&mut self, group: usize, row: usize, index: usize) -> bool {
        let staged = self
            .body
            .form_mut()
            .map(|form| form.choose_for_test(group, row, index));
        if let Some(form) = self.body.form_mut() {
            form.restate_badges();
        }
        self.restate_footer();
        matches!(staged, Some(FormOutcome::Staged))
    }

    /// What the window is holding of the machine's addressing, for a test
    /// that asks whether a privileged reading outlived its pane.
    #[cfg(test)]
    pub(crate) const fn addressing_for_test(&self) -> &Addressing {
        &self.network.addressing
    }

    /// What the window is holding of the account listing, for a test that
    /// asks whether a privileged reading outlived its pane.
    #[cfg(test)]
    pub(crate) const fn roster_for_test(&self) -> &Roster {
        &self.accounts.roster
    }

    /// Whether the window holds a salt to hash a password under, for a
    /// test that asks what an apply does without one.
    #[cfg(test)]
    pub(crate) const fn has_salt_for_test(&self) -> bool {
        self.salt.is_some()
    }

    /// What the pane's action band is saying, for a test that asks what a
    /// reader would read there.
    #[cfg(test)]
    pub(crate) fn band_line_for_test(&self) -> Option<String> {
        self.footer
            .as_ref()
            .map(|footer| footer.line().into_owned())
    }

    /// Type `text` into the pane's group `group` row `row`, and bring the
    /// plates and the band up to date with it exactly as a keystroke does.
    ///
    /// A test seam over the *routing* only: what it exercises is the
    /// staged set, the row's verdict and the band, none of which an
    /// entry's own editing mechanics (which `lib/controls` tests) has any
    /// part in.
    #[cfg(test)]
    pub(crate) fn type_for_test(&mut self, group: usize, row: usize, text: &str) -> bool {
        let typed = self
            .body
            .form_mut()
            .map(|form| form.type_for_test(group, row, text));
        if let Some(form) = self.body.form_mut() {
            form.restate_badges();
        }
        self.restate_footer();
        matches!(typed, Some(FormOutcome::Staged))
    }

    /// Put the keyboard cursor on the pane column.
    #[cfg(test)]
    pub(crate) fn focus_content_for_test(&mut self, viewport: Rect, scale: Scale, theme: &Theme) {
        self.focus_on(
            Focus::Content,
            viewport,
            scale,
            theme,
            &mut tairix_controls::damage::sink(),
        );
    }
}

/// Where a composed pane's form is drawn, gathered once for the call that
/// needs it.
fn place(bounds: Rect, viewport: Rect, scale: Scale, theme: &Theme) -> FormPlace<'_> {
    FormPlace {
        bounds,
        viewport,
        scale,
        theme,
    }
}

/// Why an elevated run did not change anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ElevateRefusal {
    /// The account and password were not accepted. One answer for a wrong
    /// password, an unknown account, and a locked one — the broker gives
    /// no more, and this repeats exactly what it was told.
    Credentials,
    /// The account was accepted and the program did not run, with the
    /// reason the caller was given.
    NotRun(String),
}

/// The title every credential question this surface asks carries.
const ASK_TITLE: &str = "Authenticate";

/// What the question says a machine setting needs an account for.
const SET_MACHINE_PURPOSE: &str =
    "Changing this machine's configuration needs an account that may write it.";

/// What the question says the clock needs an account for.
const SET_CLOCK_PURPOSE: &str = "Setting the date and time needs an account that may.";

/// What the question says reading the machine's addressing needs an
/// account for.
const SHOW_ADDRESSING_PURPOSE: &str =
    "This machine's network addressing names its hardware and its addresses, so reading it needs \
     an account that may.";

/// What the band says when a change is larger than one request carries.
///
/// The seam bounds what an unprivileged caller may hand a privileged run,
/// and the change goes in one invocation or not at all, so the answer is to
/// make it in smaller pieces — one interface's addressing is always small
/// enough.
const TOO_MANY_CHANGES: &str =
    "More changes than one command can carry. Apply one interface at a time.";

/// What the question says reading the accounts needs an account for.
const SHOW_ACCOUNTS_PURPOSE: &str =
    "An account's own fields, whether it may log in, and what it is allowed to do are not public, \
     so reading them needs an account that may administer users.";

/// What the question says changing an account needs an account for.
///
/// The whole registry is gated, so this is asked even of a reader editing
/// their own record: an unprivileged self-service change would be a new
/// authority path rather than a wider gate, and this surface offers none.
const SET_ACCOUNT_PURPOSE: &str = "Changing an account needs an account that may administer users.";

/// What the question says the machine's addressing needs an account for.
const SET_ADDRESSING_PURPOSE: &str =
    "Changing how this machine's interfaces are addressed needs an account that may write it.";

/// The tool that owns the machine's boot-time configuration store, which is
/// the only thing that writes it.
const CONFIGURE_RUN_PATH: &str = "/System/Commands/configure.app/Run";

/// The application that owns the wall clock.
const DATETIME_RUN_PATH: &str = "/System/Applications/datetime.app/Run";

/// The tool that answers the account and group listing.
const USERS_RUN_PATH: &str = "/System/Commands/users.app/Run";

/// The command line that applies everything `form` has staged: one
/// `<key> <value>` pair per row that differs, in registry order.
///
/// One invocation rather than one per row, because the tool renders the
/// document once: a run per key could leave the store holding half a
/// change if the second were refused.
fn configure_argv(form: &Form) -> Vec<String> {
    form.pending()
        .into_iter()
        .flat_map(|(key, value)| [key, value])
        .collect()
}

/// The shell outcome a form's answer implies.
fn outcome_of(acted: FormOutcome) -> ShellOutcome {
    match acted {
        FormOutcome::Idle => ShellOutcome::Idle,
        // A staged edit changed only a working copy; like any other change
        // that is not durable, all it asks for is a repaint — the pane's
        // action band included, which the caller redraws with it.
        FormOutcome::Changed | FormOutcome::Staged => ShellOutcome::Changed,
        FormOutcome::Apply(document) => ShellOutcome::Apply(document),
        FormOutcome::LockScreen => ShellOutcome::LockScreen,
        FormOutcome::PreviewScreensaver(document) => ShellOutcome::PreviewScreensaver(document),
    }
}

/// The band a pane occupies: its column and whatever the scrollbar takes
/// beside it, so a change that moves the bar reports the strip it vacated.
fn pane_band(frame: &ShellFrame, viewport: Rect) -> Rect {
    let width = u32::try_from(viewport.right().saturating_sub(frame.content.left())).unwrap_or(0);
    Rect::new(
        frame.content.left(),
        frame.content.top(),
        width,
        frame.content.height,
    )
}

/// Fold what a round drew into `damage`, and conclude at least a repaint
/// when it drew anything.
fn adopt_drawn(outcome: ShellOutcome, drew: &Region, damage: &mut Region) -> ShellOutcome {
    for rect in drew.rects() {
        damage.add(*rect);
    }
    match outcome {
        ShellOutcome::Idle if !drew.is_empty() => ShellOutcome::Changed,
        outcome => outcome,
    }
}

/// Report `column` when a scroll request moved it, answering whether it did.
///
/// The bar has already adopted the offset and reported itself.
fn scrolled(column: Rect, acted: Option<ScrollAction>, damage: &mut Region) -> bool {
    let moved = acted.is_some();
    if moved {
        damage.add(column);
    }
    moved
}

/// Scroll `bar` the least that shows `item` of a column laid out unscrolled
/// down `column`, answering whether it moved.
fn reveal(bar: &mut ScrollBar, column: Rect, item: Rect) -> bool {
    let model = bar.model();
    let start = u64::try_from(item.top().saturating_sub(column.top())).unwrap_or(0);
    let revealed = model.revealing(start, u64::from(item.height));
    bar.set_model(revealed);
    revealed.offset() != model.offset()
}

/// Whether `event` is a press, which is what moves the keyboard cursor to
/// the region it lands in; a hover passing over a region never does.
const fn is_press(event: &InputEvent) -> bool {
    matches!(event, InputEvent::PointerPressed { .. })
}

/// The scroll model an unmeasured column starts at: nothing to scroll.
fn empty_scroll() -> ScrollModel {
    ScrollModel::in_pixels(ScrollRange::EMPTY, 1)
}

/// The strip a row list implies: a badge on every row, a disclosure chevron on
/// each disclosing category stating whether its panes are listed, an indent on
/// each disclosed pane, a break wherever a new run of categories starts, and
/// the row on show selected.
fn strip_of(rows: &[StripRow], location: Location) -> Tabs {
    let mut tabs = Vec::with_capacity(rows.len());
    let mut above: Option<&CategoryRow> = None;
    for (index, row) in rows.iter().enumerate() {
        let tab = match *row {
            StripRow::Category(category) => match category.row() {
                Some(entry) => {
                    let tab = Tab::new(entry.label)
                        .with_icon(entry.icon)
                        .with_group_break(entry.breaks_from(above));
                    above = Some(entry);
                    if entry.discloses() {
                        tab.with_disclosure(first_listed_pane(rows, index, category).is_some())
                    } else {
                        tab
                    }
                }
                None => Tab::new("").with_icon(IconKind::Generic),
            },
            StripRow::Pane(_, pane) => {
                let (title, icon) = pane
                    .locate()
                    .map_or(("", None), |(_, entry)| (entry.title, entry.icon));
                Tab::new(title)
                    .with_icon(icon.unwrap_or(IconKind::Generic))
                    .nested()
            }
        };
        tabs.push(tab);
    }
    let mut strip = Tabs::new(tabs).with_orientation(TabsOrientation::Vertical);
    if let Some(index) = row_on_show(rows, location) {
        strip.adopt_selected(index);
    }
    strip
}

/// The first of `category`'s panes the strip lists beneath its row at
/// `index`, or `None` when it lists none there.
fn first_listed_pane(rows: &[StripRow], index: usize, category: Category) -> Option<StripRow> {
    rows.get(index.checked_add(1)?)
        .copied()
        .filter(|next| matches!(next, StripRow::Pane(owner, _) if *owner == category))
}

/// Which of `rows` is the pane on show: its own row where the strip lists it,
/// else its category's row, which stands for it — a category with one pane,
/// or one whose list of panes is closed.
///
/// One definition, read by the strip's selection and by the keyboard cursor,
/// so the row the reader sees selected is the row the cursor sits on.
fn row_on_show(rows: &[StripRow], location: Location) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(row, StripRow::Pane(_, pane) if *pane == location.pane))
        .or_else(|| {
            rows.iter().position(
                |row| matches!(row, StripRow::Category(category) if *category == location.category),
            )
        })
}

/// Open `location`'s category in `open` when it discloses its panes, so the
/// strip lists the row of the pane on show.
fn list_pane_of(open: &mut DisclosureSet<Category>, location: Location) {
    if location.category.row().is_some_and(CategoryRow::discloses) {
        open.set(location.category, true);
    }
}
