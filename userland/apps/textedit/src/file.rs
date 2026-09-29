//! A window's file: where its document came from and is saved to, the save
//! in flight and the ones asked for behind it, and a file chooser waiting on
//! the user — the decisions around them, apart from the syscalls that carry
//! them out, so every sequence is a host test.
//!
//! `H` is whatever holds the file open; the engine only ever shares it.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Display;

use crate::document::Snapshot;
use crate::view::{Access, View};

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
pub struct SaveJob<H> {
    /// What the document is written through.
    pub target: Arc<H>,
    /// The generation `snapshot` is of.
    pub generation: u64,
    /// The document as it was when the save was asked for.
    pub snapshot: Arc<Snapshot>,
    /// The name the document takes once saved, for a Save As.
    pub rename: Option<String>,
}

/// What asking for a save came to.
pub enum SaveStep<H> {
    /// Write this now.
    Write(SaveJob<H>),
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
pub struct Landed<H> {
    /// The save to carry out next, when one was asked for behind it.
    pub next: Option<SaveStep<H>>,
    /// The window may now close.
    pub close: bool,
    /// A close that waited on the save is given up, the save having failed.
    pub close_abandoned: bool,
}

/// A save asked for while another is in flight: the document as it was then,
/// and the file chosen for it when it is a Save As.
struct Asked<H> {
    generation: u64,
    snapshot: Arc<Snapshot>,
    save_as: Option<(Arc<H>, String)>,
    then_close: bool,
}

/// The save in flight: where it writes, and the saves asked for behind it.
struct Saving<H> {
    target: Arc<H>,
    /// It is a Save As, so a plain save behind it goes where it went.
    renames: bool,
    then_close: bool,
    /// Oldest first. A plain save joins a plain save just ahead of it — the
    /// later document is the one to write — but every Save As is its own,
    /// since the chooser has already made its file.
    next: Vec<Asked<H>>,
}

/// Where a window's document is read from and saved to, and what it waits on.
pub struct FileState<H> {
    handle: Option<Arc<H>>,
    pick: Option<PickFor>,
    saving: Option<Saving<H>>,
}

impl<H> Default for FileState<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> FileState<H> {
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

    /// Whether `view` may be replaced by a document opened into its window:
    /// no file, nothing in it, no change — and nothing under way, so no
    /// answer can land on a document it was not asked for.
    #[must_use]
    pub fn pristine(&self, view: &View) -> bool {
        view.access() == Access::Untitled
            && self.saving.is_none()
            && self.pick.is_none()
            && !view.editor().is_modified()
            && view.editor().document().is_empty()
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

    /// Ask for `view`'s document to be saved — through `save_as` for a Save
    /// As, else where it came from, else wherever the user says — closing
    /// once saved when `then_close`.
    ///
    /// Asked while a save is in flight, the document is frozen as it is now
    /// and written once the saves ahead of it land: what is saved is what was
    /// asked for.
    pub fn save(
        &mut self,
        view: &mut View,
        save_as: Option<(Arc<H>, String)>,
        then_close: bool,
    ) -> SaveStep<H> {
        let writable = self.writable(view).is_some();
        if let Some(saving) = &mut self.saving {
            let ahead_renames =
                saving.renames || saving.next.iter().any(|asked| asked.save_as.is_some());
            if save_as.is_none() && !ahead_renames && !writable {
                return SaveStep::AskWhere { then_close };
            }
            let Ok((generation, snapshot)) = view.editor_mut().snapshot() else {
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
        let Some((target, rename)) = self.destination(view, save_as) else {
            return SaveStep::AskWhere { then_close };
        };
        let Ok((generation, snapshot)) = view.editor_mut().snapshot() else {
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
    fn writable(&self, view: &View) -> Option<&Arc<H>> {
        self.handle
            .as_ref()
            .filter(|_| view.access() == Access::Writable)
    }

    /// Where a save goes: the file chosen for a Save As, else where a
    /// writable document came from.
    fn destination(
        &self,
        view: &View,
        save_as: Option<(Arc<H>, String)>,
    ) -> Option<(Arc<H>, Option<String>)> {
        match save_as {
            Some((target, name)) => Some((target, Some(name))),
            None => self.writable(view).map(|handle| (Arc::clone(handle), None)),
        }
    }

    /// Put `job` in flight, `next` behind it.
    fn write(&mut self, job: SaveJob<H>, then_close: bool, next: Vec<Asked<H>>) -> SaveStep<H> {
        self.saving = Some(Saving {
            target: Arc::clone(&job.target),
            renames: job.rename.is_some(),
            then_close,
            next,
        });
        SaveStep::Write(job)
    }

    /// The save in flight landed through `target`, or was refused with
    /// `result`'s error, which the window then states.
    pub fn saved<E: Display>(
        &mut self,
        view: &mut View,
        target: Arc<H>,
        generation: u64,
        rename: Option<String>,
        result: Result<(), E>,
    ) -> Landed<H> {
        let (then_close, next) = self.saving.take().map_or((false, Vec::new()), |saving| {
            (saving.then_close, saving.next)
        });
        let mut landed = Landed {
            next: None,
            close: false,
            close_abandoned: false,
        };
        match result {
            Ok(()) => {
                if rename.is_some() {
                    self.handle = Some(target);
                }
                view.saved(generation, rename);
                if let Some(step) = self.issue(view, next, then_close) {
                    landed.next = Some(step);
                } else if then_close && view.editor().is_modified() {
                    // Typed into while saving: what closes is what is saved.
                    landed.next = Some(self.save(view, None, true));
                } else {
                    landed.close = then_close;
                }
            }
            Err(err) => {
                view.say(alloc::format!("Could not save: {err}"));
                landed.close_abandoned = then_close;
                landed.next = self.issue(view, next, false);
            }
        }
        landed
    }

    /// Put the first of `chained` with somewhere to go in flight, the rest
    /// behind it, closing once they have all landed when any of them or
    /// `then_close` said to. A plain save left with nowhere to go — the Save
    /// As it followed failed — goes with it.
    fn issue(
        &mut self,
        view: &View,
        mut chained: Vec<Asked<H>>,
        then_close: bool,
    ) -> Option<SaveStep<H>> {
        let then_close = then_close || chained.iter().any(|asked| asked.then_close);
        while !chained.is_empty() {
            let asked = chained.remove(0);
            let Some((target, rename)) = self.destination(view, asked.save_as) else {
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
    /// user chose is left as the chooser made it.
    pub fn close(&mut self, view: &View) -> Vec<SaveJob<H>> {
        self.pick = None;
        let Some(saving) = self.saving.take() else {
            return Vec::new();
        };
        // A plain save goes where the last Save As ahead of it goes.
        let mut plain = if saving.renames {
            Some(saving.target)
        } else {
            self.writable(view).cloned()
        };
        saving
            .next
            .into_iter()
            .filter_map(|asked| {
                let (target, rename) = match asked.save_as {
                    Some((target, name)) => {
                        plain = Some(Arc::clone(&target));
                        (target, Some(name))
                    }
                    None => (Arc::clone(plain.as_ref()?), None),
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
#[path = "file_tests.rs"]
mod tests;
