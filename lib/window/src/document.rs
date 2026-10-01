//! A window's document file: where its document came from and is saved to,
//! the save in flight and the ones asked for behind it, and a file chooser
//! waiting on the user — the decisions around them, apart from the syscalls
//! that carry them out, so every sequence is a host test.
//!
//! `H` is whatever holds the file open; this only ever shares it. The
//! document itself is the application's, seen through [`SavedDocument`].
//!
//! One save is in flight at a time. A save asked for meanwhile freezes the
//! document as it is then and is written once those ahead of it land, so
//! what is saved is what was asked for. Plain saves asked in a row become
//! one of the latest document, but every Save As is its own, since the
//! chooser has already made its file. Closing writes every chained save at
//! once, each where it would have gone.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Display;

/// What a document that has never been saved is called.
pub const UNTITLED: &str = "Untitled";

/// What a document is told once a save of it lands.
const SAVED: &str = "Saved";

/// Why a document's file could not be read in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReadFailure {
    /// Reading or measuring it was refused.
    Unreadable(tairix_abi::Errno),
    /// It is not a file.
    NotAFile,
    /// There is not enough memory to hold it.
    NoMemory,
}

impl Display for ReadFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable(err) => write!(f, "it could not be read ({err})"),
            Self::NotAFile => f.write_str("it is not a file"),
            Self::NoMemory => f.write_str("there is not enough memory to hold it"),
        }
    }
}

/// How a window's document may be written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Access {
    /// A new document with no file yet.
    Untitled,
    /// A file the window was handed read-only.
    ReadOnly,
    /// A file the window may save over.
    Writable,
}

/// What a [`DocumentFile`] needs of the document it saves.
pub trait SavedDocument {
    /// The document frozen for a worker to write while editing goes on.
    type Snapshot;

    /// How the document may be written.
    fn access(&self) -> Access;

    /// Whether it has changed since it last matched its file.
    fn is_modified(&self) -> bool;

    /// Whether it holds nothing at all.
    fn is_empty(&self) -> bool;

    /// Freeze the document, answering the generation frozen, or `None` when
    /// the memory to freeze it cannot be had.
    fn snapshot(&mut self) -> Option<(u64, Arc<Self::Snapshot>)>;

    /// The document of `generation` reached its file.
    fn saved(&mut self, generation: u64);

    /// It is called `name` from now on, after the file a Save As made.
    fn rename(&mut self, name: String);

    /// It may be written as `access` says from now on.
    fn set_access(&mut self, access: Access);

    /// Tell the user `message`.
    fn say(&mut self, message: String);
}

/// What a file chooser open for a window is for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PickFor {
    /// A document to open.
    Open,
    /// Where to save the document, closing the window once it is saved when
    /// `then_close`.
    SaveAs {
        /// Close the window once saved.
        then_close: bool,
    },
}

/// A save to carry out: `snapshot`, the document as it was at `generation`,
/// written through `target`, and called `rename` once it lands when that was
/// a Save As.
pub struct SaveJob<H, S> {
    /// What the document is written through.
    pub target: Arc<H>,
    /// The generation `snapshot` is of.
    pub generation: u64,
    /// The document as it was when the save was asked for.
    pub snapshot: Arc<S>,
    /// The name of the file it writes when that is not the document's own: a
    /// Save As's, which the document takes once it lands.
    pub rename: Option<String>,
}

/// What asking for a save came to.
pub enum SaveStep<H, S> {
    /// Write this now.
    Write(SaveJob<H, S>),
    /// A save is in flight; this one is written once those ahead of it land.
    Queued,
    /// There is nowhere to write it: ask the user where.
    AskWhere {
        /// Close the window once saved.
        then_close: bool,
    },
    /// The document could not be frozen to be written.
    NoMemory,
}

/// What a save landing leaves the window to do.
pub struct Landed<H, S> {
    /// The save to carry out next, when one was asked for behind it.
    pub next: Option<SaveStep<H, S>>,
    /// The window may now close.
    pub close: bool,
    /// A close that waited on the save is given up, the save having failed.
    pub close_abandoned: bool,
}

/// A save asked for while another is in flight: the document as it was then,
/// and the file chosen for it when it is a Save As.
struct Asked<H, S> {
    generation: u64,
    snapshot: Arc<S>,
    save_as: Option<(Arc<H>, String)>,
    then_close: bool,
}

/// The save in flight: where it writes, and the saves asked for behind it.
struct Saving<H, S> {
    target: Arc<H>,
    /// The name of the file a Save As in flight makes, where a plain save
    /// behind it goes too.
    rename: Option<String>,
    then_close: bool,
    /// Oldest first. A plain save joins a plain save just ahead of it — the
    /// later document is the one to write — but every Save As is its own.
    next: Vec<Asked<H, S>>,
}

/// Where a window's document is read from and saved to, and what it waits on.
pub struct DocumentFile<H, S> {
    handle: Option<Arc<H>>,
    pick: Option<PickFor>,
    saving: Option<Saving<H, S>>,
}

impl<H, S> Default for DocumentFile<H, S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H, S> DocumentFile<H, S> {
    /// A window with no file.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            handle: None,
            pick: None,
            saving: None,
        }
    }

    /// The document was read in from `handle`, or could not be (`None`).
    pub fn opened(&mut self, handle: Option<Arc<H>>) {
        self.handle = handle;
    }

    /// Whether the window is on its way to closing once its document is
    /// saved.
    #[must_use]
    pub fn closing(&self) -> bool {
        self.pick == Some(PickFor::SaveAs { then_close: true })
            || self.saving.as_ref().is_some_and(|saving| {
                saving.then_close || saving.next.iter().any(|asked| asked.then_close)
            })
    }

    /// Whether a file chooser is open for the window.
    #[must_use]
    pub const fn picking(&self) -> bool {
        self.pick.is_some()
    }

    /// Whether a save is in flight.
    #[must_use]
    pub const fn saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The name a plain save asked for now is written under when that is not
    /// the document's own: the file of the last Save As ahead of it.
    #[must_use]
    pub fn plain_destination(&self) -> Option<&str> {
        let saving = self.saving.as_ref()?;
        saving
            .next
            .iter()
            .rev()
            .find_map(|asked| asked.save_as.as_ref().map(|(_, name)| name.as_str()))
            .or(saving.rename.as_deref())
    }

    /// Whether `document` has a file of its own a plain save writes to.
    #[must_use]
    pub fn writes_back<D: SavedDocument<Snapshot = S>>(&self, document: &D) -> bool {
        self.writable(document).is_some()
    }

    /// Whether `document` may be replaced by one opened into its window: no
    /// file, nothing in it, no change — and nothing under way, so no answer
    /// can land on a document it was not asked for.
    #[must_use]
    pub fn pristine<D: SavedDocument<Snapshot = S>>(&self, document: &D) -> bool {
        document.access() == Access::Untitled
            && self.saving.is_none()
            && self.pick.is_none()
            && !document.is_modified()
            && document.is_empty()
    }

    /// Note a file chooser opened for `pick`; `false`, changing nothing,
    /// when one already is.
    pub fn start_pick(&mut self, pick: PickFor) -> bool {
        if self.pick.is_some() {
            return false;
        }
        self.pick = Some(pick);
        true
    }

    /// The chooser closed, chosen from or cancelled: what it was for, or
    /// `None` when no chooser was open.
    pub fn end_pick(&mut self) -> Option<PickFor> {
        self.pick.take()
    }

    /// Ask for `document` to be saved — through `save_as` for a Save As,
    /// else where it came from, else wherever the user says — closing once
    /// saved when `then_close`.
    pub fn save<D: SavedDocument<Snapshot = S>>(
        &mut self,
        document: &mut D,
        save_as: Option<(Arc<H>, String)>,
        then_close: bool,
    ) -> SaveStep<H, S> {
        let writable = self.writable(document).is_some();
        if let Some(saving) = &mut self.saving {
            let ahead_renames =
                saving.rename.is_some() || saving.next.iter().any(|asked| asked.save_as.is_some());
            if save_as.is_none() && !ahead_renames && !writable {
                return SaveStep::AskWhere { then_close };
            }
            let Some((generation, snapshot)) = document.snapshot() else {
                return SaveStep::NoMemory;
            };
            if let Some(last) = saving
                .next
                .last_mut()
                .filter(|last| save_as.is_none() && last.save_as.is_none())
            {
                last.generation = generation;
                last.snapshot = snapshot;
                last.then_close |= then_close;
            } else {
                if saving.next.try_reserve(1).is_err() {
                    return SaveStep::NoMemory;
                }
                saving.next.push(Asked {
                    generation,
                    snapshot,
                    save_as,
                    then_close,
                });
            }
            return SaveStep::Queued;
        }
        let Some((target, rename)) = self.destination(document, save_as) else {
            return SaveStep::AskWhere { then_close };
        };
        let Some((generation, snapshot)) = document.snapshot() else {
            return SaveStep::NoMemory;
        };
        self.write(
            SaveJob {
                target,
                generation,
                snapshot,
                rename,
            },
            then_close,
            Vec::new(),
        )
    }

    /// Where a writable document came from.
    fn writable<D: SavedDocument<Snapshot = S>>(&self, document: &D) -> Option<&Arc<H>> {
        self.handle
            .as_ref()
            .filter(|_| document.access() == Access::Writable)
    }

    /// Where a save goes: the file chosen for a Save As, else where a
    /// writable document came from.
    fn destination<D: SavedDocument<Snapshot = S>>(
        &self,
        document: &D,
        save_as: Option<(Arc<H>, String)>,
    ) -> Option<(Arc<H>, Option<String>)> {
        match save_as {
            Some((target, name)) => Some((target, Some(name))),
            None => self
                .writable(document)
                .map(|handle| (Arc::clone(handle), None)),
        }
    }

    /// Put `job` in flight, `next` behind it.
    fn write(
        &mut self,
        job: SaveJob<H, S>,
        then_close: bool,
        next: Vec<Asked<H, S>>,
    ) -> SaveStep<H, S> {
        self.saving = Some(Saving {
            target: Arc::clone(&job.target),
            rename: job.rename.clone(),
            then_close,
            next,
        });
        SaveStep::Write(job)
    }

    /// The save in flight landed through `target` — the document, renamed
    /// after the file a Save As made, is writable from then on and told so,
    /// with what its format could not keep when `result` names that — or was
    /// refused with `result`'s error, which the document is then told.
    pub fn saved<D: SavedDocument<Snapshot = S>, E: Display>(
        &mut self,
        document: &mut D,
        target: Arc<H>,
        generation: u64,
        rename: Option<String>,
        result: Result<Option<&str>, E>,
    ) -> Landed<H, S> {
        let (then_close, next) = self.saving.take().map_or((false, Vec::new()), |saving| {
            (saving.then_close, saving.next)
        });
        let mut landed = Landed {
            next: None,
            close: false,
            close_abandoned: false,
        };
        match result {
            Ok(caveat) => {
                // Whatever it came from, the document now has a file of its own
                // it may save over.
                document.saved(generation);
                if let Some(name) = rename {
                    self.handle = Some(target);
                    document.rename(name);
                }
                document.set_access(Access::Writable);
                document.say(caveat.map_or_else(
                    || String::from(SAVED),
                    |caveat| alloc::format!("{SAVED}. {caveat}"),
                ));
                if let Some(step) = self.issue(document, next, then_close) {
                    landed.next = Some(step);
                } else if then_close && document.is_modified() {
                    // Changed while saving: what closes is what is saved.
                    landed.next = Some(self.save(document, None, true));
                } else {
                    landed.close = then_close;
                }
            }
            Err(err) => {
                document.say(alloc::format!("Could not save: {err}"));
                let mut next = next;
                // Plain saves asked behind a failed Save As were for the file it
                // would have made, so they go with it rather than overwrite the
                // one the user was saving away from.
                let orphans = if rename.is_some() {
                    next.iter()
                        .position(|asked| asked.save_as.is_some())
                        .unwrap_or(next.len())
                } else {
                    0
                };
                let orphaned_close = next.drain(..orphans).any(|asked| asked.then_close);
                landed.close_abandoned = then_close || orphaned_close;
                landed.next = self.issue(document, next, false);
            }
        }
        landed
    }

    /// Put the first of `chained` with somewhere to go in flight, the rest
    /// behind it, closing once they have all landed when any of them or
    /// `then_close` said to. A plain save of a document with no file of its
    /// own is dropped.
    fn issue<D: SavedDocument<Snapshot = S>>(
        &mut self,
        document: &D,
        mut chained: Vec<Asked<H, S>>,
        then_close: bool,
    ) -> Option<SaveStep<H, S>> {
        let then_close = then_close || chained.iter().any(|asked| asked.then_close);
        while !chained.is_empty() {
            let asked = chained.remove(0);
            let Some((target, rename)) = self.destination(document, asked.save_as) else {
                continue;
            };
            let job = SaveJob {
                target,
                generation: asked.generation,
                snapshot: asked.snapshot,
                rename,
            };
            return Some(self.write(job, then_close, chained));
        }
        None
    }

    /// The window is closing: every save asked for behind the one in flight,
    /// in order, to be written now where it would have gone — so no file the
    /// user chose is left as the chooser made it. A plain save goes where the
    /// last Save As ahead of it goes, and is named after that file.
    pub fn close<D: SavedDocument<Snapshot = S>>(&mut self, document: &D) -> Vec<SaveJob<H, S>> {
        self.pick = None;
        let Some(saving) = self.saving.take() else {
            return Vec::new();
        };
        let mut plain = match saving.rename {
            Some(name) => Some((saving.target, Some(name))),
            None => self
                .writable(document)
                .cloned()
                .map(|handle| (handle, None)),
        };
        saving
            .next
            .into_iter()
            .filter_map(|asked| {
                let (target, rename) = if let Some((target, name)) = asked.save_as {
                    plain = Some((Arc::clone(&target), Some(name.clone())));
                    (target, Some(name))
                } else {
                    let (target, name) = plain.as_ref()?;
                    (Arc::clone(target), name.clone())
                };
                Some(SaveJob {
                    target,
                    generation: asked.generation,
                    snapshot: asked.snapshot,
                    rename,
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
