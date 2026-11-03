//! The session's ownership of the user's pinboard settings: the scope they
//! live in, and the one policy that decides whether a foreign caller's
//! pinboard request may be honoured (`plans/PINBOARD.md` §2, §6,
//! `plans/APPDATA.md` §3.11).
//!
//! Settings are per-user configuration (`lib/wallpaper`), kept in this
//! application's own **published** app-data scope. Two properties come from
//! the store rather than from anything written here, and both replace a rule
//! this module used to have to enforce itself:
//!
//! - The session is the store's only **writer**, by construction: an
//!   application publishes only its own scope, so no other program the user
//!   launches — including the Settings application — can write the
//!   desktop's document at all. Every change, whether the backdrop menu
//!   asked for it or Settings did, is applied by the session and the desktop adopts it
//!   **only after the write succeeded** — and the write happens on the
//!   session's settings worker, never on its serve loop, so the compositor
//!   does not stop while a disk is written. Memory and disk can never diverge,
//!   and neither can freeze the desktop.
//! - Any application may **read** it, through one request shape that carries
//!   no scope field, so "read the desktop's private settings" is not a
//!   request that exists. That is what replaces the hand-rolled
//!   `~/Settings/Pinboard/pinboard.conf` path the chooser used to open
//!   directly — a file every application of that user could also rewrite.
//!
//! Nothing here spells a path or names a bundle: the app-data service derives
//! the store from the identity the kernel attested for this task.
//!
//! The channel's decision is here, not beside the serve loop, so it is
//! host-testable without a running kernel: the caller's uid is the
//! kernel-attested one the embedder reads off the endpoint, and this module
//! reads the carried document with the same registry the store's own document
//! is read with. Nothing here performs I/O, so the path the document names is
//! read later, by the session, under the session's own identity — a caller can
//! never use this channel to reach a file it could not read itself. The same
//! attestation admits the sound requests, which name no path at all
//! ([`crate::effects`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::pinboard_ipc::{PinboardDocument, PinboardRequest};
use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::time::Duration64;
use tairix_abi::Errno;
use tairix_appconf::{merge_admitting, Refusal, Registry};
use tairix_appdata::{AppDataHost, Settings as SettingsStore};
use tairix_browse::{FilerKey, FilerPreferences};
use tairix_geometry::Scale;
use tairix_log::{Event, EventId, Field, FieldValue, Level, Sink};
use tairix_touch::TouchSettings;
use tairix_tz::ZoneInfo;
use tairix_wallpaper::{
    merge_applied, DesktopSettings, DocumentRefusal, PointerSpeed, PrimaryButton, SettingsKey,
};

use crate::keyboard::KeyRepeat;

/// What loading the user's pinboard settings produced: the settings the
/// desktop starts on, and the ready-to-print warning lines for anything that
/// could not be used.
#[derive(Clone, Debug, Default)]
pub struct LoadedPinboard {
    /// The settings to apply before the desktop's first listing.
    pub settings: DesktopSettings,
    /// The file manager's settings, kept in the same document.
    pub files: FilerPreferences,
    /// One line per reason the stored settings were not fully used, ready for
    /// `stderr` (newline-terminated, in the session's `desktop:` diagnosis
    /// convention). Empty when the store answered and every value was one the
    /// registry accepts.
    pub warnings: Vec<String>,
    /// Whether the store answered. When it did not, these are the defaults
    /// standing in for the user's settings, and nothing made over them may
    /// replace the document the store still holds.
    pub read: bool,
}

/// Event id of a pinboard request the kernel attests as another user's than
/// the session's.
pub const PINBOARD_UNATTESTED: EventId = EventId(20_026);

/// The exact message [`PINBOARD_UNATTESTED`] is emitted with.
pub const PINBOARD_UNATTESTED_MESSAGE: &str = "pinboard request from another user refused";

/// Why a request arriving on the pinboard channel was refused.
///
/// Carries both the wire [`Errno`] the caller receives and the `stderr`
/// line the session states in its own house style, so the one refusal
/// decision names both without this pure policy performing any I/O.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinboardRefusal {
    /// The caller's kernel-attested origin uid is not the uid this session
    /// runs as, so it is asking to rewrite somebody else's desktop.
    Unattested,
    /// The request frame itself failed to decode.
    Malformed(Errno),
    /// The carried document is not a settings document this registry accepts.
    Undecodable(DocumentRefusal),
    /// The carried document is not one of the file manager's settings.
    FilesUndecodable(Refusal<FilerKey>),
    /// The document changes the region or the zone to one the zone store
    /// does not hold.
    NotInStore(SettingsKey),
}

impl PinboardRefusal {
    /// The wire [`Errno`] the caller receives.
    #[must_use]
    pub const fn errno(&self) -> Errno {
        match self {
            Self::Unattested => Errno::PermissionDenied,
            Self::Malformed(err) => *err,
            Self::Undecodable(_) | Self::FilesUndecodable(_) | Self::NotInStore(_) => {
                Errno::OutOfRange
            }
        }
    }

    /// The `stderr` diagnosis line, without the leading `desktop: ` prefix
    /// or trailing newline the caller adds in its own house style.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Unattested => String::from(PINBOARD_UNATTESTED_MESSAGE),
            Self::Malformed(err) => format!("malformed pinboard request refused ({err:?})"),
            Self::Undecodable(refusal) => {
                format!("pinboard apply carries an unusable settings document: {refusal}")
            }
            Self::FilesUndecodable(refusal) => {
                format!("file manager apply carries an unusable settings document: {refusal}")
            }
            Self::NotInStore(key) => {
                format!("pinboard apply sets {key} to one the zone store does not hold")
            }
        }
    }
}

/// What the pointer, the touch surfaces and the keyboard are driven by, as the
/// settings name it.
///
/// The one reading of the input keys, so the seat's sources, the window
/// manager and every application are handed the same policy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InputPolicy {
    /// Which physical button is primary.
    pub primary: PrimaryButton,
    /// How far the pointer moves for a reported displacement.
    pub speed: PointerSpeed,
    /// How far apart a double-click's presses may be.
    pub double_click: Duration64,
    /// How a held key repeats.
    pub repeat: KeyRepeat,
    /// What a touch means.
    pub touch: TouchSettings,
    /// The density the desktop is drawn at, which a touchscreen stating no
    /// size of its own is measured by.
    pub density: Scale,
}

impl InputPolicy {
    /// The policy `settings` name.
    #[must_use]
    pub fn of(settings: &DesktopSettings) -> Self {
        Self {
            primary: settings.primary_button,
            speed: settings.pointer_speed,
            double_click: settings.double_click,
            repeat: KeyRepeat {
                delay: settings.repeat_delay,
                interval: settings.repeat_rate.interval(),
            },
            touch: TouchSettings {
                tap_to_click: settings.touchpad.tap,
                natural_scroll: settings.touchpad.natural_scroll,
                speed_percent: settings.touchpad.speed.percent(),
            },
            density: settings.scale,
        }
    }
}

/// Read the user's pinboard settings out of this application's published
/// scope.
///
/// Never fails, and mirrors the program library's posture exactly: a store
/// the service could not serve, and one holding a value this build's registry
/// does not accept, each leave the affected settings at their documented
/// defaults *and* contribute a warning line — the desktop always comes up on a
/// fully-specified backdrop rather than a guessed one, and always says why
/// when it is not the one the user chose. A store that is simply empty is the
/// ordinary fresh-account state and warns about nothing; one that could not
/// be read is left unread ([`LoadedPinboard::read`]).
pub fn load_pinboard(host: &mut dyn AppDataHost) -> LoadedPinboard {
    let store = SettingsStore::open_published(host);
    let mut loaded = read_pinboard(&store);
    if let Some(err) = store.store_refusal() {
        loaded.read = false;
        loaded.warnings.insert(
            0,
            warning(&format!("could not be read ({err:?}); using the defaults")),
        );
    }
    loaded
}

/// Decode what `store` says, keeping the documented default for anything this
/// build's registry does not accept and saying so.
fn read_pinboard(store: &SettingsStore<'_>) -> LoadedPinboard {
    let (settings, refused) = DesktopSettings::load(store);
    let (files, files_refused) = FilerPreferences::load(store);
    let refused_names = refused
        .into_iter()
        .map(SettingsKey::name)
        .chain(files_refused.into_iter().map(FilerKey::name));
    let warnings = refused_names
        .map(|key| {
            warning(&format!(
                "`{key}` is not a value that setting accepts; using its default"
            ))
        })
        .collect();
    LoadedPinboard {
        settings,
        files,
        warnings,
        read: true,
    }
}

/// What the settings worker's round trip came to.
#[derive(Debug)]
pub enum Publication {
    /// The document was replaced, and the store now holds these.
    Published(LoadedPinboard),
    /// The ask was made over the defaults that stood in for a store that could
    /// not be read, so nothing was published: the store was read in its place,
    /// and holds these.
    Read(LoadedPinboard),
}

impl Publication {
    /// What the call that asked for the publish is answered: an ask made over
    /// the defaults was made for a publication the one read has replaced.
    pub const fn answer(&self) -> Result<(), Errno> {
        match self {
            Self::Published(_) => Ok(()),
            Self::Read(_) => Err(Errno::Stale),
        }
    }
}

/// Publish `settings` and the file manager's `files`, made `over_stored`
/// settings or over the defaults standing in for a store not yet read, and
/// answer with what the store then holds.
///
/// This is the settings worker's whole body: the store round trip a change
/// costs. A publish replaces the whole document, so one made over the defaults
/// would erase every setting the store still holds; the store is read in its
/// place instead, and nothing is published until a read has succeeded.
///
/// # Errors
///
/// The app-data service's refusal to publish, or the store's refusal to be
/// read. Nothing was published.
pub fn publish_asked(
    host: &mut dyn AppDataHost,
    settings: &DesktopSettings,
    files: &FilerPreferences,
    over_stored: bool,
) -> Result<Publication, Errno> {
    if over_stored {
        return publish_pinboard(host, settings, files).map(Publication::Published);
    }
    let store = SettingsStore::open_published(host);
    if let Some(err) = store.store_refusal() {
        return Err(err);
    }
    Ok(Publication::Read(read_pinboard(&store)))
}

/// Publish `settings` and the file manager's `files` to this application's
/// published scope, replacing what it says, and answer with what the store
/// then holds.
///
/// The desktop adopts the settings this answers with — not the ones that were
/// asked for — which is what keeps the adopted state and the published
/// document identical: a refused publish answers `Err`, and the desktop is
/// left exactly as it was rather than showing a backdrop the next login would
/// not restore.
///
/// # Errors
///
/// The app-data service's own typed refusal — no service bound, no store for a
/// caller running no signed bundle, or an unreachable volume. Nothing was
/// published.
fn publish_pinboard(
    host: &mut dyn AppDataHost,
    settings: &DesktopSettings,
    files: &FilerPreferences,
) -> Result<LoadedPinboard, Errno> {
    let mut document = settings.document();
    for setting in files.document_of(FilerPreferences::KEYS).settings() {
        document
            .set(setting.key, setting.value)
            .map_err(|_| Errno::OutOfRange)?;
    }
    let mut store = SettingsStore::open_published(host);
    store.replace(&document)?;
    Ok(read_pinboard(&store))
}

/// Attest a pinboard request against the session's own identity, laying the
/// settings an apply carries over `in_effect`, and the file manager's settings
/// a files apply carries over `files`.
///
/// `caller_uid` is the **kernel-attested** origin uid of the calling task,
/// read off the endpoint by the embedder — never anything the caller said
/// about itself. Only a caller running as this session's own user may
/// rewrite this session's desktop or its user's sounds: a request from any
/// other uid is [`PinboardRefusal::Unattested`], reads nothing, and is
/// recorded on `audit` ([`PINBOARD_UNATTESTED`]). A frame
/// that will not decode, or a document the registry's strict reading refuses,
/// is likewise a typed refusal.
///
/// The request is **merged** over the settings currently in effect rather
/// than replacing them, because the desktop has more than one surface
/// asking it to change and no surface shows every setting: the chooser
/// edits the backdrop, the Settings application edits how everything is
/// drawn. A key the caller did not name keeps the value the desktop has, so
/// one surface cannot undo the other's change by staying silent about it.
///
/// The returned settings are only *what was asked for*: the session still
/// publishes and applies them through its own path, and still reads the
/// wallpaper the document names under its own identity, so this channel
/// grants a caller no reach it did not already have.
///
/// A region or zone the apply *changes* must be one `zones` holds, so a
/// choice is refused while the person making it is looking at it. One already
/// in effect is left alone: a stored choice outlives the store that shipped
/// it, and the clock passes over it to the store's default.
///
/// # Errors
///
/// The [`PinboardRefusal`] naming why the request was refused. Nothing is
/// half-applied: `in_effect` is what the desktop keeps on a refusal.
pub fn serve_pinboard(
    audit: &dyn Sink,
    (session_uid, caller_uid): (u32, u32),
    (in_effect, files, zones): (&DesktopSettings, &FilerPreferences, Option<&ZoneInfo<'_>>),
    request: &[u8],
) -> Result<PinboardCall, PinboardRefusal> {
    if caller_uid != session_uid {
        tairix_log::log(
            audit,
            &Event {
                level: Level::Warn,
                id: PINBOARD_UNATTESTED,
                message: PINBOARD_UNATTESTED_MESSAGE,
                fields: &[
                    Field {
                        key: "caller_uid",
                        value: FieldValue::UnsignedInt(u64::from(caller_uid)),
                    },
                    Field {
                        key: "session_uid",
                        value: FieldValue::UnsignedInt(u64::from(session_uid)),
                    },
                ],
            },
        );
        return Err(PinboardRefusal::Unattested);
    }
    let sound = match PinboardRequest::from_bytes(request).map_err(PinboardRefusal::Malformed)? {
        PinboardRequest::Apply { document } => {
            let settings = merge_applied(in_effect, document.as_str())
                .map_err(PinboardRefusal::Undecodable)?;
            if let Some(key) = unheld_civil(zones, in_effect, &settings) {
                return Err(PinboardRefusal::NotInStore(key));
            }
            return Ok(PinboardCall::Apply(PinboardApplied { settings, document }));
        }
        PinboardRequest::ApplyFiles { document } => {
            return merge_admitting(files, document.as_str(), |_| true)
                .map(PinboardCall::ApplyFiles)
                .map_err(PinboardRefusal::FilesUndecodable);
        }
        PinboardRequest::SoundRead => SoundCall::Read,
        PinboardRequest::SoundThemes { from } => SoundCall::Themes { from },
        PinboardRequest::SoundApply { document } => SoundCall::Apply(document),
        PinboardRequest::SoundPick { event } => SoundCall::Pick(event),
        PinboardRequest::SoundUseTheme { event } => SoundCall::UseTheme(event),
    };
    Ok(PinboardCall::Sound(sound))
}

/// The civil key `settings` changes from `in_effect` to a value `zones` does
/// not hold, if any.
fn unheld_civil(
    zones: Option<&ZoneInfo<'_>>,
    in_effect: &DesktopSettings,
    settings: &DesktopSettings,
) -> Option<SettingsKey> {
    let region_held = settings
        .region
        .as_ref()
        .is_none_or(|code| zones.is_some_and(|zones| zones.region(code.as_str()).is_some()));
    let zone_held = settings
        .time_zone
        .as_ref()
        .is_none_or(|name| zones.is_some_and(|zones| zones.zone(name.as_str()).is_some()));
    if settings.region != in_effect.region && !region_held {
        Some(SettingsKey::Region)
    } else if settings.time_zone != in_effect.time_zone && !zone_held {
        Some(SettingsKey::TimeZone)
    } else {
        None
    }
}

/// What an attested pinboard call asks for.
#[allow(
    clippy::large_enum_variant,
    reason = "an attested call is matched at once and never kept, so its size is one stack frame's"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinboardCall {
    /// Adopt the desktop settings an apply makes.
    Apply(PinboardApplied),
    /// Adopt the file manager's settings a files apply makes over the ones
    /// in effect.
    ApplyFiles(FilerPreferences),
    /// Read or change the user's sound choices.
    Sound(SoundCall),
}

impl PinboardCall {
    /// Whether the call changes the settings, as either apply does, and so
    /// waits while a change is already in hand.
    #[must_use]
    pub const fn changes_settings(&self) -> bool {
        matches!(self, Self::Apply(_) | Self::ApplyFiles(_))
    }
}

/// A request about the user's sound choices ([`crate::effects`]).
#[allow(
    clippy::large_enum_variant,
    reason = "a request is matched at once and never kept, so its size is one stack frame's"
)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SoundCall {
    /// Say what they are.
    Read,
    /// Name the themes from the `from`th on.
    Themes {
        /// How many the pages before named.
        from: u16,
    },
    /// Lay a document over them.
    Apply(PinboardDocument),
    /// Have the user choose the file an event plays.
    Pick(SoundEvent),
    /// Have an event play its theme's sound again.
    UseTheme(SoundEvent),
}

/// What an attested apply asked for: the settings it makes over the ones in
/// effect, and the document that made them, for a request held until the
/// user answers to be merged again over whatever is in effect by then.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinboardApplied {
    /// The settings in effect with the document's keys merged over them.
    pub settings: DesktopSettings,
    /// The document the call carried.
    pub document: PinboardDocument,
}

/// The newest settings the session has asked to publish, until that ask
/// lands: what its own next change is made over, so one change never undoes
/// another still on its way to the store.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AskedSettings {
    numbered: u64,
    newest: Option<(u64, DesktopSettings, FilerPreferences)>,
    /// Whether the settings adopted are the store's, rather than the defaults
    /// standing in for a store not yet read.
    read: bool,
}

/// One ask to publish, as [`AskedSettings::ask`] numbers it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SettingsAsk {
    /// Which ask it is, in the order asked.
    pub number: u64,
    /// Whether it was made over the settings the store holds. One that was
    /// not is made over defaults, and publishing it would replace the user's
    /// document with them.
    pub over_stored: bool,
}

impl AskedSettings {
    /// Nothing asked, and the store not yet read.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            numbered: 0,
            newest: None,
            read: false,
        }
    }

    /// Note that `settings` and `files` have been asked to be published,
    /// answering the ask, whose number [`landed`](Self::landed) takes.
    pub fn ask(&mut self, settings: &DesktopSettings, files: &FilerPreferences) -> SettingsAsk {
        self.numbered = self.numbered.wrapping_add(1);
        self.newest = Some((self.numbered, settings.clone(), *files));
        SettingsAsk {
            number: self.numbered,
            over_stored: self.read,
        }
    }

    /// Note that the settings adopted are now the ones the store holds. Every
    /// ask still on its way was made over the defaults that stood in for
    /// them, so none is a base any more.
    pub fn read_store(&mut self) {
        self.read = true;
        self.newest = None;
    }

    /// Note that ask `number` has landed, adopted or refused. Only the newest
    /// ask landing leaves nothing outstanding.
    pub fn landed(&mut self, number: u64) {
        if self
            .newest
            .as_ref()
            .is_some_and(|(newest, ..)| *newest == number)
        {
            self.newest = None;
        }
    }

    /// The settings a change is made over: the newest asked for, or `adopted`
    /// once every ask has landed.
    #[must_use]
    pub fn base(&self, adopted: &DesktopSettings) -> DesktopSettings {
        self.newest
            .as_ref()
            .map_or_else(|| adopted.clone(), |(_, settings, _)| settings.clone())
    }

    /// The file manager's settings a change is made over, as
    /// [`base`](Self::base) is for the desktop's.
    #[must_use]
    pub fn files_base(&self, adopted: &FilerPreferences) -> FilerPreferences {
        self.newest.as_ref().map_or(*adopted, |(.., files)| *files)
    }

    /// Whether an ask has yet to land.
    #[must_use]
    pub const fn outstanding(&self) -> bool {
        self.newest.is_some()
    }
}

/// One ready-to-print warning line for settings the desktop could not fully
/// use.
fn warning(detail: &str) -> String {
    format!("desktop: pinboard settings {detail}\n")
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use tairix_abi::pinboard_ipc::{PinboardDocument, PinboardRequest};
    use tairix_abi::Errno;
    use tairix_appconf::Registry;
    use tairix_appdata::fake::FakeService;
    use tairix_wallpaper::{
        DesktopSettings, DocumentRefusal, IconFlow, IconSort, SettingsKey, WallpaperChoice,
        WallpaperFit,
    };

    use tairix_log::DiscardSink;

    use tairix_browse::{FilerKey, FilerPreferences, ViewMode};

    use super::{
        load_pinboard, publish_asked, publish_pinboard, serve_pinboard, AskedSettings,
        PinboardApplied, PinboardCall, PinboardRefusal, Publication, SoundCall,
        PINBOARD_UNATTESTED,
    };

    /// A change made while an earlier one is still being written is made over
    /// that one, not over the settings last adopted, which would undo it.
    #[test]
    fn a_change_is_made_over_the_newest_ask_until_it_lands() {
        let adopted = DesktopSettings::default();
        let mut asked = AskedSettings::new();
        assert_eq!(asked.base(&adopted), adopted);
        assert!(!asked.outstanding());

        let sorted = DesktopSettings {
            sort: IconSort::Size,
            ..adopted.clone()
        };
        let files = FilerPreferences::default();
        let first = asked.ask(&sorted, &files);
        assert_eq!(asked.base(&adopted), sorted, "the sort is still on its way");
        let flowed = DesktopSettings {
            icons: IconFlow::Trailing,
            ..asked.base(&adopted)
        };
        let mut listed = files;
        listed.view = ViewMode::List;
        let second = asked.ask(&flowed, &listed);
        assert_eq!(
            asked.files_base(&files),
            listed,
            "a files change is made over it too"
        );
        assert_eq!(flowed.sort, IconSort::Size, "made over the sort in flight");

        asked.landed(first.number);
        assert!(asked.outstanding(), "an older ask landing settles nothing");
        assert_eq!(asked.base(&adopted), flowed);
        asked.landed(second.number);
        assert!(!asked.outstanding());
        assert_eq!(asked.base(&adopted), adopted, "back to what was adopted");
        assert_eq!(asked.files_base(&files), files);
    }

    /// An ask made before the store is read was made over the defaults
    /// standing in for it, and once it is read no such ask is a base: the
    /// next change is made over what was read.
    #[test]
    fn an_ask_made_before_the_store_is_read_is_no_base_once_it_is() {
        let files = FilerPreferences::default();
        let mut asked = AskedSettings::new();
        let sorted = DesktopSettings {
            sort: IconSort::Size,
            ..DesktopSettings::default()
        };
        let early = asked.ask(&sorted, &files);
        assert!(!early.over_stored, "made over the defaults");
        asked.read_store();
        assert!(!asked.outstanding());
        assert_eq!(asked.base(&edited()), edited());
        asked.landed(early.number);
        assert!(asked.ask(&sorted, &files).over_stored);
    }

    /// The uid the session under test runs as.
    const SESSION_UID: u32 = 1000;

    /// The command word this application's bundle is installed under. The
    /// published scope has no bundle-shipped layer, so it selects nothing
    /// here; it is spelled only because the fake models a whole store.
    const OWN_WORD: &str = "desktop";

    /// A fake app-data service with an empty published scope — a fresh
    /// account of the shipped desktop. It speaks the real `appdata-v1` codec,
    /// so these tests drive the wire the service actually answers.
    fn service() -> FakeService {
        FakeService::for_word(OWN_WORD)
    }

    /// Settings distinguishable from the defaults in every field the
    /// document carries.
    fn edited() -> DesktopSettings {
        DesktopSettings {
            wallpaper: WallpaperChoice::None,
            fit: WallpaperFit::Centre,
            icons: IconFlow::Trailing,
            sort: IconSort::Size,
            ..DesktopSettings::default()
        }
    }

    /// An `Apply` frame carrying every key of `settings` an application may
    /// ask for.
    fn apply_frame(settings: &DesktopSettings) -> Vec<u8> {
        let applicable: Vec<SettingsKey> = SettingsKey::ALL
            .into_iter()
            .filter(|key| !SettingsKey::SOUND.contains(key))
            .collect();
        let document = PinboardDocument::new(&settings.document_of(&applicable).render())
            .expect("renders a valid document");
        frame(&PinboardRequest::Apply { document })
    }

    fn frame(request: &PinboardRequest) -> Vec<u8> {
        let mut out = [0u8; tairix_abi::pinboard_ipc::PINBOARD_MAX_REQUEST];
        let len = request.encode(&mut out).expect("encodes");
        out[..len].to_vec()
    }

    /// What an attested apply asked for, or why it was refused.
    fn applied(
        session: u32,
        caller: u32,
        in_effect: &DesktopSettings,
        request: &[u8],
    ) -> Result<DesktopSettings, PinboardRefusal> {
        let files = FilerPreferences::default();
        match serve_pinboard(
            &DiscardSink,
            (session, caller),
            (in_effect, &files, None),
            request,
        )? {
            PinboardCall::Apply(applied) => Ok(applied.settings),
            PinboardCall::ApplyFiles(files) => panic!("not a desktop apply: {files:?}"),
            PinboardCall::Sound(call) => panic!("not an apply: {call:?}"),
        }
    }

    #[test]
    fn the_pinboards_publisher_names_this_very_bundle() {
        // Two principals must agree on the identifier: this session, which is
        // what the kernel attests when it publishes, and every reader, which
        // hands `PINBOARD_PUBLISHER` to a foreign read. They cannot be one
        // definition — one is a signed manifest, the other a Rust constant —
        // so this is what stops a bundle rename turning the chooser's read
        // into a silent set of defaults.
        let manifest = include_str!("../AppInfo.toml");
        let declared = manifest
            .lines()
            .find_map(|line| line.strip_prefix("id = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("the manifest source declares an id");
        assert_eq!(declared, tairix_wallpaper::PINBOARD_PUBLISHER);
    }

    #[test]
    fn an_empty_store_is_the_silent_fresh_account_state() {
        let mut host = service();
        let loaded = load_pinboard(&mut host);
        assert_eq!(loaded.settings, DesktopSettings::default());
        assert!(loaded.warnings.is_empty());
        assert!(loaded.read, "an empty store is one that was read");
    }

    #[test]
    fn published_settings_are_read_back_verbatim() {
        let mut host = service();
        let published = publish_pinboard(&mut host, &edited(), &FilerPreferences::default())
            .expect("publishes");
        assert_eq!(
            published.settings,
            edited(),
            "the desktop adopts what the store now holds"
        );
        assert!(published.warnings.is_empty());
        let loaded = load_pinboard(&mut host);
        assert_eq!(loaded.settings, edited());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn a_publish_lands_in_the_published_scope_and_nowhere_else() {
        // The private scope is not what other applications read, so a
        // desktop that published into it would be publishing nothing.
        let mut host = service();
        assert!(publish_pinboard(&mut host, &edited(), &FilerPreferences::default()).is_ok());
        assert_eq!(
            host.published().get("fit"),
            Some(WallpaperFit::Centre.as_str())
        );
        assert_eq!(host.committed().settings().count(), 0);
    }

    #[test]
    fn a_stored_value_the_registry_refuses_warns_and_keeps_that_default() {
        let mut host = service().with_published("fit = sideways\nsort = size\n");
        let loaded = load_pinboard(&mut host);
        assert_eq!(loaded.settings.fit, WallpaperFit::default());
        assert_eq!(loaded.settings.sort, IconSort::Size);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains(SettingsKey::Fit.name()));
        assert!(loaded.warnings[0].ends_with("using its default\n"));
    }

    #[test]
    fn an_unreachable_store_warns_once_and_uses_the_defaults() {
        let mut host = service();
        host.refusal().set(Some(Errno::DeviceOffline));
        let loaded = load_pinboard(&mut host);
        assert_eq!(loaded.settings, DesktopSettings::default());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].starts_with("desktop: pinboard settings"));
        assert!(loaded.warnings[0].contains("DeviceOffline"));
        assert!(!loaded.read, "nothing may be published over these defaults");
    }

    #[test]
    fn a_refused_publish_is_reported_and_publishes_nothing() {
        let mut host = service();
        host.refusal().set(Some(Errno::NoSpace));
        assert_eq!(
            publish_pinboard(&mut host, &edited(), &FilerPreferences::default()).err(),
            Some(Errno::NoSpace)
        );
        assert_eq!(host.published().settings().count(), 0);
    }

    /// A publish removes every key the registry does not render, so a
    /// document a previous build left behind cannot outlive it.
    #[test]
    fn a_publish_replaces_the_whole_document() {
        let mut host = service().with_published("fit = centre\nleftover = yes\n");
        assert!(publish_pinboard(&mut host, &edited(), &FilerPreferences::default()).is_ok());
        assert_eq!(host.published().get("leftover"), None);
    }

    /// A login that could not read the store publishes nothing over the
    /// defaults: the worker reads the store in their place, so the user's
    /// document — the file manager's settings in it among them — survives.
    #[test]
    fn an_ask_over_an_unread_store_reads_it_and_publishes_nothing() {
        let mut host = service().with_published("fit = centre\nfiles.view = list\n");
        host.refusal().set(Some(Errno::DeviceOffline));
        let loaded = load_pinboard(&mut host);
        assert!(!loaded.read);
        assert_eq!(
            publish_asked(&mut host, &loaded.settings, &loaded.files, false).err(),
            Some(Errno::DeviceOffline),
            "while the store cannot be read, nothing is published"
        );
        host.refusal().set(None);
        let Ok(publication) = publish_asked(&mut host, &loaded.settings, &loaded.files, false)
        else {
            panic!("the store reads once it can");
        };
        assert_eq!(publication.answer(), Err(Errno::Stale));
        let Publication::Read(read) = publication else {
            panic!("an ask made over the defaults publishes nothing");
        };
        assert!(read.read);
        assert_eq!(read.files.view, ViewMode::List);
        assert_eq!(read.settings.fit, WallpaperFit::Centre);
        assert_eq!(host.published().get("files.view"), Some("list"));

        assert!(matches!(
            publish_asked(&mut host, &edited(), &read.files, true),
            Ok(Publication::Published(_))
        ));
        assert_eq!(
            host.published().get("files.view"),
            Some("list"),
            "a change made over what was read keeps it"
        );
    }

    #[test]
    fn an_apply_from_the_session_user_reads_its_document() {
        let frame = apply_frame(&edited());
        assert_eq!(
            applied(
                SESSION_UID,
                SESSION_UID,
                &DesktopSettings::default(),
                &frame
            ),
            Ok(edited())
        );
    }

    #[test]
    fn an_apply_from_another_user_is_refused_without_reading_it() {
        let frame = apply_frame(&edited());
        assert_eq!(
            applied(
                SESSION_UID,
                SESSION_UID + 1,
                &DesktopSettings::default(),
                &frame
            ),
            Err(PinboardRefusal::Unattested)
        );
        // Even a frame that could never decode is refused on identity
        // first, so a foreign caller learns nothing about the grammar.
        assert_eq!(
            applied(SESSION_UID, 0, &DesktopSettings::default(), &[]),
            Err(PinboardRefusal::Unattested)
        );
        assert_eq!(PinboardRefusal::Unattested.errno(), Errno::PermissionDenied);
    }

    #[test]
    fn a_malformed_frame_from_the_session_user_is_refused() {
        let refusal = applied(SESSION_UID, SESSION_UID, &DesktopSettings::default(), &[])
            .expect_err("a truncated frame cannot decode");
        assert_eq!(refusal, PinboardRefusal::Malformed(Errno::BufferTooSmall));
        assert_eq!(refusal.errno(), Errno::BufferTooSmall);
    }

    #[test]
    fn an_undecodable_document_is_refused_rather_than_guessed() {
        // The wire's reading is the strict one: a sender naming a value this
        // build cannot show is refused, where the *store's* same value would
        // only cost that one field its default.
        let document = PinboardDocument::new("sort = sideways\n").expect("well-formed transport");
        let frame = frame(&PinboardRequest::Apply { document });
        let refusal = applied(
            SESSION_UID,
            SESSION_UID,
            &DesktopSettings::default(),
            &frame,
        )
        .expect_err("an unusable document is refused");
        assert_eq!(
            refusal,
            PinboardRefusal::Undecodable(DocumentRefusal::InvalidValue(SettingsKey::Sort))
        );
        assert_eq!(refusal.errno(), Errno::OutOfRange);
        assert!(refusal.reason().contains("sort"));
    }

    #[test]
    fn the_input_policy_is_read_from_the_input_keys() {
        let settings = DesktopSettings {
            primary_button: tairix_wallpaper::PrimaryButton::Right,
            repeat_rate: tairix_wallpaper::RepeatRate::PerSecond(20),
            ..DesktopSettings::default()
        };
        let policy = super::InputPolicy::of(&settings);
        assert_eq!(policy.primary, tairix_wallpaper::PrimaryButton::Right);
        assert_eq!(policy.double_click, settings.double_click);
        assert_eq!(policy.repeat.delay, settings.repeat_delay);
        assert_eq!(
            policy.repeat.interval,
            Some(tairix_abi::time::Duration64::from_millis(50))
        );
        let off = DesktopSettings {
            repeat_rate: tairix_wallpaper::RepeatRate::Off,
            ..DesktopSettings::default()
        };
        assert_eq!(super::InputPolicy::of(&off).repeat.interval, None);
    }

    #[test]
    fn every_refusal_states_a_reason() {
        for refusal in [
            PinboardRefusal::Unattested,
            PinboardRefusal::Malformed(Errno::BadMagic),
            PinboardRefusal::Undecodable(DocumentRefusal::Unparsed(1)),
        ] {
            assert!(!refusal.reason().is_empty());
        }
    }

    #[test]
    fn an_apply_keeps_every_key_the_caller_did_not_name() {
        // The defect this forecloses: the Wallpaper pane posts only the
        // pinboard keys, so adopting its request must not reset the
        // appearance another pane set (and the reverse).
        let in_effect = DesktopSettings {
            appearance: tairix_abi::desktop::Appearance::Dark,
            density: tairix_abi::desktop::Density::Compact,
            ..DesktopSettings::default()
        };
        let asked = DesktopSettings {
            sort: IconSort::Size,
            ..DesktopSettings::default()
        };
        let document = asked
            .document_of(&tairix_wallpaper::SettingsKey::PINBOARD)
            .render();
        let frame = frame(&PinboardRequest::Apply {
            document: PinboardDocument::new(&document).expect("a bounded document"),
        });

        let merged = applied(SESSION_UID, SESSION_UID, &in_effect, &frame)
            .expect("the backdrop keys are valid");
        assert_eq!(merged.sort, IconSort::Size);
        assert_eq!(merged.appearance, in_effect.appearance);
        assert_eq!(merged.density, in_effect.density);
    }

    #[test]
    fn a_region_or_zone_the_store_does_not_hold_is_refused_where_it_is_set() {
        let bytes = tairix_tz::testkit::store();
        let zones = tairix_tz::ZoneInfo::parse(&bytes).expect("the fixture parses");
        let apply = |text: &str| {
            frame(&PinboardRequest::Apply {
                document: PinboardDocument::new(text).expect("a bounded document"),
            })
        };
        let served = |in_effect: &DesktopSettings, text: &str, zones| match serve_pinboard(
            &DiscardSink,
            (SESSION_UID, SESSION_UID),
            (in_effect, &FilerPreferences::default(), zones),
            &apply(text),
        ) {
            Ok(PinboardCall::Apply(applied)) => Ok(applied.settings),
            Ok(PinboardCall::ApplyFiles(files)) => panic!("not a desktop apply: {files:?}"),
            Ok(PinboardCall::Sound(call)) => panic!("not an apply: {call:?}"),
            Err(refusal) => Err(refusal),
        };
        let fresh = DesktopSettings::default();
        let chosen = served(
            &fresh,
            "region = US\ntime.zone = America/New_York\n",
            Some(&zones),
        )
        .expect("both are held");
        assert_eq!(
            chosen
                .time_zone
                .as_ref()
                .map(tairix_wallpaper::ZoneName::as_str),
            Some("America/New_York")
        );
        assert_eq!(
            served(&fresh, "time.zone = Mars/Olympus\n", Some(&zones)),
            Err(PinboardRefusal::NotInStore(SettingsKey::TimeZone))
        );
        assert_eq!(
            served(&fresh, "region = ZZ\n", Some(&zones)),
            Err(PinboardRefusal::NotInStore(SettingsKey::Region))
        );
        assert_eq!(
            served(&fresh, "region = GB\n", None),
            Err(PinboardRefusal::NotInStore(SettingsKey::Region)),
            "with no store, nothing is held"
        );
        assert!(
            served(&fresh, "region = \n", None).is_ok(),
            "the store's default is always held"
        );
        // A stored choice the store has since lost is not the caller's to
        // answer for: an apply that leaves it alone still lands.
        let stale = tairix_wallpaper::merge(&fresh, "time.zone = Mars/Olympus\n").expect("a shape");
        assert!(served(&stale, "sort = size\n", Some(&zones)).is_ok());
    }

    /// What the user set on the sound devices is the session's own reading of
    /// the audio service, never an application's to plant.
    #[test]
    fn an_apply_naming_the_sound_controls_is_refused_whole() {
        let in_effect = DesktopSettings::default();
        let frame = frame(&PinboardRequest::Apply {
            document: PinboardDocument::new("sort = size\naudio.muted = 0000000000000051.0\n")
                .expect("a bounded document"),
        });
        assert_eq!(
            applied(SESSION_UID, SESSION_UID, &in_effect, &frame),
            Err(PinboardRefusal::Undecodable(DocumentRefusal::OutsideGroup(
                tairix_wallpaper::SettingsKey::AudioMuted
            )))
        );
    }

    /// Counts the unattested requests it is handed.
    struct Unattested(core::sync::atomic::AtomicUsize);

    impl tairix_log::Sink for Unattested {
        fn write_event(&self, event: &tairix_log::Event<'_>) {
            if event.id == PINBOARD_UNATTESTED {
                self.0.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// Another user reaching for this session's desktop is a decision the
    /// audit trail keeps; the session's own requests and their refusals for
    /// other reasons are not.
    #[test]
    fn a_request_from_another_user_is_recorded() {
        let audit = Unattested(core::sync::atomic::AtomicUsize::new(0));
        let in_effect = DesktopSettings::default();
        let files = FilerPreferences::default();
        let served = |caller, request: &[u8]| {
            serve_pinboard(
                &audit,
                (SESSION_UID, caller),
                (&in_effect, &files, None),
                request,
            )
        };
        let count = || audit.0.load(core::sync::atomic::Ordering::Relaxed);
        assert!(served(SESSION_UID, &apply_frame(&edited())).is_ok());
        assert!(matches!(
            served(SESSION_UID, b"garbage"),
            Err(PinboardRefusal::Malformed(_))
        ));
        assert_eq!(count(), 0);
        assert_eq!(
            served(SESSION_UID + 1, &apply_frame(&edited())),
            Err(PinboardRefusal::Unattested)
        );
        assert_eq!(count(), 1);
    }

    #[test]
    fn an_unattested_apply_reads_nothing_and_changes_nothing() {
        let in_effect = DesktopSettings {
            appearance: tairix_abi::desktop::Appearance::Dark,
            ..DesktopSettings::default()
        };
        let frame = apply_frame(&edited());
        assert_eq!(
            applied(SESSION_UID, SESSION_UID + 1, &in_effect, &frame),
            Err(PinboardRefusal::Unattested)
        );
        // The identity check precedes the parse, so the settings in effect
        // are untouched whatever the document said.
        assert_eq!(in_effect.appearance, tairix_abi::desktop::Appearance::Dark);
    }

    /// A sound request is attested exactly as an apply is, and handed on as
    /// what it asks: the policy reads no document of the user's sounds.
    #[test]
    fn a_sound_request_is_attested_and_classified() {
        let in_effect = DesktopSettings::default();
        let document =
            PinboardDocument::new("sound.bell.enabled = false\n").expect("a bounded document");
        for (request, call) in alloc::vec![
            (PinboardRequest::SoundRead, SoundCall::Read),
            (
                PinboardRequest::SoundThemes { from: 8 },
                SoundCall::Themes { from: 8 },
            ),
            (
                PinboardRequest::SoundApply { document },
                SoundCall::Apply(document),
            ),
            (
                PinboardRequest::SoundPick {
                    event: tairix_abi::sound_cue::SoundEvent::Error,
                },
                SoundCall::Pick(tairix_abi::sound_cue::SoundEvent::Error),
            ),
            (
                PinboardRequest::SoundUseTheme {
                    event: tairix_abi::sound_cue::SoundEvent::Bell,
                },
                SoundCall::UseTheme(tairix_abi::sound_cue::SoundEvent::Bell),
            ),
        ] {
            let frame = frame(&request);
            assert_eq!(
                serve_pinboard(
                    &DiscardSink,
                    (SESSION_UID, SESSION_UID),
                    (&in_effect, &FilerPreferences::default(), None),
                    &frame
                ),
                Ok(PinboardCall::Sound(call))
            );
            assert_eq!(
                serve_pinboard(
                    &DiscardSink,
                    (SESSION_UID, SESSION_UID + 1),
                    (&in_effect, &FilerPreferences::default(), None),
                    &frame
                ),
                Err(PinboardRefusal::Unattested)
            );
        }
    }

    /// The file manager's settings are published in the one document beside
    /// the desktop's, and read back with it.
    #[test]
    fn the_file_managers_settings_are_published_and_read_back_with_the_desktops() {
        let mut host = service();
        let files = FilerPreferences {
            view: ViewMode::List,
            unmount_when_closed: false,
            ..FilerPreferences::default()
        };
        let published = publish_pinboard(&mut host, &edited(), &files).expect("publishes");
        assert_eq!((published.settings, published.files), (edited(), files));
        assert_eq!(host.published().get("files.view"), Some("list"));
        assert_eq!(
            host.published().get("fit"),
            Some(WallpaperFit::Centre.as_str())
        );
        let loaded = load_pinboard(&mut host);
        assert_eq!((loaded.settings, loaded.files), (edited(), files));
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn a_stored_file_manager_value_the_registry_refuses_warns_and_keeps_that_default() {
        let mut host = service().with_published(
            "files.view = huge
files.places = true
",
        );
        let loaded = load_pinboard(&mut host);
        assert_eq!(loaded.files.view, FilerPreferences::default().view);
        assert!(loaded.files.places);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("files.view"));
    }

    /// A files apply from the session's own user is merged over the file
    /// manager's settings in effect; each apply admits only its own
    /// registry's keys, so neither channel can write the other's settings.
    #[test]
    fn a_files_apply_is_attested_and_merged_and_admits_only_the_file_managers_keys() {
        let in_effect = DesktopSettings::default();
        let files = FilerPreferences::default();
        let serve = |caller, request: &PinboardRequest| {
            serve_pinboard(
                &DiscardSink,
                (SESSION_UID, caller),
                (&in_effect, &files, None),
                &frame(request),
            )
        };
        let files_apply = |text: &str| PinboardRequest::ApplyFiles {
            document: PinboardDocument::new(text).expect("a bounded document"),
        };
        let mut listed = files;
        listed.view = ViewMode::List;
        assert_eq!(
            serve(SESSION_UID, &files_apply("files.view = list\n")),
            Ok(PinboardCall::ApplyFiles(listed))
        );
        assert_eq!(
            serve(SESSION_UID + 1, &files_apply("files.view = list\n")),
            Err(PinboardRefusal::Unattested)
        );
        assert!(matches!(
            serve(SESSION_UID, &files_apply("fit = centre\n")),
            Err(PinboardRefusal::FilesUndecodable(
                tairix_appconf::Refusal::UnknownKey(_)
            ))
        ));
        assert!(matches!(
            serve(SESSION_UID, &files_apply("files.view = huge\n")),
            Err(PinboardRefusal::FilesUndecodable(
                tairix_appconf::Refusal::InvalidValue(FilerKey::View)
            ))
        ));
        let desktop_apply = PinboardRequest::Apply {
            document: PinboardDocument::new("files.view = list\n").expect("bounded"),
        };
        assert!(matches!(
            serve(SESSION_UID, &desktop_apply),
            Err(PinboardRefusal::Undecodable(_))
        ));
    }

    /// Either apply changes the settings, so either waits while a change is
    /// in hand; a sound request is not one.
    #[test]
    fn a_files_apply_changes_the_settings_as_a_desktop_apply_does() {
        let applied = PinboardApplied {
            settings: DesktopSettings::default(),
            document: PinboardDocument::new("sort = size\n").expect("a bounded document"),
        };
        assert!(PinboardCall::Apply(applied).changes_settings());
        assert!(PinboardCall::ApplyFiles(FilerPreferences::default()).changes_settings());
        assert!(!PinboardCall::Sound(SoundCall::Read).changes_settings());
    }
}
