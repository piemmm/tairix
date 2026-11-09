//! A PDF document: its file, its cross-reference, its pages, and the objects
//! it resolves only when they are asked for.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_collections::{try_arc, OrderedMap, OrderedSet};
use tairix_inline::ArrayVec;
use tairix_util::fallible::try_push;

use crate::crypt::Security;
use crate::error::{tolerated, Refused};
use crate::filter::{self, Decoded};
use crate::function::{Functions, Work};
use crate::lex::Lexer;
use crate::limits::{
    HEADER_WINDOW, MAX_FILTERS, MAX_LEX_PASSES, MAX_PAGES, MAX_PAGE_TREE_DEPTH, MAX_PAGE_TREE_KIDS,
    MAX_RESOLVE_DEPTH,
};
use crate::object::{Dict, Object, Resolved, Stream};
use crate::optional::Visibility;
use crate::page::{Inherited, Page};
use crate::parse::{self, Endstreams, Indirect};
use crate::pdfdoc;
use crate::xref::{self, Entry, ObjectStream, Xref};
use crate::PdfError;

/// How a document is opened.
#[derive(Clone, Copy)]
pub struct Options<'a> {
    /// The password, where the empty one does not open it. A secret: the
    /// caller holds it zeroizing, and nothing here keeps or logs it.
    pub password: Option<&'a [u8]>,
    /// The most bytes its streams may decode to, and what is built from
    /// them — a cross-reference stream's entries, an object stream's index —
    /// all of them together, so no filter chain can expand past the memory
    /// its reader was given. A share of it bounds the objects it keeps.
    pub budget: u64,
}

/// What a document states beyond its pages' content: its title and subject,
/// and whether it holds more that a reader of its pages leaves behind.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct About {
    /// Its title, where its information states one.
    pub title: Option<String>,
    /// Its subject, where its information states one.
    pub subject: Option<String>,
    /// Whether it states metadata besides those two and the programs that
    /// made it and when: an author, keywords, an XMP stream, or more.
    pub more_metadata: bool,
    /// Whether its catalog holds more than its page tree: bookmarks,
    /// attachments, forms, scripts, layers, structure, viewing preferences.
    pub more_parts: bool,
}

/// One page as the page tree lists it: its object, and what it inherits.
struct Listed {
    page: Object,
    inherited: Arc<Inherited>,
}

/// Why a document could not be assembled, and whether a repair may mend it.
struct Unassembled {
    error: PdfError,
    mendable: bool,
}

impl Unassembled {
    const fn mendable(error: PdfError) -> Self {
        Self {
            error,
            mendable: true,
        }
    }
}

/// What reading one indirect object came to.
enum Kept {
    /// Being read: met again meanwhile, its reading needs itself.
    Reading,
    Object(Arc<Object>),
    Failed(Failure),
}

/// Why an object or an object stream could not be read.
#[derive(Copy, Clone)]
struct Failure {
    error: PdfError,
    /// Where its reading met another still under way, which reading it from
    /// elsewhere may not: the read it holds for alone, so that read never
    /// walks it again and a later one reads it afresh.
    within: Option<u64>,
}

/// The share of its budget a document keeps of the objects it reads: what a
/// file makes it keep stays within the memory it was given, most of which is
/// left to decoding.
const KEPT_SHARE: u64 = 4;

/// The indirect objects a document has read, each kept so it is read once,
/// together within a share of the document's budget: one that does not fit
/// is read again when it is next asked for.
struct Objects {
    kept: RefCell<OrderedMap<u32, Kept>>,
    /// What the objects kept hold besides the map's own nodes.
    held: Cell<u64>,
    /// The most the map's nodes and what its objects hold may take.
    room: u64,
    /// How many times a read has met another still under way.
    met: Cell<u64>,
    /// How many reads are under way, each within the one before.
    under_way: Cell<usize>,
    /// The number of the last read begun with none under way.
    read: Cell<u64>,
}

impl Objects {
    fn within(budget: u64) -> Self {
        Self {
            kept: RefCell::new(OrderedMap::new()),
            held: Cell::new(0),
            room: budget / KEPT_SHARE,
            met: Cell::new(0),
            under_way: Cell::new(0),
            read: Cell::new(0),
        }
    }

    /// What reading object `num` came to, where it has been read and that
    /// still holds.
    fn get(&self, num: u32) -> Option<Result<Arc<Object>, PdfError>> {
        let kept = self.kept.try_borrow().ok()?;
        Some(match kept.get(&num)? {
            Kept::Object(object) => Ok(Arc::clone(object)),
            Kept::Failed(failure) if self.holds(failure) => Err(failure.error),
            Kept::Failed(_) => return None,
            // Met again while it is read: reading it needs itself.
            Kept::Reading => {
                self.met.set(self.met.get().wrapping_add(1));
                Err(PdfError::Malformed)
            }
        })
    }

    /// How many times a read has met another still under way.
    fn met(&self) -> u64 {
        self.met.get()
    }

    /// The failure `error` is, for a read that began when [`Self::met`]
    /// answered `met`.
    fn failure(&self, error: PdfError, met: u64) -> Failure {
        Failure {
            error,
            within: (self.met.get() != met).then(|| self.read.get()),
        }
    }

    /// Whether `failure` still holds: one kept for the reads under way when it
    /// was made holds only while they are.
    fn holds(&self, failure: &Failure) -> bool {
        failure
            .within
            .is_none_or(|read| self.under_way.get() > 0 && read == self.read.get())
    }

    /// Mark object `num` as being read, answering [`Self::met`] as it stood,
    /// which [`Self::settle`] tells this read's own meetings by.
    fn mark(&self, num: u32) -> Result<u64, PdfError> {
        self.kept
            .try_borrow_mut()
            .map_err(|_| PdfError::Malformed)?
            .try_insert(num, Kept::Reading)?;
        if self.under_way.get() == 0 {
            self.read.set(self.read.get().wrapping_add(1));
        }
        self.under_way.set(self.under_way.get().saturating_add(1));
        Ok(self.met())
    }

    /// Put what reading object `num` came to in place of its mark, where it
    /// fits: never the heap's refusal or a bound met, which a later read may
    /// not meet, and damage its reading met another read under way to, `met`
    /// what [`Self::mark`] answered, only for the reads under way.
    fn settle(&self, num: u32, met: u64, read: &Result<Arc<Object>, PdfError>) {
        self.under_way.set(self.under_way.get().saturating_sub(1));
        let Ok(mut kept) = self.kept.try_borrow_mut() else {
            return;
        };
        let settled = match read {
            Ok(object) => Some(Kept::Object(Arc::clone(object))),
            Err(PdfError::OutOfMemory | PdfError::TooComplex) => None,
            Err(error) => Some(Kept::Failed(self.failure(*error, met))),
        };
        let held = if let Some(Kept::Object(object)) = &settled {
            object.held()
        } else {
            0
        };
        let taken = u64::try_from(kept.allocated_bytes())
            .unwrap_or(u64::MAX)
            .saturating_add(self.held.get())
            .saturating_add(held);
        let replaced = match settled {
            Some(settled) if taken <= self.room => kept.try_insert(num, settled).is_ok(),
            _ => false,
        };
        if replaced {
            self.held.set(self.held.get().saturating_add(held));
        } else {
            kept.remove(&num);
        }
    }

    /// Forget every object read.
    fn clear(&mut self) {
        self.kept.get_mut().clear();
        self.held.set(0);
    }
}

/// An open PDF.
pub struct Document {
    file: Vec<u8>,
    xref: Xref,
    catalog: Dict,
    pages: Vec<Listed>,
    visibility: Visibility,
    security: Option<Security>,
    endstreams: Endstreams,
    objects: Objects,
    /// Each object stream decoded, or why it cannot be: none is decoded
    /// twice.
    object_streams: RefCell<OrderedMap<u32, Result<ObjectStream, Failure>>>,
    budget: Cell<u64>,
    /// What reading its objects may still lex.
    lexing: Cell<u64>,
    work: Work,
    /// The function objects it has read, by number.
    functions: Functions,
}

/// Whether `file` is a PDF: its `%PDF-` header within its first kilobyte,
/// as every reader allows.
#[must_use]
pub fn recognised(file: &[u8]) -> bool {
    parse::find(&file[..file.len().min(HEADER_WINDOW)], b"%PDF-").is_some()
}

impl Document {
    /// The PDF `file` holds, its cross-reference read as the file states it
    /// and rebuilt by scanning where that fails, as every reader of
    /// real-world files does. The document keeps `file`, so it may be read
    /// for as long as it is held.
    ///
    /// # Errors
    ///
    /// [`Refused`], handing `file` back: [`PdfError::Malformed`] for a file
    /// that is not a PDF or is damaged past repair, [`PdfError::NeedsPassword`]
    /// and [`PdfError::WrongPassword`] for an encrypted one the password does
    /// not open, [`PdfError::TooComplex`] past a containment bound, and
    /// [`PdfError::OutOfMemory`].
    pub fn open(file: Vec<u8>, options: &Options<'_>) -> Result<Self, Refused> {
        if !recognised(&file) {
            return Err(Refused {
                error: PdfError::Malformed,
                file,
            });
        }
        let allowance = lexable(file.len());
        let mut document = Self {
            file,
            xref: Xref::default(),
            catalog: Dict::new(),
            pages: Vec::new(),
            visibility: Visibility::default(),
            security: None,
            endstreams: Endstreams::default(),
            objects: Objects::within(options.budget),
            object_streams: RefCell::new(OrderedMap::new()),
            budget: Cell::new(options.budget),
            lexing: Cell::new(allowance),
            work: Work::new(),
            functions: Functions::default(),
        };
        match document.assemble_file(options.password) {
            Ok(()) => Ok(document),
            Err(error) => Err(Refused {
                error,
                file: document.file,
            }),
        }
    }

    /// Its cross-reference read and the document assembled over it, opened
    /// with `password`: as the file states it, else as a scan rebuilds it.
    fn assemble_file(&mut self, password: Option<&[u8]>) -> Result<(), PdfError> {
        let source = xref::Source {
            file: &self.file,
            budget: &self.budget,
            ends: &self.endstreams,
        };
        let stated = match xref::chained(source) {
            Ok(xref) => self.assemble(xref, password),
            Err(error) => Err(Unassembled::mendable(error)),
        };
        match stated {
            Ok(()) => Ok(()),
            Err(Unassembled {
                error: PdfError::Malformed | PdfError::Unsupported,
                mendable: true,
            }) => {
                let repaired = xref::repair(&self.file)?;
                self.assemble(repaired, password)
                    .map_err(|unassembled| unassembled.error)
            }
            Err(Unassembled { error, .. }) => Err(error),
        }
    }

    /// Read the document `xref` places the objects of, opened with
    /// `password` where it is encrypted: the object streams a repair found
    /// indexed, the catalog found, and the page tree walked.
    fn assemble(&mut self, xref: Xref, password: Option<&[u8]>) -> Result<(), Unassembled> {
        self.xref = xref;
        self.forget_read();
        self.pages = Vec::new();
        self.security = None;
        let security = match &self.xref.trailer {
            Some(trailer) => {
                // A refusal of what a reached encryption dictionary states
                // holds however the file is scanned, and a repair may reach
                // another: only failing to reach it may be mended.
                let reached = match trailer.get(b"Encrypt") {
                    Some(encrypt) => self
                        .resolve(encrypt)
                        .is_ok_and(|encrypt| encrypt.as_dict().is_some()),
                    None => false,
                };
                Security::open(self, trailer, password).map_err(|error| Unassembled {
                    error,
                    mendable: !reached,
                })?
            }
            None => None,
        };
        self.security = security;
        // What was read while the security was found was read clear.
        self.forget_read();
        self.read_structure().map_err(Unassembled::mendable)
    }

    /// The object streams a repair found indexed, the catalog found, and the
    /// page tree walked.
    fn read_structure(&mut self) -> Result<(), PdfError> {
        self.index_object_streams()?;
        self.catalog = self.read_catalog()?;
        let root = self.catalog.get(b"Pages").ok_or(PdfError::Malformed)?;
        self.pages = self.page_tree(root)?;
        if self.pages.is_empty() {
            return Err(PdfError::Malformed);
        }
        self.visibility = tolerated(Visibility::of(self, &self.catalog))?.unwrap_or_default();
        Ok(())
    }

    /// Forget every object read and object stream decoded.
    fn forget_read(&mut self) {
        self.objects.clear();
        self.object_streams.get_mut().clear();
    }

    /// Place the objects of the object streams a repair found. A stream that
    /// cannot be read places nothing: what remains is all a damaged file has.
    fn index_object_streams(&mut self) -> Result<(), PdfError> {
        for stream in core::mem::take(&mut self.xref.unindexed) {
            if let Some(objects) = tolerated(self.object_stream(stream, 0))? {
                self.xref.index(stream, &objects, &self.budget)?;
                xref::charged_insert(
                    self.object_streams.get_mut(),
                    stream,
                    Ok(objects),
                    &self.budget,
                )?;
            }
        }
        Ok(())
    }

    /// The catalog: the trailer's `/Root`, or where a repair found no trailer
    /// naming one, the last catalog among the objects, each read for it once
    /// and none of them kept.
    fn read_catalog(&self) -> Result<Dict, PdfError> {
        if let Some(root) = self
            .xref
            .trailer
            .as_ref()
            .and_then(|trailer| trailer.get(b"Root"))
        {
            return match self.resolve(root)?.into_owned()? {
                Object::Dict(catalog) => Ok(catalog),
                _ => Err(PdfError::Malformed),
            };
        }
        let mut found = None;
        for num in self.xref.numbers() {
            let read = self.read(num, self.xref.entry(num), 0);
            if let Some(Object::Dict(dict)) = tolerated(read)? {
                if dict.is_type(b"Catalog") && dict.get(b"Pages").is_some() {
                    found = Some(dict);
                }
            }
        }
        found.ok_or(PdfError::Malformed)
    }

    /// The pages of the tree under `root`, in order, each with what it
    /// inherits. A node or an array of kids reached twice — shared, through
    /// another reference, or in a loop — is read once, and one that cannot be
    /// read is passed over, so one broken reference does not cost the
    /// document its other pages.
    fn page_tree(&self, root: &Object) -> Result<Vec<Listed>, PdfError> {
        let mut pages = Vec::new();
        let mut walked = OrderedSet::new();
        let mut kids_seen = 0usize;
        let mut pending = Vec::new();
        try_push(
            &mut pending,
            (root.try_clone()?, try_arc(Inherited::default())?, 0usize),
        )?;
        while let Some((node, inherited, depth)) = pending.pop() {
            let Some(resolved) = tolerated(self.unwalked(&node, &mut walked))?.flatten() else {
                continue;
            };
            let Some(dict) = resolved.as_dict() else {
                continue;
            };
            let kids = dict.get(b"Kids");
            let is_tree_node = dict.is_type(b"Pages") || (!dict.is_type(b"Page") && kids.is_some());
            if !is_tree_node {
                if pages.len() >= MAX_PAGES {
                    return Err(PdfError::TooComplex);
                }
                try_push(
                    &mut pages,
                    Listed {
                        page: node,
                        inherited,
                    },
                )?;
                continue;
            }
            if depth >= MAX_PAGE_TREE_DEPTH {
                return Err(PdfError::TooComplex);
            }
            let inherited = Inherited::extended(&inherited, dict)?;
            let Some(kids) = kids else {
                continue;
            };
            let Some(kids) = tolerated(self.unwalked(kids, &mut walked))?.flatten() else {
                continue;
            };
            let kids = kids.as_array().unwrap_or_default();
            kids_seen = kids_seen.saturating_add(kids.len());
            if kids_seen > MAX_PAGE_TREE_KIDS {
                return Err(PdfError::TooComplex);
            }
            pending.try_reserve(kids.len())?;
            // Reversed, so the first kid is the next one taken.
            for kid in kids.iter().rev() {
                pending.push((kid.try_clone()?, Arc::clone(&inherited), depth + 1));
            }
        }
        Ok(pages)
    }

    /// `object` resolved, unless the chain of references it begins meets an
    /// object in `walked`: every object the chain meets is walked by it.
    fn unwalked<'a>(
        &self,
        object: &'a Object,
        walked: &mut OrderedSet<u32>,
    ) -> Result<Option<Resolved<'a>>, PdfError> {
        let Object::Ref(reference) = object else {
            return Ok(Some(Resolved::Borrowed(object)));
        };
        self.followed(reference.num, 0, |num| Ok(walked.try_insert(num)?))
    }

    /// How many pages it has.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Whether it is encrypted: a protection a reader that writes it again
    /// must not silently drop.
    #[must_use]
    pub fn encrypted(&self) -> bool {
        self.security.is_some()
    }

    /// Its page `index`, counting from 0.
    ///
    /// # Errors
    ///
    /// [`PdfError::NoSuchPage`] for an index past the last page, and those of
    /// reading the page's objects.
    pub fn page(&self, index: usize) -> Result<Page<'_>, PdfError> {
        let listed = self.pages.get(index).ok_or(PdfError::NoSuchPage)?;
        let Object::Dict(page) = self.resolve(&listed.page)?.into_owned()? else {
            return Err(PdfError::Malformed);
        };
        Page::new(self, page, &listed.inherited)
    }

    /// What it states beyond its pages' content. What cannot be read counts
    /// as more, so a reader keeps from writing over what it never saw.
    ///
    /// # Errors
    ///
    /// [`PdfError::TooComplex`] and [`PdfError::OutOfMemory`].
    pub fn about(&self) -> Result<About, PdfError> {
        let mut about = About {
            more_metadata: self.catalog.get(b"Metadata").is_some(),
            more_parts: self.catalog.iter().any(|(key, _)| {
                !matches!(
                    key.as_bytes(),
                    b"Type" | b"Pages" | b"Version" | b"Metadata"
                )
            }),
            ..About::default()
        };
        let info = match self
            .xref
            .trailer
            .as_ref()
            .and_then(|trailer| trailer.get(b"Info"))
        {
            Some(info) => tolerated(self.resolve(info).and_then(Resolved::into_owned))?,
            None => Some(Object::Dict(Dict::new())),
        };
        let Some(Object::Dict(info)) = info else {
            about.more_metadata = true;
            return Ok(about);
        };
        for (key, value) in info.iter() {
            let value = tolerated(self.resolve(value))?;
            let text = match value.as_deref().and_then(Object::as_string) {
                Some(bytes) => Some(pdfdoc::text(bytes)?),
                None => None,
            };
            match key.as_bytes() {
                b"Title" if text.is_some() => about.title = text,
                b"Subject" if text.is_some() => about.subject = text,
                b"Producer" | b"Creator" | b"CreationDate" | b"ModDate" => {}
                _ => about.more_metadata = true,
            }
        }
        Ok(about)
    }

    /// Which optional content its default configuration hides.
    pub(crate) fn visibility(&self) -> &Visibility {
        &self.visibility
    }

    /// `object`, or the object it refers to: the null object where a
    /// reference names no object.
    pub(crate) fn resolve<'a>(&self, object: &'a Object) -> Result<Resolved<'a>, PdfError> {
        match object {
            Object::Ref(reference) => self.object_at(reference.num, 0),
            direct => Ok(Resolved::Borrowed(direct)),
        }
    }

    /// The value of `key` in `dict`, resolved: the null object where it has
    /// none.
    pub(crate) fn get<'a>(&self, dict: &'a Dict, key: &[u8]) -> Result<Resolved<'a>, PdfError> {
        match dict.get(key) {
            Some(object) => self.resolve(object),
            None => Ok(Resolved::Owned(Object::Null)),
        }
    }

    /// Indirect object `num`, references it holds followed to the end of
    /// their chain. A reference's generation is not insisted on, as no reader
    /// of real-world files does; the object found keeps its own.
    fn object_at(&self, num: u32, depth: usize) -> Result<Resolved<'static>, PdfError> {
        let mut chain = ArrayVec::<u32, { MAX_RESOLVE_DEPTH + 1 }>::new();
        // A chain meeting an object again is a cycle.
        let followed = self.followed(num, depth, |met| {
            Ok(!chain.contains(&met) && chain.try_push(met).is_ok())
        })?;
        followed.ok_or(PdfError::Malformed)
    }

    /// Object `num`, references it holds followed from `depth` to the end of
    /// their chain, each object the chain meets offered first to `pass`:
    /// `None` where `pass` stops it.
    fn followed(
        &self,
        num: u32,
        depth: usize,
        mut pass: impl FnMut(u32) -> Result<bool, PdfError>,
    ) -> Result<Option<Resolved<'static>>, PdfError> {
        let mut next = num;
        for at in depth..=MAX_RESOLVE_DEPTH {
            if !pass(next)? {
                return Ok(None);
            }
            let stored = self.stored(next, at)?;
            match &*stored {
                Object::Ref(reference) => next = reference.num,
                _ => return Ok(Some(stored)),
            }
        }
        Err(PdfError::TooComplex)
    }

    /// What indirect object `num` holds, a reference it holds not followed:
    /// read at `depth` the first time it is asked for, and kept.
    fn stored(&self, num: u32, depth: usize) -> Result<Resolved<'static>, PdfError> {
        let entry = self.xref.entry(num);
        if entry == Entry::Free {
            return Ok(Resolved::Owned(Object::Null));
        }
        if let Some(kept) = self.objects.get(num) {
            return kept.map(Resolved::Shared);
        }
        let met = self.objects.mark(num)?;
        let read = self
            .read(num, entry, depth)
            .and_then(|object| Ok(try_arc(object)?));
        self.objects.settle(num, met, &read);
        read.map(Resolved::Shared)
    }

    /// Object `num`, read from where `entry` places it.
    fn read(&self, num: u32, entry: Entry, depth: usize) -> Result<Object, PdfError> {
        match entry {
            Entry::Free => Ok(Object::Null),
            Entry::Offset(offset) => self.direct(num, offset, depth),
            Entry::Compressed { stream, index } => self.compressed(stream, index, num, depth),
        }
    }

    /// Object `num`, written at `offset` of the file.
    fn direct(&self, num: u32, offset: usize, depth: usize) -> Result<Object, PdfError> {
        let mut lexer = Lexer::charged(&self.file, offset, &self.lexing);
        let found = parse::head(&mut lexer)?;
        if found.num != num {
            return Err(PdfError::Malformed);
        }
        let body = parse::body(&mut lexer)?;
        let (mut dict, data) = match body {
            Indirect::Object(mut object) => {
                if let Some(security) = &self.security {
                    security.decrypt_object(&mut object, found)?;
                }
                return Ok(object);
            }
            Indirect::Stream { dict, data } => (dict, data),
        };
        if let Some(security) = &self.security {
            security.decrypt_dict(&mut dict, found)?;
        }
        let length = match dict.get(b"Length") {
            Some(Object::Ref(length)) => {
                tolerated(self.object_at(length.num, depth + 1))?.and_then(|length| length.as_int())
            }
            Some(length) => length.as_int(),
            None => None,
        };
        let length = length.and_then(|length| usize::try_from(length).ok());
        let span = parse::stream_span(&self.file, data, length, &self.endstreams)?
            .ok_or(PdfError::Malformed)?;
        Ok(parse::stream_object(dict, span, found))
    }

    /// Object `num`, the `index`th of object stream `stream`.
    fn compressed(
        &self,
        stream: u32,
        index: u32,
        num: u32,
        depth: usize,
    ) -> Result<Object, PdfError> {
        {
            let decoded = self
                .object_streams
                .try_borrow()
                .map_err(|_| PdfError::Malformed)?;
            match decoded.get(&stream) {
                Some(Ok(objects)) => return objects.object(index, num, &self.lexing),
                Some(Err(failure)) if self.objects.holds(failure) => return Err(failure.error),
                Some(Err(_)) | None => {}
            }
        }
        let met = self.objects.met();
        let objects = match self.object_stream(stream, depth + 1) {
            // The heap's refusal or a bound met may not meet a later decoding.
            Err(error @ (PdfError::OutOfMemory | PdfError::TooComplex)) => return Err(error),
            objects => objects,
        };
        let object = objects
            .as_ref()
            .map_err(|&error| error)
            .and_then(|objects| objects.object(index, num, &self.lexing));
        let kept = objects.map_err(|error| self.objects.failure(error, met));
        let mut decoded = self
            .object_streams
            .try_borrow_mut()
            .map_err(|_| PdfError::Malformed)?;
        xref::charged_insert(&mut decoded, stream, kept, &self.budget)?;
        object
    }

    /// Object stream `stream`, decoded, its objects then lexed as the file's
    /// are, as many times over.
    fn object_stream(&self, stream: u32, depth: usize) -> Result<ObjectStream, PdfError> {
        let found = self.object_at(stream, depth)?;
        let Object::Stream(found) = &*found else {
            return Err(PdfError::Malformed);
        };
        let decoded = self.decode_at(found, depth)?;
        if decoded.image.is_some() {
            return Err(PdfError::Malformed);
        }
        let more = lexable(decoded.bytes.len());
        let objects = ObjectStream::new(&found.dict, decoded.bytes, &self.budget)?;
        self.lexing.set(self.lexing.get().saturating_add(more));
        Ok(objects)
    }

    /// `stream`'s bytes, decoded through its filters.
    pub(crate) fn decode(&self, stream: &Stream) -> Result<Decoded, PdfError> {
        self.decode_at(stream, 0)
    }

    fn decode_at(&self, stream: &Stream, depth: usize) -> Result<Decoded, PdfError> {
        // A stream whose bytes are in another file has none of its own here.
        if stream.dict.get(b"F").is_some() {
            return Err(PdfError::Unsupported);
        }
        let data = self
            .file
            .get(stream.data.clone())
            .ok_or(PdfError::Malformed)?;
        let filter = self.entry_at(&stream.dict, b"Filter", depth)?;
        let params = self.entry_at(&stream.dict, b"DecodeParms", depth)?;
        let params = self.chain_params(params, depth)?;
        let chain = filter::chain(Some(&filter), Some(&params))?;
        // A stream's own crypt filter, which comes first, names how it alone
        // is encrypted: the identity unless it names another.
        let own = match chain.first() {
            Some(&(b"Crypt", params)) => Some(params),
            _ => None,
        };
        let named = match own.flatten().and_then(|params| params.get(b"Name")) {
            Some(name) => Some(self.resolve(name)?),
            None => None,
        };
        let crypt = match (own, named.as_deref()) {
            (None, _) => None,
            (Some(_), None) => Some(crate::crypt::IDENTITY),
            (Some(_), Some(Object::Name(name))) => Some(name.as_bytes()),
            (Some(_), Some(_)) => return Err(PdfError::Malformed),
        };
        let data = match &self.security {
            Some(security) => {
                let decrypted = security.decrypt_stream(&stream.dict, data, stream.owner, crypt)?;
                if let Cow::Owned(bytes) = &decrypted {
                    filter::charge(&self.budget, bytes.len())?;
                }
                decrypted
            }
            // An unencrypted file defines no crypt filter but the identity.
            None if crypt.is_some_and(|crypt| crypt != crate::crypt::IDENTITY) => {
                return Err(PdfError::Unsupported);
            }
            None => Cow::Borrowed(data),
        };
        filter::decode(data, &chain, &self.budget)
    }

    /// `dict`'s entry `key`, resolved at `depth`.
    fn entry_at<'d>(
        &self,
        dict: &'d Dict,
        key: &[u8],
        depth: usize,
    ) -> Result<Resolved<'d>, PdfError> {
        Ok(match dict.get(key) {
            Some(Object::Ref(reference)) => self.object_at(reference.num, depth + 1)?,
            Some(object) => Resolved::Borrowed(object),
            None => Resolved::Owned(Object::Null),
        })
    }

    /// `params` as a filter chain reads them: the references among an
    /// array's first elements, as many as a chain may have filters, resolved.
    fn chain_params<'d>(
        &self,
        params: Resolved<'d>,
        depth: usize,
    ) -> Result<Resolved<'d>, PdfError> {
        let Object::Array(items) = &*params else {
            return Ok(params);
        };
        let read = &items[..items.len().min(MAX_FILTERS)];
        if !read.iter().any(|item| matches!(item, Object::Ref(_))) {
            return Ok(params);
        }
        let mut resolved = Vec::new();
        resolved.try_reserve_exact(read.len())?;
        for item in read {
            resolved.push(match item {
                Object::Ref(reference) => self.object_at(reference.num, depth + 1)?.into_owned()?,
                item => item.try_clone()?,
            });
        }
        Ok(Resolved::Owned(Object::Array(resolved)))
    }

    /// What its streams may still decode to.
    pub(crate) fn budget(&self) -> &Cell<u64> {
        &self.budget
    }

    /// The steps its functions may still take.
    pub(crate) fn work(&self) -> &Work {
        &self.work
    }

    /// The function objects it has read, by number.
    pub(crate) fn functions(&self) -> &Functions {
        &self.functions
    }
}

/// What reading the objects `bytes` bytes hold may lex.
fn lexable(bytes: usize) -> u64 {
    u64::try_from(bytes.saturating_mul(MAX_LEX_PASSES)).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
