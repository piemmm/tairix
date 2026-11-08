//! A PDF document: its file, its cross-reference, its pages, and the objects
//! it resolves only when they are asked for.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_collections::{try_arc, OrderedMap, OrderedSet};
use tairix_util::fallible::try_push;

use crate::crypt::Security;
use crate::filter::{self, Decoded};
use crate::function::Work;
use crate::lex::Lexer;
use crate::limits::{
    HEADER_WINDOW, MAX_FILTERS, MAX_PAGES, MAX_PAGE_TREE_DEPTH, MAX_PAGE_TREE_KIDS,
    MAX_RESOLVE_DEPTH,
};
use crate::object::{owned, Dict, Object, Stream};
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
    /// The most bytes its streams may decode to, all of them together, so no
    /// filter chain can expand past the memory its reader was given.
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

/// An open PDF.
pub struct Document<'f> {
    file: &'f [u8],
    xref: Xref,
    pages: Vec<Listed>,
    visibility: Visibility,
    security: Option<Security>,
    endstreams: Endstreams,
    object_streams: RefCell<OrderedMap<u32, ObjectStream>>,
    budget: Cell<u64>,
    work: Work,
}

/// Whether `file` is a PDF: its `%PDF-` header within its first kilobyte,
/// as every reader allows.
#[must_use]
pub fn recognised(file: &[u8]) -> bool {
    parse::find(&file[..file.len().min(HEADER_WINDOW)], b"%PDF-").is_some()
}

impl<'f> Document<'f> {
    /// The PDF `file` holds, its cross-reference read as the file states it
    /// and rebuilt by scanning where that fails, as every reader of
    /// real-world files does.
    ///
    /// # Errors
    ///
    /// [`PdfError::Malformed`] for a file that is not a PDF or is damaged
    /// past repair, [`PdfError::NeedsPassword`] and
    /// [`PdfError::WrongPassword`] for an encrypted one the password does not
    /// open, [`PdfError::TooComplex`] past a containment bound, and
    /// [`PdfError::OutOfMemory`].
    pub fn open(file: &'f [u8], options: &Options<'_>) -> Result<Self, PdfError> {
        if !recognised(file) {
            return Err(PdfError::Malformed);
        }
        let mut document = Self {
            file,
            xref: Xref::default(),
            pages: Vec::new(),
            visibility: Visibility::default(),
            security: None,
            endstreams: Endstreams::default(),
            object_streams: RefCell::new(OrderedMap::new()),
            budget: Cell::new(options.budget),
            work: Work::new(),
        };
        let source = xref::Source {
            file,
            budget: &document.budget,
            ends: &document.endstreams,
        };
        let stated = match xref::chained(source) {
            Ok(xref) => document.assemble(xref, options.password),
            Err(error) => Err(error),
        };
        match stated {
            Ok(()) => {}
            Err(PdfError::Malformed | PdfError::Unsupported) => {
                let repaired = xref::repair(file)?;
                document.assemble(repaired, options.password)?;
            }
            Err(error) => return Err(error),
        }
        Ok(document)
    }

    /// Read the document `xref` places the objects of, opened with
    /// `password` where it is encrypted: the object streams a repair found
    /// indexed, the catalog found, and the page tree walked.
    fn assemble(&mut self, xref: Xref, password: Option<&[u8]>) -> Result<(), PdfError> {
        self.xref = xref;
        self.object_streams = RefCell::new(OrderedMap::new());
        self.pages = Vec::new();
        self.security = None;
        let security = match &self.xref.trailer {
            Some(trailer) => Security::open(self, trailer, password)?,
            None => None,
        };
        self.security = security;
        // An object stream read while the security was found was read clear.
        self.object_streams = RefCell::new(OrderedMap::new());
        self.index_object_streams()?;
        let catalog = self.catalog()?;
        let root = catalog.get(b"Pages").ok_or(PdfError::Malformed)?;
        self.pages = self.page_tree(root)?;
        if self.pages.is_empty() {
            return Err(PdfError::Malformed);
        }
        self.visibility = tolerated(Visibility::of(self, &catalog))?.unwrap_or_default();
        Ok(())
    }

    /// Place the objects of the object streams a repair found. A stream that
    /// cannot be read places nothing: what remains is all a damaged file has.
    fn index_object_streams(&mut self) -> Result<(), PdfError> {
        for stream in core::mem::take(&mut self.xref.unindexed) {
            if let Some(objects) = tolerated(self.object_stream(stream, 0))? {
                self.xref.index(stream, &objects)?;
                self.object_streams.get_mut().try_insert(stream, objects)?;
            }
        }
        Ok(())
    }

    /// The catalog: the trailer's `/Root`, or where a repair found no trailer
    /// naming one, the last catalog among the objects.
    fn catalog(&self) -> Result<Dict, PdfError> {
        if let Some(root) = self
            .xref
            .trailer
            .as_ref()
            .and_then(|trailer| trailer.get(b"Root"))
        {
            return match owned(self.resolve(root)?)? {
                Object::Dict(catalog) => Ok(catalog),
                _ => Err(PdfError::Malformed),
            };
        }
        let mut found = None;
        for num in self.xref.numbers() {
            if let Some(Object::Dict(dict)) = tolerated(self.object_at(num, 0))? {
                if dict.is_type(b"Catalog") && dict.get(b"Pages").is_some() {
                    found = Some(dict);
                }
            }
        }
        found.ok_or(PdfError::Malformed)
    }

    /// The pages of the tree under `root`, in order, each with what it
    /// inherits. A node reached twice, shared or in a loop, is read once, and
    /// one that cannot be read is passed over, so one broken reference does
    /// not cost the document its other pages.
    fn page_tree(&self, root: &Object) -> Result<Vec<Listed>, PdfError> {
        let mut pages = Vec::new();
        let mut visited = OrderedSet::new();
        let mut kids_seen = 0usize;
        let mut pending = Vec::new();
        try_push(
            &mut pending,
            (root.try_clone()?, try_arc(Inherited::default())?, 0usize),
        )?;
        while let Some((node, inherited, depth)) = pending.pop() {
            if let Object::Ref(reference) = node {
                if !visited.try_insert(reference.num)? {
                    continue;
                }
            }
            let Some(resolved) = tolerated(self.resolve(&node))? else {
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
            let Some(kids) = tolerated(self.resolve(kids))? else {
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

    /// How many pages it has.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Its page `index`, counting from 0.
    ///
    /// # Errors
    ///
    /// [`PdfError::NoSuchPage`] for an index past the last page, and those of
    /// reading the page's objects.
    pub fn page(&self, index: usize) -> Result<Page<'_>, PdfError> {
        let listed = self.pages.get(index).ok_or(PdfError::NoSuchPage)?;
        let Object::Dict(page) = owned(self.resolve(&listed.page)?)? else {
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
        let mut about = About::default();
        let catalog = tolerated(self.catalog())?;
        about.more_parts = catalog.as_ref().is_none_or(|catalog| {
            catalog.iter().any(|(key, _)| {
                !matches!(
                    key.as_bytes(),
                    b"Type" | b"Pages" | b"Version" | b"Metadata"
                )
            })
        });
        about.more_metadata = catalog
            .as_ref()
            .is_none_or(|catalog| catalog.get(b"Metadata").is_some());
        let info = match self
            .xref
            .trailer
            .as_ref()
            .and_then(|trailer| trailer.get(b"Info"))
        {
            Some(info) => tolerated(self.resolve(info).and_then(owned))?,
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
    pub(crate) fn resolve<'a>(&self, object: &'a Object) -> Result<Cow<'a, Object>, PdfError> {
        match object {
            Object::Ref(reference) => self.object_at(reference.num, 0).map(Cow::Owned),
            direct => Ok(Cow::Borrowed(direct)),
        }
    }

    /// The value of `key` in `dict`, resolved: the null object where it has
    /// none.
    pub(crate) fn get<'a>(&self, dict: &'a Dict, key: &[u8]) -> Result<Cow<'a, Object>, PdfError> {
        match dict.get(key) {
            Some(object) => self.resolve(object),
            None => Ok(Cow::Owned(Object::Null)),
        }
    }

    /// Indirect object `num`, references it holds followed to the end of
    /// their chain. A reference's generation is not insisted on, as no reader
    /// of real-world files does; the object found keeps its own.
    fn object_at(&self, num: u32, depth: usize) -> Result<Object, PdfError> {
        // A chain this long is a cycle.
        if depth > MAX_RESOLVE_DEPTH {
            return Err(PdfError::Malformed);
        }
        let object = match self.xref.entry(num) {
            Entry::Free => return Ok(Object::Null),
            Entry::Offset(offset) => self.direct(num, offset, depth)?,
            Entry::Compressed { stream, index } => self.compressed(stream, index, num, depth)?,
        };
        match object {
            Object::Ref(next) => self.object_at(next.num, depth + 1),
            object => Ok(object),
        }
    }

    /// Object `num`, written at `offset` of the file.
    fn direct(&self, num: u32, offset: usize, depth: usize) -> Result<Object, PdfError> {
        let mut lexer = Lexer::new(self.file, offset);
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
        let span = parse::stream_span(self.file, data, length, &self.endstreams)?
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
            let cached = self
                .object_streams
                .try_borrow()
                .map_err(|_| PdfError::Malformed)?;
            if let Some(objects) = cached.get(&stream) {
                return objects.object(index, num);
            }
        }
        let objects = self.object_stream(stream, depth + 1)?;
        let object = objects.object(index, num);
        self.object_streams
            .try_borrow_mut()
            .map_err(|_| PdfError::Malformed)?
            .try_insert(stream, objects)?;
        object
    }

    /// Object stream `stream`, decoded.
    fn object_stream(&self, stream: u32, depth: usize) -> Result<ObjectStream, PdfError> {
        let Object::Stream(found) = self.object_at(stream, depth)? else {
            return Err(PdfError::Malformed);
        };
        let decoded = self.decode_at(&found, depth)?;
        if decoded.image.is_some() {
            return Err(PdfError::Malformed);
        }
        ObjectStream::new(&found.dict, decoded.bytes)
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
        let data = match &self.security {
            Some(security) => {
                // A stream's own crypt filter, which comes first, names how it
                // alone is encrypted: the identity unless it names another.
                let crypt = match chain.first() {
                    Some(&(b"Crypt", params)) => Some(
                        params
                            .and_then(|params| params.name(b"Name"))
                            .unwrap_or(b"Identity"),
                    ),
                    _ => None,
                };
                let decrypted = security.decrypt_stream(&stream.dict, data, stream.owner, crypt)?;
                if let Cow::Owned(bytes) = &decrypted {
                    filter::charge(&self.budget, bytes.len())?;
                }
                decrypted
            }
            None => Cow::Borrowed(data),
        };
        filter::decode(&data, &chain, &self.budget)
    }

    /// `dict`'s entry `key`, resolved at `depth`.
    fn entry_at<'d>(
        &self,
        dict: &'d Dict,
        key: &[u8],
        depth: usize,
    ) -> Result<Cow<'d, Object>, PdfError> {
        Ok(match dict.get(key) {
            Some(Object::Ref(reference)) => Cow::Owned(self.object_at(reference.num, depth + 1)?),
            Some(object) => Cow::Borrowed(object),
            None => Cow::Owned(Object::Null),
        })
    }

    /// `params` as a filter chain reads them: the references among an
    /// array's first elements, as many as a chain may have filters, resolved.
    fn chain_params<'d>(
        &self,
        params: Cow<'d, Object>,
        depth: usize,
    ) -> Result<Cow<'d, Object>, PdfError> {
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
                Object::Ref(reference) => self.object_at(reference.num, depth + 1)?,
                item => item.try_clone()?,
            });
        }
        Ok(Cow::Owned(Object::Array(resolved)))
    }

    /// What its streams may still decode to.
    pub(crate) fn budget(&self) -> &Cell<u64> {
        &self.budget
    }

    /// The steps its functions may still take.
    pub(crate) fn work(&self) -> &Work {
        &self.work
    }
}

/// `read`'s value, or `None` where what was read was damaged: only the heap's
/// refusal and a containment bound are errors.
pub(crate) fn tolerated<T>(read: Result<T, PdfError>) -> Result<Option<T>, PdfError> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(error @ (PdfError::OutOfMemory | PdfError::TooComplex)) => Err(error),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
