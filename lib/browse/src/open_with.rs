//! The "Open With…" type→bundle association model (`plans/NEW-FILEMANAGER.md`
//! `FM6b`).
//!
//! When the user asks to open a regular file with a chosen application, the
//! file manager offers the installed bundles whose signed `AppInfo` claims the
//! file's type. This module is the **pure model** behind that offer, host-proven
//! without a kernel exactly as the [`Activation`](crate::activate) decision is:
//!
//! * [`applications_for`] derives a file's content type from its filename
//!   extension through the shared content-type registry
//!   ([`media_for_name`](crate::media::media_for_name)) — the one bridge from a
//!   name (all the VFS listing gives us) to the media-type vocabulary a bundle
//!   declares its associations in. Because that registry is also what the icon
//!   classifier draws from, the applications offered and the glyph shown can
//!   never drift apart. It is a display *hint*, never authority: it decides
//!   which applications are *offered*, and the load gate still verifies and
//!   capability-checks whichever one the user picks.
//! * [`applications_for`] selects the bundles that handle a file's type or any
//!   broader type it is a subclass of
//!   ([`MediaType::parent`](crate::media::MediaType::parent)), so a text editor
//!   declaring `text/plain` is offered for a `.rs` file while an application
//!   declaring `text/x-rust` is offered ahead of it. Bundles that declare the
//!   same type keep the table's order, which [`association_of`] builds in the
//!   store walk's precedence. No match is an **honest empty answer** — the
//!   caller shows a "no application" notice, never a crash and never a
//!   fabricated default.
//! * [`association_of`] is the one rule for which installed bundles open
//!   documents, and [`AppAssociation::opens`] the one matching rule, so the
//!   file manager, the desktop and the picker cannot offer different
//!   applications for one file.
//! * [`note_handed`] and [`Unclaimed`] are what a surface records of an open:
//!   the application a document was handed to and with what access, or the
//!   type no application claimed.
//! * [`OpenWithChooser`] is the surface the user picks on: the ranked
//!   candidates, a selection, and a scroll offset. It is deliberately **not** a
//!   menu — the set grows with the applications a user installs, so no menu
//!   plate can promise to hold it (`plans/NEW-MENUS.md` §6, decision 2).
//!
//! The engine holds no launch authority: it *names* the candidate bundles and
//! *what should happen*; spawning the chosen bundle through the signed load gate
//! stays in the file manager's own capability-checked tail under the user's
//! identity (so the read-only picker, which composes the same engine, never
//! launches). Deciding a file's type here never opens it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::{mime_type_at, AppInfoHeader};
use tairix_appstore::Bundle;
use tairix_fsmeta::preset::acorn::Filetype;
use tairix_log::{EventId, Field, FieldValue, Level, Sink};

use crate::column::ScrollColumn;
use crate::entry::EntryKind;
use crate::media::{ancestry, media_for_named, BlankDocument, MediaType, Site, ALL};
use crate::rowlist::RowList;

/// Event id of a document handed to the application its user opened it
/// with.
pub const DOCUMENT_HANDED: EventId = EventId(38_000);

/// The exact message [`DOCUMENT_HANDED`] is emitted with.
pub const DOCUMENT_HANDED_MESSAGE: &str = "document handed to an application";

/// Event id of a document opened in nothing, because no installed
/// application claims its type.
pub const DOCUMENT_UNCLAIMED: EventId = EventId(38_001);

/// The exact message [`DOCUMENT_UNCLAIMED`] is emitted with.
pub const DOCUMENT_UNCLAIMED_MESSAGE: &str = "document claimed by no application";

/// One installed application and the file types its signed `AppInfo` claims to
/// open — a single "Open With…" candidate.
///
/// The [`mime_types`](Self::mime_types) are the bundle's *own* declared
/// associations (`AppInfo`'s MIME table), never a registry the file manager
/// invents: the manager reads what each bundle claims and offers only those.
/// [`bundle_path`](Self::bundle_path) is the absolute path of the `<Name>.app`
/// directory the caller launches through the ordinary signed load gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppAssociation {
    name: String,
    bundle_path: String,
    mime_types: Vec<String>,
    writes_documents: bool,
    documents_folder: Option<String>,
}

impl AppAssociation {
    /// Construct an association from a bundle's display name, the absolute path
    /// of its `<Name>.app` directory, and the MIME types its `AppInfo` declares.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        bundle_path: impl Into<String>,
        mime_types: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            bundle_path: bundle_path.into(),
            mime_types,
            writes_documents: false,
            documents_folder: None,
        }
    }

    /// The same association for a bundle whose signed manifest declares it
    /// edits the documents it opens.
    #[must_use]
    pub fn writing_documents(mut self) -> Self {
        self.writes_documents = true;
        self
    }

    /// Whether the bundle is handed its documents read-write, where the user
    /// may write them.
    #[must_use]
    pub const fn writes_documents(&self) -> bool {
        self.writes_documents
    }

    /// The folder beneath the user's `Home/Documents/` the bundle's signed
    /// manifest asks to be granted, as the manifest spells it.
    #[must_use]
    pub fn documents_folder(&self) -> Option<&str> {
        self.documents_folder.as_deref()
    }

    /// The bundle's human-readable name — the "Open With…" menu label.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The absolute path of the `<Name>.app` bundle to launch through the
    /// signed load gate.
    #[must_use]
    pub fn bundle_path(&self) -> &str {
        &self.bundle_path
    }

    /// The MIME types the bundle's `AppInfo` declares it can open.
    #[must_use]
    pub fn mime_types(&self) -> &[String] {
        &self.mime_types
    }

    /// Whether this bundle declares an association with `mime`, matched
    /// ASCII-case-insensitively so a type reads the same however it was cased.
    ///
    /// This is the bundle's *own* declaration, tested exactly: a bundle that
    /// declares only `text/plain` does not "handle" `text/x-rust` here.
    /// Whether it opens a Rust file is [`opens`](Self::opens)'s question,
    /// which walks the subclass chain and asks this one once per broader type.
    #[must_use]
    pub fn handles(&self, mime: &str) -> bool {
        self.mime_types
            .iter()
            .any(|declared| declared.eq_ignore_ascii_case(mime))
    }

    /// Whether this bundle opens a file named `name`, whose volume records
    /// `recorded` as its RISC OS filetype: whether it claims the file's type
    /// or any broader type it is a subclass of.
    ///
    /// The one matching rule: Open With, a drop on an icon-bar slot and a
    /// folder pick all ask it, so no surface offers a bundle another would
    /// refuse. An empty name names no file and is opened by nothing.
    #[must_use]
    pub fn opens(&self, name: &str, recorded: Option<Filetype>) -> bool {
        document_media(name, recorded).is_some_and(|media| self.claim(media).is_some())
    }

    /// How far up `media`'s subclass chain the first type this bundle
    /// declares lies: 0 for the file's own type.
    fn claim(&self, media: MediaType) -> Option<usize> {
        ancestry(media).position(|claim| self.handles(claim.as_str()))
    }
}

/// What a document named `name`, its volume recording `recorded`, is opened
/// as, or `None` for an empty name, which names no file.
fn document_media(name: &str, recorded: Option<Filetype>) -> Option<MediaType> {
    (!name.is_empty()).then(|| media_for_named(name, EntryKind::File, recorded, Site::Plain))
}

/// The association an installed bundle the store walk found offers, or `None`
/// for one that opens no document.
///
/// The one rule for which bundles open documents, so the file manager and the
/// desktop offer the same applications whichever stores each walks: a command
/// or an application, in a place its signed kind may run from, whose declared
/// MIME table decodes. A service, a component or a screensaver is started by
/// the system and never with a document, and a bundle somewhere its kind may
/// not run would be refused by the load gate, so neither is offered.
///
/// **Fail-closed**: a MIME table that is malformed or non-UTF-8 yields `None`,
/// so a corrupt bundle is skipped rather than offered on a guess. The MIME set
/// is a display *hint* only: nothing here verifies the manifest signature (the
/// signed load gate does that when the chosen bundle is launched), it only
/// reads what the bundle claims.
#[must_use]
pub fn association_of(bundle: &Bundle<'_>) -> Option<AppAssociation> {
    let kind = bundle.header.program_kind()?;
    if !kind.is_searched() || !kind.launchable_from(bundle.path) {
        return None;
    }
    association_from_manifest(bundle.path, bundle.header, bundle.manifest)
}

/// The association the decoded manifest of the bundle at `bundle_path`
/// declares, the MIME table read from the body past the header.
fn association_from_manifest(
    bundle_path: &str,
    header: &AppInfoHeader,
    manifest: &[u8],
) -> Option<AppAssociation> {
    let body = manifest.get(AppInfoHeader::WIRE_LEN..)?;
    let caps = usize::from(header.capability_count);
    let mut mimes = Vec::with_capacity(usize::from(header.mime_count));
    for index in 0..usize::from(header.mime_count) {
        mimes.push(mime_type_at(body, caps, index).ok()?.to_string());
    }
    let mut association = AppAssociation::new(header.bundle_title(), bundle_path, mimes);
    association.documents_folder = header.documents_folder().map(String::from);
    Some(if header.writes_documents() {
        association.writing_documents()
    } else {
        association
    })
}

/// The installed applications that can open a file named `name`, most specific
/// declaration first — the "Open With…" candidate list.
///
/// The file's type is derived by the shared content-type registry
/// ([`media_for_name`](crate::media::media_for_name)) and named by its media-type spelling
/// ([`MediaType::as_str`](crate::media::MediaType::as_str)), so the association
/// vocabulary is exactly the one the icon classifier draws from — the two can
/// never drift apart.
///
/// A bundle is offered when it [`opens`](AppAssociation::opens) the file. The
/// order is the one precedence rule: the bundle claiming the file most
/// specifically first — an application declaring the file's own type before
/// one declaring an ancestor — and at one specificity, `bundles`' own order.
/// A table built from the store walk through [`association_of`] is in the
/// walk's precedence, so the system's stores outrank the user's and, within a
/// store, a shallower bundle a deeper one, then folder and name order.
///
/// The file is typed as [`media_for_document`](crate::media_for_document)
/// types it, from its name and `recorded`, the RISC OS filetype its listing
/// found recorded for it. A file neither types is
/// [`ApplicationOctetStream`](crate::media::MediaType::ApplicationOctetStream):
/// bytes. The result is empty — an honest "no application" answer — for an
/// empty name, which names no file, and when no installed bundle claims the
/// file's type or any of its broader types; it never falls back to a guessed
/// default.
#[must_use]
pub fn applications_for<'a>(
    name: &str,
    recorded: Option<Filetype>,
    bundles: &'a [AppAssociation],
) -> Vec<&'a AppAssociation> {
    let Some(media) = document_media(name, recorded) else {
        return Vec::new();
    };
    let mut ranked: Vec<(usize, &AppAssociation)> = bundles
        .iter()
        .filter_map(|bundle| bundle.claim(media).map(|distance| (distance, bundle)))
        .collect();
    ranked.sort_by_key(|(distance, _)| *distance);
    ranked.into_iter().map(|(_, bundle)| bundle).collect()
}

/// Record that a document the user opened was handed to the bundle at
/// `bundle`, for writing where `writable`. The document's name is the user's
/// and stays out of the record.
pub fn note_handed<S: Sink + ?Sized>(audit: &S, bundle: &str, writable: bool) {
    tairix_log::log(
        audit,
        &tairix_log::Event {
            level: Level::Info,
            id: DOCUMENT_HANDED,
            message: DOCUMENT_HANDED_MESSAGE,
            fields: &[
                Field {
                    key: "application",
                    value: FieldValue::Str(bundle),
                },
                Field {
                    key: "access",
                    value: FieldValue::Str(if writable { "write" } else { "read" }),
                },
            ],
        },
    );
}

/// A document opened in nothing because no installed application claims it:
/// the type it was taken for, which is all its record keeps.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Unclaimed(MediaType);

impl Unclaimed {
    /// The document named `name`, its volume recording `recorded`.
    #[must_use]
    pub fn of(name: &str, recorded: Option<Filetype>) -> Self {
        Self(media_for_named(
            name,
            EntryKind::File,
            recorded,
            Site::Plain,
        ))
    }

    /// The type the document was taken for.
    #[must_use]
    pub const fn media(self) -> MediaType {
        self.0
    }

    /// Record it under [`DOCUMENT_UNCLAIMED`].
    pub fn note<S: Sink + ?Sized>(self, audit: &S) {
        tairix_log::log(
            audit,
            &tairix_log::Event {
                level: Level::Info,
                id: DOCUMENT_UNCLAIMED,
                message: DOCUMENT_UNCLAIMED_MESSAGE,
                fields: &[Field {
                    key: "type",
                    value: FieldValue::Str(self.0.as_str()),
                }],
            },
        );
    }
}

/// The documents New ▸ offers: every [`BlankDocument`] type an installed
/// application both writes and declares by name, in registry order.
///
/// The declaration must be exact. An editor declaring `text/plain` opens Rust
/// source through the subclass chain, but counting that would make every text
/// editor a maker of every textual format.
#[must_use]
pub fn blank_documents(bundles: &[AppAssociation]) -> Vec<BlankDocument> {
    ALL.iter()
        .filter_map(|media| BlankDocument::of(*media))
        .filter(|document| {
            bundles.iter().any(|bundle| {
                bundle.writes_documents() && bundle.handles(document.media().as_str())
            })
        })
        .collect()
}

/// Most candidate applications the context menu's "Open With…" submenu
/// offers.
///
/// A **format** bound on a plate, not on the candidate set: a plate does not
/// scroll and cannot promise to hold a list that grows with what a user
/// installs, so the submenu offers the highest-ranked candidates that fit and
/// the row's own click opens the complete, scrolling chooser
/// ([`OpenWithChooser`]). Half a plate's worth, so the menu stays a menu with
/// its command rows still on it.
pub const OPEN_WITH_QUICK_MAX: usize = 6;

/// The highest-ranked candidates a menu plate can promise to hold, from the
/// ranked list [`applications_for`] answered.
///
/// Ranked order is preserved, so the submenu's first row is the application
/// that claims the file most specifically — the same one the chooser opens on.
/// An empty answer is the honest "nothing to offer as a submenu"; the row then
/// carries no chevron and its click opens the chooser exactly as before.
#[must_use]
pub fn quick_applications<'a>(ranked: &[&'a AppAssociation]) -> Vec<&'a AppAssociation> {
    ranked.iter().take(OPEN_WITH_QUICK_MAX).copied().collect()
}

/// One candidate application the "Open With…" chooser offers: what the row
/// says, and the bundle a chosen row launches.
///
/// The chooser holds its own copies rather than borrowing the enumerated
/// associations, because the enumeration is a one-shot read of the app store
/// and the chooser outlives it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenWithCandidate {
    name: String,
    bundle_path: String,
    writes_documents: bool,
}

impl OpenWithCandidate {
    /// One candidate: what its row says, and the bundle a choice of it
    /// launches.
    ///
    /// Public because the chooser is not the only surface that offers
    /// candidates: the context menu's own submenu offers the top of the same
    /// ranked list, and the answer it reads back has to name the same
    /// candidates the rows were built from.
    #[must_use]
    pub fn new(name: impl Into<String>, bundle_path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            bundle_path: bundle_path.into(),
            writes_documents: false,
        }
    }

    /// The candidate an association offers, carrying whether it edits.
    #[must_use]
    pub fn of(association: &AppAssociation) -> Self {
        Self {
            writes_documents: association.writes_documents(),
            ..Self::new(association.name(), association.bundle_path())
        }
    }

    /// Whether the candidate is handed its documents read-write, where the
    /// user may write them.
    #[must_use]
    pub const fn writes_documents(&self) -> bool {
        self.writes_documents
    }

    /// The bundle's human-readable name — the chooser row's label.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The absolute path of the `<Name>.app` bundle to launch through the
    /// signed load gate.
    #[must_use]
    pub fn bundle_path(&self) -> &str {
        &self.bundle_path
    }
}

/// The open "Open With…" chooser: the candidate applications, which one is
/// current, and where the list is scrolled to.
///
/// **This is not a menu** (`plans/NEW-MENUS.md` §6, decision 2). The candidate
/// set is as long as the applications a user has installed, so no format bound
/// can promise a plate holds it, and the desktop's menu model crosses the wire
/// complete, so no row of one can be filled in lazily. A chooser over an
/// unbounded set is a *list*, and a list scrolls: this one is the file
/// manager's own modal surface, reached by the one context-menu row that
/// concludes the chain.
///
/// It is a pure model — the candidates, a selection, and a scroll offset — so
/// what the chooser *decides* is host-proven, exactly as the association
/// matching above it is. It performs nothing: launching the chosen bundle is
/// the file manager's own capability-checked hand-off under the user's
/// identity, so composing it grants no authority (the read-only picker never
/// launches, so it never builds one). Its rows are scrolled, revealed, and hit
/// through the renderer, which knows the geometry
/// ([`open_with_scroll_wheel`](crate::render::open_with_scroll_wheel),
/// [`open_with_reveal`](crate::render::open_with_reveal)).
#[derive(Clone, Debug)]
pub struct OpenWithChooser {
    candidates: Vec<OpenWithCandidate>,
    file_path: String,
    display_name: String,
    /// Which candidate is current — the one shared row cursor, so the
    /// chooser's traversal and the Properties window's attribute list clamp
    /// identically.
    rows: RowList,
    /// Where the list is scrolled to, and its bar.
    scroll: ScrollColumn,
}

impl OpenWithChooser {
    /// Open a chooser over `apps` — the candidates [`applications_for`]
    /// returned, in that ranked order — for the file at absolute `file_path`,
    /// whose leaf name is `display_name`.
    ///
    /// `None` when `apps` is empty: no installed application claiming the type
    /// is an honest answer the caller states, never an empty chooser.
    #[must_use]
    pub fn new(
        apps: &[&AppAssociation],
        file_path: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Option<Self> {
        if apps.is_empty() {
            return None;
        }
        let candidates: Vec<OpenWithCandidate> =
            apps.iter().map(|app| OpenWithCandidate::of(app)).collect();
        Some(Self {
            rows: RowList::new(candidates.len()),
            candidates,
            file_path: file_path.into(),
            display_name: display_name.into(),
            scroll: ScrollColumn::new(),
        })
    }

    /// The candidates, most specific claim first.
    #[must_use]
    pub fn candidates(&self) -> &[OpenWithCandidate] {
        &self.candidates
    }

    /// Which candidate is current.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.rows.cursor()
    }

    /// The current candidate — what activating the chooser launches.
    ///
    /// A chooser is never built over an empty list and [`select`](Self::select)
    /// clamps, so this always answers; it reads the row rather than indexing
    /// it, so an index that somehow left the list refuses instead of faulting.
    #[must_use]
    pub fn chosen(&self) -> Option<&OpenWithCandidate> {
        self.candidates.get(self.rows.cursor())
    }

    /// The absolute path of the file the chosen application opens.
    #[must_use]
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// The file's leaf name — the title handed to the launched application.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// How far the list is scrolled, in pixels.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.scroll.offset()
    }

    /// Make `index` current, clamped to the candidates, reporting whether the
    /// selection moved.
    pub fn select(&mut self, index: usize) -> bool {
        self.rows.select(index)
    }

    /// Move the selection by `delta` rows (positive moves toward the end),
    /// stopping at either end, reporting whether it moved.
    pub fn step(&mut self, delta: i64) -> bool {
        self.rows.step(delta)
    }

    /// Where the list is scrolled to, and its bar.
    pub(crate) const fn scroll(&self) -> &ScrollColumn {
        &self.scroll
    }

    /// The same, for the renderer's scrolling paths to move.
    pub(crate) fn scroll_mut(&mut self) -> &mut ScrollColumn {
        &mut self.scroll
    }

    /// When the list's held scrollbar press steps next, as a monotonic
    /// instant, or `None` when nothing held would move.
    pub fn scroll_repeat_deadline(&mut self, now_ns: u64) -> Option<u64> {
        self.scroll.repeat_deadline(now_ns)
    }
}
