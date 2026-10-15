//! Resolving an icon to drawable pixels: the shared artwork layer.
//!
//! [`IconKind`] and [`builtin_picture`] give the desktop a *total* set of
//! scalable built-in pictures — every kind always draws something: a settings
//! category its colour badge, every other kind a tintable glyph.
//! On top of that floor this module adds the preferred tiers: the shipped
//! **class artwork** under `/System/Graphics/Icons`, as either an
//! `<asset-id>.png` raster master or an `<asset-id>.svg` vector one, and above
//! it the icon a thing carries of its *own* — an application bundle's
//! `Resources/` master, named by its signed manifest. A draw site states both
//! in one [`IconRequest`] and the answer resolves in one order: the thing's
//! own icon, then its class's raster artwork, then its class's vector
//! artwork, then the built-in picture. Resolution is therefore total however
//! much of it is missing, and the order lives here rather than being
//! re-decided by each surface.
//!
//! Both the desktop session and the file manager are separate processes that
//! need this exact behaviour, so it lives here rather than in either of them.
//! Like [`IconAssetSource`](crate::IconAssetSource), the crate stays `no_std`
//! and owns no path to the filesystem or to a decoder: the bytes come through
//! an injected [`ArtworkReader`] (a capability-gated read) and the pixels
//! through an injected [`ArtworkRasteriser`] (the parser sandbox in
//! production), so the untrusted decode never runs in this library or in the
//! renderer that consumes it.
//!
//! # Who does the decode, and on which thread
//!
//! A miss costs a read plus a sandbox round trip, which is far too much to
//! spend inside a compositor's paint. [`ArtworkResolver`] is therefore the seam
//! between *deciding what a draw needs* and *producing it*: [`InlineArtwork`]
//! reads and decodes on the calling thread, and a caller with a worker thread
//! implements the trait over its own hand-off and answers [`Resolved::Pending`]
//! until the pixels land — the draw falls to the built-in glyph in the
//! meantime and the same lookup serves the artwork once it is there. Both use
//! [`render_artwork`], so an off-thread decode is the very same work the
//! inline one would have done.
//!
//! [`ArtworkCache`] retains each decode — success *or* refusal — keyed by what
//! was resolved and the requested side, over the one shared reclaimable-memory
//! cache (`lib/reclaim`) so a crowded or crafted bundle store can never grow a
//! session without bound. [`IconArtworkSource`] binds a cache to its resolver
//! so a renderer can be handed a plain [`IconArtwork`] lookup that borrows the
//! pixels without knowing anything about I/O, and [`NoArtwork`] is the
//! all-glyph lookup a headless build or a test uses.

use alloc::string::String;
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::cell::Cell;
use core::hash::{Hash, Hasher};

use tairix_abi::appinfo::BUNDLE_SUFFIX;
use tairix_abi::BundleEntry;
use tairix_appstore::{decode_manifest, identity_roots, manifest_path};
use tairix_hash::BuildFastHash;
use tairix_log::Sink;

use tairix_geometry::Point;
use tairix_raster::{cast_shadow, Color, ShadowCast, Surface};
use tairix_reclaim::{
    working_set_ui_budget, working_set_ui_cache, CacheLedger, CachedBytes, PressureGauge,
    ReclaimCache, Served,
};
use tairix_util::fallible;

use crate::badge::badge_picture;
use crate::folder::{
    card_slots, paper_card, print_card, DrawnSample, FolderSample, MIN_COMPOSITE_SIDE,
};
use crate::glyph::{builtin_icon, IconKind};
use crate::picture::{Artwork, CastShadow, Fitted, IconPicture};
use crate::thumbnail::{render_thumbnail, ArtworkDocument, DocumentStamp, Reading, Thumbnail};

/// Where the OS ships its desktop graphics assets.
pub const GRAPHICS_DIR: &str = "/System/Graphics";

/// The icon subdirectory of [`GRAPHICS_DIR`].
pub const ICONS_DIR: &str = "/System/Graphics/Icons";

/// Largest icon artwork file the desktop will ever read, in bytes.
///
/// A fixed validation bound on untrusted input, not a growable capacity: real
/// desktop icon artwork is a tiny fraction of this, so the ceiling exists
/// purely to bound how much hostile work a single asset can demand before any
/// byte is decoded. This is the one definition of that bound — the sandboxed
/// rasteriser refuses over-long input against the same value.
pub const MAX_ARTWORK_BYTES: usize = 256 * 1024;

/// Largest source side, in pixels, an icon is ever decoded at.
///
/// A fixed validation bound like [`MAX_ARTWORK_BYTES`], and for the same
/// reason: kept well above any real icon master's native resolution but far
/// below a size that would turn one small tile into an expensive decode. It
/// is deliberately independent of the output side a draw site asks for, so a
/// tiny request cannot smuggle a huge source image through. One definition,
/// shared by the sandboxed decoder that enforces it at runtime and the image
/// build that refuses artwork the desktop would later refuse.
pub const MAX_ARTWORK_SIDE: u32 = 2048;

/// Smallest side, in pixels, a shipped icon master may have.
///
/// Icon artwork is authored at a resolution that exceeds the slots the
/// desktop draws it in, so a slot only ever downscales — an upscaled master
/// is visibly soft. This is the floor the image build holds every shipped
/// master and every bundle's own icon to; it is a *quality* contract on
/// first-party artwork, not a validation bound on untrusted input, so the
/// runtime still draws a smaller icon a third party ships rather than
/// refusing it.
pub const MIN_ARTWORK_SIDE: u32 = 256;

/// Extension of a raster class master, the format the class tier prefers.
const RASTER_SUFFIX: &str = ".png";

/// Extension of a vector class master, the format the class tier falls back
/// to.
const VECTOR_SUFFIX: &str = ".svg";

/// The on-disk path of the vector asset for `kind` (`<ICONS_DIR>/<id>.svg`),
/// or `None` when the heap refuses it, so a draw falls a tier.
#[must_use]
pub fn icon_vector_path(kind: IconKind) -> Option<String> {
    fallible::concatenated(&[ICONS_DIR, "/", kind.asset_id(), VECTOR_SUFFIX])
}

/// The on-disk path of the raster artwork for `kind` (`<ICONS_DIR>/<id>.png`),
/// or `None` when the heap refuses it, so a draw falls a tier.
#[must_use]
pub fn icon_artwork_path(kind: IconKind) -> Option<String> {
    fallible::concatenated(&[ICONS_DIR, "/", kind.asset_id(), RASTER_SUFFIX])
}

/// Whether `name` is a legal shipped-artwork file name: a known
/// [`IconKind::asset_id`] followed by `.png` or `.svg`, and nothing else.
///
/// Both class formats are legal because both tiers of
/// [`ArtworkCache::artwork`] read from this one directory; which of the two a
/// kind ships is the artwork's business, not the name check's.
///
/// Used by the image build to refuse an asset the desktop could never
/// resolve. The identity check is exact — a name that decodes to a kind whose
/// own `asset_id` does not spell that same stem (an unknown id, a wrong
/// extension, an empty name, or a path with directory separators such as
/// `../../etc/x.png`) is rejected, so this never accepts a name the loader
/// would not later map back to the same kind.
#[must_use]
pub fn artwork_kind_for_file(name: &str) -> Option<IconKind> {
    let stem = name
        .strip_suffix(RASTER_SUFFIX)
        .or_else(|| name.strip_suffix(VECTOR_SUFFIX))?;
    let kind = IconKind::for_asset(stem);
    (kind.asset_id() == stem).then_some(kind)
}

/// Reads an asset's bytes.
///
/// The production reader is a capability-gated filesystem read; tests supply a
/// fake. `None` for any unreadable path — a missing or refused asset is never
/// fatal, the caller falls back to the glyph.
pub trait ArtworkReader {
    /// The bytes at `path`, or `None` when the path is missing or unreadable.
    fn read(&mut self, path: &str) -> Option<Vec<u8>>;

    /// The file at `path`, opened to stream a thumbnail from, or `None` when
    /// it is missing or unreadable.
    ///
    /// The default opens nothing, which is right for a reader of icon assets
    /// alone: every picture file then draws its class picture.
    fn open(&mut self, _path: &str) -> Option<alloc::boxed::Box<dyn ArtworkDocument + '_>> {
        None
    }
}

/// Turns encoded icon bytes into `side`×`side` straight-alpha RGBA8.
///
/// The desktop backs this with the parser sandbox so the untrusted decode
/// never runs in the calling process; tests supply a fake. The rasteriser is
/// trusted only to the extent the caller verifies: [`ArtworkCache`] re-checks
/// the returned pixel length before building a surface from it.
pub trait ArtworkRasteriser {
    /// Rasterise `bytes` to a `side`-pixel square of straight-alpha RGBA8, or
    /// refuse with `None`.
    fn rasterise(&mut self, side: u32, bytes: &[u8]) -> Option<Vec<u8>>;

    /// Decode the picture `document` holds, its format read as `reading`
    /// says, fitted inside a `side`-pixel square of straight-alpha RGBA8 and
    /// centred on it, with where in the square the picture lies, or refuse
    /// with `None`.
    ///
    /// The default refuses, which is right for a rasteriser of icons alone.
    fn thumbnail(
        &mut self,
        _side: u32,
        _reading: Reading,
        _document: &mut dyn ArtworkDocument,
    ) -> Option<Fitted> {
        None
    }
}

impl<T: ArtworkReader + ?Sized> ArtworkReader for &mut T {
    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        (**self).read(path)
    }

    fn open(&mut self, path: &str) -> Option<alloc::boxed::Box<dyn ArtworkDocument + '_>> {
        (**self).open(path)
    }
}

impl<T: ArtworkRasteriser + ?Sized> ArtworkRasteriser for &mut T {
    fn rasterise(&mut self, side: u32, bytes: &[u8]) -> Option<Vec<u8>> {
        (**self).rasterise(side, bytes)
    }

    fn thumbnail(
        &mut self,
        side: u32,
        reading: Reading,
        document: &mut dyn ArtworkDocument,
    ) -> Option<Fitted> {
        (**self).thumbnail(side, reading, document)
    }
}

/// What a resolver has for one cache slot.
pub enum Resolved {
    /// The decode has run. `None` is a refusal — an absent, over-long, or
    /// undecodable asset — which the cache retains just like artwork, so the
    /// same bad asset is never read twice.
    Done(Option<Artwork>),
    /// Nobody has decoded it yet. The cache retains nothing and the draw site
    /// falls back to the tier below, which for the last tier is the built-in
    /// glyph; the same lookup answers with pixels once the producer has them.
    Pending,
}

/// How one cache miss becomes pixels.
///
/// [`InlineArtwork`] is the whole of what a program without a worker thread
/// needs. A program that has one implements this over its own hand-off so a
/// paint never waits on a disk or a sandbox: it answers [`Resolved::Pending`]
/// for a key it has just queued and [`Resolved::Done`] once the worker has
/// delivered.
pub trait ArtworkResolver {
    /// Produce what `key` names at `side` pixels.
    fn resolve(&mut self, key: &ArtworkKey, side: u32) -> Resolved;

    /// Start producing `key` at `side` without waiting for it.
    ///
    /// A caller that knows what it is *about* to draw says so here, so the
    /// decode is finished before the frame that needs it rather than after it.
    /// Without that, a surface showing a screenful of icons paints every one of
    /// them as a built-in glyph and only replaces them a round trip later.
    ///
    /// The default does nothing, which is right for a resolver that produces on
    /// the calling thread: it has nothing to prepare, and "preparing" would be
    /// exactly the stall a caller prefetches to avoid.
    fn prefetch(&mut self, _key: &ArtworkKey, _side: u32) {}

    /// The cache could not retain what this resolver produced for `key` at
    /// `side`: its budget has no room the current pressure band allows.
    ///
    /// The draw that asked has already fallen back to the built-in glyph. What
    /// matters is the *next* one: a resolver that simply re-answers will have
    /// the same answer refused again, so a desktop redrawing its icons pays a
    /// read and a decode per icon per repaint and never keeps one. A resolver
    /// that defers should therefore stop offering this key until something
    /// changes the answer — the band relaxing, or a scale change asking for a
    /// different side.
    ///
    /// The default does nothing, which is right for a resolver that produces
    /// on the calling thread: it holds no queue to hold back.
    fn declined(&mut self, _key: &ArtworkKey, _side: u32) {}
}

/// The resolver that reads and decodes on the calling thread.
///
/// Correct wherever there is no worker thread to hand the decode to — a
/// program with one window, a process the kernel granted no thread, a host
/// test — and the exact work a worker performs, since both go through
/// [`render_artwork`].
pub struct InlineArtwork<R: ArtworkReader, D: ArtworkRasteriser> {
    reader: R,
    rasteriser: D,
}

impl<R: ArtworkReader, D: ArtworkRasteriser> InlineArtwork<R, D> {
    /// Read through `reader` and decode through `rasteriser`.
    pub const fn new(reader: R, rasteriser: D) -> Self {
        Self { reader, rasteriser }
    }
}

impl<R: ArtworkReader, D: ArtworkRasteriser> ArtworkResolver for InlineArtwork<R, D> {
    fn resolve(&mut self, key: &ArtworkKey, side: u32) -> Resolved {
        // A thumbnail reads and decodes a whole picture file, which a thread
        // that owes a frame cannot afford: the tile draws the tier below.
        if key.is_thumbnail_class() {
            return Resolved::Done(None);
        }
        Resolved::Done(render_artwork(
            &mut self.reader,
            &mut self.rasteriser,
            key,
            side,
        ))
    }
}

/// A thing's *own* artwork, preferred over its kind's shipped artwork.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum OwnIcon<'a> {
    /// An asset path the caller has already resolved (the program-library
    /// catalog stores one per listed application).
    Asset(&'a str),
    /// A `<Name>.app` directory whose signed manifest names the asset. The
    /// artwork layer reads and validates the manifest itself, so a draw site
    /// holding only a directory entry needs no manifest knowledge of its own.
    Bundle(&'a str),
    /// The bundle of the program of this *name*, resolved through the
    /// program-store order, with the asking session's own home root so its
    /// user's stores are searched last.
    Program {
        /// The program's name.
        name: &'a str,
        /// The asking session's home root, if it has one.
        home: Option<&'a str>,
    },
    /// A folder's picture of what it holds.
    Folder(&'a FolderSample),
    /// A picture file drawn as its own content.
    Thumbnail {
        /// The file's absolute path.
        path: &'a str,
        /// The version of the file the listing named.
        stamp: DocumentStamp,
        /// How its format is read.
        reading: Reading,
    },
}

/// What a draw site is asking for a picture of.
///
/// Every request names the [`IconKind`] that always resolves — the shipped
/// class artwork, and failing that the built-in glyph — and may additionally
/// name the thing's own icon, which takes precedence when it resolves. The
/// resulting order (own icon, then class artwork, then glyph) is the desktop's
/// one icon-resolution rule, stated here so every surface obeys the same one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct IconRequest<'a> {
    kind: IconKind,
    own: Option<OwnIcon<'a>>,
}

impl<'a> IconRequest<'a> {
    /// A picture for a `kind` alone: shipped class artwork, else the glyph.
    #[must_use]
    pub const fn kind(kind: IconKind) -> Self {
        Self { kind, own: None }
    }

    /// A picture for a thing whose own asset path is already known, falling
    /// back to `kind` when that asset will not serve.
    #[must_use]
    pub const fn asset(kind: IconKind, path: &'a str) -> Self {
        Self {
            kind,
            own: Some(OwnIcon::Asset(path)),
        }
    }

    /// A picture for the application bundle at `dir` (a `<Name>.app`
    /// directory), falling back to `kind` when the bundle declares no icon or
    /// its icon will not serve.
    #[must_use]
    pub const fn bundle(kind: IconKind, dir: &'a str) -> Self {
        Self {
            kind,
            own: Some(OwnIcon::Bundle(dir)),
        }
    }

    /// A picture for the program called `name`, resolved to the first bundle of
    /// that name in the fixed program-store order and then through that
    /// bundle's own manifest; falling back to `kind` when no such bundle
    /// declares an icon that will serve.
    ///
    /// What a surface listing *processes* asks for. The kernel attests a task's
    /// name from the store path it loaded and carries no image path, so for a
    /// task nobody has separately attested a bundle for, the name is the
    /// identity there is — and it is not caller-supplied, so a task cannot
    /// choose the picture it wears.
    #[must_use]
    pub const fn program(kind: IconKind, name: &'a str, home: Option<&'a str>) -> Self {
        Self {
            kind,
            own: Some(OwnIcon::Program { name, home }),
        }
    }

    /// A picture file at `path`, at the version its listing named, drawn as
    /// its own content and falling back to `kind` where that will not serve.
    #[must_use]
    pub const fn thumbnail(
        kind: IconKind,
        path: &'a str,
        stamp: DocumentStamp,
        reading: Reading,
    ) -> Self {
        Self {
            kind,
            own: Some(OwnIcon::Thumbnail {
                path,
                stamp,
                reading,
            }),
        }
    }

    /// A picture of a folder holding `sample`: its cards fanned out of it,
    /// falling back to the plain filled folder where that picture will not
    /// draw, and the plain filled folder outright for an empty sample. While
    /// the members' own pictures are produced the folder draws its cards as
    /// their kinds.
    #[must_use]
    pub fn folder(sample: &'a FolderSample) -> Self {
        Self {
            kind: IconKind::FolderFilled,
            own: if sample.is_empty() {
                None
            } else {
                Some(OwnIcon::Folder(sample))
            },
        }
    }

    /// The kind that resolves when nothing of the thing's own does.
    #[must_use]
    pub const fn icon_kind(&self) -> IconKind {
        self.kind
    }

    /// The candidates this request resolves through, in the order they are
    /// tried. The one statement of the desktop's icon-resolution order.
    pub(crate) fn tiers(self) -> impl Iterator<Item = Tier<'a>> {
        let kinds = match self.own {
            Some(OwnIcon::Folder(sample)) if sample.has_pictures() => {
                Some(Tier::FolderKinds(sample))
            }
            _ => None,
        };
        [
            self.own.map(Tier::Own),
            kinds,
            Some(Tier::Raster(self.kind)),
            Some(Tier::Vector(self.kind)),
        ]
        .into_iter()
        .flatten()
    }
}

/// One candidate in the resolution order.
///
/// Held as the kind rather than as a built path so a tier that is never
/// reached never formats one.
#[derive(Clone, Copy)]
pub(crate) enum Tier<'a> {
    /// The thing's own icon.
    Own(OwnIcon<'a>),
    /// A folder holding pictures, drawn with each picture card as its kind.
    FolderKinds(&'a FolderSample),
    /// The class's shipped raster master.
    Raster(IconKind),
    /// The class's shipped vector master.
    Vector(IconKind),
}

impl<'a> Tier<'a> {
    /// Whether this candidate is `key`, compared without building a key.
    pub(crate) fn names(self, key: &ArtworkKey) -> bool {
        self.view() == key.view()
    }

    /// The artwork this candidate names, borrowed from the request.
    fn view(self) -> ArtView<'a> {
        match self {
            Self::Own(OwnIcon::Asset(path)) => ArtView::Asset(path),
            Self::Own(OwnIcon::Bundle(dir)) => ArtView::Bundle(dir),
            Self::Own(OwnIcon::Program { name, home }) => ArtView::Program { name, home },
            Self::Own(OwnIcon::Folder(sample)) => ArtView::Folder(sample.drawn()),
            Self::FolderKinds(sample) => ArtView::Folder(sample.drawn_as_kinds()),
            Self::Own(OwnIcon::Thumbnail {
                path,
                stamp,
                reading,
            }) => ArtView::Thumbnail {
                path,
                stamp,
                reading,
            },
            Self::Raster(kind) => ArtView::Raster(kind),
            Self::Vector(kind) => ArtView::Vector(kind),
        }
    }
}

/// A draw-site lookup: the pre-rasterised picture for `request` at `side`, or
/// `None` when this lookup holds nothing at all.
///
/// The borrow keeps the pixels in the cache the lookup owns, so a grid that
/// draws many icons a frame reads each surface in place rather than copying
/// it.
pub trait IconArtwork {
    /// The picture for `request` at `side` pixels.
    ///
    /// A real cache answers for *every* request: the built-in glyph is the last
    /// tier and always resolves, so a draw site never has to rasterise vector
    /// art itself. `None` means this lookup is [`NoArtwork`] — it holds no
    /// cache to rasterise into, and the caller falls back to drawing the glyph
    /// inline.
    fn artwork(&mut self, request: IconRequest<'_>, side: u32) -> Option<IconPicture<'_>>;

    /// [`artwork`](Self::artwork), casting `shadow` beneath the picture when
    /// one is given: the same picture, with the soft shadow its own coverage
    /// casts, retained beside it so no frame blurs.
    ///
    /// The default casts none, which is right for a lookup that retains
    /// nothing.
    fn shadowed(
        &mut self,
        request: IconRequest<'_>,
        side: u32,
        _shadow: Option<ShadowCast>,
    ) -> Option<IconPicture<'_>> {
        self.artwork(request, side)
    }
}

/// The refusing read-and-decode seam: no asset is ever produced.
///
/// What a cache resolves through before its embedder installs the real one,
/// and what a process holding no filesystem or spawn authority resolves
/// through permanently — a monitor that may sample the system but must not
/// read a user's files, say. Every request refuses, so every icon falls to its
/// built-in glyph.
///
/// A cache built on this is still worth holding: the glyph tier is resolved in
/// this process and retained like any other entry, and resolving a
/// multi-layer glyph's coverage is the expensive part (tens of microseconds
/// per icon), so the surface pays it once instead of once per frame.
pub struct NoArtworkSeam;

impl ArtworkReader for NoArtworkSeam {
    fn read(&mut self, _path: &str) -> Option<Vec<u8>> {
        None
    }
}

impl ArtworkRasteriser for NoArtworkSeam {
    fn rasterise(&mut self, _side: u32, _bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// The all-glyph lookup: never any artwork.
///
/// Used by a headless build with no shipped raster assets and by tests: every
/// query returns `None`, so every draw site falls back to its built-in glyph.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoArtwork;

impl IconArtwork for NoArtwork {
    fn artwork(&mut self, _request: IconRequest<'_>, _side: u32) -> Option<IconPicture<'_>> {
        None
    }
}

/// One decode outcome, retained: the rasterised artwork, or the refusal that
/// decoding it produced.
///
/// A refusal is worth remembering — it stops a malformed or absent asset being
/// re-read on every frame — and it is charged its bookkeeping like any other
/// entry, so a store full of broken artwork cannot grow the cache past its
/// budget.
///
/// The key it is retained under is charged with it: a folder's key holds its
/// members' paths, which no fixed per-entry figure can bound.
pub(crate) struct CachedArtwork {
    artwork: Option<Artwork>,
    key_bytes: usize,
}

impl CachedArtwork {
    /// `artwork`, retained under `key`.
    fn new(artwork: Option<Artwork>, key: &ArtworkKey) -> Self {
        Self {
            artwork,
            key_bytes: key.heap_bytes(),
        }
    }

    /// The retained picture, if this decode produced one.
    fn artwork(&self) -> Option<&Artwork> {
        self.artwork.as_ref()
    }
}

impl CachedBytes for CachedArtwork {
    fn payload_bytes(&self) -> usize {
        self.artwork
            .as_ref()
            .map_or(0, CachedBytes::payload_bytes)
            .saturating_add(self.key_bytes)
    }

    fn wipe(&mut self) {
        if let Some(artwork) = self.artwork.as_mut() {
            artwork.wipe();
        }
    }
}

/// Bytes of bookkeeping each retained decode costs beyond its pixels and the
/// heap its key holds: the key itself, the pixel side, the recency index node,
/// and the map nodes holding them.
///
/// Both consumers build their cache with this one value so a change to the
/// budget's per-entry overhead cannot diverge between them.
pub const ARTWORK_ENTRY_METADATA_BYTES: usize = 256;

/// Build the reclaimable cache an [`ArtworkCache`] wraps, classified and
/// budgeted by the one shared desktop policy — the *working-set* one, because
/// re-deriving an entry here means a capability-gated read and a
/// parser-sandbox round trip, so pressure short of severe leaves it alone
/// (`tairix_reclaim::working_set_ui_cache`).
///
/// `label` names the cache in audit records, `seat` is the owning seat,
/// `fb_bytes` is the output's frame size (so the artwork a consumer may retain
/// scales with the display it draws on rather than a fixed ceiling), and
/// `pressure` / `sink` are the process's live memory-pressure gauge and audit
/// sink. Both the desktop session and the file manager call this so they build
/// the cache identically rather than each inventing budget and metadata
/// numbers.
#[must_use]
pub fn artwork_cache(
    label: &'static str,
    seat: u64,
    fb_bytes: usize,
    pressure: &'static (dyn PressureGauge + 'static),
    sink: &'static (dyn Sink + Sync),
) -> ArtworkCache {
    // The keys name assets inside this session's own user's stores, so a
    // grind that crowded one bucket would only slow the grinder's own
    // desktop: the fast unkeyed hash, named here rather than defaulted.
    ArtworkCache::new(working_set_ui_cache(
        label,
        seat,
        fb_bytes,
        ARTWORK_ENTRY_METADATA_BYTES,
        pressure,
        sink,
        BuildFastHash::new(),
    ))
}

/// What one retained decode is keyed by.
///
/// A bundle is keyed by its *directory*, not by the asset its manifest names,
/// so the manifest read is paid once per bundle and a bundle that declares no
/// icon (or names one that will not decode) remembers that refusal too. A
/// directory and an asset file can never spell the same path, but keeping the
/// two apart in the key type says so rather than relying on it.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArtworkKey {
    /// An asset file read directly: an icon path the caller had already
    /// resolved.
    Asset(String),
    /// A kind's shipped raster master, [`icon_artwork_path`]. Named by the
    /// kind, so a lookup formats no path.
    Raster(IconKind),
    /// A kind's shipped vector master, [`icon_vector_path`].
    Vector(IconKind),
    /// An application-bundle directory, resolved through its own manifest.
    Bundle(String),
    /// A program *name* and the asking session's home root, resolved to the
    /// first bundle of that name in the program-store order and then through
    /// that bundle's own manifest.
    ///
    /// What a surface listing *processes* has to work from: the kernel attests
    /// a task's name from the store path it loaded, and carries no image path,
    /// so the name is the only identity a monitor is given. Resolving it here
    /// keeps the store order and the manifest read on the one resolver — and
    /// keys the cache by the name, so a listing of a hundred tasks costs one
    /// resolution per distinct program rather than a directory walk per row
    /// per frame.
    Program {
        /// The program's name.
        name: String,
        /// The asking session's home root, if it has one. Part of the key
        /// because it is part of the question: two sessions asking about the
        /// same name may legitimately resolve different bundles.
        home: Option<String>,
    },
    /// A kind's built-in picture ([`builtin_picture`]), rasterised in this
    /// process from the first-party vector art compiled into it — no read, no
    /// decode, no sandbox. Retained like any asset because rasterising is the
    /// expensive part of vector art: a glyph's coverage mask serves every tint
    /// and state, and a badge carries its own colours.
    Builtin(IconKind),
    /// A folder's picture of what it holds: its back, a card for each kind
    /// sampled, and its front, composed from their own artwork. Keyed by the
    /// sample, so every folder holding the same kinds shares one picture.
    Folder(FolderSample),
    /// A picture file drawn as its own content.
    Thumbnail(Thumbnail),
}

impl ArtworkKey {
    /// Whether producing this reads a picture file's own content — a
    /// thumbnail, or a folder whose cards show its members' pictures — which
    /// is queued apart from icons, never decoded inline, and drawn as the
    /// tier below while it is produced.
    #[must_use]
    pub fn is_thumbnail_class(&self) -> bool {
        match self {
            Self::Thumbnail(_) => true,
            Self::Folder(sample) => sample.has_pictures(),
            _ => false,
        }
    }

    /// The bytes this key holds on the heap, which a cache retaining an entry
    /// under it charges.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        match self {
            Self::Asset(path) | Self::Bundle(path) => path.capacity(),
            Self::Program { name, home } => {
                name.capacity() + home.as_ref().map_or(0, String::capacity)
            }
            Self::Builtin(_) | Self::Raster(_) | Self::Vector(_) => 0,
            Self::Folder(sample) => sample.heap_bytes(),
            Self::Thumbnail(thumbnail) => thumbnail.path.capacity(),
        }
    }

    /// This key borrowed, as a lookup compares it.
    fn view(&self) -> ArtView<'_> {
        match self {
            Self::Asset(path) => ArtView::Asset(path),
            Self::Raster(kind) => ArtView::Raster(*kind),
            Self::Vector(kind) => ArtView::Vector(*kind),
            Self::Bundle(dir) => ArtView::Bundle(dir),
            Self::Program { name, home } => ArtView::Program {
                name,
                home: home.as_deref(),
            },
            Self::Builtin(kind) => ArtView::Builtin(*kind),
            Self::Folder(sample) => ArtView::Folder(sample.drawn()),
            Self::Thumbnail(thumbnail) => ArtView::Thumbnail {
                path: &thumbnail.path,
                stamp: thumbnail.stamp,
                reading: thumbnail.reading,
            },
        }
    }
}

/// An [`ArtworkKey`] borrowed — from a key, or from a request that has not
/// built one — so a cache lookup hashes and compares it without allocating.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ArtView<'a> {
    Asset(&'a str),
    Raster(IconKind),
    Vector(IconKind),
    Bundle(&'a str),
    Program {
        name: &'a str,
        home: Option<&'a str>,
    },
    Builtin(IconKind),
    Folder(DrawnSample<'a>),
    Thumbnail {
        path: &'a str,
        stamp: DocumentStamp,
        reading: Reading,
    },
}

impl ArtView<'_> {
    /// The key this names, built: what a miss retains its slot under, or
    /// `None` when the heap refuses the text it owns — a draw on a starved
    /// machine falls a tier rather than aborting.
    fn to_key(self) -> Option<ArtworkKey> {
        Some(match self {
            Self::Asset(path) => ArtworkKey::Asset(fallible::concatenated(&[path])?),
            Self::Raster(kind) => ArtworkKey::Raster(kind),
            Self::Vector(kind) => ArtworkKey::Vector(kind),
            Self::Bundle(dir) => ArtworkKey::Bundle(fallible::concatenated(&[dir])?),
            Self::Program { name, home } => ArtworkKey::Program {
                name: fallible::concatenated(&[name])?,
                home: match home {
                    Some(home) => Some(fallible::concatenated(&[home])?),
                    None => None,
                },
            },
            Self::Builtin(kind) => ArtworkKey::Builtin(kind),
            Self::Folder(drawn) => ArtworkKey::Folder(drawn.to_sample()),
            Self::Thumbnail {
                path,
                stamp,
                reading,
            } => ArtworkKey::Thumbnail(Thumbnail {
                path: fallible::concatenated(&[path])?,
                stamp,
                reading,
            }),
        })
    }

    fn is_thumbnail_class(self) -> bool {
        match self {
            Self::Thumbnail { .. } => true,
            Self::Folder(drawn) => drawn.has_pictures(),
            _ => false,
        }
    }
}

/// The cache slot a decode is retained in: the artwork, the pixel side, and
/// the shadow it casts, if this slot is that shadow rather than the picture.
#[derive(Clone, Debug)]
struct SlotKey {
    art: ArtworkKey,
    side: u32,
    shadow: Option<ShadowCast>,
}

/// A slot named borrowed: what [`ArtworkCache`] looks its entries up by.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct SlotView<'a> {
    art: ArtView<'a>,
    side: u32,
    shadow: Option<ShadowCast>,
}

impl SlotView<'_> {
    fn to_key(self) -> Option<SlotKey> {
        Some(SlotKey {
            art: self.art.to_key()?,
            side: self.side,
            shadow: self.shadow,
        })
    }

    /// The shadow this picture's slot casts as `cast` says.
    fn shadow(self, cast: ShadowCast) -> Self {
        Self {
            shadow: Some(cast),
            ..self
        }
    }
}

/// Either form of a slot's name. The cache's keys borrow as this, so a lookup
/// by a [`SlotView`] finds an entry retained under the [`SlotKey`] it names:
/// both hash and compare through [`SlotLookup::view`].
trait SlotLookup {
    fn view(&self) -> SlotView<'_>;
}

impl SlotLookup for SlotKey {
    fn view(&self) -> SlotView<'_> {
        SlotView {
            art: self.art.view(),
            side: self.side,
            shadow: self.shadow,
        }
    }
}

impl SlotLookup for SlotView<'_> {
    fn view(&self) -> SlotView<'_> {
        *self
    }
}

impl<'a> Borrow<dyn SlotLookup + 'a> for SlotKey {
    fn borrow(&self) -> &(dyn SlotLookup + 'a) {
        self
    }
}

impl Hash for dyn SlotLookup + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.view().hash(state);
    }
}

impl PartialEq for dyn SlotLookup + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.view() == other.view()
    }
}

impl Eq for dyn SlotLookup + '_ {}

impl Hash for SlotKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.view().hash(state);
    }
}

impl PartialEq for SlotKey {
    fn eq(&self, other: &Self) -> bool {
        self.view() == other.view()
    }
}

impl Eq for SlotKey {}

/// The outcome of building one cache slot.
enum Slot {
    /// Built and retained, and it has artwork to draw.
    Served,
    /// Retained, but there is no artwork for it — the next tier answers.
    Empty,
    /// Produced but not retained (pressure forbids growth, or the cache has
    /// disabled itself), so it is handed straight back: no borrow can be
    /// served from the cache, but the pixels themselves exist.
    Uncached(Option<Artwork>),
    /// The resolver has not produced it yet. Nothing is retained and no later
    /// tier is tried, because whether one is even reached depends on this
    /// answer.
    Pending,
}

/// What one resolution produced, for a caller that copies the pixels out
/// instead of drawing them from the cache in place.
///
/// A draw site that paints directly from the cache reads the borrow
/// [`ArtworkCache::artwork`] hands it and needs no such distinction: it asks
/// again next frame whatever the answer was. A caller that *stores* the
/// picture — a window's title-bar identity, resolved once when the window
/// opens — must know whether asking again would change anything.
#[derive(Debug)]
pub enum ArtworkOutcome {
    /// The picture to use, copied out of the cache.
    Ready(Surface),
    /// Every tier of the request refused. The caller draws the built-in glyph,
    /// and asking again would only repeat the refusal.
    Refused,
    /// A tier is still being produced. The caller draws the built-in glyph and
    /// asks again once its resolver reports the decode has landed.
    Pending,
}

/// The reclaim-governed decode cache, keyed by what was resolved and the pixel
/// side.
///
/// One decode outcome per `(key, side)`: a scale change alters the side and so
/// misses, re-rasterising at the new geometry. Nothing invalidates the whole
/// cache at once — shipped artwork changes at install time, not while an icon
/// is on screen — so the generation is the unit value; what bounds this cache
/// is its budget, and what ends it is the owner's teardown.
///
/// The keys are paths, some of them bundle-supplied, so an unbounded map here
/// would let a crafted or merely crowded store grow a session without limit;
/// the budget forecloses that.
pub struct ArtworkCache {
    entries: ReclaimCache<SlotKey, CachedArtwork, (), BuildFastHash>,
}

impl ArtworkCache {
    /// Hold the cache to what an output of `fb_bytes` allows, the output
    /// having changed size: the budget [`artwork_cache`] builds it under.
    pub fn rescale(&mut self, fb_bytes: usize) {
        let _ = self.entries.rebudget(working_set_ui_budget(fb_bytes));
    }

    /// An empty cache over a ready-built reclaimable cache, which
    /// [`artwork_cache`] assembles from what only the owning process knows.
    #[must_use]
    const fn new(entries: ReclaimCache<SlotKey, CachedArtwork, (), BuildFastHash>) -> Self {
        Self { entries }
    }

    /// Release every retained decode, overwriting the artwork first.
    ///
    /// Called when the owner is going away, so one user's decoded artwork
    /// never outlives their session in reusable heap.
    pub fn teardown(&mut self) {
        self.entries.teardown();
    }

    /// Apply the current memory-pressure band's forced shrink, returning the
    /// bytes released.
    pub fn trim(&mut self) -> usize {
        self.entries.enforce_pressure()
    }

    /// Bytes currently charged for retained artwork, payload plus this cache's
    /// own per-entry bookkeeping.
    #[must_use]
    pub fn charged_bytes(&self) -> usize {
        self.entries.charged_bytes()
    }

    /// A shared handle to this cache's ledger, for the owning process to
    /// register with its process-wide cache reporter.
    ///
    /// This crate stays free of a runtime dependency deliberately: a cache
    /// this library merely wraps is registered by the process that built
    /// it, not by the library. `None` only for a cache declared
    /// unclassifiable, which retains nothing and so has no footprint to
    /// report.
    #[must_use]
    pub fn ledger(&self) -> Option<CacheLedger> {
        self.entries.ledger()
    }

    /// The artwork for one already-resolved asset `path` at `side` pixels,
    /// with no fallback tier: served from the cache, or produced through
    /// `resolver` (a read bounded by [`MAX_ARTWORK_BYTES`] plus a decode),
    /// verified, and cached. A caller that wants the whole resolution order
    /// asks [`artwork`](Self::artwork) instead.
    ///
    /// `None` — also cached, so a bad asset is not re-read every frame — when
    /// the side is zero, the asset is unreadable, over-long, refused by the
    /// decoder, or the returned pixel block is not exactly `side`×`side`. It
    /// is also the answer while a deferring `resolver` is still producing the
    /// decode, in which case a later call serves it. The surface is borrowed,
    /// never cloned, so a grid draws from the cache in place.
    pub fn path_artwork(
        &mut self,
        resolver: &mut dyn ArtworkResolver,
        path: &str,
        side: u32,
    ) -> Option<&Surface> {
        let slot = SlotView {
            art: ArtView::Asset(path),
            side,
            shadow: None,
        };
        let _ = self.build_slot(slot, resolver);
        self.borrow_slot(slot).map(Artwork::surface)
    }

    /// The picture for `request` at `side` pixels: the thing's own icon when
    /// it resolves, else the shipped artwork for its kind, else `None` for the
    /// caller to draw the built-in glyph.
    ///
    /// This is the one place the desktop's icon-resolution order is decided,
    /// so every surface — a taskbar button, a launcher row, a file-manager
    /// tile — resolves identically. A bundle's own icon is read and decoded
    /// exactly like a shipped asset: bounded, sandboxed by the injected
    /// rasteriser, and accepted only as exactly the pixels asked for.
    ///
    /// Each tier it reaches leaves a retained outcome behind, refusal
    /// included, so a kind that ships no artwork at all costs one read per
    /// class format once and none thereafter.
    ///
    /// A deferring `resolver` walks the same order one tier per answer: a tier
    /// still being produced stops the walk, because whether the next tier is
    /// reached at all depends on what this one turns out to be. The request
    /// therefore costs exactly the reads a synchronous walk would, spread over
    /// as many frames as it has tiers to try. A thumbnail-class tier is the
    /// exception: the tier below it is drawn while it is produced.
    pub fn artwork(
        &mut self,
        resolver: &mut dyn ArtworkResolver,
        request: IconRequest<'_>,
        side: u32,
    ) -> Option<IconPicture<'_>> {
        let served = self.serve(resolver, request, side)?;
        self.picture(served.slot, served.mask)
    }

    /// [`artwork`](Self::artwork), with the soft shadow the picture casts as
    /// `cast` says beneath it, or none for no cast.
    ///
    /// The shadow is cast once per picture, side and cast, in this process,
    /// from the picture's own coverage, and retained beside it as an entry of
    /// its own — so a frame composites a mask and never blurs, and pressure
    /// may take a shadow without taking its picture. A shadow never takes its
    /// picture's place: one the cache could hold only by evicting the picture
    /// is refused, and one the band leaves no room for beside it is not cast
    /// at all. A shadow that cannot be cast or kept is withheld for this draw
    /// ([`IconPicture::shadow_withheld`]) and asked for again next time, so a
    /// draw site neither blurs it itself nor loses it for good.
    pub fn shadowed(
        &mut self,
        resolver: &mut dyn ArtworkResolver,
        request: IconRequest<'_>,
        side: u32,
        cast: Option<ShadowCast>,
    ) -> Option<IconPicture<'_>> {
        let ServedPicture { slot, mask } = self.serve(resolver, request, side)?;
        let Some(cast) = cast else {
            return self.picture(slot, mask);
        };
        let shadow = slot.shadow(cast);
        if self.entries.find(&(), &shadow as &dyn SlotLookup).is_none()
            && self.has_room_for(slot, cast)
        {
            // Only a cast shadow is kept: a picture not retained, or memory that
            // could not be had, is asked about again rather than remembered.
            if let Some(((cast_mask, _), key)) = self
                .borrow_slot(slot)
                .and_then(|picture| cast_shadow(picture.surface(), cast))
                .zip(shadow.to_key())
            {
                let cached = CachedArtwork::new(Some(Artwork::new(cast_mask)), &key.art);
                self.entries
                    .retain_beside(&(), key, cached, &slot as &dyn SlotLookup);
            }
        }
        let picture = self.picture(slot, mask)?;
        Some(match self.borrow_slot(shadow) {
            Some(retained) => picture.with_shadow(CastShadow {
                mask: retained.surface(),
                offset: {
                    let (x, y) = cast.origin();
                    Point::new(x, y)
                },
            }),
            None => picture.withholding_shadow(),
        })
    }

    /// Walk `request`'s tiers at `side`, building each slot it reaches, and
    /// answer where the picture that serves is retained: a tier's artwork, or
    /// failing every tier the kind's built-in picture. `None` only when even
    /// the built-in picture is too dear to retain, for the caller to draw it
    /// inline.
    ///
    /// A deferring `resolver` walks the same order one tier per answer: a tier
    /// still being produced stops the walk, because whether the next tier is
    /// reached at all depends on what this one turns out to be — except a
    /// thumbnail-class tier, whose picture always replaces the tier below once
    /// it lands, so that tier is drawn meanwhile.
    fn serve<'a>(
        &mut self,
        resolver: &mut dyn ArtworkResolver,
        request: IconRequest<'a>,
        side: u32,
    ) -> Option<ServedPicture<'a>> {
        for tier in request.tiers() {
            let slot = SlotView {
                art: tier.view(),
                side,
                shadow: None,
            };
            match self.build_slot(slot, resolver) {
                Slot::Served => return Some(ServedPicture { slot, mask: false }),
                Slot::Pending if slot.art.is_thumbnail_class() => {}
                // Nothing is being retained, so a later tier could only
                // repeat the same refusal at the cost of another decode. The
                // glyph below is still reached: it costs no decode at all, and
                // drawing nothing while a decode is in flight would leave the
                // slot empty on screen.
                Slot::Uncached(_) | Slot::Pending => break,
                Slot::Empty => {}
            }
        }
        // The built-in picture is resolved here rather than through the
        // resolver because it needs neither a read nor a sandbox: the vector
        // art is compiled in, and resolving it synchronously puts a picture
        // on screen in the first frame that asks.
        let kind = request.icon_kind();
        let slot = SlotView {
            art: ArtView::Builtin(kind),
            side,
            shadow: None,
        };
        self.entries.get_or_build_borrowed(
            &(),
            &slot as &dyn SlotLookup,
            || slot.to_key(),
            || {
                Some(CachedArtwork::new(
                    builtin_picture(kind, side).map(Artwork::new),
                    &ArtworkKey::Builtin(kind),
                ))
            },
        )?;
        Some(ServedPicture {
            slot,
            mask: kind.badge().is_none(),
        })
    }

    /// The picture retained where `served` says, as a draw site is handed it.
    fn picture(&self, slot: SlotView<'_>, mask: bool) -> Option<IconPicture<'_>> {
        let artwork = self.borrow_slot(slot)?;
        let picture = if mask {
            IconPicture::mask(artwork.surface())
        } else {
            IconPicture::coloured(artwork.surface())
        };
        Some(picture.with_frame(artwork.frame()))
    }

    /// The picture for `request` at `side` pixels, copied out, together with
    /// whether an answer is still to come.
    ///
    /// The same resolution order as [`artwork`](Self::artwork) — this differs
    /// only in handing the caller an owned surface and an honest
    /// [`ArtworkOutcome::Pending`], which is what a caller that stores the
    /// picture rather than drawing it needs in order to know whether to ask
    /// again. A cache too tight to retain the decode still yields the pixels
    /// here rather than throwing them away.
    pub fn owned_artwork(
        &mut self,
        resolver: &mut dyn ArtworkResolver,
        request: IconRequest<'_>,
        side: u32,
    ) -> ArtworkOutcome {
        let mut served = None;
        for tier in request.tiers() {
            let slot = SlotView {
                art: tier.view(),
                side,
                shadow: None,
            };
            match self.build_slot(slot, resolver) {
                Slot::Served => {
                    served = Some(slot);
                    break;
                }
                Slot::Pending => return ArtworkOutcome::Pending,
                // Nothing is being retained, so a later tier could only
                // repeat the same refusal at the cost of another decode.
                Slot::Uncached(artwork) => {
                    return artwork.map_or(ArtworkOutcome::Refused, |artwork| {
                        ArtworkOutcome::Ready(artwork.into_surface())
                    })
                }
                Slot::Empty => {}
            }
        }
        served
            .and_then(|slot| {
                self.borrow_slot(slot)
                    .map(|artwork| artwork.surface().clone())
            })
            .map_or(ArtworkOutcome::Refused, ArtworkOutcome::Ready)
    }

    /// Start producing whatever `request` will resolve to at `side`, drawing
    /// nothing and waiting for nothing.
    ///
    /// Exactly one tier is asked for: the first this cache does not already
    /// hold, because resolution stops at the first tier that serves and a later
    /// one is only reached if that one refuses. A request whose serving tier is
    /// already retained asks for nothing at all, so warming a whole catalog
    /// repeatedly costs a lookup per icon rather than a decode.
    pub fn prefetch(
        &mut self,
        resolver: &mut dyn ArtworkResolver,
        request: IconRequest<'_>,
        side: u32,
    ) {
        for tier in request.tiers() {
            let slot = SlotView {
                art: tier.view(),
                side,
                shadow: None,
            };
            match self
                .entries
                .peek(&(), &slot as &dyn SlotLookup)
                .map(CachedArtwork::artwork)
            {
                // This tier will serve, so no later one is reached.
                Some(Some(_)) => return,
                // A retained refusal: the next tier is the one that matters.
                Some(None) => {}
                // A thumbnail-class tier draws the tier below until it lands,
                // so that one is wanted too.
                None if slot.art.is_thumbnail_class() => {
                    if let Some(key) = slot.art.to_key() {
                        resolver.prefetch(&key, side);
                    }
                }
                None => {
                    if let Some(key) = slot.art.to_key() {
                        resolver.prefetch(&key, side);
                    }
                    return;
                }
            }
        }
    }

    /// Build `key`'s slot once (a refusal is retained as an empty slot), and
    /// report what came of it.
    ///
    /// A hit builds no key; a miss builds the one the resolver is handed and
    /// the slot is retained under.
    fn build_slot(&mut self, slot: SlotView<'_>, resolver: &mut dyn ArtworkResolver) -> Slot {
        let mut pending = false;
        let built_key: Cell<Option<SlotKey>> = Cell::new(None);
        let served = self.entries.get_or_build_borrowed(
            &(),
            &slot as &dyn SlotLookup,
            || built_key.take().or_else(|| slot.to_key()),
            || {
                let key = slot.to_key()?;
                let cached = match resolver.resolve(&key.art, slot.side) {
                    Resolved::Done(artwork) => Some(CachedArtwork::new(artwork, &key.art)),
                    Resolved::Pending => {
                        pending = true;
                        None
                    }
                };
                built_key.set(Some(key));
                cached
            },
        );
        let built = match served {
            Some(Served::Uncached(artwork)) => {
                if let Some(key) = built_key.take().or_else(|| slot.to_key()) {
                    resolver.declined(&key.art, slot.side);
                }
                Slot::Uncached(artwork.artwork)
            }
            Some(served) if served.artwork().is_some() => Slot::Served,
            _ => Slot::Empty,
        };
        if pending {
            Slot::Pending
        } else {
            built
        }
    }

    /// Whether the band leaves room for the shadow `cast` casts beside the
    /// picture `slot` holds: what decides whether casting it is worth a blur.
    fn has_room_for(&self, slot: SlotView<'_>, cast: ShadowCast) -> bool {
        let Some(picture) = self.entries.peek(&(), &slot as &dyn SlotLookup) else {
            return false;
        };
        let Some(surface) = picture.artwork().map(Artwork::surface) else {
            return false;
        };
        let shadow = cast
            .extent(surface.width(), surface.height())
            .and_then(|(width, height)| usize::try_from(u64::from(width) * u64::from(height)).ok())
            .and_then(|pixels| pixels.checked_mul(size_of::<tairix_raster::Pixel>()));
        shadow.is_some_and(|shadow| {
            self.entries
                .holds(2, picture.payload_bytes().saturating_add(shadow))
        })
    }

    /// Borrow a built slot's artwork back out of the cache.
    ///
    /// Returning the admitted value directly would tie the borrow to the
    /// `&mut` build call, so the read-back is how a surface leaves as a shared
    /// borrow the caller can hold while drawing.
    fn borrow_slot(&self, slot: SlotView<'_>) -> Option<&Artwork> {
        self.entries
            .peek(&(), &slot as &dyn SlotLookup)
            .and_then(CachedArtwork::artwork)
    }
}

/// Where the picture a request resolved to is retained, and whether it is a
/// glyph mask the drawing control tints.
struct ServedPicture<'a> {
    slot: SlotView<'a>,
    mask: bool,
}

/// Read, rasterise, and verify whatever one cache slot names (the cache-miss
/// path). A bundle resolves its manifest first; every other key is an asset
/// path already.
///
/// Public because it is the whole of one decode: a worker thread running it
/// off an [`ArtworkResolver`]'s hand-off performs the identical work
/// [`InlineArtwork`] would have, so where the decode happens cannot change
/// what it produces.
pub fn render_artwork<R: ArtworkReader + ?Sized, D: ArtworkRasteriser + ?Sized>(
    reader: &mut R,
    rasteriser: &mut D,
    key: &ArtworkKey,
    side: u32,
) -> Option<Artwork> {
    match key {
        ArtworkKey::Asset(path) => render_icon(reader, rasteriser, path, side).map(Artwork::new),
        ArtworkKey::Bundle(dir) => {
            let path = bundle_icon_path(reader, dir)?;
            render_icon(reader, rasteriser, &path, side).map(Artwork::new)
        }
        ArtworkKey::Program { name, home } => program_bundles(name, home.as_deref())?
            .iter()
            .find_map(|dir| bundle_icon_path(reader, dir))
            .and_then(|path| render_icon(reader, rasteriser, &path, side))
            .map(Artwork::new),
        ArtworkKey::Raster(kind) => icon_artwork_path(*kind)
            .and_then(|path| render_icon(reader, rasteriser, &path, side))
            .map(Artwork::new),
        ArtworkKey::Vector(kind) => icon_vector_path(*kind)
            .and_then(|path| render_icon(reader, rasteriser, &path, side))
            .map(Artwork::new),
        // Built-in art is first-party and compiled into this binary, so the
        // cache rasterises it in place and no resolver is ever handed one.
        ArtworkKey::Builtin(kind) => builtin_picture(*kind, side).map(Artwork::new),
        ArtworkKey::Folder(sample) => {
            folder_picture(reader, rasteriser, sample, side).map(Artwork::new)
        }
        ArtworkKey::Thumbnail(thumbnail) => render_thumbnail(reader, rasteriser, thumbnail, side),
    }
}

/// A folder's picture of `sample` at `side` pixels: its back, its cards fanned
/// out of its mouth, and its front over them. `None` where the back or front
/// will not draw, or the cards would be too small to read, so the request
/// falls to the plain filled folder.
///
/// A picture card prints the member's own picture, decoded at the folder's
/// side so the decode is the one the member's own tile asks for; a member
/// that will not decode draws its kind's card instead.
fn folder_picture<R: ArtworkReader + ?Sized, D: ArtworkRasteriser + ?Sized>(
    reader: &mut R,
    rasteriser: &mut D,
    sample: &FolderSample,
    side: u32,
) -> Option<Surface> {
    if side < MIN_COMPOSITE_SIDE {
        return None;
    }
    let part = |reader: &mut R, rasteriser: &mut D, kind: IconKind, side: u32| {
        icon_artwork_path(kind)
            .and_then(|path| render_icon(reader, rasteriser, &path, side))
            .or_else(|| {
                icon_vector_path(kind).and_then(|path| render_icon(reader, rasteriser, &path, side))
            })
    };
    let back = part(reader, rasteriser, IconKind::FolderBack, side)?;
    let front = part(reader, rasteriser, IconKind::FolderFront, side)?;
    let mut picture = Surface::new(side, side)?;
    picture.blit(0, 0, &back);
    for (place, card) in card_slots(sample, side) {
        let printed = card.picture().and_then(|thumbnail| {
            let artwork = render_thumbnail(reader, rasteriser, thumbnail, side)?;
            print_card(artwork.surface(), artwork.frame()?, place.side)
        });
        let drawn = if let Some(print) = printed {
            print
        } else {
            let kind = card.kind();
            part(reader, rasteriser, kind, place.side).or_else(|| paper_card(kind, place.side))?
        };
        picture.blit_transformed(&drawn, place.transform());
    }
    picture.blit(0, 0, &front);
    Some(picture)
}

/// The coverage mask for `kind`'s built-in glyph at `side` pixels: the glyph
/// rasterised in opaque white, so every pixel's alpha *is* its coverage and the
/// drawing control supplies the colour ([`Surface::blit_tinted`]).
///
/// Rasterising a multi-layer glyph is the expensive step — the layers are
/// painted enlarged and averaged back down to resolve the seams between them —
/// which is exactly why the result is retained rather than redrawn per frame.
///
/// A badge kind's mask is its symbol alone: a control that draws only glyphs
/// in its own colour — a button's leading mark, a menu row — shows the
/// category's symbol, not a tinted plate.
///
/// Public so a glyph-only draw site composites through the same mask-and-tint
/// arithmetic a cached one does. Baking the colour into the rasterise instead
/// would round it at a different step, and a cached icon and an uncached one
/// would not be the same pixels.
#[must_use]
pub fn glyph_mask(kind: IconKind, side: u32) -> Option<Surface> {
    builtin_icon(kind, MASK_COLOR).rasterise(side)
}

/// `kind`'s built-in picture at `side` pixels, the last tier every request
/// resolves to: a settings category's or pane's colour badge, or any other
/// kind's coverage mask ([`glyph_mask`]). [`IconPicture::builtin`] says which.
///
/// Public so the one uncached draw path — a control handed no picture at all —
/// draws the very pixels the cache would have retained.
#[must_use]
pub fn builtin_picture(kind: IconKind, side: u32) -> Option<Surface> {
    if kind.badge().is_some() {
        badge_picture(kind, side)
    } else {
        glyph_mask(kind, side)
    }
}

/// The colour a glyph mask is rasterised in: opaque white, so the mask's alpha
/// carries the coverage and its colour channels never tint the result.
const MASK_COLOR: Color = Color::rgba(255, 255, 255, 255);

/// The bundle directories a program `name` could be installed in, in the order
/// they are tried: [`identity_roots`], so every read-only system store precedes
/// the asking session's own and a bundle planted there cannot change the
/// picture a system program wears.
///
/// Not the command-search order (`lib/cmdres`): a running image can have come
/// from the service store, and never from a `PATH` entry. Only the
/// asking session's home is searched, so no account chooses the picture
/// another's task wears. `None` when the heap refuses the paths, rather than a
/// shorter list that would let a later store's bundle answer for an earlier's.
fn program_bundles(name: &str, home: Option<&str>) -> Option<Vec<String>> {
    if name.is_empty() || name.contains('/') {
        return Some(Vec::new());
    }
    let roots = identity_roots(home);
    let mut bundles = Vec::new();
    bundles.try_reserve_exact(roots.len()).ok()?;
    for root in &roots {
        bundles.push(fallible::concatenated(&[root, "/", name, BUNDLE_SUFFIX])?);
    }
    Some(bundles)
}

/// The path of the icon a bundle's own manifest names, or `None` when it names
/// none.
///
/// The manifest is untrusted: it is decoded under the shared wire bound, and
/// the asset is accepted only as a plain file name resolved inside the bundle's
/// own `Resources/`, so a bundle cannot aim the desktop at any other file.
fn bundle_icon_path<R: ArtworkReader + ?Sized>(reader: &mut R, dir: &str) -> Option<String> {
    let header = decode_manifest(&reader.read(&manifest_path(dir))?)?;
    let asset = header.library_icon()?;
    tairix_path::validate_file_name(asset).ok()?;
    fallible::concatenated(&[dir, "/", BundleEntry::Resources.as_str(), "/", asset])
}

/// Read, rasterise, and verify one asset (the cache-miss path).
///
/// A zero side, an unreadable path, an over-long asset (refused *before* the
/// decoder runs), a refused decode, or a reply that is not exactly
/// `side`×`side` straight-alpha RGBA8 all yield `None`; the pixel-length check
/// is checked arithmetic that never panics.
fn render_icon<R: ArtworkReader + ?Sized, D: ArtworkRasteriser + ?Sized>(
    reader: &mut R,
    rasteriser: &mut D,
    path: &str,
    side: u32,
) -> Option<Surface> {
    if side == 0 {
        return None;
    }
    let bytes = reader.read(path)?;
    if bytes.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    let pixels = rasteriser.rasterise(side, &bytes)?;
    // The pixel count is validated here as well as in the transport: a surface
    // is built only from a block of exactly the promised shape, wherever it
    // came from.
    let expected = (side as usize)
        .checked_mul(side as usize)
        .and_then(|area| area.checked_mul(4))?;
    if pixels.len() != expected {
        return None;
    }
    Surface::from_rgba8(side, side, &pixels)
}

/// Binds an [`ArtworkCache`] to its resolver so it can be handed to a
/// renderer as a plain [`IconArtwork`] lookup without the renderer knowing
/// about I/O — or about which thread the decode happens on.
pub struct IconArtworkSource<'a> {
    cache: &'a mut ArtworkCache,
    resolver: &'a mut dyn ArtworkResolver,
}

impl<'a> IconArtworkSource<'a> {
    /// Bind `cache` to the `resolver` its misses are produced through.
    pub fn new(cache: &'a mut ArtworkCache, resolver: &'a mut dyn ArtworkResolver) -> Self {
        Self { cache, resolver }
    }
}

impl IconArtwork for IconArtworkSource<'_> {
    fn artwork(&mut self, request: IconRequest<'_>, side: u32) -> Option<IconPicture<'_>> {
        self.cache.artwork(self.resolver, request, side)
    }

    fn shadowed(
        &mut self,
        request: IconRequest<'_>,
        side: u32,
        shadow: Option<ShadowCast>,
    ) -> Option<IconPicture<'_>> {
        self.cache.shadowed(self.resolver, request, side, shadow)
    }
}

#[cfg(test)]
#[path = "artwork_tests.rs"]
mod tests;
